use crate::graph::{NodeId, OutputPort, OutputRef};
use indexmap::IndexMap;
use std::{borrow::Cow, collections::BTreeSet, sync::Arc};
use thiserror::Error;
use wes_core::{
    Data, Provenance, Shape, ValidationIssue, Value,
    capability::{Capability, ProviderDescription, Typing},
    contracts::{
        Contract,
        boundary::{self, BoundaryError},
    },
    literals,
};
use wes_language::{
    Call, Diagnostic, Span, Statement, Value as SyntaxValue, resolve::Resolution,
    vocabulary::CommandSpec,
};

pub type Guards = IndexMap<String, Vec<Arc<Contract>>>;
mod projection;
pub use projection::{InputProblem, InputResolutionError, project_value};
#[derive(Clone, Debug)]
pub enum Input {
    Literal(Value),
    FromNode(OutputRef),
    FieldPath {
        output: OutputRef,
        fields: Vec<String>,
    },
    Record(IndexMap<String, Input>),
    List {
        items: Vec<Input>,
        item_shape: Shape,
    },
}
impl Input {
    /// Read only the runtime-selected ticket snapshot, never a mutable current binding or value.
    pub fn resolve<'a>(
        &'a self,
        inputs: &'a IndexMap<NodeId, Value>,
    ) -> Result<Cow<'a, Value>, InputResolutionError> {
        self.resolve_with_limit(inputs, u64::MAX)
    }
    pub fn resolve_with_limit<'a>(
        &'a self,
        inputs: &'a IndexMap<NodeId, Value>,
        limit: u64,
    ) -> Result<Cow<'a, Value>, InputResolutionError> {
        let policy = self
            .literal_values()
            .into_iter()
            .chain(
                self.dependencies()
                    .into_iter()
                    .filter_map(|output| inputs.get(&output.node)),
            )
            .fold(wes_core::flow::FlowPolicy::default(), |policy, value| {
                policy.join(value.provenance().policy())
            });
        self.resolve_inner(inputs, limit)
            .map_err(|error| error.with_policy(&policy))
    }
    fn resolve_inner<'a>(
        &'a self,
        inputs: &'a IndexMap<NodeId, Value>,
        limit: u64,
    ) -> Result<Cow<'a, Value>, InputResolutionError> {
        match self {
            Self::Record(fields) => {
                let mut values = IndexMap::new();
                let mut remaining = 128 * 1024;
                for (key, input) in fields {
                    let value = input
                        .resolve_with_limit(inputs, limit)
                        .map_err(|error| error.at_argument_field(key))?;
                    charge_part(&value, &mut remaining)
                        .map_err(|error| error.at_argument_field(key))?;
                    values.insert(key.clone(), value.into_owned());
                }
                compose_record(values).map(Cow::Owned)
            }
            Self::List { items, item_shape } => {
                let mut values = vec![];
                let mut remaining = 128 * 1024;
                for (index, input) in items.iter().enumerate() {
                    let value = input
                        .resolve_with_limit(inputs, limit)
                        .map_err(|error| error.at_argument_field(&index.to_string()))?;
                    charge_part(&value, &mut remaining)
                        .map_err(|error| error.at_argument_field(&index.to_string()))?;
                    values.push(value.into_owned());
                }
                compose_list(values, item_shape).map(Cow::Owned)
            }
            Self::Literal(value) => Ok(Cow::Borrowed(value)),
            Self::FromNode(output) => inputs
                .get(&output.node)
                .map(Cow::Borrowed)
                .ok_or_else(|| InputResolutionError::unavailable(output, &[])),
            Self::FieldPath { output, fields } => {
                let root = inputs
                    .get(&output.node)
                    .ok_or_else(|| InputResolutionError::unavailable(output, fields))?;
                projection::project_value_with_limit(root, fields, limit)
                    .map(Cow::Owned)
                    .map_err(|error| error.with_source(output))
            }
        }
    }
    pub fn dependency(&self) -> Option<&OutputRef> {
        match self {
            Self::Literal(_) | Self::Record(_) | Self::List { .. } => None,
            Self::FromNode(output) | Self::FieldPath { output, .. } => Some(output),
        }
    }
    pub fn dependencies(&self) -> Vec<&OutputRef> {
        match self {
            Self::FromNode(output) | Self::FieldPath { output, .. } => vec![output],
            Self::Record(fields) => fields.values().flat_map(Self::dependencies).collect(),
            Self::List { items, .. } => items.iter().flat_map(Self::dependencies).collect(),
            Self::Literal(_) => vec![],
        }
    }
    pub fn literal_values(&self) -> Vec<&Value> {
        match self {
            Self::Literal(value) => vec![value],
            Self::Record(fields) => fields.values().flat_map(Self::literal_values).collect(),
            Self::List { items, .. } => items.iter().flat_map(Self::literal_values).collect(),
            _ => vec![],
        }
    }
    pub fn typing(&self, origin: &impl Fn(&OutputRef) -> Option<Typing>) -> Option<Typing> {
        match self {
            Self::Literal(value) => Some(Typing {
                shape: value.shape().clone(),
                provenance: value.provenance().clone(),
            }),
            Self::FromNode(output) => origin(output),
            Self::FieldPath { output, fields } => {
                let mut typing = origin(output)?;
                typing.shape = field_shape(&typing.shape, fields)?;
                Some(typing)
            }
            Self::Record(fields) => {
                let children = fields
                    .iter()
                    .map(|(key, input)| Some((key.clone(), input.typing(origin)?)))
                    .collect::<Option<IndexMap<_, _>>>()?;
                Some(Typing {
                    shape: Shape::Record(
                        wes_core::RecordShape::new(
                            "",
                            children
                                .iter()
                                .map(|(key, t)| (key.clone(), t.shape.clone())),
                        )
                        .ok()?,
                    ),
                    provenance: Provenance::agreed_by(children.values().map(|t| &t.provenance)),
                })
            }
            Self::List { items, item_shape } => {
                let children = items
                    .iter()
                    .map(|input| input.typing(origin))
                    .collect::<Option<Vec<_>>>()?;
                Some(Typing {
                    shape: wes_language::structured::list_shape(
                        &children.iter().map(|t| t.shape.clone()).collect::<Vec<_>>(),
                        item_shape,
                    ),
                    provenance: Provenance::agreed_by(children.iter().map(|t| &t.provenance)),
                })
            }
        }
    }
}

