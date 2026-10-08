//! Bound workspace work. Dispatch preserves provider admission and keeps pure local work separate.
use crate::{
    calls::{AdmittedCommand, CallJournal},
    driver::{CancellationToken, ExecutionFuture, Executor, StreamExecutionFuture},
    graph::OutputRef,
    plan::{Input, MetaTask},
    providers::{BoundCall, CallExecutor},
    runtime::{ExecutionTraits, Outcome, RunTicket, RuntimeCode},
};
use std::sync::Arc;
use uuid::Uuid;
use wes_core::{
    Data, ErrorId, ErrorValue, Provenance,
    capability::Typing,
    contracts::{Contract, ContractRegistry},
    literals,
};
use wes_language::{Diagnostic, Span};
mod help;
pub(crate) mod import_plan;
pub(crate) mod management;
pub(crate) mod view;
pub use help::{BoundHelp, help_query, help_tree};
pub(crate) mod query;
pub use query::BoundQuery;
pub(crate) use query::types::completion_names as type_completion_names;

#[derive(Clone, Debug)]
pub enum BoundTask {
    /// Immutable native input installed only in an isolated execution scope.
    Input(wes_core::Value),
    Call(BoundCall),
    Describe(crate::describe::BoundDescribe),
    Calculation(crate::calc::BoundCalculation),
    Scan(crate::scan::BoundScan),
    Stream(crate::stream_ops::BoundOperator),
    TypeCheck(BoundTypeCheck),
    Accumulation(crate::accumulation::BoundAccumulation),
    Help(BoundHelp),
    Management(management::BoundManagement),
    ImportPlan(import_plan::BoundImportPlan),
    Query(BoundQuery),
    View(view::BoundView),
}
/// Producer-defined successful-completion semantics, separate from result contents
/// and authority. Exposing this notice requires permission to read the source.
#[derive(Clone, Debug)]
pub struct CompletionNotice {
    pub code: &'static str,
    pub message: &'static str,
    pub result_access: &'static str,
    pub grants_changed: bool,
}
impl BoundTask {
    pub fn completion_notice(&self) -> Option<CompletionNotice> {
        match self {
            Self::Describe(task) => Some(CompletionNotice {
                code: "DSC000",
                message: task.public_completion(),
                result_access: "private",
                grants_changed: false,
            }),
            _ => None,
        }
    }
    pub fn observational(&self) -> bool {
        matches!(
            self,
            Self::Management(_)
                | Self::ImportPlan(_)
                | Self::Query(_)
                | Self::Help(_)
                | Self::View(_)
        )
    }

