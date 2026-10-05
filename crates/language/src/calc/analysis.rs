use super::*;
use crate::{Diagnostic, Name, Span};
use indexmap::IndexMap;
use std::{collections::BTreeSet, sync::Arc};
use wes_core::{
    Data, Primitive, RecordShape, Shape,
    capability::{Capability, Catalogue, ProviderDescription},
    contracts::{Contract, ContractRegistry},
};

pub type Symbol = usize;
#[derive(Clone, Copy, Debug)]
pub enum Resolution {
    Local(Symbol),
    Operation(OperationSpec),
    IterNamespace,
}
#[derive(Clone, Debug)]
pub struct CallSelection {
    pub provider: Arc<ProviderDescription>,
    pub capability: Arc<Capability>,
}
#[derive(Clone, Debug, Default)]
pub struct FunctionSymbols {
    pub parameters: Vec<Symbol>,
    pub own_name: Option<Symbol>,
}
#[derive(Clone, Debug)]
pub struct PurityAnalysis {
    /// Definition-source spans stay internal; public inspect uses only the safe summary.
    pub external_operations: Vec<Span>,
}
#[derive(Clone, Debug)]
pub struct Compiled {
    pub purity: PurityAnalysis,
    pub parameters: IndexMap<String, Symbol>,
    pub program: Arc<Program>,
    pub resolutions: IndexMap<ExprId, Resolution>,
    pub declarations: IndexMap<StmtId, Symbol>,
    pub functions: Vec<FunctionSymbols>,
    pub workspace: IndexMap<String, Shape>,
    pub calls: IndexMap<ExprId, CallSelection>,
    pub contracts: IndexMap<ExprId, Arc<Contract>>,
    pub shapes: Vec<Shape>,
    pub symbols: usize,
    pub mutable_symbols: BTreeSet<Symbol>,
    pub initializers: IndexMap<Symbol, ExprId>,
    pub iter_contracts: IndexMap<ExprId, wes_core::ContractCapture>,
    pub iter_recipes: IndexMap<ExprId, (wes_core::IterRecipe, wes_core::ContractCapture)>,
}
impl Compiled {
    /// A declaration prediction, never a promise based on a previous runtime value.
    /// Only reachable returns in this program participate; nested function returns do not.
    pub fn output_shape(&self) -> Shape {
        self.return_shape(self.program.root)
    }
    fn return_shape(&self, root: StmtId) -> Shape {
        fn returns(compiled: &Compiled, id: StmtId, shapes: &mut Vec<Shape>) -> bool {
            match &compiled.program.statements[id].kind {
                StmtKind::Return(value) => {
                    shapes.push(compiled.shapes[*value].clone());
                    true
                }
                StmtKind::Block(body) => {
                    for statement in body {
                        if returns(compiled, *statement, shapes) {
                            return true;
                        }
                    }
                    false
                }
                StmtKind::If { yes, no, .. } => {
                    let yes = returns(compiled, *yes, shapes);
                    let no = no.is_some_and(|no| returns(compiled, no, shapes));
                    yes && no
                }
                StmtKind::While { body, .. } | StmtKind::For { body, .. } => {
                    returns(compiled, *body, shapes);
                    false
                }
                // Break/continue may bypass later returns. Treat these paths conservatively.
                StmtKind::Break | StmtKind::Continue => {
                    shapes.push(Shape::Unknown);
                    true
                }
                _ => false,
            }
        }
        let mut shapes = Vec::new();
        let complete = returns(self, root, &mut shapes);
        if complete {
            common(shapes.iter())
        } else {
            Shape::Unknown
        }
    }
    pub fn effectful(&self) -> bool {
        !self.purity.external_operations.is_empty()
    }
    pub fn purity_reason(&self) -> &'static str {
        if self.effectful() {
            "A resolved operation may call an external provider, including in an unexecuted branch or helper."
        } else {
            "All resolved operations are local to captured immutable inputs; no external observation or provider call."
        }
    }
}
/// Metadata only: no callable handles, recording, filesystem or execution callbacks.
pub struct Environment<'a> {
    pub catalogue: &'a Catalogue,
    pub contracts: &'a ContractRegistry,
    pub workspace: &'a dyn Fn(&str) -> Option<Shape>,
}
#[derive(Clone)]
struct Binding {
    symbol: Symbol,
    mutable: bool,
    shape: Shape,
    arity: Option<usize>,
    depth: usize,
    initialized: bool,
}
struct Analyzer<'a> {
    compiled: Compiled,
    environment: Environment<'a>,
    scopes: Vec<IndexMap<String, Binding>>,
    loop_depth: usize,
    direct_callees: BTreeSet<ExprId>,
    function_depth: usize,
}
fn problem(span: Span, code: &'static str, message: impl Into<String>) -> Diagnostic {
    Diagnostic::error(code, span, message)
}
pub fn analyze(
    program: Arc<Program>,
    environment: Environment<'_>,
) -> Result<Compiled, Diagnostic> {
    analyze_with_parameters(program, environment, &IndexMap::new())
}