fn composed(
    shape: Shape,
    data: Data,
    provenance: Provenance,
) -> Result<Value, InputResolutionError> {
    let value = Value::new(shape, data, provenance)
        .expect("constructor derives its shape from captured children");
    if value.shape().contains_meta()
        || !value.data().is_materialized()
        || crate::value_size::value_charge(&value, 128 * 1024).is_none()
    {
        return Err(InputResolutionError::constructor(&value));
    }
    Ok(value)
}
fn compose_record(values: IndexMap<String, Value>) -> Result<Value, InputResolutionError> {
    let shape = Shape::Record(
        wes_core::RecordShape::new(
            "",
            values
                .iter()
                .map(|(key, v)| (key.clone(), v.shape().clone())),
        )
        .expect("unique constructor fields"),
    );
    let provenance = Provenance::agreed_by(values.values().map(Value::provenance));
    composed(
        shape,
        Data::Record(
            values
                .into_iter()
                .map(|(key, value)| (key, value.data().clone()))
                .collect(),
        ),
        provenance,
    )
}
fn compose_list(values: Vec<Value>, item_shape: &Shape) -> Result<Value, InputResolutionError> {
    let shape = wes_language::structured::list_shape(
        &values.iter().map(|v| v.shape().clone()).collect::<Vec<_>>(),
        item_shape,
    );
    let provenance = Provenance::agreed_by(values.iter().map(Value::provenance));
    composed(
        shape,
        Data::List(
            values
                .into_iter()
                .map(|value| value.data().clone())
                .collect(),
        ),
        provenance,
    )
}

fn charge_part(value: &Value, remaining: &mut u64) -> Result<(), InputResolutionError> {
    if value.shape().contains_meta() || !value.data().is_materialized() {
        return Err(InputResolutionError::constructor(value));
    }
    let Some(charge) = crate::value_size::value_charge(value, *remaining) else {
        return Err(InputResolutionError::constructor(value));
    };
    *remaining -= charge;
    Ok(())
}