    pub(crate) fn set_authority(&mut self, authority: crate::environments::InvocationAuthority) {
        match self {
            Self::Call(call) => call.set_authority(authority),
            Self::Calculation(calc) => calc.set_authority(authority),
            _ => (),
        }
    }
    pub fn environments(&self) -> impl Iterator<Item = &wes_core::environments::Binding> {
        self.call()
            .and_then(BoundCall::environment)
            .into_iter()
            .chain(
                match self {
                    Self::Calculation(calc) => Some(calc),
                    _ => None,
                }
                .into_iter()
                .flat_map(crate::calc::BoundCalculation::environments),
            )
    }
    pub fn call(&self) -> Option<&BoundCall> {
        match self {
            Self::Call(call) => Some(call),
            Self::Input(_)
            | Self::Describe(_)
            | Self::Stream(_)
            | Self::Calculation(_)
            | Self::Scan(_)
            | Self::TypeCheck(_)
            | Self::Accumulation(_)
            | Self::Help(_)
            | Self::Query(_)
            | Self::Management(_)
            | Self::ImportPlan(_)
            | Self::View(_) => None,
        }
    }
    pub fn traits(&self) -> ExecutionTraits {
        match self {
            Self::View(_) => ExecutionTraits {
                pure: false,
                repeatable: false,
                bounded: true,
            },
            Self::Call(call) => call.traits(),
            Self::Describe(_) => ExecutionTraits {
                pure: false,
                repeatable: false,
                bounded: true,
            },
            Self::Calculation(calc) => calc.traits(),
            Self::Scan(_) => ExecutionTraits {
                pure: false,
                repeatable: false,
                bounded: true,
            },
            Self::Input(_)
            | Self::Management(_)
            | Self::ImportPlan(_)
            | Self::Stream(_)
            | Self::Accumulation(_)
            | Self::TypeCheck(_)
            | Self::Help(_)
            | Self::Query(_) => ExecutionTraits {
                pure: false,
                repeatable: true,
                bounded: true,
            },
        }
    }
    pub(crate) fn dependency_lifetime(&self) -> crate::runtime::DependencyLifetime {
        match self {
            Self::View(view) if view.pipe_input.is_some() => {
                crate::runtime::DependencyLifetime::Creation
            }
            _ => crate::runtime::DependencyLifetime::Continuous,
        }
    }
    pub fn dependencies(&self) -> impl Iterator<Item = OutputRef> + '_ {
        let view = match self {
            Self::View(view) => view.pipe_input.clone(),
            _ => None,
        };
        let call = self
            .call()
            .into_iter()
            .flat_map(BoundCall::dependencies)
            .chain(view);
        let checked = match self {
            Self::TypeCheck(checked) => checked.input().dependencies(),
            _ => vec![],
        };
        let calc = match self {
            Self::Calculation(calc) => Some(calc),
            _ => None,
        };
        call.chain(
            calc.into_iter()
                .flat_map(crate::calc::BoundCalculation::dependencies),
        )
        .chain(checked.into_iter().cloned())
        .chain(
            match self {
                Self::ImportPlan(plan) => Some(plan),
                _ => None,
            }
            .into_iter()
            .flat_map(import_plan::BoundImportPlan::dependencies),
        )
        .chain(
            match self {
                Self::Stream(op) => Some(op),
                _ => None,
            }
            .into_iter()
            .flat_map(crate::stream_ops::BoundOperator::dependencies),
        )
        .chain(match self {
            Self::Accumulation(accumulation) => Some(accumulation.dependency().clone()),
            _ => None,
        })
        .chain(
            match self {
                Self::Scan(scan) => Some(scan),
                _ => None,
            }
            .into_iter()
            .flat_map(crate::scan::BoundScan::dependencies),
        )
        .chain(
            match self {
                Self::Query(query) => Some(query),
                _ => None,
            }
            .into_iter()
            .flat_map(BoundQuery::dependencies),
        )
    }
    pub(crate) fn with_admission(self, admission: AdmittedCommand) -> Self {
        match self {
            Self::Call(call) => Self::Call(call.with_admission(admission)),
            Self::Calculation(calc) => Self::Calculation(calc.with_admission(admission)),
            // Pure local validation has no external call receipt to write. Its declaration still
            // belongs in the accepted command record, and the coordinator must admit before commit.
            other => other,
        }
    }
}

/// The immutable contract is resolved once during preparation, not looked up in mutable state by
/// a worker. Only an explicitly written literal may be interpreted under the requested shape.
#[derive(Clone)]
pub struct BoundTypeCheck {
    task: MetaTask,
    contract: Arc<Contract>,
}
impl std::fmt::Debug for BoundTypeCheck {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("BoundTypeCheck")
            .field("contract", &self.contract.name())
            .finish_non_exhaustive()
    }
}
impl BoundTypeCheck {
    pub(crate) fn bind(
        task: MetaTask,
        registry: &ContractRegistry,
        span: Span,
    ) -> Result<Self, Diagnostic> {
        let invalid = |message| Diagnostic::error("TYP002", span, message);
        if task.tail.first().map(String::as_str) != Some("check") || task.tail.len() > 2 {
            return Err(invalid("expected ':type check'"));
        }
        if task.tail.len() == 2 && task.inputs.contains_key("as") {
            return Err(invalid("specify the type once"));
        }
        let name = if task.tail.len() == 2 {
            task.tail[1].as_str()
        } else {
            let Some(Input::Literal(value)) = task.inputs.get("as") else {
                return Err(invalid(
                    "type check requires a type name or a literal as: argument",
                ));
            };
            let Data::Text(name) = value.data() else {
                return Err(invalid("type name must be literal text"));
            };
            name
        };
        let contract = registry
            .resolve(name)
            .map_err(|error| Diagnostic::error(error.code, span, error.message))?;
        if !task.inputs.contains_key("value") {
            return Err(invalid("type check requires value:"));
        }
        Ok(Self { task, contract })
    }
    fn input(&self) -> &Input {
        &self.task.inputs["value"]
    }
    pub(crate) fn predicted_typing(&self, origin: impl Fn(&OutputRef) -> Option<Typing>) -> Typing {
        let provenance = self
            .input()
            .typing(&origin)
            .map_or_else(Provenance::default, |typing| typing.provenance);
        Typing {
            shape: self.contract.shape(),
            provenance,
        }
    }
}

