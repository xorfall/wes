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
pub(crate) mod dataset;
mod help;
pub(crate) mod import_plan;
pub(crate) mod management;
pub(crate) mod reconcile;
pub(crate) mod recording;
pub use reconcile::ReconciliationControl;
pub use recording::RecordingControl;
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
    SourceLaunch(recording::BoundSourceLaunch),
    Describe(crate::describe::BoundDescribe),
    Calculation(crate::calc::BoundCalculation),
    Scan(crate::scan::BoundScan),
    ScanAttempt(crate::scan::BoundAttempt),
    ScanExcerpt(crate::scan::BoundExcerpt),
    ScanContinuation(crate::scan::BoundContinuation),
    Reconcile(reconcile::BoundReconcile),
    Dataset(dataset::BoundDataset),
    Recording(recording::BoundRecording),
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
}
/// Only native operations can select the control-plane result contract.
/// Provider values, pages, inspections and failures always carry full read labels.
#[derive(Clone, Copy, Default)]
pub(crate) enum ResultFlow {
    #[default]
    Content,
    ControlAcknowledgement,
}
impl ResultFlow {
    fn policy(
        self,
        outcome: &Outcome,
        inputs: &wes_core::flow::FlowPolicy,
    ) -> wes_core::flow::FlowPolicy {
        match (self, outcome) {
            (Self::ControlAcknowledgement, Outcome::Produced(_)) => {
                inputs.for_control_acknowledgement()
            }
            _ => inputs.clone(),
        }
    }
}
impl BoundTask {
    pub(crate) fn with_replaced_call(&self, source: BoundCall) -> Self {
        match self {
            Self::SourceLaunch(launch) => Self::SourceLaunch(recording::BoundSourceLaunch {
                source,
                setup: launch.setup.clone(),
                setup_run: None,
            }),
            _ => Self::Call(source),
        }
    }
    pub fn lifetime(&self) -> bool {
        match self {
            Self::Recording(recording) => recording.starts_lifetime(),
            Self::Scan(scan) => scan.live(),
            Self::ScanAttempt(resume) => resume.live(),
            _ => false,
        }
    }

    pub fn reconciliation_control(&self, joined: bool) -> Option<ReconciliationControl> {
        let command = match self {
            Self::Scan(scan) if scan.durable() => "scan",
            Self::ScanAttempt(_) => "scan",
            Self::Recording(recording) if recording.starts_lifetime() => "dataset",
            _ => return None,
        };
        Some(ReconciliationControl {
            command,
            available: joined,
        })
    }
    pub fn recording_control(&self, run: &str) -> Option<RecordingControl> {
        match self {
            Self::Recording(recording) => recording.control(run),
            _ => None,
        }
    }

    pub fn observational(&self) -> bool {
        if matches!(self, Self::ScanExcerpt(_) | Self::ScanContinuation(_)) {
            return true;
        }
        if let Self::Recording(task) = self {
            return task.command() == wes_language::vocabulary::MetaCommand::DatasetRecordingStatus;
        }
        if let Self::Dataset(task) = self {
            return task.observational();
        }
        matches!(
            self,
            Self::Management(_)
                | Self::Recording(_)
                | Self::Reconcile(_)
                | Self::Dataset(_)
                | Self::ImportPlan(_)
                | Self::Query(_)
                | Self::Help(_)
                | Self::View(_)
        )
    }