#[derive(Clone, Debug)]
pub struct Invocation {
    pub provider: Arc<ProviderDescription>,
    pub capability: Arc<Capability>,
    pub inputs: IndexMap<String, Input>,
    pub cautions: BTreeSet<String>,
    pub interactive: bool,
    /// Explicit profile selected by the annotation; adapters decide which profiles they support.
    pub trace_profile: Option<Arc<str>>,
    pub guards: Guards,
}
impl Invocation {
    /// Declaration-time knowledge, distinct from the runtime's accepted actual typing. Only Text
    /// literals explicitly declared as provenance arguments create predicted producer facts.
    pub fn predicted_typing<'a>(&self, origins: impl Fn(&NodeId) -> Option<&'a Typing>) -> Typing {
        let empty = Provenance::default();
        let carried =
            Provenance::agreed_by(self.inputs.values().flat_map(Input::dependencies).map(
                |output| {
                    if output.port == OutputPort::Data {
                        origins(&output.node).map_or(&empty, |typing| &typing.provenance)
                    } else {
                        &empty
                    }
                },
            ));
        let mut producer = Provenance::default();
        for argument in &self.capability.provenance_arguments {
            if let Some(Input::Literal(value)) = self.inputs.get(argument)
                && let Data::Text(text) = value.data()
            {
                producer = producer.with_fact(argument, text.as_ref());
            }
        }
        Typing {
            shape: if self.capability.streaming {
                Shape::List(Box::new(self.capability.result.clone()))
            } else {
                self.capability.result.clone()
            },
            provenance: producer
                .inheriting(&carried)
                .cautioned(self.cautions.iter().cloned()),
        }
    }
}
#[derive(Clone, Debug)]
pub struct MetaTask {
    pub spec: CommandSpec,
    pub target: Option<wes_language::targets::QueryTarget>,
    pub tail: Vec<String>,
    pub subjects: Vec<Input>,
    pub inputs: IndexMap<String, Input>,
}
#[derive(Clone, Debug)]
pub enum Task {
    Invoke(Invocation),
    Meta(MetaTask),
}
#[derive(Clone, Debug)]
pub struct NewNode {
    pub task: Task,
    pub dependencies: IndexMap<NodeId, OutputPort>,
    pub name: Option<String>,
    pub error_name: Option<String>,
}
#[derive(Clone, Debug)]
pub enum Plan {
    NewNode(NewNode),
    Action {
        task: MetaTask,
        targets: Vec<NodeId>,
    },
}

#[derive(Clone, Debug, Error)]
pub enum PlanError {
    #[error("{diagnostic}")]
    Rejected {
        diagnostic: Diagnostic,
        issues: Vec<ValidationIssue>,
    },
    #[error("planning cancelled")]
    Cancelled,
}
impl PlanError {
    fn diagnostic(diagnostic: Diagnostic) -> Self {
        Self::Rejected {
            diagnostic,
            issues: Vec::new(),
        }
    }
    fn boundary(error: BoundaryError, span: Span) -> Self {
        let message = error.to_string();
        let (code, issues) = match error {
            BoundaryError::Declaration(error) => (error.code, Vec::new()),
            BoundaryError::Invalid { issues, .. } => ("TYP005", issues),
            BoundaryError::Cancelled(_) => return Self::Cancelled,
        };
        Self::Rejected {
            diagnostic: Diagnostic::error(code, span, message),
            issues,
        }
    }
}

pub fn guarded_shapes(
    guards: &Guards,
    capability: &Capability,
) -> Result<IndexMap<String, Shape>, wes_core::contracts::ContractError> {
    guards
        .iter()
        .map(|(key, checks)| {
            let expected = capability
                .parameter(key)
                .map_or(&Shape::Unknown, |p| &p.shape);
            Ok((key.clone(), boundary::shape(checks, expected)?))
        })
        .collect()
}