pub struct TaskExecutor {
    calls: CallExecutor,
    scan_memory: crate::scan::ledger::MemoryPool,
}
impl TaskExecutor {
    pub fn ephemeral() -> Self {
        Self {
            calls: CallExecutor::ephemeral(),
            scan_memory: crate::scan::ledger::MemoryPool::new(wes_budgets::get(
                "scan.aggregate.bytes",
            ))
            .expect("positive scan aggregate budget"),
        }
    }
    pub fn recorded(journal: CallJournal) -> Self {
        Self {
            calls: CallExecutor::recorded(journal),
            scan_memory: crate::scan::ledger::MemoryPool::new(wes_budgets::get(
                "scan.aggregate.bytes",
            ))
            .expect("positive scan aggregate budget"),
        }
    }
    pub(crate) fn with_scan_memory(mut self, pool: crate::scan::ledger::MemoryPool) -> Self {
        self.scan_memory = pool;
        self
    }
}
impl Executor<BoundTask> for TaskExecutor {
    fn interactive(&self, payload: &BoundTask) -> bool {
        payload.call().is_some_and(BoundCall::interactive)
    }
    fn execute_interactive(
        &self,
        ticket: RunTicket<BoundTask>,
        cancellation: CancellationToken,
    ) -> crate::driver::InteractiveExecutionFuture {
        match ticket.payload {
            BoundTask::Call(call) => self.calls.execute_interactive(
                RunTicket {
                    payload: call,
                    run: ticket.run,
                    inputs: ticket.inputs,
                    input_origins: ticket.input_origins,
                },
                cancellation,
            ),
            _ => Box::pin(async {
                Err(Outcome::Failed(RuntimeCode::ExecutionFailed.error(
                    "Local work does not implement the conversation execution port.",
                    None,
                ))
                .into())
            }),
        }
    }
    fn streaming(&self, payload: &BoundTask) -> bool {
        payload.call().is_some_and(BoundCall::streaming)
    }
    fn execute_stream(
        &self,
        ticket: RunTicket<BoundTask>,
        cancellation: CancellationToken,
    ) -> StreamExecutionFuture {
        match ticket.payload {
            BoundTask::Call(call) => self.calls.execute_stream(
                RunTicket {
                    payload: call,
                    run: ticket.run,
                    inputs: ticket.inputs,
                    input_origins: ticket.input_origins,
                },
                cancellation,
            ),
            _ => Box::pin(async {
                Err(Outcome::Failed(RuntimeCode::ExecutionFailed.error(
                    "Local work does not implement the streaming execution port.",
                    None,
                ))
                .into())
            }),
        }
    }

