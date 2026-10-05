use super::{Budget, Failure, Limits, value::Item};
mod collections;
mod iter;
mod utilities;
use crate::driver::CancellationToken;
use collections::FiniteWork;
use indexmap::IndexMap;
use iter::{Consumer, Frame, IterWork};
use std::{collections::BTreeMap, sync::Arc};
use wes_core::{Data, Provenance, Value, contracts::Contract};
use wes_language::{
    Span,
    calc::{
        Binary, Compiled, ExprId, ExprKind, Operation, Resolution, StmtId, StmtKind, Symbol, Unary,
        diagnostics,
    },
};

#[derive(Clone, Debug)]
pub enum Request {
    Call {
        id: u64,
        expression: ExprId,
        arguments: IndexMap<String, Value>,
        span: Span,
    },
    Http {
        id: u64,
        operation: super::HttpOperation,
        input: Value,
        span: Span,
    },
    ParseJson {
        id: u64,
        text: String,
        contract: Option<Arc<Contract>>,
        span: Span,
    },
}
impl Request {
    pub fn id(&self) -> u64 {
        match self {
            Self::Call { id, .. } | Self::ParseJson { id, .. } | Self::Http { id, .. } => *id,
        }
    }
}
#[derive(Debug)]
pub enum Step {
    Yield,
    Request(Request),
    Complete(Value),
}
struct Environment {
    parent: Option<usize>,
    bindings: BTreeMap<Symbol, Option<Item>>,
}
enum Loop {
    Iter(Box<Frame>),
    While {
        condition: ExprId,
        body: StmtId,
        env: usize,
    },
    For {
        symbol: Symbol,
        items: Arc<Vec<Item>>,
        index: usize,
        body: StmtId,
        env: usize,
    },
}
enum Work {
    Utility(utilities::Utility),
    Finite(FiniteWork),
    Iter(IterWork),
    Statement(StmtId, usize),
    Expression(ExprId, usize),
    Drop,
    Bind(Symbol, usize),
    Assign(Symbol, usize),
    Branch {
        yes: StmtId,
        no: Option<StmtId>,
        env: usize,
        span: Span,
    },
    WhileCondition {
        condition: ExprId,
        body: StmtId,
        env: usize,
        span: Span,
    },
    ForStart {
        symbol: Symbol,
        body: StmtId,
        env: usize,
        span: Span,
    },
    Loop(Loop),
    Return,
    CallBoundary {
        base: usize,
        span: Span,
        function: Option<usize>,
    },
    Unary(Unary, Span),
    AfterLeft {
        op: Binary,
        right: ExprId,
        env: usize,
        span: Span,
    },
    Binary(Binary, Item, Span),
    Bool(Span),
    List(usize, Span),
    Record(Vec<String>, Span),
    Field(String, Span),
    Index(Span),
    Invoke {
        count: usize,
        expression: ExprId,
        span: Span,
    },
    Collection {
        operation: Operation,
        items: Arc<Vec<Item>>,
        callback: Item,
        index: usize,
        output: Vec<Item>,
        accumulator: Item,
        span: Span,
    },
    Collected {
        operation: Operation,
        items: Arc<Vec<Item>>,
        callback: Item,
        index: usize,
        output: Vec<Item>,
        accumulator: Item,
        span: Span,
    },
}
pub struct Machine {
    pub(super) compiled: Arc<Compiled>,
    pub(super) budget: Budget,
    pub(super) span: Span,
    pub(super) provenance: Provenance,
    limits: Limits,
    environments: Vec<Environment>,
    values: Vec<Item>,
    work: Vec<Work>,
    inputs: IndexMap<String, Item>,
    pending: Option<u64>,
    pending_origin: bool,
    has_origin: bool,
    sequence: u64,
    calls: u64,
    halted: bool,
    iter_regex: wes_core::IterRegexCache,
}
impl Machine {
    /// A pipeline's selected input is a control dependency even when no parameter
    /// reads its data. Join attribution exactly as for the explicit VM inputs.
    pub(crate) fn capture_control_provenance(&mut self, provenance: Provenance) {
        self.provenance = if self.has_origin {
            Provenance::agreed_by([&self.provenance, &provenance])
        } else {
            provenance
        };
        self.has_origin = true;
    }
    pub fn new(
        compiled: Arc<Compiled>,
        inputs: IndexMap<String, Value>,
        limits: Limits,
    ) -> Result<Self, Failure> {
        Self::new_with_token(compiled, inputs, limits, CancellationToken::new())
    }
    pub fn new_with_token(
        compiled: Arc<Compiled>,
        inputs: IndexMap<String, Value>,
        limits: Limits,
        token: CancellationToken,
    ) -> Result<Self, Failure> {
        let span = compiled.program.span;
        if limits.work == 0
            || limits.work > 100_000_000
            || limits.bytes == 0
            || limits.bytes > 1024 * 1024 * 1024
            || limits.frames == 0
            || limits.frames > 256
            || limits.quantum == 0
            || limits.quantum > 4096
            || limits.calls > 10_000
        {
            return Err(Failure::new("CAL006", span, "invalid calculation limits"));
        }
        let provenance = Provenance::agreed_by(inputs.values().map(Value::provenance));
        let mut budget = Budget {
            token,
            left: limits.work,
            work_limit: limits.work,
            charged: 0,
            limit: limits.bytes,
        };
        let mut captured = IndexMap::new();
        for name in compiled.workspace.keys() {
            let input = inputs
                .get(name)
                .ok_or_else(|| Failure::new("CAL004", span, "missing captured workspace input"))?;
            let charge = crate::value_size::value_charge(input, limits.bytes)
                .ok_or_else(|| Failure::new("CAL006", span, "input exceeds calculation budget"))?;
            budget.allocate(charge, span)?;
            captured.insert(
                name.clone(),
                Item::from_data(input.data(), &mut budget, span, 0)?.typed(input.shape().clone()),
            );
        }
        let mut parameters = BTreeMap::new();
        for (name, symbol) in &compiled.parameters {
            let input = inputs
                .get(name)
                .ok_or_else(|| Failure::new("CAL004", span, "missing calculation parameter"))?;
            let charge = crate::value_size::value_charge(input, limits.bytes)
                .ok_or_else(|| Failure::new("CAL006", span, "input exceeds calculation budget"))?;
            budget.allocate(charge, span)?;
            parameters.insert(
                *symbol,
                Some(
                    Item::from_data(input.data(), &mut budget, span, 0)?
                        .typed(input.shape().clone()),
                ),
            );
        }
        let root = compiled.program.root;
        Ok(Self {
            compiled,
            iter_regex: Default::default(),
            budget,
            span,
            provenance,
            limits,
            environments: vec![Environment {
                parent: None,
                bindings: parameters,
            }],
            values: vec![],
            work: vec![
                Work::CallBoundary {
                    base: 0,
                    span,
                    function: None,
                },
                Work::Statement(root, 0),
            ],
            inputs: captured,
            pending: None,
            pending_origin: false,
            has_origin: !inputs.is_empty(),
            sequence: 0,
            calls: 0,
            halted: false,
        })
    }
    pub fn poll(&mut self, token: &CancellationToken) -> Result<Step, Failure> {
        if self.halted || self.pending.is_some() {
            return Err(Failure::new(
                "CAL007",
                self.span,
                "calculation is not runnable",
            ));
        }
        self.budget.token = token.clone();
        let mut result = self.poll_inner(token);
        if let Err(error) = &mut result {
            if let Some(Work::Iter(work)) =
                self.work.iter().rev().find(|w| matches!(w, Work::Iter(_)))
            {
                error.message = format!("{}: {}", work.location(), error.message);
            }

            self.halted = true;
            error.trace = self
                .work
                .iter()
                .rev()
                .filter_map(|w| {
                    if let Work::CallBoundary { span, .. } = w {
                        Some(*span)
                    } else {
                        None
                    }
                })
                .take(16)
                .collect();
        }
        result
    }
    pub fn resume(&mut self, id: u64, result: Result<Value, Failure>) -> Result<(), Failure> {
        if self.halted || self.pending != Some(id) {
            return Err(Failure::new(
                "CAL007",
                self.span,
                "stale calculation continuation",
            ));
        }
        self.pending = None;
        match result {
            Ok(value) => {
                if self.pending_origin {
                    self.provenance = if self.has_origin {
                        Provenance::agreed_by([&self.provenance, value.provenance()])
                    } else {
                        value.provenance().clone()
                    };
                    self.has_origin = true;
                }
                let result = Item::from_data(value.data(), &mut self.budget, self.span, 0);
                match result {
                    Ok(item) => self.values.push(item.typed(value.shape().clone())),
                    Err(e) => {
                        self.halted = true;
                        return Err(e);
                    }
                }
                Ok(())
            }
            Err(mut error) => {
                if let Some(Work::Iter(work)) = self.work.last() {
                    error.message = format!("{}: {}", work.location(), error.message);
                }
                self.halted = true;
                Err(error)
            }
        }
    }
    fn env(&mut self, parent: usize) -> Result<usize, Failure> {
        self.budget.allocate(128, self.span)?;
        let id = self.environments.len();
        self.environments.push(Environment {
            parent: Some(parent),
            bindings: BTreeMap::new(),
        });
        Ok(id)
    }
    fn declare(&mut self, env: usize, symbol: Symbol, value: Option<Item>) -> Result<(), Failure> {
        self.budget.allocate(128, self.span)?;
        self.environments[env].bindings.insert(symbol, value);
        Ok(())
    }
    fn lookup(&self, mut env: usize, symbol: Symbol) -> Result<Item, Failure> {
        loop {
            if let Some(value) = self.environments[env].bindings.get(&symbol) {
                return value.clone().ok_or_else(|| {
                    Failure::new(
                        "CAL013",
                        self.span,
                        "local binding used before initialization",
                    )
                });
            }
            env = self.environments[env]
                .parent
                .ok_or_else(|| Failure::new("CAL003", self.span, "local binding is unavailable"))?;
        }
    }
    fn assign(&mut self, mut env: usize, symbol: Symbol, value: Item) -> Result<(), Failure> {
        loop {
            if let Some(slot) = self.environments[env].bindings.get_mut(&symbol) {
                *slot = Some(value);
                return Ok(());
            }
            env = self.environments[env]
                .parent
                .ok_or_else(|| Failure::new("CAL003", self.span, "local binding is unavailable"))?;
        }
    }
    fn pop(&mut self) -> Result<Item, Failure> {
        self.values
            .pop()
            .ok_or_else(|| Failure::new("CAL003", self.span, "missing calculation operand"))
    }
    fn drain(&mut self, count: usize) -> Result<Vec<Item>, Failure> {
        let start = self
            .values
            .len()
            .checked_sub(count)
            .ok_or_else(|| Failure::new("CAL003", self.span, "missing operands"))?;
        Ok(self.values.split_off(start))
    }
    fn symbol(&self, expression: ExprId) -> Result<Symbol, Failure> {
        if let Some(Resolution::Local(symbol)) = self.compiled.resolutions.get(&expression) {
            Ok(*symbol)
        } else {
            Err(Failure::new("CAL003", self.span, "unresolved local"))
        }
    }
    fn poll_inner(&mut self, token: &CancellationToken) -> Result<Step, Failure> {
        for _ in 0..self.limits.quantum {
            if token.is_cancelled() {
                return Err(Failure::cancelled(self.span));
            }
            self.budget.work(1, self.span)?;
            if self.work.len() > 100_000
                || self.values.len() > 100_000
                || self
                    .budget
                    .charged
                    .saturating_add((self.work.len() + self.values.len()) as u64 * 256)
                    > self.limits.bytes
            {
                return Err(Failure::new(
                    "CAL006",
                    self.span,
                    format!(
                        "calculation stack capacity exceeded ({} bytes; at most 100000 entries per stack). Bound the input or split the calculation.",
                        self.limits.bytes
                    ),
                ));
            }
            let Some(work) = self.work.pop() else {
                let item = self.pop()?;
                let data = item.data(&mut self.budget, self.span, 0)?;
                let shape = item.output_shape(&data);
                let value = Value::new(shape, data, self.provenance.clone())
                    .map_err(|_| Failure::new("CAL004", self.span, "invalid calculation result"))?;
                self.halted = true;
                return Ok(Step::Complete(value));
            };
            match work {
                Work::Utility(work) => self.utility_work(work)?,
                Work::Finite(work) => self.finite_work(work, token)?,
                Work::Iter(work) => {
                    if let Some(request) = self.iter_work(work, token)? {
                        return Ok(Step::Request(request));
                    }
                }
                Work::Statement(id, env) => self.statement(id, env)?,
                Work::Expression(id, env) => self.expression(id, env)?,
                Work::Drop => {
                    self.pop()?;
                }
                Work::Bind(symbol, env) => {
                    let value = self.pop()?;
                    if self.environments[env].bindings.contains_key(&symbol) {
                        self.assign(env, symbol, value)?;
                    } else {
                        self.declare(env, symbol, Some(value))?;
                    }
                }
                Work::Assign(symbol, env) => {
                    self.lookup(env, symbol)?; // assignment cannot initialize a temporal-dead-zone binding
                    let value = self.pop()?;
                    self.assign(env, symbol, value.clone())?;
                    self.values.push(value);
                }
                Work::Return => {
                    let result = self.pop()?;
                    let at = self
                        .work
                        .iter()
                        .rposition(|w| matches!(w, Work::CallBoundary { .. }))
                        .ok_or_else(|| {
                            Failure::new("CAL003", self.span, "return outside function")
                        })?;
                    let Work::CallBoundary { base, .. } = self.work[at] else {
                        unreachable!()
                    };
                    self.work.truncate(at);
                    self.values.truncate(base);
                    self.values.push(result);
                }
                Work::CallBoundary { span, function, .. } => {
                    return Err(Failure::new(
                        "CAL013",
                        span,
                        function.and_then(|id| self.compiled.program.functions[id].name.as_ref()).map_or_else(|| "calculation or anonymous function completed without return".into(), |name| format!("function {} completed without return; return a value on every executed path", diagnostics::label(&name.text))),
                    ));
                }
                Work::Branch { yes, no, env, span } => {
                    let yes_selected = self.pop()?.bool(span)?;
                    if yes_selected {
                        self.work.push(Work::Statement(yes, env));
                    } else if let Some(no) = no {
                        self.work.push(Work::Statement(no, env));
                    }
                }
                Work::WhileCondition {
                    condition,
                    body,
                    env,
                    span,
                } => {
                    if self.pop()?.bool(span)? {
                        self.work.push(Work::Loop(Loop::While {
                            condition,
                            body,
                            env,
                        }));
                        self.work.push(Work::Statement(body, env));
                    }
                }
                Work::ForStart {
                    symbol,
                    body,
                    env,
                    span,
                } => {
                    let value = self.pop()?;
                    if let Item::Iter(pipeline) = value.untyped() {
                        self.start_iter(
                            pipeline.clone(),
                            Consumer::For { symbol, body, env },
                            span,
                        )?;
                        continue;
                    }
                    let items = value.list(span)?;
                    self.advance_loop(Loop::For {
                        symbol,
                        items,
                        index: 0,
                        body,
                        env,
                    })?;
                }
                Work::Loop(next) => self.advance_loop(next)?,
                Work::Unary(op, span) => {
                    let item = self.pop()?;
                    let result = self.unary(op, item, span)?;
                    self.values.push(result);
                }
                Work::AfterLeft {
                    op,
                    right,
                    env,
                    span,
                } => {
                    let left = self.pop()?;
                    if matches!(op, Binary::And | Binary::Or) {
                        let flag = left.bool(span)?;
                        if (op == Binary::And && !flag) || (op == Binary::Or && flag) {
                            self.values.push(left);
                        } else {
                            self.work.push(Work::Bool(span));
                            self.work.push(Work::Expression(right, env));
                        }
                    } else {
                        self.work.push(Work::Binary(op, left, span));
                        self.work.push(Work::Expression(right, env));
                    }
                }
                Work::Binary(op, left, span) => {
                    let right = self.pop()?;
                    let result = self.binary(op, left, right, span)?;
                    self.values.push(result);
                }
                Work::Bool(span) => {
                    let flag = self.pop()?.bool(span)?;
                    self.values.push(Item::scalar(Data::Bool(flag)));
                }
                Work::List(count, span) => {
                    self.budget.allocate(count as u64 * 96, span)?;
                    let items = self.drain(count)?;
                    self.values.push(Item::List(Arc::new(items)));
                }
                Work::Record(keys, span) => {
                    self.budget
                        .allocate(keys.iter().map(|k| 96 + k.len() as u64).sum(), span)?;
                    let items = self.drain(keys.len())?;
                    self.values.push(Item::Record(Arc::new(
                        keys.into_iter().zip(items).collect(),
                    )));
                }
                Work::Field(field, span) => {
                    let item = self.pop()?;
                    let value = self.field(item, &field, span)?;
                    self.values.push(value);
                }
                Work::Index(span) => {
                    let index = self.pop()?;
                    let item = self.pop()?;
                    let value = self.index(item, index, span)?;
                    self.values.push(value);
                }
                Work::Invoke {
                    count,
                    expression,
                    span,
                } => {
                    let args = self.drain(count)?;
                    let function = self.pop()?;
                    if let Some(request) = self.invoke(function, args, expression, span, token)? {
                        return Ok(Step::Request(request));
                    }
                }
                Work::Collection {
                    operation,
                    items,
                    callback,
                    index,
                    output,
                    accumulator,
                    span,
                } => {
                    if index == items.len() {
                        self.values.push(if operation == Operation::Reduce {
                            accumulator
                        } else {
                            Item::List(Arc::new(output))
                        });
                    } else {
                        let args = if operation == Operation::Reduce {
                            vec![accumulator.clone(), items[index].clone()]
                        } else {
                            vec![items[index].clone()]
                        };
                        self.work.push(Work::Collected {
                            operation,
                            items,
                            callback: callback.clone(),
                            index,
                            output,
                            accumulator,
                            span,
                        });
                        if self
                            .invoke(callback, args, usize::MAX, span, token)?
                            .is_some()
                        {
                            return Err(Failure::new(
                                "CAL002",
                                span,
                                "metadata operations require direct calls",
                            ));
                        }
                    }
                }
                Work::Collected {
                    operation,
                    items,
                    callback,
                    index,
                    mut output,
                    mut accumulator,
                    span,
                } => {
                    let result = self.pop()?;
                    match operation {
                        Operation::Map => {
                            self.budget.allocate(96, span)?;
                            output.push(result);
                        }
                        Operation::Filter => {
                            if result.bool(span)? {
                                self.budget.allocate(96, span)?;
                                output.push(items[index].clone());
                            }
                        }
                        Operation::Reduce => accumulator = result,
                        _ => unreachable!(),
                    }
                    self.work.push(Work::Collection {
                        operation,
                        items,
                        callback,
                        index: index + 1,
                        output,
                        accumulator,
                        span,
                    });
                }
            }
        }
        Ok(Step::Yield)
    }
    fn statement(&mut self, id: StmtId, env: usize) -> Result<(), Failure> {
        let statement = self.compiled.program.statements[id].clone();
        self.span = statement.span;
        match statement.kind {
            StmtKind::Block(body) => {
                let inner = self.env(env)?;
                for id in &body {
                    if let Some(symbol) = self.compiled.declarations.get(id).copied()
                        && matches!(
                            self.compiled.program.statements[*id].kind,
                            StmtKind::Binding { .. }
                        )
                    {
                        self.declare(inner, symbol, None)?;
                    }
                }
                self.work
                    .extend(body.into_iter().rev().map(|id| Work::Statement(id, inner)));
            }
            StmtKind::Binding { value, .. } => {
                let symbol = self.compiled.declarations[&id];
                self.work.push(Work::Bind(symbol, env));
                self.work.push(Work::Expression(value, env));
            }
            StmtKind::Expression(value) => {
                self.work.push(Work::Drop);
                self.work.push(Work::Expression(value, env));
            }
            StmtKind::Return(value) => {
                self.work.push(Work::Return);
                self.work.push(Work::Expression(value, env));
            }
            StmtKind::If { condition, yes, no } => {
                self.work.push(Work::Branch {
                    yes,
                    no,
                    env,
                    span: statement.span,
                });
                self.work.push(Work::Expression(condition, env));
            }
            StmtKind::While { condition, body } => self.advance_loop(Loop::While {
                condition,
                body,
                env,
            })?,
            StmtKind::For { values, body, .. } => {
                self.work.push(Work::ForStart {
                    symbol: self.compiled.declarations[&id],
                    body,
                    env,
                    span: statement.span,
                });
                self.work.push(Work::Expression(values, env));
            }
            StmtKind::Break | StmtKind::Continue => {
                let boundary = self
                    .work
                    .iter()
                    .rposition(|w| matches!(w, Work::Loop(_) | Work::CallBoundary { .. }))
                    .ok_or_else(|| {
                        Failure::new("CAL003", statement.span, "loop boundary missing")
                    })?;
                if !matches!(self.work[boundary], Work::Loop(_)) {
                    return Err(Failure::new(
                        "CAL003",
                        statement.span,
                        "loop control crosses a function",
                    ));
                }
                self.work
                    .truncate(boundary + usize::from(matches!(statement.kind, StmtKind::Continue)));
            }
        };
        Ok(())
    }
    fn advance_loop(&mut self, next: Loop) -> Result<(), Failure> {
        match next {
            Loop::Iter(frame) => self.work.push(Work::Iter(IterWork::Pull(frame))),
            Loop::While {
                condition,
                body,
                env,
            } => {
                self.work.push(Work::WhileCondition {
                    condition,
                    body,
                    env,
                    span: self.span,
                });
                self.work.push(Work::Expression(condition, env));
            }
            Loop::For {
                symbol,
                items,
                index,
                body,
                env,
            } => {
                if index < items.len() {
                    let inner = self.env(env)?;
                    self.declare(inner, symbol, Some(items[index].clone()))?;
                    self.work.push(Work::Loop(Loop::For {
                        symbol,
                        items,
                        index: index + 1,
                        body,
                        env,
                    }));
                    self.work.push(Work::Statement(body, inner));
                }
            }
        };
        Ok(())
    }
    fn expression(&mut self, id: ExprId, env: usize) -> Result<(), Failure> {
        let expression = self.compiled.program.expressions[id].clone();
        self.span = expression.span;
        self.budget.allocate(16, self.span)?;
        match expression.kind {
            ExprKind::Literal(data) => {
                let value = Item::from_data(&data, &mut self.budget, self.span, 0)?;
                self.values.push(value);
            }
            ExprKind::Name(_) => {
                let value = match self.compiled.resolutions[&id] {
                    Resolution::Local(symbol) => self.lookup(env, symbol)?,
                    Resolution::Operation(spec) => Item::Builtin(spec),
                    Resolution::IterNamespace => Item::IterNamespace,
                };
                self.values.push(value);
            }
            ExprKind::Workspace(name) => self.values.push(self.inputs[&name].clone()),
            ExprKind::Function(function) => self.values.push(Item::Function {
                function,
                environment: env,
            }),
            ExprKind::Assign(_, value) => {
                self.work.push(Work::Assign(self.symbol(id)?, env));
                self.work.push(Work::Expression(value, env));
            }
            ExprKind::List(items) => {
                self.work.push(Work::List(items.len(), self.span));
                self.work
                    .extend(items.into_iter().rev().map(|id| Work::Expression(id, env)));
            }
            ExprKind::Record(fields) => {
                self.work.push(Work::Record(
                    fields.iter().map(|(k, _)| k.clone()).collect(),
                    self.span,
                ));
                self.work.extend(
                    fields
                        .into_iter()
                        .rev()
                        .map(|(_, id)| Work::Expression(id, env)),
                );
            }
            ExprKind::Unary(op, value) => {
                self.work.push(Work::Unary(op, self.span));
                self.work.push(Work::Expression(value, env));
            }
            ExprKind::Binary(op, left, right) => {
                self.work.push(Work::AfterLeft {
                    op,
                    right,
                    env,
                    span: self.span,
                });
                self.work.push(Work::Expression(left, env));
            }
            ExprKind::Field(value, name) => {
                self.work.push(Work::Field(name, self.span));
                self.work.push(Work::Expression(value, env));
            }
            ExprKind::Index(value, index) => {
                self.work.push(Work::Index(self.span));
                self.work.push(Work::Expression(index, env));
                self.work.push(Work::Expression(value, env));
            }
            ExprKind::Call(function, args) => {
                self.work.push(Work::Invoke {
                    count: args.len(),
                    expression: id,
                    span: self.span,
                });
                self.work
                    .extend(args.into_iter().rev().map(|id| Work::Expression(id, env)));
                self.work.push(Work::Expression(function, env));
            }
        };
        Ok(())
    }
    fn invoke(
        &mut self,
        function: Item,
        mut args: Vec<Item>,
        expression: ExprId,
        span: Span,
        token: &CancellationToken,
    ) -> Result<Option<Request>, Failure> {
        self.span = span;
        match function.untyped().clone() {
            Item::Function {
                function,
                environment,
            } => {
                let definition = self.compiled.program.functions[function].clone();
                let symbols = self.compiled.functions[function].clone();
                if args.len() != symbols.parameters.len() {
                    return Err(Failure::new(
                        "CAL012",
                        span,
                        diagnostics::arity(
                            &definition.name.as_ref().map_or_else(
                                || "function".into(),
                                |name| format!("function {}", diagnostics::label(&name.text)),
                            ),
                            symbols.parameters.len(),
                            symbols.parameters.len(),
                            args.len(),
                        ),
                    ));
                }
                if self
                    .work
                    .iter()
                    .filter(|w| matches!(w, Work::CallBoundary { .. }))
                    .count()
                    >= self.limits.frames
                {
                    return Err(Failure::new(
                        "CAL006",
                        span,
                        format!(
                            "calculation call-frame limit reached ({} frames). Reduce recursion or use an iterative calculation.",
                            self.limits.frames
                        ),
                    ));
                }
                let env = self.env(environment)?;
                if let Some(symbol) = symbols.own_name {
                    self.declare(
                        env,
                        symbol,
                        Some(Item::Function {
                            function,
                            environment,
                        }),
                    )?;
                }
                for (symbol, value) in symbols.parameters.into_iter().zip(args) {
                    self.declare(env, symbol, Some(value))?;
                }
                self.work.push(Work::CallBoundary {
                    base: self.values.len(),
                    span: definition.span,
                    function: Some(function),
                });
                self.work.push(Work::Statement(definition.body, env));
            }
            Item::Method(spec, receiver) => {
                if args.len() + 1 < spec.min as usize || args.len() + 1 > spec.max as usize {
                    return Err(Failure::new(
                        "CAL012",
                        span,
                        diagnostics::operation_arity(
                            &self.compiled.program.package,
                            spec,
                            args.len(),
                            true,
                        ),
                    ));
                }
                args.insert(0, receiver.as_ref().clone());
                return self.invoke(Item::Builtin(spec), args, expression, span, token);
            }
            Item::Builtin(spec) => {
                if args.len() < spec.min as usize || args.len() > spec.max as usize {
                    return Err(Failure::new(
                        "CAL012",
                        span,
                        diagnostics::operation_arity(
                            &self.compiled.program.package,
                            spec,
                            args.len(),
                            false,
                        ),
                    ));
                }
                if self.iter_operation(spec.operation, &args, expression, span, token)? {
                    return Ok(None);
                }
                match spec.operation {
                    Operation::Join | Operation::WithFields | Operation::Slice => {
                        self.start_utility(spec.operation, args, span)?;
                    }
                    Operation::Concat | Operation::SortBy => {
                        self.start_finite(spec.operation, args, span)?
                    }
                    Operation::Map | Operation::Filter | Operation::Reduce => {
                        let items = args[0].list(span)?;
                        let callback = args[1].clone();
                        let accumulator = args.get(2).cloned().unwrap_or(Item::Option(None));
                        self.work.push(Work::Collection {
                            operation: spec.operation,
                            items,
                            callback,
                            index: 0,
                            output: vec![],
                            accumulator,
                            span,
                        });
                    }
                    Operation::Call => {
                        if !self.compiled.calls.contains_key(&expression) {
                            return Err(Failure::new(
                                "CAL002",
                                span,
                                "uncaptured provider selection",
                            ));
                        }
                        self.calls += 1;
                        if self.calls > self.limits.calls {
                            return Err(Failure::new(
                                "CAL006",
                                span,
                                format!(
                                    "calculation provider-call limit reached ({} calls). Reduce the calls or split the calculation.",
                                    self.limits.calls
                                ),
                            ));
                        }
                        let Item::Record(fields) = args[2].untyped() else {
                            return Err(Failure::new(
                                "CAL004",
                                span,
                                "provider arguments require a record",
                            ));
                        };
                        let mut arguments = IndexMap::new();
                        for (key, value) in fields.iter() {
                            let data = value.data(&mut self.budget, span, 0)?;
                            if !data.is_materialized() {
                                return Err(Failure::new(
                                    "CAL004",
                                    span,
                                    "provider arguments require materialized data; collect Iter explicitly",
                                ));
                            }
                            arguments.insert(
                                key.clone(),
                                Value::new(
                                    value.output_shape(&data),
                                    data,
                                    self.provenance.clone(),
                                )
                                .map_err(|_| {
                                    Failure::new("CAL004", span, "invalid provider argument")
                                })?,
                            );
                        }
                        let id = self.suspend()?;
                        self.pending_origin = true;
                        return Ok(Some(Request::Call {
                            id,
                            expression,
                            arguments,
                            span,
                        }));
                    }
                    Operation::HttpStatus
                    | Operation::HttpError
                    | Operation::HttpCatalogue
                    | Operation::HttpAnalysis => {
                        let operation = match spec.operation {
                            Operation::HttpStatus => super::HttpOperation::Status,
                            Operation::HttpError => super::HttpOperation::Error,
                            Operation::HttpCatalogue => super::HttpOperation::Catalogue,
                            _ => super::HttpOperation::Analysis,
                        };
                        let data = args[0].data(&mut self.budget, span, 0)?;
                        let input =
                            Value::new(args[0].output_shape(&data), data, self.provenance.clone())
                                .map_err(|_| {
                                    Failure::new("CAL004", span, "invalid local function input")
                                })?;
                        let id = self.suspend()?;
                        self.pending_origin = false;
                        return Ok(Some(Request::Http {
                            id,
                            operation,
                            input,
                            span,
                        }));
                    }
                    Operation::ParseJson => {
                        let text = args[0].text(span)?;
                        self.budget.allocate(text.len() as u64, span)?;
                        let text = text.to_owned();
                        let contract = self.compiled.contracts.get(&expression).cloned();
                        let id = self.suspend()?;
                        self.pending_origin = false;
                        return Ok(Some(Request::ParseJson {
                            id,
                            text,
                            contract,
                            span,
                        }));
                    }
                    Operation::Check => {
                        let contract = self
                            .compiled
                            .contracts
                            .get(&expression)
                            .cloned()
                            .ok_or_else(|| Failure::new("CAL002", span, "uncaptured contract"))?;
                        let data = args[1].data(&mut self.budget, span, 0)?;
                        let issues = contract
                            .issues_with_cancel(&data, &|| token.is_cancelled())
                            .map_err(|_| Failure::cancelled(span))?;
                        if !issues.is_empty() {
                            return Err(Failure {
                                issues,
                                ..Failure::new(
                                    wes_language::calc::diagnostics::Category::Contract.code(),
                                    span,
                                    "calculation contract validation failed",
                                )
                            });
                        }
                        self.values.push(args[1].clone().typed(contract.shape()));
                    }
                    operation => {
                        let value = self.builtin(operation, args, span, token)?;
                        self.values.push(value);
                    }
                }
            }
            _ => {
                return Err(Failure::new(
                    "CAL004",
                    span,
                    format!(
                        "value of kind {} is not callable; expected Function",
                        function.kind()
                    ),
                ));
            }
        };
        Ok(None)
    }
    fn suspend(&mut self) -> Result<u64, Failure> {
        self.sequence = self
            .sequence
            .checked_add(1)
            .ok_or_else(|| Failure::new("CAL006", self.span, "continuation identity exhausted"))?;
        self.pending = Some(self.sequence);
        Ok(self.sequence)
    }
    pub(super) fn field(&self, item: Item, name: &str, span: Span) -> Result<Item, Failure> {
        if matches!(item.untyped(), Item::IterNamespace) {
            return self
                .compiled
                .program
                .package
                .operation(&format!("iter.{name}"))
                .map(Item::Builtin)
                .ok_or_else(|| {
                    Failure::new(
                        "CAL004",
                        span,
                        format!("unknown Iter operation {}", diagnostics::label(name)),
                    )
                });
        }
        if let Item::Scalar(data) = item.untyped()
            && let Some(value) = data.project_path(&[name.to_owned()])
        {
            return Ok(item.project_field_shape(Item::scalar(value.into_owned()), name));
        }
        if let Item::Record(fields) = item.untyped()
            && let Some(value) = fields.get(name)
        {
            return Ok(item.project_field_shape(value.clone(), name));
        }
        if let Some(spec) = self.compiled.program.package.operation(name)
            && matches!(
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
            return Ok(Item::Method(spec, Arc::new(item)));
        }
        let mut message = format!(
            "field or method {} is absent on {}",
            diagnostics::label(name),
            item.kind()
        );
        if name == "toString"
            && matches!(item.untyped(), Item::Scalar(_))
            && self
                .compiled
                .program
                .package
                .operation("text")
                .is_some_and(|spec| spec.operation == Operation::Text)
        {
            message.push_str("; use text(value) to convert a scalar to Text");
        }
        if name == "includes" {
            message.push_str("; calc is not JavaScript: use filter/reduce for list membership, or iter.lines(text) for line-based matching; :help calc lists supported operations");
        }
        Err(Failure::new("CAL004", span, message))
    }
}
