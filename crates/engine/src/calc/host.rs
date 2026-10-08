//! Structured subcalls retain the parent's physical execution lease. No second graph or queue.
use super::{Failure, Limits, Machine, Request, Step};
use crate::{
    calls::AdmittedCommand,
    driver::{CancellationToken, ExecutionFuture, ExecutionReport, Executor},
    graph::{NodeId, OutputRef},
    plan::{Input, Invocation},
    providers::{BoundCall, CallExecutor, Providers},
    runtime::{ExecutionTraits, Outcome, RunTicket, RuntimeCode},
};
use indexmap::IndexMap;
use std::sync::Arc;
use wes_core::{ErrorId, ErrorValue, Value, contracts::Contract};
use wes_language::{
    Span,
    calc::{Compiled, ExprId},
};

/// Fixed, bounded local functions owned by adapters. No I/O, workspace lookup or dynamic
/// converter registry. The language declares their semantics and verifies purity.
#[derive(Clone, Copy, Debug)]
pub enum HttpOperation {
    Status,
    Error,
    Catalogue,
    Analysis,
}

pub trait LocalServices: Send + Sync + std::fmt::Debug + 'static {
    fn http(
        &self,
        _operation: HttpOperation,
        _input: &Value,
        _token: &CancellationToken,
        span: Span,
    ) -> Result<Value, Failure> {
        Err(Failure::new(
            "CAL004",
            span,
            "HTTP local functions are unavailable",
        ))
    }
    fn read(
        &self,
        text: &str,
        contract: Option<&Contract>,
        token: &CancellationToken,
        span: Span,
    ) -> Result<Value, Failure>;
}
#[derive(Clone, Debug)]
pub struct BoundCalculation {
    pub compiled: Arc<Compiled>,
    inputs: IndexMap<String, Input>,
    guards: crate::plan::Guards,
    definition: Option<Arc<wes_language::templates::CalculationDefinition>>,
    pipe_input: Option<OutputRef>,
    calls: IndexMap<ExprId, BoundCall>,
    json: Option<Arc<dyn LocalServices>>,
}
impl BoundCalculation {
    pub(crate) fn safe_observation(&self) -> bool {
        self.calls.values().all(|call| call.invocation().capability.safety == wes_core::capability::Safety::Safe && !call.interactive())
        // All effectful operations currently lower to captured calls. Fail closed if this grows.
        && self.compiled.purity.external_operations.len() == self.calls.len()
    }

    /// Validate known native source shapes before an owned live scope starts any producer.
    pub(crate) fn observation_inputs_match(
        &self,
        typings: &IndexMap<NodeId, Arc<wes_core::capability::Typing>>,
    ) -> bool {
        self.guards.iter().all(|(name, contracts)| {
            let Some(input) = self.inputs.get(name) else {
                return false;
            };
            let shape = input
                .typing(&|output| match output.port {
                    crate::graph::OutputPort::Data => {
                        typings.get(&output.node).map(|t| (**t).clone())
                    }
                    crate::graph::OutputPort::Error => Some(wes_core::capability::Typing::new(
                        wes_core::ErrorValue::shape(),
                    )),
                    crate::graph::OutputPort::Cancel => Some(wes_core::capability::Typing::new(
                        wes_core::ErrorValue::cancellation_shape(),
                    )),
                })
                .map(|t| t.shape);
            shape.is_some_and(|shape| {
                shape != wes_core::Shape::Unknown
                    && contracts.iter().all(|c| shape.is_assignable_to(&c.shape()))
            })
        })
    }