    fn execute(
        &self,
        ticket: RunTicket<BoundTask>,
        cancellation: CancellationToken,
    ) -> ExecutionFuture {
        self.execute_reporting(
            ticket,
            cancellation,
            crate::driver::progress::Reporter::silent(),
        )
    }
    fn execute_reporting(
        &self,
        ticket: RunTicket<BoundTask>,
        cancellation: CancellationToken,
        progress: crate::driver::progress::Reporter,
    ) -> ExecutionFuture {
        let mut policy = ticket
            .inputs
            .values()
            .fold(wes_core::flow::FlowPolicy::default(), |p, v| {
                p.join(v.provenance().policy())
            });
        match &ticket.payload {
            BoundTask::Scan(scan) => policy = policy.join(&scan.literal_policy()),
            BoundTask::Accumulation(accumulation) => {
                policy = policy.join(&accumulation.captured_policy())
            }
            BoundTask::TypeCheck(checked) => {
                if let Input::Literal(value) = checked.input() {
                    policy = policy.join(value.provenance().policy());
                }
            }
            _ => {}
        }
        let private = policy.is_private()
            || ticket
                .inputs
                .values()
                .any(|v| v.provenance().policy().is_private())
            || match &ticket.payload {
                BoundTask::Call(call) => call.private_output(),
                BoundTask::Calculation(calc) => calc.private_output(),
                _ => false,
            };
        if private {
            policy = policy.private();
        }
        let progress = progress.with_policy(&policy);
        let work: ExecutionFuture = match ticket.payload {
            BoundTask::Input(value) => Box::pin(async move { Outcome::Produced(value).into() }),
            BoundTask::View(task) => Box::pin(async move { task.outcome().into() }),
            BoundTask::Management(task) => task.execute(cancellation),
            BoundTask::ImportPlan(task) => Box::pin(async move { task.outcome().into() }),
            BoundTask::Describe(task) => task.execute(cancellation),
            BoundTask::Call(call) => self.calls.execute(
                RunTicket {
                    payload: call,
                    run: ticket.run,
                    inputs: ticket.inputs,
                    input_origins: ticket.input_origins,
                },
                cancellation,
            ),
            BoundTask::Stream(op) => local_work(cancellation, move |token| {
                op.evaluate(&ticket.inputs, token)
            }),
            BoundTask::Calculation(calc) => {
                calc.execute(ticket.run, ticket.inputs, self.calls.clone(), cancellation)
            }
            BoundTask::Scan(scan) => scan.execute(
                RunTicket {
                    payload: (),
                    run: ticket.run,
                    inputs: ticket.inputs,
                    input_origins: ticket.input_origins,
                },
                self.scan_memory.clone(),
                cancellation,
                progress,
            ),
            BoundTask::TypeCheck(checked) => local_work(cancellation, move |token| {
                if token.is_cancelled() {
                    return cancelled();
                }
                let mut value = match checked
                    .input()
                    .resolve(&ticket.inputs)
                    .map(std::borrow::Cow::into_owned)
                {
                    Ok(value) => value,
                    Err(error) => {
                        return Outcome::Failed(error.error(RuntimeCode::InputFailed, "value"));
                    }
                };
                if !value.shape().is_assignable_to(&wes_core::Shape::Unknown) {
                    return Outcome::Failed(
                        RuntimeCode::InputFailed
                            .error("Data type checks cannot retype management values.", None),
                    );
                }
                let shape = checked.contract.shape();
                if matches!(checked.input(), Input::Literal(_))
                    && let Data::Text(text) = value.data()
                    && let Some(data) = literals::read(text, &shape)
                {
                    value = wes_core::Value::new(shape.clone(), data, value.provenance().clone())
                        .expect("contextual literal matches contract shape");
                }
                let issues = match checked
                    .contract
                    .issues_with_cancel(value.data(), &|| token.is_cancelled())
                {
                    Ok(issues) => issues,
                    Err(_) => return cancelled(),
                };
                if !issues.is_empty() {
                    return Outcome::Failed(
                        ErrorValue::new(
                            ErrorId::new(Uuid::new_v4().to_string()).expect("UUID is nonblank"),
                            "TYP005",
                            format!("value does not satisfy {}", checked.contract.name()),
                            issues,
                            None,
                        )
                        .expect("contract issues are valid"),
                    );
                }
                Outcome::Produced(
                    value
                        .with_shape(shape)
                        .expect("deep validation guarantees checked shape")
                        .with_metadata(Some(
                            wes_core::contracts::metadata::ValueMetadata::capture(
                                &checked.contract,
                            ),
                        )),
                )
            }),
            BoundTask::Accumulation(accumulation) => local_work(cancellation, move |token| {
                accumulation.evaluate(&ticket.inputs, token)
            }),
            BoundTask::Help(help) => local_work(cancellation, move |token| help.evaluate(token)),
            BoundTask::Query(query) => local_work(cancellation, move |token| {
                query.evaluate(&ticket.inputs, token)
            }),
        };
        Box::pin(async move {
            let mut report = work.await;
            report.notices = report
                .notices
                .into_iter()
                .map(|error| error.with_policy(&policy))
                .collect();
            report.outcome = report.outcome.with_policy(&policy);
            report
        })
    }
}
fn cancelled() -> Outcome {
    Outcome::Cancelled(RuntimeCode::Cancelled.error("The local operation was cancelled.", None))
}

/// One joined blocking boundary for trusted local work. Cancellation never detaches its closure.
fn local_work(
    cancellation: CancellationToken,
    work: impl FnOnce(&CancellationToken) -> Outcome + Send + 'static,
) -> ExecutionFuture {
    Box::pin(async move {
        if cancellation.is_cancelled() {
            return cancelled().into();
        }
        let token = cancellation.clone();
        let result = tokio::task::spawn_blocking(move || {
            if token.is_cancelled() {
                cancelled()
            } else {
                work(&token)
            }
        })
        .await;
        if cancellation.is_cancelled() {
            return cancelled().into();
        }
        result
            .unwrap_or_else(|_| {
                Outcome::Failed(
                    RuntimeCode::ExecutionFailed.error("Local work terminated unexpectedly.", None),
                )
            })
            .into()
    })
}