/// Explicit lexical inputs are separate from ambient workspace dependencies.
pub fn analyze_with_parameters(
    program: Arc<Program>,
    environment: Environment<'_>,
    parameters: &IndexMap<String, Shape>,
) -> Result<Compiled, Diagnostic> {
    let direct_callees = program
        .expressions
        .iter()
        .filter_map(|e| {
            if let ExprKind::Call(id, _) = e.kind {
                Some(id)
            } else {
                None
            }
        })
        .collect();
    let mut analyzer = Analyzer {
        compiled: Compiled {
            purity: PurityAnalysis {
                external_operations: vec![],
            },
            parameters: IndexMap::new(),
            shapes: vec![Shape::Unknown; program.expressions.len()],
            functions: vec![FunctionSymbols::default(); program.functions.len()],
            program,
            resolutions: IndexMap::new(),
            declarations: IndexMap::new(),
            workspace: IndexMap::new(),
            calls: IndexMap::new(),
            contracts: IndexMap::new(),
            symbols: 0,
            mutable_symbols: BTreeSet::new(),
            initializers: IndexMap::new(),
            iter_contracts: IndexMap::new(),
            iter_recipes: IndexMap::new(),
        },
        environment,
        scopes: vec![IndexMap::new()],
        loop_depth: 0,
        direct_callees,
        function_depth: 0,
    };
    for (name, shape) in parameters {
        let symbol = analyzer.declare(
            &Name {
                text: name.clone(),
                span: analyzer.compiled.program.span,
            },
            false,
            None,
        )?;
        analyzer.scopes[0].get_mut(name).expect("declared").shape = shape.clone();
        analyzer.compiled.parameters.insert(name.clone(), symbol);
    }
    analyzer.statement(analyzer.compiled.program.root)?;
    analyzer.compiled.purity.external_operations = analyzer
        .compiled
        .resolutions
        .iter()
        .filter_map(|(id, resolution)| {
            let expression = &analyzer.compiled.program.expressions[*id];
            (matches!(expression.kind, ExprKind::Call(..))
                && matches!(resolution, Resolution::Operation(spec) if spec.operation.effectful()))
            .then_some(expression.span)
        })
        .collect();
    if analyzer.compiled.program.requires_pure && analyzer.compiled.effectful() {
        return Err(problem(
            analyzer.compiled.purity.external_operations[0],
            "CAL009",
            "pure calculation contains a possible external provider call",
        ));
    }
    // Resolve every helper before checking callback purity. Dynamic receivers/callbacks
    // keep the machine checks; eager List callbacks intentionally allow local effects.
    for (id, resolution) in &analyzer.compiled.resolutions {
        let Resolution::Operation(spec) = resolution else {
            continue;
        };
        let ExprKind::Call(callee, args) = &analyzer.compiled.program.expressions[*id].kind else {
            continue;
        };
        let receiver = if let ExprKind::Field(receiver, _) =
            analyzer.compiled.program.expressions[*callee].kind
        {
            Some(receiver)
        } else {
            None
        };
        let lazy = matches!(
            spec.operation,
            Operation::Map | Operation::Filter | Operation::Reduce
        ) && receiver
            .or_else(|| args.first().copied())
            .is_some_and(|id| matches!(analyzer.compiled.shapes[id], Shape::Iter(_)));
        if !lazy && spec.operation != Operation::SortBy {
            continue;
        }
        if let Some(callback) = args.get(usize::from(receiver.is_none()))
            && let Some(function) = analyzer.compiled.callback_function(*callback)
            && !analyzer.compiled.pure_callback(function)
        {
            let message = if lazy {
                "lazy Iter callbacks must be pure and cannot capture mutable outer bindings; use for-of for effects"
            } else {
                "sortBy selector must be pure and cannot capture mutable outer bindings"
            };
            return Err(problem(
                analyzer.compiled.program.expressions[*id].span,
                "CAL009",
                message,
            ));
        }
    }
    analyzer.compiled.check_completion()?;
    Ok(analyzer.compiled)
}
impl Analyzer<'_> {
    fn binding(&self, name: &str) -> Option<&Binding> {
        self.scopes.iter().rev().find_map(|s| s.get(name))
    }
    fn declare(
        &mut self,
        name: &Name,
        mutable: bool,
        arity: Option<usize>,
    ) -> Result<Symbol, Diagnostic> {
        // Operations are ordinary fallback bindings. Only syntactic vocabulary and the
        // special iterator namespace are reserved; lexical bindings may shadow operations.
        if self.compiled.program.package.keyword(&name.text).is_some()
            || (name.text == "iter" && self.compiled.program.package.iter_namespace())
            || matches!(name.text.as_str(), "true" | "false" | "none")
        {
            return Err(problem(
                name.span,
                "CAL010",
                format!(
                    "local name {} conflicts with the language vocabulary; choose another name",
                    diagnostics::label(&name.text)
                ),
            ));
        }
        let scope = self.scopes.last_mut().expect("scope");
        if scope.contains_key(&name.text) {
            return Err(problem(name.span, "CAL010", "duplicate local declaration"));
        }
        let symbol = self.compiled.symbols;
        self.compiled.symbols += 1;
        if mutable {
            self.compiled.mutable_symbols.insert(symbol);
        }
        scope.insert(
            name.text.clone(),
            Binding {
                symbol,
                mutable,
                shape: Shape::Unknown,
                arity: if mutable { None } else { arity },
                depth: self.function_depth,
                initialized: true,
            },
        );
        Ok(symbol)
    }
    fn statement(&mut self, id: StmtId) -> Result<(), Diagnostic> {
        let statement = self.compiled.program.statements[id].clone();
        match statement.kind {
            StmtKind::Block(body) => {
                self.scopes.push(IndexMap::new());
                // Resolve lexical closures without hoisting initialization; runtime enforces TDZ.
                for id in &body {
                    if let StmtKind::Binding {
                        name,
                        mutable,
                        value,
                    } = &self.compiled.program.statements[*id].kind
                    {
                        let name = name.clone();
                        let mutable = *mutable;
                        let arity = if let ExprKind::Function(f) =
                            self.compiled.program.expressions[*value].kind
                        {
                            Some(self.compiled.program.functions[f].parameters.len())
                        } else {
                            None
                        };
                        let symbol = self.declare(&name, mutable, arity)?;
                        self.scopes
                            .last_mut()
                            .expect("scope")
                            .get_mut(&name.text)
                            .expect("declared")
                            .initialized = false;
                        self.compiled.declarations.insert(*id, symbol);
                    }
                }
                for id in body {
                    self.statement(id)?;
                }
                self.scopes.pop();
            }
            StmtKind::Binding {
                name,
                mutable,
                value,
            } => {
                if !self.compiled.declarations.contains_key(&id) {
                    let symbol = self.declare(&name, mutable, None)?;
                    self.compiled.declarations.insert(id, symbol);
                }
                self.compiled
                    .initializers
                    .insert(self.compiled.declarations[&id], value);
                let shape = self.expression(value)?;
                self.scopes
                    .last_mut()
                    .expect("scope")
                    .get_mut(&name.text)
                    .expect("declared")
                    .initialized = true;
                if !mutable {
                    self.scopes
                        .last_mut()
                        .expect("scope")
                        .get_mut(&name.text)
                        .expect("declaration")
                        .shape = shape;
                }
            }
            StmtKind::Expression(value) | StmtKind::Return(value) => {
                self.expression(value)?;
            }
            StmtKind::If { condition, yes, no } => {
                self.condition(condition)?;
                self.statement(yes)?;
                if let Some(no) = no {
                    self.statement(no)?;
                }
            }
            StmtKind::While { condition, body } => {
                self.condition(condition)?;
                self.loop_depth += 1;
                self.statement(body)?;
                self.loop_depth -= 1;
            }
            StmtKind::For {
                name,
                mutable,
                values,
                body,
            } => {
                let shape = self.expression(values)?;
                if !matches!(shape, Shape::Unknown | Shape::List(_) | Shape::Iter(_)) {
                    return Err(problem(
                        statement.span,
                        "CAL004",
                        format!(
                            "for-of requires List or Iter; received {}",
                            diagnostics::kind(&shape)
                        ),
                    ));
                }
                self.scopes.push(IndexMap::new());
                let symbol = self.declare(&name, mutable, None)?;
                self.compiled.declarations.insert(id, symbol);
                self.loop_depth += 1;
                self.statement(body)?;
                self.loop_depth -= 1;
                self.scopes.pop();
            }
            StmtKind::Break | StmtKind::Continue => {
                if self.loop_depth == 0 {
                    return Err(problem(
                        statement.span,
                        "CAL013",
                        "loop control is outside a loop in this function",
                    ));
                }
            }
        };
        Ok(())
    }
    fn condition(&mut self, id: ExprId) -> Result<(), Diagnostic> {
        let shape = self.expression(id)?;
        if shape != Shape::Unknown && shape != Shape::Primitive(Primitive::Bool) {
            Err(problem(
                self.compiled.program.expressions[id].span,
                "CAL004",
                format!(
                    "condition requires Bool; received {}",
                    diagnostics::kind(&shape)
                ),
            ))
        } else {
            Ok(())
        }
    }
    fn expression(&mut self, root: ExprId) -> Result<Shape, Diagnostic> {
        let mut pending = vec![(root, false)];
        while let Some((id, visited)) = pending.pop() {
            let expression = self.compiled.program.expressions[id].clone();
            if !visited {
                pending.push((id, true));
                let mut children = match &expression.kind {
                    ExprKind::Unary(_, x) | ExprKind::Assign(_, x) | ExprKind::Field(x, _) => {
                        vec![*x]
                    }
                    ExprKind::Binary(_, a, b) | ExprKind::Index(a, b) => vec![*a, *b],
                    ExprKind::List(items) => items.clone(),
                    ExprKind::Record(fields) => fields.iter().map(|(_, v)| *v).collect(),
                    ExprKind::Call(callee, args) => std::iter::once(*callee)
                        .chain(args.iter().copied())
                        .collect(),
                    _ => vec![],
                };
                children.reverse();
                pending.extend(children.into_iter().map(|id| (id, false)));
                continue;
            }
            let shape = match expression.kind {
                ExprKind::Literal(data) => literal_shape(&data),
                ExprKind::Name(name) => {
                    if name == "iter" && self.compiled.program.package.iter_namespace() {
                        if !self
                            .compiled
                            .program
                            .expressions
                            .iter()
                            .any(|e| matches!(e.kind,ExprKind::Field(receiver,_) if receiver==id))
                        {
                            return Err(problem(
                                expression.span,
                                "CAL002",
                                "Iter namespace requires a direct operation selection",
                            ));
                        }
                        self.compiled
                            .resolutions
                            .insert(id, Resolution::IterNamespace);
                        Shape::Unknown
                    } else if let Some(binding) = self.binding(&name).cloned() {
                        if binding.depth == self.function_depth && !binding.initialized {
                            return Err(problem(
                                expression.span,
                                "CAL013",
                                format!(
                                    "local {} is used before initialization; keep declarations before their use",
                                    diagnostics::label(&name)
                                ),
                            ));
                        }
                        self.compiled
                            .resolutions
                            .insert(id, Resolution::Local(binding.symbol));
                        binding.shape
                    } else if let Some(spec) = self.compiled.program.package.operation(&name) {
                        if matches!(
                            spec.operation,
                            Operation::Call | Operation::Check | Operation::ParseJson
                        ) && !self.direct_callees.contains(&id)
                        {
                            return Err(problem(
                                expression.span,
                                "CAL002",
                                "metadata operations must be called directly, not aliased",
                            ));
                        }
                        self.compiled
                            .resolutions
                            .insert(id, Resolution::Operation(spec));
                        Shape::Unknown
                    } else {
                        return Err(problem(
                            expression.span,
                            "CAL010",
                            format!("undefined local '{name}'"),
                        ));
                    }
                }
                ExprKind::Workspace(name) => {
                    let shape = (self.environment.workspace)(&name).ok_or_else(|| {
                        problem(
                            expression.span,
                            "CAL010",
                            format!("unknown workspace output '${name}'"),
                        )
                    })?;
                    self.compiled.workspace.insert(name, shape.clone());
                    shape
                }
                ExprKind::Function(function) => {
                    let f = self.compiled.program.functions[function].clone();
                    let old_loop = self.loop_depth;
                    self.loop_depth = 0;
                    self.function_depth += 1;
                    self.scopes.push(IndexMap::new());
                    let own_name = if let Some(name) = &f.name {
                        Some(self.declare(name, false, Some(f.parameters.len()))?)
                    } else {
                        None
                    };
                    let mut parameters = vec![];
                    for name in &f.parameters {
                        parameters.push(self.declare(name, true, None)?);
                    }
                    self.compiled.functions[function] = FunctionSymbols {
                        parameters,
                        own_name,
                    };
                    self.statement(f.body)?;
                    self.scopes.pop();
                    self.function_depth -= 1;
                    self.loop_depth = old_loop;
                    Shape::Unknown
                }
                ExprKind::Assign(name, value) => {
                    let b = self.binding(&name).cloned().ok_or_else(|| {
                        problem(expression.span, "CAL010", "assignment to undefined local")
                    })?;
                    if !b.mutable {
                        return Err(problem(
                            expression.span,
                            "CAL011",
                            "cannot assign a const binding; use let for a variable that needs rebinding",
                        ));
                    }
                    self.compiled
                        .resolutions
                        .insert(id, Resolution::Local(b.symbol));
                    self.compiled.shapes[value].clone()
                }
                ExprKind::List(items) => Shape::List(Box::new(common(
                    items.iter().map(|id| &self.compiled.shapes[*id]),
                ))),
                ExprKind::Record(fields) => {
                    let mut seen = BTreeSet::new();
                    for (key, _) in &fields {
                        if !seen.insert(key) {
                            return Err(problem(
                                expression.span,
                                "CAL014",
                                "duplicate record field",
                            ));
                        }
                    }
                    Shape::Record(
                        RecordShape::new(
                            "",
                            fields
                                .iter()
                                .map(|(k, v)| (k.clone(), self.compiled.shapes[*v].clone())),
                        )
                        .expect("distinct fields"),
                    )
                }
                ExprKind::Unary(op, value) => {
                    let shape = &self.compiled.shapes[value];
                    if op == Unary::Not {
                        require_known(shape, &[Primitive::Bool], expression.span)?;
                        Shape::Primitive(Primitive::Bool)
                    } else {
                        require_known(
                            shape,
                            &[Primitive::Int, Primitive::Decimal],
                            expression.span,
                        )?;
                        shape.clone()
                    }
                }
                ExprKind::Binary(op, a, b) => {
                    let a = &self.compiled.shapes[a];
                    let b = &self.compiled.shapes[b];
                    match op {
                        Binary::And | Binary::Or => {
                            require_known(a, &[Primitive::Bool], expression.span)?;
                            require_known(b, &[Primitive::Bool], expression.span)?;
                            Shape::Primitive(Primitive::Bool)
                        }
                        Binary::Eq | Binary::Ne => Shape::Primitive(Primitive::Bool),
                        Binary::Lt | Binary::Le | Binary::Gt | Binary::Ge => {
                            let kinds = &[
                                Primitive::Int,
                                Primitive::Decimal,
                                Primitive::Text,
                                Primitive::Instant,
                                Primitive::Duration,
                            ];
                            require_known(a, kinds, expression.span)?;
                            require_known(b, kinds, expression.span)?;
                            if *a != Shape::Unknown && *b != Shape::Unknown && a != b {
                                return Err(problem(
                                    expression.span,
                                    "CAL004",
                                    format!(
                                        "comparison requires matching kinds and explicit conversion; received {} and {}",
                                        diagnostics::kind(a),
                                        diagnostics::kind(b)
                                    ),
                                ));
                            }
                            Shape::Primitive(Primitive::Bool)
                        }
                        _ if matches!(
                            a,
                            Shape::Primitive(Primitive::Instant | Primitive::Duration)
                        ) || matches!(
                            b,
                            Shape::Primitive(Primitive::Instant | Primitive::Duration)
                        ) =>
                        {
                            use Primitive::{Decimal, Duration, Instant, Int};
                            match (op, a, b) {
                                (
                                    Binary::Add | Binary::Sub,
                                    Shape::Primitive(Instant),
                                    Shape::Primitive(Duration),
                                )
                                | (
                                    Binary::Add,
                                    Shape::Primitive(Duration),
                                    Shape::Primitive(Instant),
                                ) => Shape::Primitive(Instant),
                                (
                                    Binary::Sub,
                                    Shape::Primitive(Instant),
                                    Shape::Primitive(Instant),
                                )
                                | (
                                    Binary::Add | Binary::Sub,
                                    Shape::Primitive(Duration),
                                    Shape::Primitive(Duration),
                                ) => Shape::Primitive(Duration),
                                (
                                    Binary::Mul | Binary::Divide,
                                    Shape::Primitive(Duration),
                                    Shape::Primitive(Int | Decimal),
                                )
                                | (
                                    Binary::Mul,
                                    Shape::Primitive(Int | Decimal),
                                    Shape::Primitive(Duration),
                                ) => Shape::Primitive(Duration),
                                (
                                    Binary::Divide,
                                    Shape::Primitive(Duration),
                                    Shape::Primitive(Duration),
                                ) => Shape::Primitive(Decimal),
                                (_, Shape::Unknown, _) | (_, _, Shape::Unknown) => Shape::Unknown,
                                _ => {
                                    return Err(problem(
                                        expression.span,
                                        "CAL004",
                                        format!("invalid temporal operation: {a} {} {b}", op.id()),
                                    ));
                                }
                            }
                        }
                        _ => {
                            let kinds = if op == Binary::Add {
                                &[Primitive::Int, Primitive::Decimal, Primitive::Text][..]
                            } else {
                                &[Primitive::Int, Primitive::Decimal][..]
                            };
                            require_known(a, kinds, expression.span)?;
                            require_known(b, kinds, expression.span)?;
                            if *a != Shape::Unknown && *b != Shape::Unknown && a != b {
                                return Err(problem(
                                    expression.span,
                                    "CAL004",
                                    format!(
                                        "mixed operands require explicit conversion; received {} and {}",
                                        diagnostics::kind(a),
                                        diagnostics::kind(b)
                                    ),
                                ));
                            }
                            if op == Binary::Divide {
                                Shape::Primitive(Primitive::Decimal)
                            } else if a == b {
                                a.clone()
                            } else {
                                Shape::Unknown
                            }
                        }
                    }
                }
                ExprKind::Field(value, field)
                    if matches!(
                        self.compiled.resolutions.get(&value),
                        Some(Resolution::IterNamespace)
                    ) =>
                {
                    let spec = self
                        .compiled
                        .program
                        .package
                        .operation(&format!("iter.{field}"))
                        .ok_or_else(|| {
                            problem(
                                expression.span,
                                "CAL010",
                                format!("unknown Iter operation {}", diagnostics::label(&field)),
                            )
                        })?;
                    if matches!(spec.operation, Operation::IterUse | Operation::IterChecked)
                        && !self.direct_callees.contains(&id)
                    {
                        return Err(problem(
                            expression.span,
                            "CAL002",
                            "Iter metadata requires a direct call",
                        ));
                    }
                    self.compiled
                        .resolutions
                        .insert(id, Resolution::Operation(spec));
                    Shape::Unknown
                }
                ExprKind::Field(value, field) => {
                    let shape = &self.compiled.shapes[value];
                    match shape.field(&field) {
                        Some(found) => found.clone(),
                        None if matches!(
                            shape,
                            Shape::Primitive(Primitive::Instant | Primitive::Duration)
                        ) && !self.direct_callees.contains(&id) =>
                        {
                            return Err(problem(
                                expression.span,
                                "CAL004",
                                format!(
                                    "field {} is absent on {}; {}",
                                    diagnostics::label(&field),
                                    diagnostics::kind(shape),
                                    if *shape == Shape::Primitive(Primitive::Instant) {
                                        "use utcParts(instant) for UTC calendar fields"
                                    } else {
                                        "use toSeconds/toMillis/toNanos(duration) for explicit units"
                                    }
                                ),
                            ));
                        }
                        None if self.literal_record_fields(value, 64).is_some()
                            && !(self.direct_callees.contains(&id)
                                && self.compiled.program.package.operation(&field).is_some()) =>
                        {
                            return Err(problem(
                                expression.span,
                                "CAL004",
                                format!(
                                    "record field {} is absent; use keys(record) to inspect available fields",
                                    diagnostics::label(&field)
                                ),
                            ));
                        }
                        _ => Shape::Unknown,
                    }
                }
                ExprKind::Index(value, _) => match &self.compiled.shapes[value] {
                    Shape::List(element) => *element.clone(),
                    Shape::Iter(_) => {
                        return Err(problem(
                            expression.span,
                            "CAL004",
                            "Iter does not support indexing; use .take(1).collect() for a bounded list, then index that list",
                        ));
                    }
                    _ => Shape::Unknown,
                },
                ExprKind::Call(callee, args) => self.call(id, callee, &args, expression.span)?,
            };
            self.compiled.shapes[id] = shape;
        }
        Ok(self.compiled.shapes[root].clone())
    }
    // Shape records guarantee fields; contract records may also contain undeclared fields.
    // Only syntax-proven immutable literal records provide a closed set of keys.
    fn literal_record_fields(&self, id: ExprId, left: usize) -> Option<&[(String, ExprId)]> {
        if left == 0 {
            return None;
        }
        match &self.compiled.program.expressions[id].kind {
            ExprKind::Record(fields) => Some(fields),
            ExprKind::Name(_) => match self.compiled.resolutions.get(&id)? {
                Resolution::Local(symbol) if !self.compiled.mutable_symbols.contains(symbol) => {
                    self.literal_record_fields(*self.compiled.initializers.get(symbol)?, left - 1)
                }
                _ => None,
            },
            ExprKind::Field(base, name) => {
                let fields = self.literal_record_fields(*base, left - 1)?;
                let (_, value) = fields.iter().find(|(key, _)| key == name)?;
                self.literal_record_fields(*value, left - 1)
            }
            _ => None,
        }
    }
    fn literal_text(&self, id: ExprId) -> Result<String, Diagnostic> {
        match &self.compiled.program.expressions[id].kind {
            ExprKind::Literal(Data::Text(text)) => Ok(text.to_string()),
            _ => Err(problem(
                self.compiled.program.expressions[id].span,
                "CAL002",
                "metadata selection must be literal text",
            )),
        }
    }
    fn call(
        &mut self,
        id: ExprId,
        callee: ExprId,
        args: &[ExprId],
        span: Span,
    ) -> Result<Shape, Diagnostic> {
        let method = if let ExprKind::Field(receiver, name) =
            &self.compiled.program.expressions[callee].kind
        {
            if matches!(
                self.compiled.resolutions.get(receiver),
                Some(Resolution::IterNamespace)
            ) || matches!(&self.compiled.shapes[*receiver], Shape::Record(record) if record.field(name).is_some())
            {
                None
            } else {
                self.compiled.program.package.operation(name)
            }
        } else {
            None
        };
        let spec = match self.compiled.resolutions.get(&callee) {
            Some(Resolution::Operation(spec)) => Some(*spec),
            _ => method,
        };
        let Some(spec) = spec else {
            // Function values have Unknown shape. A known shape proves this expression
            // cannot be called; mutable bindings deliberately keep Unknown shape.
            if self.compiled.shapes[callee] != Shape::Unknown {
                return Err(problem(
                    self.compiled.program.expressions[callee].span,
                    "CAL004",
                    "This value is not callable; expected Function.",
                ));
            }
            let arity = match &self.compiled.program.expressions[callee].kind {
                ExprKind::Function(f) => Some(self.compiled.program.functions[*f].parameters.len()),
                ExprKind::Name(name) => self.binding(name).and_then(|b| b.arity),
                _ => None,
            };
            if let Some(arity) = arity
                && arity != args.len()
            {
                let subject = match &self.compiled.program.expressions[callee].kind {
                    ExprKind::Name(name) => {
                        format!("function {}", diagnostics::label(name.as_str()))
                    }
                    _ => "function".into(),
                };
                return Err(problem(
                    span,
                    "CAL012",
                    diagnostics::arity(&subject, arity, arity, args.len()),
                ));
            }
            return Ok(Shape::Unknown);
        };
        if method.is_some()
            && !matches!(
                spec.operation,
                Operation::Map
                    | Operation::Filter
                    | Operation::Reduce
                    | Operation::Join
                    | Operation::WithFields
                    | Operation::Slice
                    | Operation::Concat
                    | Operation::SortBy
                    | Operation::Length
                    | Operation::Keys
                    | Operation::IsSome
                    | Operation::UnwrapOr
                    | Operation::Take
                    | Operation::Skip
                    | Operation::Collect
                    | Operation::Count
                    | Operation::Field
            )
        {
            return Err(problem(
                span,
                "CAL002",
                format!(
                    "operation {} does not support method syntax; call it as a function",
                    diagnostics::label(
                        self.compiled
                            .program
                            .package
                            .operations()
                            .find(|(_, candidate)| *candidate == spec)
                            .map_or(spec.operation.id(), |(name, _)| name)
                    )
                ),
            ));
        }
        let count = args.len() + usize::from(method.is_some());
        if count < usize::from(spec.min) || count > usize::from(spec.max) {
            return Err(problem(
                span,
                "CAL012",
                diagnostics::operation_arity(
                    &self.compiled.program.package,
                    spec,
                    args.len(),
                    method.is_some(),
                ),
            ));
        }
        self.compiled
            .resolutions
            .insert(id, Resolution::Operation(spec));
        if let Some(mode) = spec.operation.iter_mode() {
            let source = &self.compiled.shapes[args[0]];
            if !mode.accepts_source(source) {
                return Err(problem(
                    span,
                    "CAL004",
                    format!(
                        "iter.{} expects {}; received {}",
                        mode.name(),
                        mode.expected_source(),
                        diagnostics::kind(source)
                    ),
                ));
            }
        }
        match spec.operation {
            Operation::IterUse => {
                let name = self.literal_text(args[0])?;
                let recipe = self
                    .environment
                    .contracts
                    .iterators()
                    .get(&name)
                    .cloned()
                    .ok_or_else(|| {
                        problem(
                            span,
                            "CAL010",
                            format!("unknown iterator recipe {}", diagnostics::label(&name)),
                        )
                    })?;
                let element = recipe.output.iter_element().expect("validated output");
                let capture = self
                    .environment
                    .contracts
                    .capture(element.name())
                    .map_err(|e| problem(span, e.code, e.message))?;
                let shape = recipe.output.shape();
                self.compiled.iter_recipes.insert(id, (recipe, capture));
                Ok(shape)
            }
            Operation::IterChecked => {
                let name = self.literal_text(args[1])?;
                let contract = self
                    .environment
                    .contracts
                    .resolve(&name)
                    .map_err(|e| problem(span, e.code, e.message))?;
                let capture = self
                    .environment
                    .contracts
                    .capture(&name)
                    .map_err(|e| problem(span, e.code, e.message))?;
                self.compiled.iter_contracts.insert(id, capture);
                Ok(Shape::Iter(Box::new(contract.shape())))
            }
            Operation::IterCaptures => {
                Ok(Shape::Iter(Box::new(wes_core::IterMode::capture_shape())))
            }
            Operation::IterLines
            | Operation::IterChars
            | Operation::IterWords
            | Operation::IterSplit
            | Operation::IterRegexSplit
            | Operation::IterMatches
            | Operation::IterKeys => Ok(Shape::Iter(Box::new(Shape::Primitive(Primitive::Text)))),
            Operation::IterItems
            | Operation::IterValues
            | Operation::IterEntries
            | Operation::IterJsonLines => Ok(Shape::Iter(Box::new(Shape::Unknown))),
            Operation::Take | Operation::Skip | Operation::Field => {
                Ok(Shape::Iter(Box::new(Shape::Unknown)))
            }
            Operation::Collect => Ok(Shape::List(Box::new(Shape::Unknown))),
            Operation::Count => Ok(Shape::Primitive(Primitive::Int)),
            Operation::Call => {
                let name = self.literal_text(args[0])?;
                let ExprKind::List(parts) = &self.compiled.program.expressions[args[1]].kind else {
                    return Err(problem(
                        span,
                        "CAL002",
                        "call path must be a literal list of words",
                    ));
                };
                let path = parts
                    .iter()
                    .map(|id| self.literal_text(*id))
                    .collect::<Result<Vec<_>, _>>()?;
                let provider = self
                    .environment
                    .catalogue
                    .provider(&name)
                    .cloned()
                    .ok_or_else(|| {
                        problem(
                            span,
                            "CAL010",
                            format!("unknown provider {}", diagnostics::label(&name)),
                        )
                    })?;
                let capability = provider.capability(&path).cloned().ok_or_else(|| {
                    problem(
                        span,
                        "CAL010",
                        format!(
                            "unknown capability {} in provider {}",
                            diagnostics::label(
                                path.iter()
                                    .flat_map(|word| word.chars().chain(std::iter::once(' ')))
                                    .take(81)
                                    .collect::<String>()
                                    .trim_end()
                            ),
                            diagnostics::label(&name)
                        ),
                    )
                })?;
                if capability.streaming {
                    return Err(problem(
                        span,
                        "CAL002",
                        "calc call requires a finite capability",
                    ));
                }
                if let Shape::Record(record) = &self.compiled.shapes[args[2]] {
                    for parameter in &capability.parameters {
                        if parameter.required && record.field(&parameter.name).is_none() {
                            return Err(problem(
                                span,
                                "CAL004",
                                format!("missing argument '{}'", parameter.name),
                            ));
                        }
                    }
                    for (name, shape) in record.fields() {
                        let parameter = capability.parameter(name).ok_or_else(|| {
                            problem(span, "CAL004", format!("unknown argument '{name}'"))
                        })?;
                        if *shape != Shape::Unknown && !shape.is_assignable_to(&parameter.shape) {
                            return Err(problem(
                                span,
                                "CAL004",
                                format!(
                                    "argument '{name}' requires {}; received {}",
                                    parameter.shape,
                                    diagnostics::kind(shape)
                                ),
                            ));
                        }
                    }
                } else if self.compiled.shapes[args[2]] != Shape::Unknown {
                    return Err(problem(
                        span,
                        "CAL004",
                        format!(
                            "call arguments require Record; received {}",
                            diagnostics::kind(&self.compiled.shapes[args[2]])
                        ),
                    ));
                }
                let result = capability.result.clone();
                self.compiled.calls.insert(
                    id,
                    CallSelection {
                        provider,
                        capability,
                    },
                );
                Ok(result)
            }
            Operation::Check | Operation::ParseJson => {
                let type_arg = if spec.operation == Operation::Check {
                    Some(args[0])
                } else {
                    args.get(1).copied()
                };
                if let Some(type_arg) = type_arg {
                    let name = self.literal_text(type_arg)?;
                    let contract = self
                        .environment
                        .contracts
                        .resolve(&name)
                        .map_err(|e| problem(span, e.code, e.message))?;
                    let shape = contract.shape();
                    self.compiled.contracts.insert(id, contract);
                    Ok(shape)
                } else {
                    Ok(Shape::Unknown)
                }
            }
            Operation::Instant
            | Operation::Duration
            | Operation::Interval
            | Operation::Around
            | Operation::FromEpochSeconds
            | Operation::FromEpochMillis
            | Operation::FromEpochNanos
            | Operation::ToEpochSeconds
            | Operation::ToEpochMillis
            | Operation::ToEpochNanos
            | Operation::DurationSeconds
            | Operation::DurationMillis
            | Operation::DurationNanos
            | Operation::ToSeconds
            | Operation::ToMillis
            | Operation::ToNanos
            | Operation::UtcParts => {
                use Primitive::*;
                if spec.operation == Operation::UtcParts {
                    require_known(&self.compiled.shapes[args[0]], &[Instant], span)?;
                    return Ok(Shape::Record(
                        RecordShape::new(
                            "",
                            [
                                "year",
                                "month",
                                "day",
                                "hour",
                                "minute",
                                "second",
                                "nanosecond",
                                "weekday",
                            ]
                            .into_iter()
                            .map(|key| (key.to_owned(), Shape::Primitive(Int))),
                        )
                        .expect("distinct calendar fields"),
                    ));
                }
                let signature: (&[&[Primitive]], Primitive) = match spec.operation {
                    Operation::Instant => (&[&[Text]], Instant),
                    Operation::Duration => (&[&[Text]], Duration),
                    Operation::Interval => (&[&[Instant], &[Instant]], Interval),
                    Operation::Around => (&[&[Instant], &[Duration]], Interval),
                    Operation::FromEpochSeconds
                    | Operation::FromEpochMillis
                    | Operation::FromEpochNanos => (&[&[Int, Decimal]], Instant),
                    Operation::DurationSeconds
                    | Operation::DurationMillis
                    | Operation::DurationNanos => (&[&[Int, Decimal]], Duration),
                    Operation::ToSeconds | Operation::ToMillis | Operation::ToNanos => {
                        (&[&[Duration]], Decimal)
                    }
                    _ => (&[&[Instant]], Decimal),
                };
                for (arg, allowed) in args.iter().zip(signature.0) {
                    require_known(&self.compiled.shapes[*arg], allowed, span)?;
                }
                Ok(Shape::Primitive(signature.1))
            }
            Operation::Map | Operation::Filter | Operation::Reduce => {
                let mut inputs = args.to_vec();
                if method.is_some()
                    && let ExprKind::Field(receiver, _) =
                        self.compiled.program.expressions[callee].kind
                {
                    inputs.insert(0, receiver);
                }
                let mapped = if spec.operation == Operation::Map {
                    let callback = inputs[1];
                    let expression = match self.compiled.resolutions.get(&callback) {
                        Some(Resolution::Local(symbol))
                            if !self.compiled.mutable_symbols.contains(symbol) =>
                        {
                            self.compiled
                                .initializers
                                .get(symbol)
                                .copied()
                                .unwrap_or(callback)
                        }
                        _ => callback,
                    };
                    match self.compiled.program.expressions[expression].kind {
                        ExprKind::Function(function) => self
                            .compiled
                            .return_shape(self.compiled.program.functions[function].body),
                        _ => Shape::Unknown,
                    }
                } else {
                    Shape::Unknown
                };
                let source = &self.compiled.shapes[inputs[0]];
                if matches!(source, Shape::Iter(_)) {
                    if spec.operation == Operation::Filter {
                        Ok(source.clone())
                    } else if spec.operation == Operation::Map {
                        Ok(Shape::Iter(Box::new(mapped)))
                    } else {
                        Ok(Shape::Unknown)
                    }
                } else if matches!(source, Shape::List(_)) {
                    if spec.operation == Operation::Filter {
                        Ok(source.clone())
                    } else if spec.operation == Operation::Map {
                        Ok(Shape::List(Box::new(mapped)))
                    } else {
                        Ok(Shape::Unknown)
                    }
                } else {
                    Ok(Shape::Unknown)
                }
            }
            Operation::Join | Operation::WithFields | Operation::Slice => {
                let mut inputs = args
                    .iter()
                    .map(|id| self.compiled.shapes[*id].clone())
                    .collect::<Vec<_>>();
                if let ExprKind::Field(receiver, _) = self.compiled.program.expressions[callee].kind
                    && method.is_some()
                {
                    inputs.insert(0, self.compiled.shapes[receiver].clone());
                }
                match spec.operation {
                    Operation::Join => {
                        if !matches!(&inputs[0], Shape::Unknown | Shape::List(_)) {
                            return Err(problem(span, "CAL004", "join expects List<Text>"));
                        }
                        if let Shape::List(element) = &inputs[0] {
                            require_known(element, &[Primitive::Text], span)?;
                        }
                        require_known(&inputs[1], &[Primitive::Text], span)?;
                        Ok(Shape::Primitive(Primitive::Text))
                    }
                    Operation::WithFields => {
                        for input in &inputs {
                            if !matches!(input, Shape::Unknown | Shape::Record(_)) {
                                return Err(problem(
                                    span,
                                    "CAL004",
                                    "withFields expects two Records",
                                ));
                            }
                        }
                        if let (Shape::Record(original), Shape::Record(patch)) =
                            (&inputs[0], &inputs[1])
                        {
                            let mut fields = original
                                .fields()
                                .map(|(name, shape)| (name.to_owned(), shape.clone()))
                                .collect::<IndexMap<_, _>>();
                            for (name, shape) in patch.fields() {
                                fields.insert(name.to_owned(), shape.clone());
                            }
                            Ok(Shape::Record(
                                RecordShape::new("", fields).expect("distinct fields"),
                            ))
                        } else {
                            Ok(Shape::Unknown)
                        }
                    }
                    _ => {
                        for input in &inputs[1..] {
                            require_known(input, &[Primitive::Int], span)?;
                        }
                        match &inputs[0] {
                            Shape::Unknown | Shape::List(_) => Ok(inputs[0].clone()),
                            Shape::Primitive(Primitive::Text) => Ok(inputs[0].clone()),
                            _ => Err(problem(
                                span,
                                "CAL004",
                                "slice expects Text or a finite List; Bytes require explicit decoding",
                            )),
                        }
                    }
                }
            }
            Operation::Concat | Operation::SortBy => {
                let mut inputs = args.to_vec();
                if method.is_some()
                    && let ExprKind::Field(receiver, _) =
                        &self.compiled.program.expressions[callee].kind
                {
                    inputs.insert(0, *receiver);
                }
                let element = |id: ExprId| -> Result<Shape, Diagnostic> {
                    match &self.compiled.shapes[id] {
                        Shape::List(item) => Ok(item.as_ref().clone()),
                        Shape::Unknown => Ok(Shape::Unknown),
                        actual => Err(problem(
                            span,
                            "CAL004",
                            format!(
                                "{} requires finite List inputs; received {}",
                                spec.operation.id(),
                                diagnostics::kind(actual)
                            ),
                        )),
                    }
                };
                let first = element(inputs[0])?;
                let common = if spec.operation == Operation::Concat {
                    super::merge_collection_shapes(&first, &element(inputs[1])?).ok_or_else(|| problem(span, "CAL004", "concat requires compatible element types; map both inputs to a common record or convert numbers explicitly"))?
                } else {
                    first
                };
                Ok(Shape::List(Box::new(common)))
            }
            Operation::Some => Ok(Shape::Option(Box::new(
                self.compiled.shapes[args[0]].clone(),
            ))),
            Operation::IsSome | Operation::Has => Ok(Shape::Primitive(Primitive::Bool)),
            Operation::Length => {
                let receiver = if method.is_some() {
                    match self.compiled.program.expressions[callee].kind {
                        ExprKind::Field(receiver, _) => receiver,
                        _ => unreachable!(),
                    }
                } else {
                    args[0]
                };
                if matches!(self.compiled.shapes[receiver], Shape::Iter(_)) {
                    return Err(problem(
                        span,
                        "CAL004",
                        "length does not accept Iter; use .count() to traverse and count it, or .take(n).collect() for a bounded list",
                    ));
                }
                Ok(Shape::Primitive(Primitive::Int))
            }
            Operation::Int | Operation::Div | Operation::Rem => {
                Ok(Shape::Primitive(Primitive::Int))
            }
            Operation::Decimal | Operation::RoundDiv => Ok(Shape::Primitive(Primitive::Decimal)),
            Operation::Text => Ok(Shape::Primitive(Primitive::Text)),
            Operation::Range => Ok(Shape::List(Box::new(Shape::Primitive(Primitive::Int)))),
            _ => Ok(Shape::Unknown),
        }
    }
}
fn require_known(shape: &Shape, allowed: &[Primitive], span: Span) -> Result<(), Diagnostic> {
    if *shape == Shape::Unknown || allowed.iter().any(|p| *shape == Shape::Primitive(*p)) {
        Ok(())
    } else {
        Err(problem(
            span,
            "CAL004",
            format!(
                "expected {}; received {}",
                allowed
                    .iter()
                    .map(ToString::to_string)
                    .collect::<Vec<_>>()
                    .join(" or "),
                diagnostics::kind(shape)
            ),
        ))
    }
}
fn common<'a>(mut shapes: impl Iterator<Item = &'a Shape>) -> Shape {
    let Some(first) = shapes.next() else {
        return Shape::Unknown;
    };
    shapes
        .try_fold(first.clone(), |left, right| {
            if left == Shape::Unknown || *right == Shape::Unknown {
                return None;
            }
            super::merge_inferred_shapes(&left, right)
        })
        .unwrap_or(Shape::Unknown)
}
pub fn literal_shape(data: &Data) -> Shape {
    match data {
        Data::Iter(iter) => Shape::Iter(Box::new(iter.item_shape().clone())),
        Data::Text(_) => Shape::Primitive(Primitive::Text),
        Data::Int(_) => Shape::Primitive(Primitive::Int),
        Data::Decimal(_) => Shape::Primitive(Primitive::Decimal),
        Data::Bool(_) => Shape::Primitive(Primitive::Bool),
        Data::Instant(_) => Shape::Primitive(Primitive::Instant),
        Data::Duration(_) => Shape::Primitive(Primitive::Duration),
        Data::Interval(_) => Shape::Primitive(Primitive::Interval),
        Data::Bytes(_) => Shape::Primitive(Primitive::Bytes),
        Data::Option(None) => Shape::Option(Box::new(Shape::Unknown)),
        Data::Option(Some(_)) | Data::List(_) | Data::Record(_) => Shape::Unknown,
    }
}