/// Produces an immutable plan without creating nodes. Names are resolved once at the call site.
pub fn plan(
    resolution: &Resolution,
    statement: &Statement,
    guards: &Guards,
    names: &impl Fn(&str) -> Option<OutputRef>,
) -> Result<Plan, PlanError> {
    plan_with_expectations(resolution, statement, guards, names, &IndexMap::new())
}
/// Owner-supplied immutable signatures for open management arguments and installed view inputs.
pub fn plan_with_expectations(
    resolution: &Resolution,
    statement: &Statement,
    guards: &Guards,
    names: &impl Fn(&str) -> Option<OutputRef>,
    expected: &IndexMap<String, Shape>,
) -> Result<Plan, PlanError> {
    match resolution {
        Resolution::Capability {
            call,
            provider,
            capability,
        } => {
            let refined = guarded_shapes(guards, capability)
                .map_err(|error| PlanError::boundary(error.into(), statement.span))?;
            let mut inputs = inputs(
                call,
                |key| {
                    refined.get(key).cloned().unwrap_or_else(|| {
                        capability
                            .parameter(key)
                            .map_or(Shape::Unknown, |p| p.shape.clone())
                    })
                },
                names,
            )?;
            for (key, checks) in guards {
                let expected = capability
                    .parameter(key)
                    .map_or(&Shape::Unknown, |p| &p.shape);
                // Shape compatibility is checked for references as well as immediately known literals.
                boundary::shape(checks, expected)
                    .map_err(|e| PlanError::boundary(e.into(), statement.span))?;
                if let Some(Input::Literal(value)) = inputs.get_mut(key) {
                    *value = boundary::literal(key, checks, expected, value)
                        .map_err(|e| PlanError::boundary(e, statement.span))?;
                }
            }
            let dependencies = dependencies(inputs.values(), statement.span)?;
            let cautions = statement
                .annotations
                .iter()
                .filter(|a| a.name.text == "unchecked")
                .flat_map(|a| {
                    a.targets
                        .iter()
                        .map(|target| format!("unchecked:{}", target.text))
                })
                .collect();
            let task = Task::Invoke(Invocation {
                provider: provider.clone(),
                capability: capability.clone(),
                inputs,
                cautions,
                interactive: statement
                    .annotations
                    .iter()
                    .any(|a| a.name.text == "interactive"),
                trace_profile: statement
                    .annotations
                    .iter()
                    .find(|a| a.name.text == "trace")
                    .and_then(|a| a.targets.first())
                    .map(|name| Arc::from(name.text.as_str())),
                guards: guards.clone(),
            });
            Ok(new_node(task, dependencies, statement))
        }
        Resolution::Meta {
            call,
            spec,
            tail,
            target,
        } => {
            let inputs = inputs(
                call,
                |key| {
                    expected.get(key).cloned().unwrap_or_else(|| {
                        spec.parameter(key)
                            .map_or(Shape::Unknown, |p| p.shape.clone())
                    })
                },
                names,
            )?;
            let subjects = call
                .operands
                .iter()
                .map(|value| {
                    let input = input(value, &Shape::Unknown, names)?;
                    if matches!(input, Input::FieldPath { .. }) {
                        return Err(PlanError::diagnostic(Diagnostic::error("PLN004", value.name().span,
                            "field paths are supported in named arguments; this operand requires a whole result")));
                    }
                    Ok(input)
                })
                .collect::<Result<Vec<_>, _>>()?;
            if !spec.produces_value {
                if statement.binding.is_some() || statement.error_binding.is_some() {
                    return Err(PlanError::diagnostic(Diagnostic::error(
                        "PLN003",
                        statement.span,
                        "this action has no output to bind",
                    )));
                }
                let mut targets = Vec::new();
                for subject in &subjects {
                    if let Input::FromNode(output) = subject
                        && !targets.contains(&output.node)
                    {
                        targets.push(output.node.clone());
                    }
                }
                return Ok(Plan::Action {
                    task: MetaTask {
                        spec: spec.clone(),
                        target: target.clone(),
                        tail: tail.clone(),
                        subjects,
                        inputs,
                    },
                    targets,
                });
            }
            let dependencies = if spec.command == wes_language::vocabulary::MetaCommand::Trace {
                if !matches!(subjects.as_slice(), [Input::FromNode(_)]) {
                    return Err(PlanError::diagnostic(Diagnostic::error(
                        "TRC002",
                        statement.span,
                        ":trace requires a node reference",
                    )));
                }
                if inputs.get("run").is_some_and(|v| !matches!(v, Input::Literal(value) if matches!(value.data(), wes_core::Data::Text(_)))) { return Err(PlanError::diagnostic(Diagnostic::error("TRC002",statement.span,"trace run must be a literal run identity"))); }
                IndexMap::new()
            } else if matches!(
                spec.command,
                wes_language::vocabulary::MetaCommand::Inspect
                    | wes_language::vocabulary::MetaCommand::Read
                    | wes_language::vocabulary::MetaCommand::ImportApply
                    | wes_language::vocabulary::MetaCommand::ViewCreate
                    | wes_language::vocabulary::MetaCommand::ViewBind
                    | wes_language::vocabulary::MetaCommand::ViewConnect
                    | wes_language::vocabulary::MetaCommand::ViewDisconnect
                    | wes_language::vocabulary::MetaCommand::ViewOutput
                    | wes_language::vocabulary::MetaCommand::ViewCapture
                    | wes_language::vocabulary::MetaCommand::ViewPin
                    | wes_language::vocabulary::MetaCommand::ViewLink
                    | wes_language::vocabulary::MetaCommand::ViewUnlink
                    | wes_language::vocabulary::MetaCommand::ViewStart
                    | wes_language::vocabulary::MetaCommand::ViewStop
            ) {
                IndexMap::new()
            } else {
                dependencies(subjects.iter().chain(inputs.values()), statement.span)?
            };
            Ok(new_node(
                Task::Meta(MetaTask {
                    spec: spec.clone(),
                    target: target.clone(),
                    tail: tail.clone(),
                    subjects,
                    inputs,
                }),
                dependencies,
                statement,
            ))
        }
    }
}
fn new_node(task: Task, dependencies: IndexMap<NodeId, OutputPort>, statement: &Statement) -> Plan {
    Plan::NewNode(NewNode {
        task,
        dependencies,
        name: statement.binding.as_ref().map(|b| b.name.text.clone()),
        error_name: statement
            .error_binding
            .as_ref()
            .map(|b| b.name.text.clone()),
    })
}
fn inputs(
    call: &Call,
    expected: impl Fn(&str) -> Shape,
    names: &impl Fn(&str) -> Option<OutputRef>,
) -> Result<IndexMap<String, Input>, PlanError> {
    call.arguments
        .iter()
        .map(|argument| {
            Ok((
                argument.key.text.clone(),
                input(&argument.value, &expected(&argument.key.text), names)?,
            ))
        })
        .collect()
}
pub(crate) fn input(
    value: &SyntaxValue,
    expected: &Shape,
    names: &impl Fn(&str) -> Option<OutputRef>,
) -> Result<Input, PlanError> {
    match value {
        SyntaxValue::Structured(_, structure) => {
            let input = match structure {
                wes_language::Structure::Record(fields) => Input::Record(
                    fields
                        .iter()
                        .map(|(key, value)| {
                            Ok((
                                key.text.clone(),
                                structured_input(
                                    value,
                                    expected.field(&key.text).unwrap_or(&Shape::Unknown),
                                    names,
                                )?,
                            ))
                        })
                        .collect::<Result<_, PlanError>>()?,
                ),
                wes_language::Structure::List(items) => {
                    let item_shape = if let Shape::List(item) = expected {
                        item.as_ref().clone()
                    } else {
                        Shape::Unknown
                    };
                    Input::List {
                        items: items
                            .iter()
                            .map(|value| structured_input(value, &item_shape, names))
                            .collect::<Result<_, _>>()?,
                        item_shape,
                    }
                }
            };
            if input.dependencies().is_empty() {
                input.resolve(&IndexMap::new()).map_err(|_| {
                    PlanError::diagnostic(Diagnostic::error(
                        "ARG002",
                        value.name().span,
                        "Structured argument requires bounded materialized data.",
                    ))
                })?;
            }
            Ok(input)
        }
        SyntaxValue::Reference(reference) => {
            let mut parts = reference.text.split('.');
            let root = parts.next().expect("reference root");
            let fields: Vec<String> = parts.map(str::to_owned).collect();
            names(root)
                .map(|output| {
                    if fields.is_empty() {
                        Input::FromNode(output)
                    } else {
                        Input::FieldPath { output, fields }
                    }
                })
                .ok_or_else(|| {
                    PlanError::diagnostic(
                        Diagnostic::error(
                            "PLN001",
                            reference.span,
                            format!("nothing is named '${}'", reference.text),
                        )
                        .with_public_message("Referenced name is not defined in this workspace."),
                    )
                })
        }
        SyntaxValue::Word(word) | SyntaxValue::Text(word) => {
            let data = literals::read(&word.text, expected).ok_or_else(|| {
                PlanError::diagnostic(
                    Diagnostic::error(
                        "CHK004",
                        word.span,
                        format!("'{}' cannot be read as {expected}", word.text),
                    )
                    .with_public_message("Literal cannot be read as the required parameter type."),
                )
            })?;
            Ok(Input::Literal(
                Value::new(expected.clone(), data, Provenance::default())
                    .expect("literal reader guarantees shallow shape"),
            ))
        }
    }
}
fn structured_input(
    value: &SyntaxValue,
    expected: &Shape,
    names: &impl Fn(&str) -> Option<OutputRef>,
) -> Result<Input, PlanError> {
    if matches!(value, SyntaxValue::Word(_) | SyntaxValue::Text(_)) {
        input(
            value,
            &wes_language::structured::scalar_shape(expected),
            names,
        )
    } else {
        input(value, expected, names)
    }
}
fn dependencies<'a>(
    inputs: impl IntoIterator<Item = &'a Input>,
    span: Span,
) -> Result<IndexMap<NodeId, OutputPort>, PlanError> {
    let mut selected = IndexMap::new();
    for input in inputs {
        for output in input.dependencies() {
            if let Some(previous) = selected.insert(output.node.clone(), output.port)
                && previous != output.port
            {
                return Err(PlanError::diagnostic(Diagnostic::error(
                    "PLN002",
                    span,
                    format!(
                        "one execution cannot provide both '{}' and '{}' from {}",
                        previous.selector(),
                        output.port.selector(),
                        output.node
                    ),
                )));
            }
        }
    }
    Ok(selected)
}