    pub(crate) fn set_authority(&mut self, authority: crate::environments::InvocationAuthority) {
        match self {
            Self::Call(call) => call.set_authority(authority),
            Self::SourceLaunch(launch) => launch.source.set_authority(authority),
            Self::Calculation(calc) => calc.set_authority(authority),
            Self::Dataset(task) => task.set_authority(authority),
            Self::Reconcile(task) => task.set_authority(authority),
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
            Self::SourceLaunch(launch) => Some(&launch.source),
            Self::Input(_)
            | Self::Describe(_)
            | Self::Stream(_)
            | Self::Calculation(_)
            | Self::Scan(_)
            | Self::ScanAttempt(_)
            | Self::ScanExcerpt(_)
            | Self::ScanContinuation(_)
            | Self::Reconcile(_)
            | Self::Dataset(_)
            | Self::Recording(_)
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
            Self::SourceLaunch(launch) => launch.source.traits(),
            Self::Describe(_) => ExecutionTraits {
                pure: false,
                repeatable: false,
                bounded: true,
            },
            Self::Calculation(calc) => calc.traits(),
            Self::Dataset(task) => ExecutionTraits {
                pure: false,
                repeatable: task.repeatable(),
                bounded: true,
            },
            Self::ScanExcerpt(_) | Self::ScanContinuation(_) | Self::Reconcile(_) => {
                ExecutionTraits {
                    pure: false,
                    repeatable: true,
                    bounded: true,
                }
            }
            Self::Scan(_) | Self::ScanAttempt(_) | Self::Recording(_) => ExecutionTraits {
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
            Self::Scan(_)
            | Self::ScanAttempt(_)
            | Self::ScanExcerpt(_)
            | Self::ScanContinuation(_)
            | Self::Reconcile(_)
            | Self::SourceLaunch(_) => crate::runtime::DependencyLifetime::Captured,
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
        let setup = match self {
            Self::SourceLaunch(launch) => Some(launch.setup.clone()),
            _ => None,
        };
        let call = self
            .call()
            .into_iter()
            .flat_map(BoundCall::dependencies)
            .chain(view)
            .chain(setup);
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
            Self::SourceLaunch(mut launch) => {
                launch.source = launch.source.with_admission(admission);
                Self::SourceLaunch(launch)
            }
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
    recordings: crate::eventlog::owner::Owner,
    calls: CallExecutor,
    scan_memory: crate::scan::ledger::MemoryPool,
    storage: Option<crate::storage::StoreWorker>,
}
impl TaskExecutor {
    pub fn ephemeral() -> Self {
        Self {
            recordings: Default::default(),
            calls: CallExecutor::ephemeral(),
            storage: None,
            scan_memory: crate::scan::ledger::MemoryPool::new(wes_budgets::get(
                "scan.aggregate.bytes",
            ))
            .expect("positive scan aggregate budget"),
        }
    }
    pub fn recorded(journal: CallJournal) -> Self {
        Self {
            recordings: Default::default(),
            calls: CallExecutor::recorded(journal),
            storage: None,
            scan_memory: crate::scan::ledger::MemoryPool::new(wes_budgets::get(
                "scan.aggregate.bytes",
            ))
            .expect("positive scan aggregate budget"),
        }
    }
    pub(crate) fn with_recordings(mut self, recordings: crate::eventlog::owner::Owner) -> Self {
        self.recordings = recordings;
        self
    }
    pub(crate) fn with_scan_memory(mut self, pool: crate::scan::ledger::MemoryPool) -> Self {
        self.scan_memory = pool;
        self
    }
    pub(crate) fn with_storage(mut self, storage: Option<crate::storage::StoreWorker>) -> Self {
        self.storage = storage;
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
    fn lifetime(&self, payload: &BoundTask) -> bool {
        payload.lifetime()
    }
    fn streaming(&self, payload: &BoundTask) -> bool {
        payload.call().is_some_and(BoundCall::streaming)
    }
    fn execute_stream(
        &self,
        ticket: RunTicket<BoundTask>,
        cancellation: CancellationToken,
    ) -> StreamExecutionFuture {
        let owner = self.recordings.clone();
        let run = ticket.run.clone();
        let required_setup = match &ticket.payload {
            BoundTask::SourceLaunch(launch) => match &launch.setup_run {
                Some(run) => Some((launch.setup.node.clone(), run.clone())),
                None => {
                    return Box::pin(async {
                        Err(Outcome::Failed(
                            RuntimeCode::ExecutionFailed
                                .error("Recording launch has no admitted setup run", None),
                        )
                        .into())
                    });
                }
            },
            _ => None,
        };
        let payload = match ticket.payload {
            BoundTask::SourceLaunch(launch) => BoundTask::Call(launch.source),
            other => other,
        };
        let opening = match payload {
            BoundTask::Call(call) => self.calls.prepare_stream(
                RunTicket {
                    payload: call,
                    run: ticket.run,
                    inputs: ticket.inputs,
                    input_origins: ticket.input_origins,
                },
                cancellation,
            ),
            _ => {
                return Box::pin(async {
                    Err(Outcome::Failed(
                        RuntimeCode::ExecutionFailed
                            .error("Local work has no source streaming port", None),
                    )
                    .into())
                });
            }
        };
        Box::pin(async move {
            let admission = opening.await?;
            let source_reservation = owner
                .reserve_source(&run)
                .map_err(|message| admission.refusal(message))?;
            let claims = match admission.recording_admission() {
                Some((definition, _)) => {
                    owner.claim_start(run.node(), &definition, required_setup.as_ref())
                }
                None if required_setup.is_some() => {
                    Err("Recording launch has no workspace-captured source definition".into())
                }
                None => Ok(Vec::new()),
            }
            .map_err(|message| admission.refusal(message))?;
            let policy = admission
                .recording_admission()
                .map(|(_, policy)| policy)
                .unwrap_or_default();
            let mut prepared = Vec::new();
            let mut preparation_failure = None;
            let mut claims = claims.into_iter();
            while let Some(claim) = claims.next() {
                match claim.prepare_writer(run.id().to_string(), &policy).await {
                    Ok(writer) => prepared.push((claim, writer)),
                    Err(message) => {
                        let required = claim.required;
                        claim.complete(Err(message.clone()));
                        if required {
                            for unentered in claims.by_ref() {
                                unentered.complete(Err(message.clone()));
                            }
                            preparation_failure = Some(message);
                            break;
                        }
                    }
                }
            }
            let mut admitted = Vec::new();
            for (claim, writer) in prepared {
                if claim.cancelled() {
                    if claim.required {
                        preparation_failure =
                            Some("Recording setup was cancelled before producer admission".into());
                    }
                    claim.reject(
                        writer,
                        "Recording was cancelled during preparation; this does not cancel its source".into(),
                    );
                } else {
                    admitted.push((claim, writer));
                }
            }
            let prepared = admitted;
            if let Some(message) = preparation_failure {
                for (claim, writer) in prepared {
                    claim.reject(writer, message.clone());
                }
                return Err(admission.refusal(message));
            }
            let archives = prepared
                .iter()
                .map(|(_, (writer, _))| writer.receiver.branch.clone())
                .collect();
            let (handle, task) = match admission.start(archives) {
                Ok(started) => started,
                Err(report) => {
                    for (claim, writer) in prepared {
                        claim.reject(
                            writer,
                            "Producer admission was refused before capture".into(),
                        );
                    }
                    return Err(report);
                }
            };
            for (claim, writer) in prepared {
                claim.complete(Ok(writer));
            }
            source_reservation.install(handle.clone());
            let task = task.after_join(move || owner.remove_source(&run));
            Ok((handle, task))
        })
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
        self.execute_owned(
            ticket,
            cancellation,
            progress,
            crate::driver::lifetime::Reporter::silent(),
        )
    }
    fn execute_lifetime(
        &self,
        ticket: RunTicket<BoundTask>,
        cancellation: CancellationToken,
        progress: crate::driver::progress::Reporter,
        values: crate::driver::lifetime::Reporter,
    ) -> ExecutionFuture {
        self.execute_owned(ticket, cancellation, progress, values)
    }
}
impl TaskExecutor {
    fn execute_owned(
        &self,
        ticket: RunTicket<BoundTask>,
        cancellation: CancellationToken,
        progress: crate::driver::progress::Reporter,
        values: crate::driver::lifetime::Reporter,
    ) -> ExecutionFuture {
        let mut policy = ticket
            .inputs
            .values()
            .fold(wes_core::flow::FlowPolicy::default(), |p, v| {
                p.join(v.provenance().policy())
            });
        match &ticket.payload {
            BoundTask::Scan(scan) => policy = policy.join(&scan.literal_policy()),
            BoundTask::Dataset(read) => policy = policy.join(&read.policy()),
            BoundTask::Recording(recording) => policy = policy.join(&recording.policy()),
            BoundTask::ScanAttempt(resume) => policy = policy.join(&resume.policy()),
            BoundTask::ScanContinuation(preview) => policy = policy.join(&preview.policy()),
            BoundTask::ScanExcerpt(excerpt) => policy = policy.join(&excerpt.policy()),
            BoundTask::Reconcile(reconcile) => policy = policy.join(&reconcile.policy()),
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
        policy = policy.join(&match &ticket.payload {
            BoundTask::Call(call) => call.output_policy(),
            BoundTask::SourceLaunch(launch) => launch.source.output_policy(),
            BoundTask::Calculation(calc) => calc.output_policy(),
            _ => Default::default(),
        });
        let result_flow = match &ticket.payload {
            BoundTask::Dataset(task) => task.result_flow(),
            _ => ResultFlow::Content,
        };
        let progress = progress.with_policy(&policy);
        let values = values.with_policy(&policy);
        let work: ExecutionFuture = match ticket.payload {
            BoundTask::Input(value) => Box::pin(async move { Outcome::Produced(value).into() }),
            BoundTask::View(task) => Box::pin(async move { task.outcome().into() }),
            BoundTask::SourceLaunch(_) => Box::pin(async {
                Outcome::Failed(RuntimeCode::ExecutionFailed.error(
                    "Recording launch requires its joined source admission port",
                    None,
                ))
                .into()
            }),
            BoundTask::Management(task) => task.execute(cancellation),
            BoundTask::ImportPlan(task) => Box::pin(async move { task.outcome().into() }),
            BoundTask::Describe(task) => task.execute(cancellation),
            BoundTask::Dataset(read) => read.execute(self.storage.clone(), cancellation),
            BoundTask::Recording(recording) => recording.execute(
                ticket.run,
                self.recordings.clone(),
                self.storage.clone(),
                cancellation,
                progress,
                values,
            ),
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
                self.storage.clone(),
                cancellation,
                progress,
                values,
            ),
            BoundTask::ScanContinuation(preview) => {
                preview.execute(self.storage.clone(), cancellation)
            }
            BoundTask::ScanExcerpt(excerpt) => excerpt.execute(self.storage.clone(), cancellation),
            BoundTask::Reconcile(reconcile) => {
                reconcile.execute(self.storage.clone(), cancellation)
            }
            BoundTask::ScanAttempt(resume) => resume.execute(
                ticket.run.id().to_string(),
                self.scan_memory.clone(),
                self.storage.clone(),
                cancellation,
                progress,
                values,
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
            let result_policy = result_flow.policy(&report.outcome, &policy);
            report.outcome = report.outcome.with_policy(&result_policy);
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

#[cfg(test)]
mod result_flow_tests {
    use super::*;
    #[test]
    fn only_successful_native_control_acknowledgements_project_read_labels() {
        let read_origin = serde_json::from_value(serde_json::json!({
            "store": Uuid::new_v4().to_string(), "dataset": Uuid::new_v4().to_string()
        }))
        .unwrap();
        let inputs = wes_core::flow::FlowPolicy::default()
            .with_dataset_read(read_origin)
            .from_origin("synthetic-environment")
            .private()
            .unknown();
        let value = wes_core::Value::new(
            wes_core::Shape::Primitive(wes_core::Primitive::Int),
            Data::Int(1),
            Provenance::default(),
        )
        .unwrap();
        let produced = Outcome::Produced(value);
        assert_eq!(ResultFlow::Content.policy(&produced, &inputs), inputs);
        let receipt = ResultFlow::ControlAcknowledgement.policy(&produced, &inputs);
        assert!(receipt.dataset_reads().is_empty());
        assert_eq!(receipt.origins(), inputs.origins());
        assert!(receipt.is_private() && receipt.is_unknown());
        for outcome in [
            cancelled(),
            Outcome::Failed(RuntimeCode::ExecutionFailed.error("synthetic refusal", None)),
            Outcome::Skipped,
        ] {
            assert_eq!(
                ResultFlow::ControlAcknowledgement.policy(&outcome, &inputs),
                inputs
            );
        }
        // Projection does not modify the input policy or defeat ordinary data-plane joins.
        assert_eq!(receipt.join(&inputs), inputs);
        assert_eq!(inputs.dataset_reads().len(), 1);
    }
}