    pub(crate) fn set_authority(&mut self, authority: crate::environments::InvocationAuthority) {
        for call in self.calls.values_mut() {
            call.set_authority(authority.clone());
        }
    }
    pub fn environments(&self) -> impl Iterator<Item = &wes_core::environments::Binding> {
        self.calls.values().filter_map(BoundCall::environment)
    }
    pub(crate) fn private_output(&self) -> bool {
        self.calls.values().any(BoundCall::private_output)
    }
    pub(crate) fn bind(
        compiled: impl Into<Arc<Compiled>>,
        inputs: IndexMap<String, OutputRef>,
        providers: &Providers,
        json: Option<Arc<dyn LocalServices>>,
    ) -> Result<Self, crate::providers::BindError> {
        let compiled = compiled.into();
        let calls = compiled
            .calls
            .iter()
            .map(|(id, selection)| {
                let call = providers.bind_finite(Invocation {
                    provider: selection.provider.clone(),
                    capability: selection.capability.clone(),
                    inputs: IndexMap::new(),
                    cautions: Default::default(),
                    interactive: false,
                    trace_profile: None,
                    guards: IndexMap::new(),
                })?;
                Ok((*id, call))
            })
            .collect::<Result<_, _>>()?;
        Ok(Self {
            compiled,
            inputs: inputs
                .into_iter()
                .map(|(name, output)| (name, Input::FromNode(output)))
                .collect(),
            guards: IndexMap::new(),
            definition: None,
            pipe_input: None,
            calls,
            json,
        })
    }
    pub fn dependencies(&self) -> impl Iterator<Item = OutputRef> + '_ {
        self.inputs
            .values()
            .flat_map(Input::dependencies)
            .chain(self.pipe_input.iter())
            .cloned()
    }
    pub(crate) fn with_pipe_input(mut self, input: Option<OutputRef>) -> Self {
        self.pipe_input = input;
        self
    }
    pub(crate) fn with_parameters(
        mut self,
        inputs: IndexMap<String, Input>,
        guards: crate::plan::Guards,
        definition: Arc<wes_language::templates::CalculationDefinition>,
    ) -> Self {
        self.inputs = inputs;
        self.guards = guards;
        self.definition = Some(definition);
        self
    }
    pub fn definition(&self) -> Option<&wes_language::templates::CalculationDefinition> {
        self.definition.as_deref()
    }
    pub fn traits(&self) -> ExecutionTraits {
        ExecutionTraits {
            pure: !self.compiled.effectful(),
            repeatable: !self.compiled.effectful(),
            bounded: true,
        }
    }
    pub(crate) fn with_admission(mut self, admission: AdmittedCommand) -> Self {
        self.calls = self
            .calls
            .into_iter()
            .map(|(id, call)| (id, call.with_admission(admission.clone())))
            .collect();
        self
    }
    pub(crate) fn execute(
        self,
        run: crate::runtime::Run,
        inputs: IndexMap<NodeId, Value>,
        calls: CallExecutor,
        token: CancellationToken,
    ) -> ExecutionFuture {
        Box::pin(async move {
            let program = self.compiled.clone();
            let locations = self.compiled.clone();
            let failure = |error| failure(error, &locations.program);
            let cancel = token.clone();
            let span = program.program.span;
            let arguments = self.inputs;
            let guards = self.guards;
            let pipe_input = self.pipe_input;
            let initialized = tokio::task::spawn_blocking(move || {
                let control = pipe_input
                    .as_ref()
                    .map(|output| {
                        inputs
                            .get(&output.node)
                            .map(|v| v.provenance().clone())
                            .ok_or_else(|| {
                                Failure::new(
                                    "CAL004",
                                    span,
                                    "pipeline control input is unavailable",
                                )
                            })
                    })
                    .transpose()?;
                let values = crate::plan::resolve_arguments(&arguments, &inputs).map_err(
                    |(name, error)| {
                        let mut failure = Failure::new("CAL004", span, error.message());
                        failure.issues = error.issue(&name).into_iter().collect();
                        failure.policy = error.policy().clone();
                        failure
                    },
                )?;
                let mut captured = IndexMap::new();
                for (name, value) in values {
                    let value = if let Some(checks) = guards.get(&name) {
                        wes_core::contracts::boundary::require(
                            &name,
                            checks,
                            &wes_core::Shape::Unknown,
                            &value,
                            &|| cancel.is_cancelled(),
                        )
                        .map_err(|e| contract_failure(e, span))?
                    } else {
                        value
                    };
                    captured.insert(name, value);
                }
                let mut machine =
                    Machine::new_with_token(program, captured, Limits::default(), cancel)?;
                if let Some(control) = control {
                    machine.capture_control_provenance(control);
                }
                Ok::<_, Failure>(machine)
            })
            .await;
            let mut machine = match initialized {
                Ok(Ok(machine)) => machine,
                Ok(Err(error)) => return failure(error).into(),
                Err(_) => return panic_failure().into(),
            };
            let mut notices = vec![];
            loop {
                let cancel = token.clone();
                let polled = tokio::task::spawn_blocking(move || {
                    let result = machine.poll(&cancel);
                    (machine, result)
                })
                .await;
                let step;
                (machine, step) = match polled {
                    Ok(pair) => pair,
                    Err(_) => {
                        return ExecutionReport {
                            outcome: panic_failure(),
                            notices,
                            stream_start: None,
                            holds: vec![],
                            progress: None,
                        };
                    }
                };
                let request = match step {
                    Ok(Step::Yield) => {
                        tokio::task::yield_now().await;
                        continue;
                    }
                    Ok(Step::Complete(value)) => {
                        let outcome = if let Some(definition) = &self.definition {
                            let contract = definition.output.clone();
                            let cancel = token.clone();
                            match tokio::task::spawn_blocking(move || {
                                wes_core::contracts::boundary::checked_result(
                                    &contract,
                                    &value,
                                    &|| cancel.is_cancelled(),
                                )
                            })
                            .await
                            {
                                Ok(Ok(value)) => Outcome::Produced(value),
                                Ok(Err(error)) => failure(contract_failure(error, span)),
                                Err(_) => panic_failure(),
                            }
                        } else {
                            Outcome::Produced(value)
                        };
                        return ExecutionReport {
                            outcome,
                            notices,
                            stream_start: None,
                            holds: vec![],
                            progress: None,
                        };
                    }
                    Err(error) => {
                        return ExecutionReport {
                            outcome: failure(error),
                            notices,
                            stream_start: None,
                            holds: vec![],
                            progress: None,
                        };
                    }
                    Ok(Step::Request(request)) => request,
                };
                let id = request.id();
                let result = match request {
                    Request::Call {
                        expression,
                        arguments,
                        span,
                        ..
                    } => {
                        let Some(bound) = self.calls.get(&expression) else {
                            return failure(Failure::new(
                                "CAL002",
                                span,
                                "missing captured callable",
                            ))
                            .into();
                        };
                        let bound = bound.clone();
                        let cancel = token.clone();
                        let prepared = tokio::task::spawn_blocking(move || {
                            validate_arguments(&bound, &arguments, &cancel, span)?;
                            Ok::<_, Failure>(
                                bound.with_inputs(
                                    arguments
                                        .into_iter()
                                        .map(|(k, v)| (k, Input::Literal(v)))
                                        .collect(),
                                ),
                            )
                        })
                        .await;
                        let bound = match prepared {
                            Ok(Ok(bound)) => bound,
                            Ok(Err(error)) => {
                                return ExecutionReport {
                                    outcome: failure(error),
                                    notices,
                                    stream_start: None,
                                    holds: vec![],
                                    progress: None,
                                };
                            }
                            Err(_) => {
                                return ExecutionReport {
                                    outcome: panic_failure(),
                                    notices,
                                    stream_start: None,
                                    holds: vec![],
                                    progress: None,
                                };
                            }
                        };
                        let report = calls
                            .execute(
                                RunTicket {
                                    run: run.calculation_child(id),
                                    payload: bound,
                                    inputs: IndexMap::new(),
                                    input_origins: IndexMap::new(),
                                },
                                token.clone(),
                            )
                            .await;
                        notices.extend(report.notices);
                        match report.outcome {
                            Outcome::Skipped => {
                                Err(Failure::new("CAL004", span, "provider returned no value"))
                            }
                            Outcome::Produced(value) => Ok(value),
                            Outcome::Failed(error) | Outcome::Incomplete { error, .. } => {
                                notices.push(error.clone());
                                Err(Failure::provider(span, error))
                            }
                            Outcome::Cancelled(_) => Err(Failure::cancelled(span)),
                        }
                    }
                    Request::Http {
                        operation,
                        input,
                        span,
                        ..
                    } => {
                        let services = self.json.clone();
                        let cancel = token.clone();
                        match tokio::task::spawn_blocking(move || match services {
                            Some(services) => services.http(operation, &input, &cancel, span),
                            None => Err(Failure::new(
                                "CAL004",
                                span,
                                "HTTP local functions are unavailable",
                            )),
                        })
                        .await
                        {
                            Ok(result) => result,
                            Err(_) => {
                                return ExecutionReport {
                                    outcome: panic_failure(),
                                    notices,
                                    stream_start: None,
                                    holds: vec![],
                                    progress: None,
                                };
                            }
                        }
                    }
                    Request::ParseJson {
                        text,
                        contract,
                        span,
                        ..
                    } => {
                        let reader = self.json.clone();
                        let cancel = token.clone();
                        match tokio::task::spawn_blocking(move || match reader {
                            Some(reader) => reader.read(&text, contract.as_deref(), &cancel, span),
                            None => Err(Failure::new(
                                "CAL004",
                                span,
                                "JSON reader is not configured",
                            )),
                        })
                        .await
                        {
                            Ok(result) => result,
                            Err(_) => {
                                return ExecutionReport {
                                    outcome: panic_failure(),
                                    notices,
                                    stream_start: None,
                                    holds: vec![],
                                    progress: None,
                                };
                            }
                        }
                    }
                };
                let resumed = tokio::task::spawn_blocking(move || {
                    let result = machine.resume(id, result);
                    (machine, result)
                })
                .await;
                let result;
                (machine, result) = match resumed {
                    Ok(pair) => pair,
                    Err(_) => {
                        return ExecutionReport {
                            outcome: panic_failure(),
                            notices,
                            stream_start: None,
                            holds: vec![],
                            progress: None,
                        };
                    }
                };
                if let Err(error) = result {
                    return ExecutionReport {
                        outcome: failure(error),
                        notices,
                        stream_start: None,
                        holds: vec![],
                        progress: None,
                    };
                }
            }
        })
    }
}
fn validate_arguments(
    call: &BoundCall,
    arguments: &IndexMap<String, Value>,
    token: &CancellationToken,
    span: Span,
) -> Result<(), Failure> {
    let capability = &call.invocation().capability;
    for (name, value) in arguments {
        if token.is_cancelled() {
            return Err(Failure::cancelled(span));
        }
        let parameter = capability.parameter(name).ok_or_else(|| {
            Failure::new(
                "CAL004",
                span,
                format!("unknown provider argument '{name}'"),
            )
        })?;
        // Validate actual data deeply; a retained Unknown shape is not evidence of conformance.
        if !value.data().is_materialized() || !fits(value.data(), &parameter.shape, token, 0) {
            return Err(Failure::new(
                "CAL004",
                span,
                format!(
                    "provider argument '{name}' does not satisfy {}",
                    parameter.shape
                ),
            ));
        }
    }
    for parameter in &capability.parameters {
        if parameter.required && !arguments.contains_key(&parameter.name) {
            return Err(Failure::new(
                "CAL004",
                span,
                format!("missing provider argument '{}'", parameter.name),
            ));
        }
    }
    use wes_core::{Data, capability::Rule};
    for declared in capability.rules.iter().filter(|rule| rule.is_binding()) {
        if token.is_cancelled() {
            return Err(Failure::cancelled(span));
        }
        let broken = match &declared.rule {
            Rule::MutuallyExclusive(keys) => {
                keys.iter()
                    .filter(|key| arguments.contains_key(*key))
                    .count()
                    > 1
            }
            Rule::Requires { key, needs } => {
                arguments.contains_key(key) && !arguments.contains_key(needs)
            }
            Rule::OneOf { key, values } => {
                arguments.get(key).is_some_and(|value| match value.data() {
                    Data::Text(text) => !values.contains(text.as_ref()),
                    Data::Int(value) => !values.contains(&value.to_string()),
                    Data::Decimal(value) => !values.contains(&value.to_string()),
                    Data::Bool(value) => !values.contains(&value.to_string()),
                    _ => true,
                })
            }
            Rule::ProvenanceFact {
                key,
                fact,
                expected,
            } => arguments
                .get(key)
                .is_some_and(|value| value.provenance().fact(fact) != Some(expected.as_str())),
        };
        if broken {
            return Err(Failure::new(
                "CAL004",
                span,
                "computed provider arguments violate a documented capability rule",
            ));
        }
    }
    Ok(())
}
pub(super) fn fits(
    data: &wes_core::Data,
    shape: &wes_core::Shape,
    token: &CancellationToken,
    depth: usize,
) -> bool {
    use wes_core::{Data, Shape};
    if depth > 128 || token.is_cancelled() {
        return false;
    }
    match (data, shape) {
        (_, Shape::Unknown) => true,
        (Data::List(items), Shape::List(element)) => {
            items.iter().all(|v| fits(v, element, token, depth + 1))
        }
        (Data::Record(fields), Shape::Record(record)) => record
            .fields()
            .all(|(k, s)| fields.get(k).is_some_and(|v| fits(v, s, token, depth + 1))),
        (Data::Option(item), Shape::Option(element)) => item
            .as_deref()
            .is_none_or(|v| fits(v, element, token, depth + 1)),
        _ => super::value::shape(data, 0).is_assignable_to(shape),
    }
}
fn failure(error: Failure, program: &wes_language::calc::Program) -> Outcome {
    let policy = error.cause.as_ref().map_or_else(
        || error.policy.clone(),
        |cause| error.policy.join(cause.policy()),
    );
    let locations = std::iter::once(error.span)
        .chain(error.trace.iter().copied().take(16))
        .filter_map(|span| {
            let start = program
                .origin
                .position(span.start().checked_sub(program.origin_offset)?)
                .ok()?;
            let end = program
                .origin
                .position(span.end().checked_sub(program.origin_offset)?)
                .ok()?;
            Some(wes_core::SourceLocation {
                source: program.origin.name().into(),
                start: span.start(),
                end: span.end(),
                line: start.line,
                column: start.column,
                end_line: end.line,
                end_column: end.column,
            })
        })
        .collect();
    let value = ErrorValue::new(
        ErrorId::new(uuid::Uuid::new_v4().to_string()).expect("UUID"),
        error.code,
        error.message,
        error.issues,
        error.cause.as_ref().map(|e| e.id().clone()),
    )
    .expect("valid calculation failure")
    .with_locations(locations)
    .expect("bounded valid calculation locations");
    let value = value.with_policy(&policy);
    if error.cancelled {
        Outcome::Cancelled(value)
    } else {
        Outcome::Failed(value)
    }
}
fn panic_failure() -> Outcome {
    Outcome::Failed(
        RuntimeCode::ExecutionFailed.error("Calculation worker terminated unexpectedly.", None),
    )
}