/// Read metadata for :change using the same input interpretation as initial command checking.
pub fn given(
    input: &Input,
    typing: &impl Fn(&OutputRef) -> Option<wes_core::capability::Typing>,
) -> wes_language::check::Given {
    use wes_core::capability::Typing;
    use wes_language::check::{Given, GivenValue};
    let value = match input {
        Input::Literal(value) => match value.data() {
            Data::Text(text) => GivenValue::Written(text.to_string()),
            _ => GivenValue::Known(Typing {
                shape: value.shape().clone(),
                provenance: value.provenance().clone(),
            }),
        },
        Input::FromNode(output) => {
            GivenValue::Known(typing(output).unwrap_or_else(|| Typing::new(Shape::Unknown)))
        }
        Input::FieldPath { output, fields } => {
            match typing(output).and_then(|mut typing| {
                typing.shape = field_shape(&typing.shape, fields)?;
                Some(typing)
            }) {
                Some(typing) => GivenValue::Known(typing),
                None => GivenValue::Missing(format!("{}.{}", output.node, fields.join("."))),
            }
        }
        Input::Record(_) | Input::List { .. } => input
            .typing(typing)
            .map(GivenValue::Known)
            .unwrap_or_else(|| GivenValue::Missing("structured input".into())),
    };
    Given {
        value,
        key: Span::at(0),
        span: Span::at(0),
    }
}

