use super::{
    Keyword, Package,
    ast::*,
    lexer::{self, Kind, Token},
};
use crate::{Diagnostic, Name, Span};
use std::sync::Arc;
use wes_core::Data;

pub fn parse_body(
    source: &str,
    offset: usize,
    package: Arc<Package>,
) -> Result<Program, Diagnostic> {
    let tokens = lexer::lex(source, offset, &package)?;
    let span = Span::new(offset, offset + source.len()).expect("lexer checked range");
    let mut parser = Parser {
        tokens,
        at: 0,
        depth: 0,
        program: Program {
            requires_pure: false,
            source: source.into(),
            origin: Arc::new(crate::SourceText::new("calculation", source)),
            origin_offset: offset,
            package,
            span,
            expressions: vec![],
            statements: vec![],
            functions: vec![],
            root: 0,
        },
    };
    let mut statements = vec![];
    while !parser.end() {
        statements.push(parser.statement()?);
    }
    parser.program.root = parser.stmt(StmtKind::Block(statements), span)?;
    Ok(parser.program)
}
pub fn parse_context(
    source: &str,
    offset: usize,
    package: Arc<Package>,
) -> Result<Program, Diagnostic> {
    if offset.checked_add(source.len()).is_none() {
        return Err(Diagnostic::error(
            "CAL001",
            Span::at(offset),
            "source offset exceeds range",
        ));
    }
    let (body_start, end) = lexer::context_bounds(source, 0).map_err(|mut error| {
        error.span =
            Span::new(offset + error.span.start(), offset + error.span.end()).expect("ordered");
        error
    })?;
    if !source[end..].trim().is_empty() {
        return Err(lexer::error(
            offset + end,
            offset + source.len(),
            "unexpected text after calculation block",
        ));
    }
    let mut program = parse_body(&source[body_start..end - 1], offset + body_start, package)?;
    program.origin = Arc::new(crate::SourceText::new("calculation", source));
    program.origin_offset = offset;
    program.requires_pure = lexer::context_requires_pure(source, 0)?;
    Ok(program)
}
struct Parser {
    tokens: Vec<Token>,
    at: usize,
    depth: usize,
    program: Program,
}
impl Parser {
    fn peek(&self) -> &Token {
        &self.tokens[self.at]
    }
    fn end(&self) -> bool {
        self.peek().kind == Kind::End
    }
    fn next(&mut self) -> Token {
        let token = self.peek().clone();
        if !self.end() {
            self.at += 1;
        }
        token
    }
    fn is(&self, s: &str) -> bool {
        matches!(&self.peek().kind,Kind::Symbol(v) if v==s)
    }
    fn eat(&mut self, s: &str) -> bool {
        if self.is(s) {
            self.next();
            true
        } else {
            false
        }
    }
    fn need(&mut self, s: &str) -> Result<(), Diagnostic> {
        if self.eat(s) {
            Ok(())
        } else {
            Err(self.problem(&format!("expected '{s}'")))
        }
    }
    fn problem(&self, message: &str) -> Diagnostic {
        Diagnostic::error("CAL001", self.peek().span, message)
    }
    fn keyword(&self, k: Keyword) -> bool {
        matches!(&self.peek().kind,Kind::Word(w) if self.program.package.keyword(w)==Some(k))
    }
    fn name(&mut self) -> Result<Name, Diagnostic> {
        let t = self.next();
        match t.kind {
            Kind::Word(text)
                if self.program.package.keyword(&text).is_none()
                    && !matches!(text.as_str(), "true" | "false" | "none") =>
            {
                Ok(Name { text, span: t.span })
            }
            _ => Err(Diagnostic::error("CAL001", t.span, "expected a local name")),
        }
    }
    fn span(&self, start: usize) -> Span {
        Span::new(start, self.tokens[self.at.saturating_sub(1)].span.end()).expect("ordered")
    }
    fn budget(&self) -> Result<(), Diagnostic> {
        if self.program.expressions.len()
            + self.program.statements.len()
            + self.program.functions.len()
            >= 100_000
        {
            Err(self.problem("calculation AST exceeds limit"))
        } else {
            Ok(())
        }
    }
    fn expr(&mut self, kind: ExprKind, span: Span) -> Result<ExprId, Diagnostic> {
        self.budget()?;
        let id = self.program.expressions.len();
        self.program.expressions.push(Expr { kind, span });
        Ok(id)
    }
    fn stmt(&mut self, kind: StmtKind, span: Span) -> Result<StmtId, Diagnostic> {
        self.budget()?;
        let id = self.program.statements.len();
        self.program.statements.push(Stmt { kind, span });
        Ok(id)
    }
    fn enter(&mut self) -> Result<(), Diagnostic> {
        self.depth += 1;
        if self.depth > 64 {
            Err(self.problem("calculation nesting exceeds 64"))
        } else {
            Ok(())
        }
    }
    fn semi(&mut self) -> Result<(), Diagnostic> {
        if self.eat(";") || self.is("}") || self.end() {
            Ok(())
        } else {
            Err(self.problem(if self.is(":") { "expected a statement, not a record field; braces after => start a block. Return a record with => ({field: value}) or { return {field: value}; }" } else { "expected ';'" }))
        }
    }
    fn statement(&mut self) -> Result<StmtId, Diagnostic> {
        self.enter()?;
        let r = self.statement_inner();
        self.depth -= 1;
        r
    }
    fn statement_inner(&mut self) -> Result<StmtId, Diagnostic> {
        let start = self.peek().span.start();
        let kind = if self.eat("{") {
            let mut body = vec![];
            while !self.is("}") && !self.end() {
                body.push(self.statement()?);
            }
            self.need("}")?;
            StmtKind::Block(body)
        } else if self.keyword(Keyword::Const) || self.keyword(Keyword::Let) {
            let mutable = self.keyword(Keyword::Let);
            self.next();
            let name = self.name()?;
            if !self.eat("=") {
                return Err(self.problem("binding declarations require an initial value: use let name = value or const name = value"));
            }
            let value = self.expression(0)?;
            self.semi()?;
            StmtKind::Binding {
                name,
                mutable,
                value,
            }
        } else if self.keyword(Keyword::Function) {
            self.next();
            let name = self.name()?;
            let value = self.function(Some(name.clone()), start)?;
            StmtKind::Binding {
                name,
                mutable: false,
                value,
            }
        } else if self.keyword(Keyword::Return) {
            self.next();
            let value = self.expression(0)?;
            self.semi()?;
            StmtKind::Return(value)
        } else if self.keyword(Keyword::If) {
            self.next();
            self.need("(")?;
            let condition = self.expression(0)?;
            self.need(")")?;
            let yes = self.statement()?;
            let no = if self.keyword(Keyword::Else) {
                self.next();
                Some(self.statement()?)
            } else {
                None
            };
            StmtKind::If { condition, yes, no }
        } else if self.keyword(Keyword::While) {
            self.next();
            self.need("(")?;
            let condition = self.expression(0)?;
            self.need(")")?;
            let body = self.statement()?;
            StmtKind::While { condition, body }
        } else if self.keyword(Keyword::For) {
            self.next();
            self.need("(")?;
            let mutable = self.keyword(Keyword::Let);
            if !mutable && !self.keyword(Keyword::Const) {
                return Err(self.problem("for requires a local const/let binding"));
            }
            self.next();
            let name = self.name()?;
            if !self.keyword(Keyword::Of) {
                return Err(self.problem("expected 'of'"));
            }
            self.next();
            let values = self.expression(0)?;
            self.need(")")?;
            let body = self.statement()?;
            StmtKind::For {
                name,
                mutable,
                values,
                body,
            }
        } else if self.keyword(Keyword::Break) || self.keyword(Keyword::Continue) {
            let stop = self.keyword(Keyword::Break);
            self.next();
            self.semi()?;
            if stop {
                StmtKind::Break
            } else {
                StmtKind::Continue
            }
        } else if self.eat(";") {
            StmtKind::Block(vec![])
        } else {
            let value = self.expression(0)?;
            self.semi()?;
            StmtKind::Expression(value)
        };
        self.stmt(kind, self.span(start))
    }
    fn parameters(&mut self) -> Result<Vec<Name>, Diagnostic> {
        self.need("(")?;
        let mut params = vec![];
        if !self.is(")") {
            loop {
                params.push(self.name()?);
                if params.len() > 256 {
                    return Err(self.problem("too many parameters"));
                }
                if !self.eat(",") {
                    break;
                }
            }
        }
        self.need(")")?;
        Ok(params)
    }
    fn function(&mut self, name: Option<Name>, start: usize) -> Result<ExprId, Diagnostic> {
        let parameters = self.parameters()?;
        if !self.is("{") {
            return Err(self.problem("function requires a block"));
        }
        let body = self.statement()?;
        self.make_function(name, parameters, body, start)
    }
    fn make_function(
        &mut self,
        name: Option<Name>,
        parameters: Vec<Name>,
        body: StmtId,
        start: usize,
    ) -> Result<ExprId, Diagnostic> {
        self.budget()?;
        let id = self.program.functions.len();
        self.program.functions.push(Function {
            name,
            parameters,
            body,
            span: self.span(start),
        });
        self.expr(ExprKind::Function(id), self.span(start))
    }
    fn arrow_body(&mut self, parameters: Vec<Name>, start: usize) -> Result<ExprId, Diagnostic> {
        self.need("=>")?;
        let body = if self.is("{") {
            self.statement()?
        } else {
            let value = self.expression(0)?;
            self.stmt(StmtKind::Return(value), self.span(start))?
        };
        self.make_function(None, parameters, body, start)
    }
    fn is_arrow(&self) -> bool {
        let mut at = self.at + 1;
        if matches!(&self.tokens.get(at).map(|t|&t.kind),Some(Kind::Symbol(s)) if s==")") {
            at += 1;
        } else {
            loop {
                if !matches!(self.tokens.get(at).map(|t| &t.kind), Some(Kind::Word(_))) {
                    return false;
                }
                at += 1;
                match self.tokens.get(at).map(|t| &t.kind) {
                    Some(Kind::Symbol(s)) if s == "," => at += 1,
                    Some(Kind::Symbol(s)) if s == ")" => {
                        at += 1;
                        break;
                    }
                    _ => return false,
                }
            }
        }
        matches!(self.tokens.get(at).map(|t|&t.kind),Some(Kind::Symbol(s)) if s=="=>")
    }
    fn expression(&mut self, min: u8) -> Result<ExprId, Diagnostic> {
        self.enter()?;
        let r = self.expression_inner(min);
        self.depth -= 1;
        r
    }
    fn expression_inner(&mut self, min: u8) -> Result<ExprId, Diagnostic> {
        let start = self.peek().span.start();
        let mut left = self.prefix()?;
        loop {
            if self.eat("(") {
                let mut args = vec![];
                if !self.is(")") {
                    loop {
                        args.push(self.expression(0)?);
                        if args.len() > 256 {
                            return Err(self.problem("too many arguments"));
                        }
                        if !self.eat(",") {
                            break;
                        }
                    }
                }
                self.need(")")?;
                left = self.expr(ExprKind::Call(left, args), self.span(start))?;
            } else if self.eat("[") {
                let index = self.expression(0)?;
                self.need("]")?;
                left = self.expr(ExprKind::Index(left, index), self.span(start))?;
            } else if self.eat(".") {
                let name = self.name()?;
                left = self.expr(ExprKind::Field(left, name.text), self.span(start))?;
            } else {
                break;
            }
        }
        loop {
            if min == 0 && self.eat("=") {
                let ExprKind::Name(name) = self.program.expressions[left].kind.clone() else {
                    return Err(self.problem("only local let bindings may be assigned; record fields and list items are immutable values. Build a new record with withFields or a new list with slice/concat, then rebind the local let variable"));
                };
                let right = self.expression(0)?;
                left = self.expr(ExprKind::Assign(name, right), self.span(start))?;
                continue;
            }
            let Kind::Symbol(symbol) = &self.peek().kind else {
                break;
            };
            let Some(spec) = self.program.package.binary(symbol) else {
                break;
            };
            if spec.precedence < min {
                break;
            }
            self.next();
            let right = self.expression(spec.precedence + 1)?;
            left = self.expr(
                ExprKind::Binary(spec.operation, left, right),
                self.span(start),
            )?;
        }
        Ok(left)
    }
    fn prefix(&mut self) -> Result<ExprId, Diagnostic> {
        let start = self.peek().span.start();
        if self.is("(") && self.is_arrow() {
            let parameters = self.parameters()?;
            return self.arrow_body(parameters, start);
        }
        if self.keyword(Keyword::Function) {
            self.next();
            let name = if self.is("(") {
                None
            } else {
                Some(self.name()?)
            };
            return self.function(name, start);
        }
        let t = self.next();
        let kind = match t.kind {
            Kind::Word(name) => {
                if self.is("=>") {
                    return self.arrow_body(
                        vec![Name {
                            text: name,
                            span: t.span,
                        }],
                        start,
                    );
                }
                match name.as_str() {
                    "true" => ExprKind::Literal(Data::Bool(true)),
                    "false" => ExprKind::Literal(Data::Bool(false)),
                    "none" => ExprKind::Literal(Data::Option(None)),
                    _ => {
                        if self.program.package.keyword(&name).is_some() {
                            return Err(Diagnostic::error(
                                "CAL001",
                                t.span,
                                "unexpected keyword in expression",
                            ));
                        }
                        ExprKind::Name(name)
                    }
                }
            }
            Kind::Reference(name) => ExprKind::Workspace(name),
            Kind::Text(text) => ExprKind::Literal(Data::Text(text.into())),
            Kind::Number(number) => ExprKind::Literal(number_data(&number, t.span)?),
            Kind::Symbol(s) if s == "(" => {
                let expression = self.expression(0)?;
                self.need(")")?;
                return Ok(expression);
            }
            Kind::Symbol(s) if s == "[" => {
                let mut items = vec![];
                if !self.is("]") {
                    loop {
                        items.push(self.expression(0)?);
                        if !self.eat(",") || self.is("]") {
                            break;
                        }
                    }
                }
                self.need("]")?;
                ExprKind::List(items)
            }
            Kind::Symbol(s) if s == "{" => {
                let mut fields = vec![];
                if !self.is("}") {
                    loop {
                        let key = self.next();
                        let name = match key.kind {
                            Kind::Word(n) | Kind::Text(n) => n,
                            _ => {
                                return Err(Diagnostic::error(
                                    "CAL001",
                                    key.span,
                                    "expected record field name",
                                ));
                            }
                        };
                        self.need(":")?;
                        fields.push((name, self.expression(0)?));
                        if !self.eat(",") || self.is("}") {
                            break;
                        }
                    }
                }
                self.need("}")?;
                ExprKind::Record(fields)
            }
            Kind::Symbol(s) if matches!(s.as_str(), "-" | "+" | "!") => {
                if s == "-"
                    && let Kind::Number(n) = self.peek().kind.clone()
                {
                    let span = t.span.union(self.next().span);
                    return self.expr(
                        ExprKind::Literal(number_data(&format!("-{n}"), span)?),
                        span,
                    );
                }
                let op = match s.as_str() {
                    "-" => Unary::Negate,
                    "+" => Unary::Positive,
                    _ => Unary::Not,
                };
                ExprKind::Unary(op, self.expression(33)?)
            }
            _ => {
                return Err(Diagnostic::error(
                    "CAL001",
                    t.span,
                    "expected calculation expression",
                ));
            }
        };
        self.expr(kind, self.span(start))
    }
}
fn number_data(text: &str, span: Span) -> Result<Data, Diagnostic> {
    if text.contains(['.', 'e', 'E']) {
        text.parse()
            .map(Data::Decimal)
            .map_err(|_| Diagnostic::error("CAL005", span, "invalid decimal literal"))
    } else {
        text.parse().map(Data::Int).map_err(|_| {
            Diagnostic::error(
                "CAL005",
                span,
                "integer literal exceeds signed 64-bit range",
            )
        })
    }
}