fn contract_failure(error: wes_core::contracts::boundary::BoundaryError, span: Span) -> Failure {
    use wes_core::contracts::boundary::BoundaryError;
    match error {
        BoundaryError::Cancelled(_) => Failure::cancelled(span),
        BoundaryError::Invalid { issues, .. } => {
            let mut failure =
                Failure::new("TYP005", span, "calculation contract validation failed");
            failure.issues = issues;
            failure
        }
        BoundaryError::Declaration(_) => {
            Failure::new("TYP009", span, "incompatible calculation contracts")
        }
    }
}

#[cfg(test)]
mod failure_tests {
    use super::*;
    #[test]
    fn private_input_resolution_redacts_issues_and_locations_without_a_provider_cause() {
        let program =
            wes_language::calc::parse_body("return 1;", 0, wes_language::calc::Package::standard())
                .unwrap();
        let error = Failure {
            policy: wes_core::flow::FlowPolicy::default().private(),
            issues: vec![wes_core::ValidationIssue {
                path: "/arguments/value/hidden".into(),
                code: "INP002".into(),
                message: "hidden field metadata".into(),
            }],
            ..Failure::new("CAL004", wes_language::Span::at(0), "private input detail")
        };
        let Outcome::Failed(error) = failure(error, &program) else {
            panic!("failure expected");
        };
        assert!(error.policy().is_private());
        assert_eq!(error.code(), "ENV021");
        assert!(error.issues().is_empty() && error.locations().is_empty());
        assert!(!error.message().contains("private input detail"));
    }
    #[test]
    fn a_private_provider_cause_taints_the_outer_failure_and_removes_details() {
        let program =
            wes_language::calc::parse_body("return 1;", 0, wes_language::calc::Package::standard())
                .unwrap();
        let cause = wes_core::ErrorValue::new(
            wes_core::ErrorId::new("private-cause").unwrap(),
            "HTTP001",
            "private provider detail",
            vec![],
            None,
        )
        .unwrap()
        .with_policy(
            &wes_core::flow::FlowPolicy::default()
                .private()
                .from_origin("protected"),
        );
        let Outcome::Failed(error) = failure(
            Failure::provider(wes_language::Span::at(0), cause),
            &program,
        ) else {
            panic!("failure expected");
        };
        assert!(error.policy().is_private());
        assert_eq!(error.code(), "ENV021");
        assert!(error.locations().is_empty() && error.issues().is_empty());
        assert!(error.cause().is_none());
        assert!(!error.message().contains("private provider detail"));
    }
}
