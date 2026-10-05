use crate::{
    Annotation, Argument, Binding, Call, Diagnostic, Expression, Name, Parameter, Script,
    SourceText, Span, Statement, Template, Token, TokenKind as K, Value, lex,
};

#[derive(Clone, Debug)]
pub struct Parsed {
    pub script: Script,
    pub diagnostics: Vec<Diagnostic>,
}

pub fn parse(source: &SourceText) -> Parsed {
    parse_with_calculation(source, crate::calc::Package::standard())
}
pub fn parse_with_calculation(
    source: &SourceText,
    package: std::sync::Arc<crate::calc::Package>,
) -> Parsed {
    let lexed = lex(source);
    Parser {
        origin: std::sync::Arc::new(source.clone()),
        package,
        tokens: lexed.tokens,
        diagnostics: lexed.diagnostics,
        index: 0,
        depth: 0,
    }
    .script(source.byte_len())
}

struct Parser {
    origin: std::sync::Arc<SourceText>,
    package: std::sync::Arc<crate::calc::Package>,
    tokens: Vec<Token>,
    diagnostics: Vec<Diagnostic>,
    index: usize,
    depth: usize,
}

impl Parser {
    fn peek(&self) -> &Token {
        &self.tokens[self.index]
    }
    fn at(&self, kind: K) -> bool {
        self.peek().kind == kind
    }
    fn advance(&mut self) -> Token {
        let token = self.peek().clone();
        if token.kind != K::Eof {
            self.index += 1;
        }
        token
    }
    fn previous(&self) -> &Token {
        &self.tokens[self.index.saturating_sub(1)]
    }
    fn span_from(&self, start: usize) -> Span {
        Span::new(start, self.previous().span.end())
            .expect("parser consumes tokens in source order")
    }
    fn error(&mut self, code: &'static str, span: Span, message: impl Into<String>) {
        self.diagnostics
            .push(Diagnostic::error(code, span, message));
    }
    fn script(mut self, length: usize) -> Parsed {
        let mut statements = Vec::new();
        while !self.at(K::Eof) {
            if self.at(K::Newline) {
                self.advance();
                continue;
            }
            if let Some(statement) = self.pipeline() {
                statements.push(statement);
            }
            while !self.at(K::Eof) && !self.at(K::Newline) {
                self.advance();
            }
        }
        Parsed {
            script: Script {
                statements,
                span: Span::new(0, length).expect("source length is nonnegative"),
            },
            diagnostics: self.diagnostics,
        }
    }
    fn pipeline(&mut self) -> Option<Statement> {
        let first = self.statement()?;
        let start = first.span.start();
        let mut stages = vec![first];
        loop {
            // A leading pipe on the following line continues the same declaration.
            let mut next = self.index;
            while self.tokens[next].kind == K::Newline {
                next += 1;
            }
            if self.tokens[next].kind != K::Pipe {
                break;
            }
            self.index = next;
            self.advance();
            while self.at(K::Newline) {
                self.advance();
            }
            if stages.len() >= 1000 {
                self.error(
                    "PIP001",
                    self.peek().span,
                    "a pipeline is limited to 1000 stages",
                );
                return None;
            }
            stages.push(self.statement()?);
        }
        let annotation = stages[0]
            .annotations
            .iter()
            .position(|a| a.name.text == "sandbox");
        if let Some(index) = annotation {
            let annotation = stages[0].annotations.remove(index);
            if !annotation.targets.is_empty() {
                self.error("SBX001", annotation.span, "@sandbox takes no arguments");
            }
        }
        let mut statement = if stages.len() == 1 {
            stages.pop().expect("one stage")
        } else {
            Statement {
                annotations: vec![],
                expression: Expression::Pipeline(stages),
                binding: None,
                error_binding: None,
                span: self.span_from(start),
            }
        };
        if annotation.is_some() {
            let binding = match &mut statement.expression {
                Expression::Pipeline(stages) => stages.last_mut().and_then(|s| s.binding.take()),
                _ => statement.binding.take(),
            };
            let result = Binding {
                name: Name {
                    text: "result".into(),
                    span: statement.span,
                },
                span: statement.span,
            };
            match &mut statement.expression {
                Expression::Pipeline(stages) => {
                    stages.last_mut().expect("nonempty pipeline").binding = Some(result)
                }
                _ => statement.binding = Some(result),
            }
            statement = Statement {
                annotations: vec![],
                expression: Expression::Sandbox(vec![statement]),
                binding,
                error_binding: None,
                span: self.span_from(start),
            };
        }
        Some(statement)
    }
    fn statement(&mut self) -> Option<Statement> {
        let start = self.peek().span.start();
        let mut annotations = Vec::new();
        while self.at(K::At) {
            annotations.push(self.annotation()?);
        }
        let expression = match self.peek().kind {
            K::Calc => {
                let source = token_name(self.advance());
                if let Err(error) = crate::calc::parse_context(
                    &source.text,
                    source.span.start(),
                    self.package.clone(),
                ) {
                    self.diagnostics.push(error);
                }
                Expression::Calculation(crate::CalculationSource {
                    text: source.text,
                    span: source.span,
                    origin: self.origin.clone(),
                })
            }
            K::Def => Expression::Definition(self.definition()?),
            K::Ref => Expression::Reference(token_name(self.advance())),
            K::Meta
                if self
                    .tokens
                    .get(self.index + 1)
                    .is_some_and(|t| t.kind == K::Word && t.text == "sandbox") =>
            {
                self.sandbox()?
            }
            K::Meta
                if self
                    .tokens
                    .get(self.index + 1)
                    .is_some_and(|t| t.kind == K::Word && t.text == "fork") =>
            {
                self.fork()?
            }
            K::Meta | K::Word => Expression::Call(self.call()?),
            _ => {
                self.error("PAR001", self.peek().span, "expected a command");
                return None;
            }
        };
        let binding = self.binding(K::Bind);
        let error_binding = self.binding(K::ErrorBind);
        if !matches!(self.peek().kind, K::Newline | K::Eof | K::Pipe)
            && !(self.depth > 0 && self.at(K::BraceClose))
        {
            self.error(
                "PAR004",
                self.peek().span,
                "unexpected token after the command",
            );
        }
        Some(Statement {
            annotations,
            expression,
            binding,
            error_binding,
            span: self.span_from(start),
        })
    }
    fn sandbox(&mut self) -> Option<Expression> {
        self.advance();
        self.advance();
        if self.depth >= 32 || !self.at(K::BraceOpen) {
            self.error(
                "SBX001",
                self.peek().span,
                "sandbox requires a block; nesting is limited to 32",
            );
            return None;
        }
        self.depth += 1;
        self.advance();
        let mut statements = vec![];
        loop {
            while self.at(K::Newline) {
                self.advance();
            }
            if self.at(K::BraceClose) {
                break;
            }
            if self.at(K::Eof) || statements.len() >= 1000 {
                self.error(
                    "SBX001",
                    self.peek().span,
                    "sandbox must close with '}'; at most 1000 statements",
                );
                return None;
            }
            statements.push(self.pipeline()?);
            if !self.at(K::Newline) && !self.at(K::BraceClose) {
                self.error(
                    "SBX001",
                    self.peek().span,
                    "sandbox statements must be separated by a newline",
                );
                return None;
            }
        }
        self.advance();
        self.depth -= 1;
        if statements.is_empty() {
            self.error("SBX001", self.previous().span, "sandbox requires a program");
            return None;
        }
        Some(Expression::Sandbox(statements))
    }
    fn fork(&mut self) -> Option<Expression> {
        use crate::{Branch, BranchSelector};
        self.advance();
        self.advance();
        if self.depth >= 32 || !self.at(K::BraceOpen) {
            self.error(
                "PIP004",
                self.peek().span,
                "fork requires a block; nesting is limited to 32",
            );
            return None;
        }
        self.depth += 1;
        self.advance();
        let mut branches = vec![];
        loop {
            while self.at(K::Newline) {
                self.advance();
            }
            if self.at(K::BraceClose) {
                break;
            }
            let start = self.peek().span.start();
            let selector = if self.at(K::Word) && self.peek().text == "on" {
                self.advance();
                let token = self.advance();
                match (token.kind, token.text.as_str()) {
                    (K::Word, "success") => BranchSelector::Success,
                    (K::Word, "failed") => BranchSelector::Failed,
                    (K::Word, "cancelled") => BranchSelector::Cancelled,
                    _ => {
                        self.error(
                            "PIP004",
                            token.span,
                            "expected success, failed or cancelled",
                        );
                        return None;
                    }
                }
            } else if self.at(K::Word) && self.peek().text == "when" {
                self.advance();
                if !self.at(K::Word) {
                    self.error(
                        "PIP004",
                        self.peek().span,
                        "when requires a predicate definition name",
                    );
                    return None;
                }
                BranchSelector::When(token_name(self.advance()))
            } else {
                BranchSelector::Value
            };
            if !self.at(K::BraceOpen) || branches.len() >= 64 {
                self.error(
                    "PIP004",
                    self.peek().span,
                    "expected a branch block; at most 64 branches per fork",
                );
                return None;
            }
            self.advance();
            while self.at(K::Newline) {
                self.advance();
            }
            let body = self.pipeline()?;
            while self.at(K::Newline) {
                self.advance();
            }
            if !self.at(K::BraceClose) {
                self.error(
                    "PIP004",
                    self.peek().span,
                    "a branch contains one pipeline and must close with '}'",
                );
                return None;
            }
            self.advance();
            branches.push(Branch {
                selector,
                body,
                span: self.span_from(start),
            });
        }
        self.advance();
        self.depth -= 1;
        if branches.is_empty() {
            self.error(
                "PIP004",
                self.previous().span,
                "fork requires at least one branch",
            );
            return None;
        }
        Some(Expression::Fork(branches))
    }
    fn annotation(&mut self) -> Option<Annotation> {
        let marker = self.advance();
        let start = marker.span.start();
        if !self.at(K::Word) || self.peek().span.start() != marker.span.end() {
            self.error("PAR005", marker.span, "expected a name straight after '@'");
            return None;
        }
        let name = token_name(self.advance());
        let mut targets = Vec::new();
        if self.at(K::BraceOpen) || self.at(K::ParenOpen) {
            let closing = if self.at(K::ParenOpen) {
                K::ParenClose
            } else {
                K::BraceClose
            };
            self.advance();
            while self.peek().kind != closing && !matches!(self.peek().kind, K::Newline | K::Eof) {
                match self.peek().kind {
                    K::Comma => {
                        self.advance();
                    }
                    K::Word => targets.push(token_name(self.advance())),
                    _ => {
                        self.error(
                            "PAR004",
                            self.peek().span,
                            "unexpected token inside annotation",
                        );
                        return None;
                    }
                }
            }
            if !self.at(closing) {
                self.error(
                    "PAR007",
                    name.span,
                    format!("'@{}' was never closed", name.text),
                );
                return None;
            }
            self.advance();
        }
        Some(Annotation {
            name,
            targets,
            span: self.span_from(start),
        })
    }
    fn definition(&mut self) -> Option<Template> {
        let start = self.advance().span.start();
        if !self.at(K::Word) {
            return self.bad_definition("expected a template name");
        }
        let name = token_name(self.advance());
        let mut parameters = Vec::new();
        if self.at(K::Signature) {
            let signature = self.advance();
            let mut depth: i32 = 0;
            let mut from = 0;
            for (i, ch) in signature
                .text
                .char_indices()
                .chain([(signature.text.len(), ',')])
            {
                if ch == '<' {
                    depth += 1;
                }
                if ch == '>' {
                    depth -= 1;
                }
                if depth < 0 {
                    return self.bad_definition("unbalanced type expression");
                }
                if ch != ',' || depth != 0 {
                    continue;
                }
                let part = signature.text[from..i].trim_matches(|ch| ch <= '\u{20}');
                if part.is_empty() && signature.text.chars().all(crate::lexer::whitespace) {
                    break;
                }
                let Some((key, value)) = part.split_once(':') else {
                    return self.bad_definition("expected 'parameter: Type' in template signature");
                };
                if key.is_empty() || value.chars().all(crate::lexer::whitespace) {
                    return self.bad_definition("expected 'parameter: Type' in template signature");
                }
                parameters.push(Parameter {
                    name: key.trim_matches(|ch| ch <= '\u{20}').into(),
                    type_expression: value.trim_matches(|ch| ch <= '\u{20}').into(),
                    span: Span::new(
                        signature.span.start() + 1 + from,
                        signature.span.start() + 1 + i,
                    )
                    .expect("signature offsets are ordered"),
                });
                from = i + 1;
            }
            if depth != 0 {
                return self.bad_definition("unbalanced type expression");
            }
        }
        let output = if self.at(K::ReturnType) {
            Some(token_name(self.advance()))
        } else {
            None
        };
        if !self.at(K::Word) || self.peek().text != "as" {
            return self.bad_definition("expected 'as' before the template body");
        }
        self.advance();
        if !matches!(self.peek().kind, K::Word | K::Meta | K::Calc) {
            return self.bad_definition("expected one command as the template body");
        }
        let body = if self.at(K::Calc) {
            if output.is_none() {
                return self
                    .bad_definition("calc definitions require an output contract (-> Type)");
            }
            let source = token_name(self.advance());
            if let Err(error) =
                crate::calc::parse_context(&source.text, source.span.start(), self.package.clone())
            {
                self.diagnostics.push(error);
            }
            crate::TemplateBody::Calculation(crate::CalculationSource {
                text: source.text,
                span: source.span,
                origin: self.origin.clone(),
            })
        } else {
            if output.is_some() {
                return self.bad_definition("output contracts require a calc body");
            }
            crate::TemplateBody::Call(self.call()?)
        };
        Some(Template {
            name,
            parameters,
            output,
            body,
            span: self.span_from(start),
        })
    }
    fn bad_definition(&mut self, message: &str) -> Option<Template> {
        self.error("PAR008", self.peek().span, message);
        None
    }
    fn call(&mut self) -> Option<Call> {
        let start = self.peek().span.start();
        let marker = if self.at(K::Meta) {
            let span = self.advance().span;
            if !self.at(K::Word) {
                self.error("PAR002", span, "expected a command name after ':'");
                return None;
            }
            Some(span)
        } else {
            None
        };
        let mut path = Vec::new();
        while self.at(K::Word) {
            path.push(token_name(self.advance()));
        }
        let mut operands = Vec::new();
        let mut arguments = Vec::new();
        while !matches!(
            self.peek().kind,
            K::Bind | K::ErrorBind | K::Pipe | K::Newline | K::Eof
        ) {
            if self.at(K::Key) {
                let key = token_name(self.advance());
                if self.value_start() {
                    let value = self.argument_value()?;
                    let span = key.span.union(value.name().span);
                    arguments.push(Argument { key, value, span });
                }
            } else if self.value_start() {
                operands.push(self.argument_value()?);
            } else {
                break;
            }
        }
        if arguments
            .iter()
            .map(|a| a.value.node_count())
            .chain(operands.iter().map(Value::node_count))
            .sum::<usize>()
            > crate::structured::MAX_NODES
        {
            self.error(
                "ARG001",
                self.span_from(start),
                "Command arguments exceed their total node budget.",
            );
            return None;
        }
        Some(Call {
            marker,
            path,
            operands,
            arguments,
            span: self.span_from(start),
        })
    }
    fn binding(&mut self, kind: K) -> Option<Binding> {
        if !self.at(kind) {
            return None;
        }
        let operator = self.advance();
        if !self.at(K::Word) {
            self.error(
                "PAR003",
                operator.span,
                "expected a name after the binding operator",
            );
            return None;
        }
        let name = token_name(self.advance());
        if !crate::binding_name(&name.text) {
            self.error("PAR003", name.span, "Binding names may contain only letters, digits and underscores; use error_rate instead of error-rate.");
            return None;
        }
        Some(Binding {
            span: operator.span.union(name.span),
            name,
        })
    }
    fn value_start(&self) -> bool {
        matches!(
            self.peek().kind,
            K::Word | K::String | K::Ref | K::Structured
        )
    }
    fn argument_value(&mut self) -> Option<Value> {
        let token = self.advance();
        if token.kind != K::Structured {
            return Some(value(token));
        }
        match crate::structured::parse(&token.text, token.span.start()) {
            Ok(value) => Some(value),
            Err(error) => {
                self.diagnostics.push(error);
                None
            }
        }
    }
}

fn token_name(token: Token) -> Name {
    Name {
        text: token.text,
        span: token.span,
    }
}
fn value(token: Token) -> Value {
    match token.kind {
        K::String => Value::Text(token_name(token)),
        K::Ref => Value::Reference(token_name(token)),
        _ => Value::Word(token_name(token)),
    }
}