/// Project only declared record fields. Unknown metadata stays unknown, never guessed or widened.
pub fn field_shape(shape: &Shape, fields: &[String]) -> Option<Shape> {
    let mut current = shape;
    for field in fields {
        current = current.field(field)?;
    }
    Some(current.clone())
}

/// One construction budget per argument set, separate from preexisting referenced payloads.
pub fn resolve_arguments(
    arguments: &IndexMap<String, Input>,
    inputs: &IndexMap<NodeId, Value>,
) -> Result<IndexMap<String, Value>, (String, InputResolutionError)> {
    let policy = arguments
        .values()
        .flat_map(|input| {
            input.literal_values().into_iter().chain(
                input
                    .dependencies()
                    .into_iter()
                    .filter_map(|output| inputs.get(&output.node)),
            )
        })
        .fold(wes_core::flow::FlowPolicy::default(), |policy, value| {
            policy.join(value.provenance().policy())
        });
    let mut values = IndexMap::new();
    let mut remaining = 128 * 1024;
    for (name, input) in arguments {
        let value = input
            .resolve(inputs)
            .map_err(|error| (name.clone(), error.with_policy(&policy)))?;
        if matches!(input, Input::Record(_) | Input::List { .. }) {
            charge_part(&value, &mut remaining)
                .map_err(|error| (name.clone(), error.with_policy(&policy)))?;
        }
        values.insert(name.clone(), value.into_owned());
    }
    Ok(values)
}
