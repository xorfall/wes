//! Explicit lifetime attachment and controls captured from owned workspace work.
use crate::{
    driver::{CancellationToken, ExecutionFuture},
    eventlog::{self, owner::Owner},
    graph::{OutputPort, OutputRef},
    plan::{Input, MetaTask},
    runtime::{Outcome, Run, RuntimeCode},
    storage::StoreWorker,
    tasks::BoundTask,
    workspace::Workspace,
};
use std::sync::{Arc, Mutex, Weak};
use wes_core::{
    Data, Provenance, Value,
    contracts::{
        Contract, ContractField, ContractRegistry, ResolvedContractBundle, metadata::ValueMetadata,
    },
    flow::FlowPolicy,
};
use wes_language::{Diagnostic, Span, vocabulary::MetaCommand};
#[derive(Clone, Debug)]
pub struct BoundRecording {
    command: MetaCommand,
    subject: OutputRef,
    expected_run: Option<String>,
    from_start: bool,
    limits: eventlog::Limits,
    schema: Option<ResolvedContractBundle>,
    captured: Option<Result<Captured, String>>,
    policy: FlowPolicy,
    capability: RecordingCapability,
}
#[derive(Clone, Debug)]
enum Captured {
    Setup(Arc<()>),
    Run(String),
}
/// One-submit recording launch. The setup is a control prerequisite, never an event input.
#[derive(Clone, Debug)]
pub struct BoundSourceLaunch {
    pub(crate) source: crate::providers::BoundCall,
    pub(crate) setup: OutputRef,
    pub(crate) setup_run: Option<String>,
}
impl BoundSourceLaunch {
    pub(crate) fn capture(&mut self, workspace: &Workspace) {
        self.setup_run = workspace
            .runtime()
            .run_of(&self.setup.node)
            .map(ToString::to_string);
    }
}
/// Projection facts are derived from the admitted run, never from a descriptor's data.
#[derive(Clone, Copy, Debug, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RecordingControl {
    pub active: bool,
    pub status_available: bool,
    pub stop_available: bool,
    pub discard_available: bool,
}
#[derive(Clone, Default)]
struct RecordingCapability(Arc<Mutex<Option<(String, Weak<eventlog::owner::Recording>)>>>);
impl std::fmt::Debug for RecordingCapability {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("RecordingCapability")
    }
}
impl BoundRecording {
    pub(crate) fn bind(task: MetaTask, span: Span) -> Result<Self, Diagnostic> {
        let invalid = |message| Diagnostic::error("PLN004", span, message);
        let subject = if task.spec.command == MetaCommand::DatasetRecord {
            if !task.subjects.is_empty() {
                return Err(invalid("Recording uses one explicit source: selection"));
            }
            task.inputs.get("source")
        } else {
            match task.subjects.as_slice() {
                [subject] => Some(subject),
                _ => None,
            }
        };
        let Some(Input::FromNode(subject)) = subject else {
            return Err(invalid(
                "Recording requires one whole owned source or recording node",
            ));
        };
        if subject.port != OutputPort::Data {
            return Err(invalid(
                "Recording controls require the owned work's data selection",
            ));
        }
        let from_start = matches!(task.inputs.get("from"), Some(Input::Literal(value)) if matches!(value.data(), Data::Text(text) if text.as_ref() == "start"));
        if task.spec.command == MetaCommand::DatasetRecord {
            if task.inputs.get("budget").is_some_and(|input| !matches!(input, Input::Literal(value) if matches!(value.data(), Data::Text(text) if text.as_ref() == "Capture"))) {
                return Err(invalid("Recording uses the bounded Capture budget profile"));
            }
            if !matches!(task.inputs.get("from"), Some(Input::Literal(value)) if matches!(value.data(), Data::Text(text) if matches!(text.as_ref(), "start" | "next")))
            {
                return Err(invalid(
                    "Use from:start on a held or physically joined source or from:next on an active source; earlier window records cannot be recovered",
                ));
            }
        } else if task.inputs.keys().any(|key| key != "run") {
            return Err(invalid(
                "Recording controls preserve the exact owned recording run",
            ));
        }
        let expected_run = match task.inputs.get("run") {
            None => None,
            Some(Input::Literal(value)) => match value.data() {
                Data::Text(text)
                    if uuid::Uuid::parse_str(text)
                        .is_ok_and(|id| id.hyphenated().to_string() == text.as_ref()) =>
                {
                    Some(text.to_string())
                }
                _ => return Err(invalid("run: must be a literal canonical owned run UUID")),
            },
            _ => return Err(invalid("run: must be a literal canonical owned run UUID")),
        };
        Ok(Self {
            command: task.spec.command,
            subject: subject.clone(),
            expected_run,
            from_start,
            limits: eventlog::Limits::default(),
            schema: None,
            captured: None,
            policy: Default::default(),
            capability: Default::default(),
        })
    }
    pub(crate) fn control(&self, run: &str) -> Option<RecordingControl> {
        if !self.starts_lifetime() {
            return None;
        }
        let capability = self
            .capability
            .0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let (admitted, weak) = capability.as_ref()?;
        if admitted != run {
            return None;
        }
        weak.upgrade()?.control()
    }
    pub(crate) fn command(&self) -> MetaCommand {
        self.command
    }
    pub(crate) fn starts_lifetime(&self) -> bool {
        self.command == MetaCommand::DatasetRecord
    }
    pub(crate) fn policy(&self) -> FlowPolicy {
        self.policy.clone()
    }
    pub(crate) fn capture(&mut self, workspace: &Workspace) {
        self.captured = Some((|| {
            let runtime = workspace.runtime();
            let node = runtime.graph().node(&self.subject.node).ok_or("Selected work is no longer in this workspace")?;
            self.policy = workspace.typing(&self.subject).map_or_else(FlowPolicy::default, |typing| typing.provenance.policy().clone());
            if self.policy.is_private() || self.policy.is_unknown() { return Err("Recording selection is not exportable"); }
            if self.starts_lifetime() {
                let Some(call) = node.payload().call() else { return Err("Record requires an owned provider subscription, not a rolling list or stored value"); };
                if !call.streaming() { return Err("Selected work is not a streaming source"); }
                self.schema = Some(call.recording_schema().ok_or("Source has no captured complete inline element contract; recording cannot infer a schema from a window")?.clone());
                if self.from_start {
                    let launch = matches!(node.payload(), BoundTask::SourceLaunch(_));
                    let initial_launch = launch && runtime.run_of(&self.subject.node).is_none();
                    if runtime.is_executing(&self.subject.node) || (!initial_launch && matches!(node.state(), crate::graph::NodeState::Running | crate::graph::NodeState::Pending)) {
                        return Err("from:start requires a held or physically joined source; prepare before refreshing it explicitly, or use from:next for an active run");
                    }
                    return Ok(Captured::Setup(node.definition()));
                }
            } else {
                let BoundTask::Recording(original) = node.payload() else { return Err("Recording control requires original owned recording work, not a copied descriptor"); };
                if !original.starts_lifetime() { return Err("Recording controls cannot control another control operation"); }

            }
            let selected = runtime.run_of(&self.subject.node).ok_or("Selected work has no owned run")?;
            if self.expected_run.as_ref().is_some_and(|expected| expected != selected.as_str()) {
                return Err("Selected recording run changed; review the new status before controlling it");
            }
            Ok(Captured::Run(selected.to_string()))
        })().map_err(str::to_owned));
    }
    pub(crate) fn execute(
        self,
        run: Run,
        owner: Owner,
        worker: Option<StoreWorker>,
        token: CancellationToken,
        progress: crate::driver::progress::Reporter,
        values: crate::driver::lifetime::Reporter,
    ) -> ExecutionFuture {
        Box::pin(async move {
            let fail = |message| Outcome::Failed(RuntimeCode::ExecutionFailed.error(message, None));
            let selected = match self.captured {
                Some(Ok(run)) => run,
                Some(Err(message)) => return fail(message).into(),
                None => {
                    return fail("Recording was not captured by its workspace owner".into()).into();
                }
            };
            if token.is_cancelled() {
                return Outcome::Cancelled(
                    RuntimeCode::Cancelled.error("Recording admission cancelled", None),
                )
                .into();
            }
            let Some(worker) = worker else {
                return fail("Recording requires an owned durable store".into()).into();
            };
            if self.command != MetaCommand::DatasetRecord {
                let receipt = match owner.recording(
                    &self.subject.node,
                    match &selected {
                        Captured::Run(run) => run,
                        _ => return fail("Control requires an owned recording run".into()).into(),
                    },
                ) {
                    Ok(r) => r,
                    Err(e) => return fail(e).into(),
                };
                if self.command == MetaCommand::DatasetDiscardRecording {
                    return match receipt.discard() {
                        Ok(snapshot) => {
                            Outcome::Produced(setup_value(snapshot, &receipt.policy)).into()
                        }
                        Err(message) => fail(message).into(),
                    };
                }
                if let Some(snapshot) = receipt.setup_snapshot() {
                    return if self.command == MetaCommand::DatasetStopRecording {
                        fail(match snapshot.phase {
                            eventlog::intent::Phase::Prepared => "Recording has no attached writer; discard an unused setup instead".into(),
                            eventlog::intent::Phase::Attaching | eventlog::intent::Phase::Attached => "Recording attachment is being acknowledged; wait for the local preparation to join".into(),
                            eventlog::intent::Phase::Failed => "Recording setup failed to attach; its local preparation is being joined".into(),
                            phase => format!("Recording setup is {}; it has no attached writer", phase.name()),
                        }).into()
                    } else {
                        Outcome::Produced(setup_value(snapshot, &receipt.policy)).into()
                    };
                }
                let status = if self.command == MetaCommand::DatasetStopRecording {
                    receipt.stop();
                    // A Stop reply acknowledges joined physical work even when this control is cancelled.
                    receipt.joined().await
                } else {
                    receipt.snapshot()
                };
                return match status {
                    Ok(status) => {
                        checked_outcome(&worker, &receipt.schema, &receipt.policy, status).await
                    }
                    Err(message) => fail(message).into(),
                };
            }
            let Some(schema) = self.schema else {
                return fail("Recording has no captured schema".into()).into();
            };
            let limits = self.limits;
            let policy;
            let receipt;
            let prepared;
            let _lifetime;
            if self.from_start {
                let Captured::Setup(definition) = selected else {
                    return fail("Held source definition was not captured".into()).into();
                };
                let (reserved, intent, receive) = match owner.reserve_start(
                    &run,
                    self.subject.node.clone(),
                    definition,
                    schema.clone(),
                    self.policy.clone(),
                    worker.clone(),
                    limits,
                ) {
                    Ok(reserved) => reserved,
                    Err(message) => return fail(message).into(),
                };
                receipt = reserved;
                _lifetime = receipt.lifetime();
                policy = self.policy;
                *self
                    .capability
                    .0
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner) =
                    Some((run.id().to_string(), Arc::downgrade(&receipt)));
                if !values
                    .publish(setup_value(intent.snapshot(), &policy))
                    .await
                {
                    let _ = intent.discard();
                }
                prepared = intent.receive(receive, &token).await;
                if let Err(message) = &prepared {
                    receipt.finish(Err(message.clone()));
                    let snapshot = intent.snapshot();
                    return if matches!(
                        snapshot.phase,
                        eventlog::intent::Phase::Discarded
                            | eventlog::intent::Phase::Expired
                            | eventlog::intent::Phase::Cancelled
                    ) {
                        Outcome::Produced(setup_value(snapshot, &policy)).into()
                    } else {
                        fail(message.clone()).into()
                    };
                }
            } else {
                let source = match owner.source(
                    &self.subject.node,
                    match &selected {
                        Captured::Run(run) => run,
                        _ => return fail("Active source run was not captured".into()).into(),
                    },
                ) {
                    Ok(source) => source,
                    Err(message) => return fail(message).into(),
                };
                policy = self
                    .policy
                    .join(source.snapshot().window.provenance().policy());
                if policy.is_private() || policy.is_unknown() {
                    return fail("Source run is not exportable".into()).into();
                }
                receipt = match owner.reserve(
                    &run,
                    schema.clone(),
                    policy.clone(),
                    worker.dataset_access(),
                ) {
                    Ok(receipt) => receipt,
                    Err(message) => return fail(message).into(),
                };
                _lifetime = receipt.lifetime();
                *self
                    .capability
                    .0
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner) =
                    Some((run.id().to_string(), Arc::downgrade(&receipt)));
                prepared = eventlog::Prepared::from_next(
                    worker.clone(),
                    &source,
                    run.id().to_string(),
                    schema.clone(),
                    policy.clone(),
                    limits,
                )
                .await
                .map(|(writer, handle)| eventlog::intent::Admission::Attached(writer, handle))
                .map_err(|e| e.to_string());
            }
            let (prepared, handle, refusal) = match prepared {
                Ok(eventlog::intent::Admission::Attached(writer, handle)) => (writer, handle, None),
                Ok(eventlog::intent::Admission::Refused {
                    writer,
                    handle,
                    reason,
                }) => (writer, handle, Some(reason)),
                Err(message) => {
                    receipt.finish(Err(message.clone()));
                    return fail(message).into();
                }
            };
            receipt.install(handle.clone());
            let progress = progress.with_policy(
                &policy
                    .clone()
                    .read_from_dataset(&handle.snapshot().reference),
            );
            progress.report(crate::driver::progress::ExecutionProgress::recording(
                &handle.snapshot(),
                limits,
            ));
            let task = match prepared.start() {
                Ok(task) => task,
                Err(error) => {
                    let message = error.to_string();
                    receipt.finish(Err(message.clone()));
                    return fail(message).into();
                }
            };
            // The initial descriptor names only the acknowledged prefix. Publishing it
            // does not finish the writer, create another run, or expose a queue suffix.
            let admitted = if refusal.is_some() || token.is_cancelled() {
                false
            } else {
                let initial = checked_outcome(&worker, &schema, &policy, handle.snapshot()).await;
                match initial.outcome {
                    Outcome::Produced(value) if !token.is_cancelled() => {
                        values.publish(value).await
                    }
                    _ => false,
                }
            };
            if !admitted && refusal.is_none() {
                handle.stop();
            }
            let joined = task.join();
            tokio::pin!(joined);
            let cadence = std::time::Duration::from_millis(100);
            let mut ticks =
                tokio::time::interval_at(tokio::time::Instant::now() + cadence, cadence);
            let mut updates = handle.subscribe();
            updates.borrow_and_update();
            let mut dirty = false;
            let mut updates_open = true;
            ticks.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
            let result = loop {
                tokio::select! {
                    biased;
                    result = &mut joined => break result,
                    _ = token.cancelled() => { handle.stop(); break joined.await; }
                    _ = ticks.tick(), if dirty => {
                        progress.report(crate::driver::progress::ExecutionProgress::recording(&handle.snapshot(), limits));
                        dirty = false;
                    }
                    changed = updates.changed(), if updates_open => {
                        updates_open = changed.is_ok();
                        dirty |= updates_open;
                    }
                }
            }.map_err(|_| "Recording lifetime ended without a physical join acknowledgement".to_owned());
            if let Ok(status) = &result {
                progress.report(crate::driver::progress::ExecutionProgress::recording(
                    status, limits,
                ));
            }
            receipt.finish(result.clone());
            match result {
                Ok(status) => {
                    let final_progress =
                        crate::driver::progress::ExecutionProgress::recording(&status, limits)
                            .restricted(&policy.clone().read_from_dataset(&status.reference));
                    let mut report = checked_outcome(&worker, &schema, &policy, status).await;
                    report.progress = Some(final_progress);
                    if !admitted
                        && let Outcome::Produced(value) | Outcome::Incomplete { value, .. } =
                            report.outcome
                    {
                        report.outcome = Outcome::Incomplete { value, error: RuntimeCode::ExecutionFailed.error(
                            refusal.as_ref().map(|reason| format!("{reason}; local recording preparation was joined and only its confirmed incomplete prefix remains")).unwrap_or_else(|| "Recording stopped because its initial descriptor was not admitted; only the confirmed prefix remains".into()), None) };
                    }
                    report
                }
                Err(message) => fail(message).into(),
            }
        })
    }
}
async fn checked_outcome(
    worker: &StoreWorker,
    schema: &ResolvedContractBundle,
    policy: &FlowPolicy,
    status: eventlog::Status,
) -> crate::driver::ExecutionReport {
    let info = match worker.dataset_inspect(status.reference.clone()).await {
        Ok(info) => info,
        Err(error) => {
            return Outcome::Failed(RuntimeCode::ExecutionFailed.error(error.to_string(), None))
                .into();
        }
    };
    let policy = policy
        .join(&info.policy)
        .read_from_dataset(&status.reference);
    let registry = ContractRegistry::new();
    let text = registry.resolve("Text").expect("builtin text");
    let dataset = Arc::new(
        Contract::dataset("RecordedEvents", schema.root().clone())
            .expect("captured dataset contract"),
    );
    let contract = Contract::record(
        "Recording",
        [
            (
                "dataset".into(),
                ContractField {
                    contract: dataset,
                    optional: false,
                },
            ),
            (
                "phase".into(),
                ContractField {
                    contract: text.clone(),
                    optional: false,
                },
            ),
            (
                "acceptedThrough".into(),
                ContractField {
                    contract: text.clone(),
                    optional: false,
                },
            ),
            (
                "committedThrough".into(),
                ContractField {
                    contract: text.clone(),
                    optional: false,
                },
            ),
            (
                "termination".into(),
                ContractField {
                    contract: text,
                    optional: false,
                },
            ),
        ]
        .into(),
    )
    .expect("recording descriptor contract");
    let value = Value::new(
        contract.shape(),
        Data::Record(
            [
                ("dataset".into(), Data::Dataset(Arc::new(status.reference))),
                (
                    "phase".into(),
                    Data::Text(format!("{:?}", status.phase).to_lowercase().into()),
                ),
                (
                    "acceptedThrough".into(),
                    Data::Text(status.coverage.accepted_through.to_string().into()),
                ),
                (
                    "committedThrough".into(),
                    Data::Text(status.coverage.committed_through.to_string().into()),
                ),
                (
                    "termination".into(),
                    Data::Text(
                        status
                            .coverage
                            .termination
                            .map_or_else(
                                || "open".into(),
                                |end| format!("{:?}", end).to_lowercase(),
                            )
                            .into(),
                    ),
                ),
            ]
            .into(),
        ),
        Provenance::default().with_policy(&policy),
    )
    .expect("captured recording descriptor")
    .with_metadata(Some(ValueMetadata::capture(&contract)));
    if crate::value_size::value_charge(
        &value,
        wes_budgets::get("execution.publication.value.bytes").min(wes_budgets::get("query.bytes")),
    )
    .is_none()
    {
        return Outcome::Failed(RuntimeCode::ExecutionFailed.error(
            "Recording descriptor exceeds its publication charge limit",
            None,
        ))
        .into();
    }
    match status.phase {
        eventlog::Phase::Incomplete | eventlog::Phase::Unconfirmed => Outcome::Incomplete { value, error: RuntimeCode::ExecutionFailed.error("Recording stopped with incomplete coverage; the descriptor reports only its acknowledged prefix", None) }.into(),
        _ => Outcome::Produced(value).into(),
    }
}

fn setup_value(snapshot: eventlog::intent::Snapshot, policy: &FlowPolicy) -> Value {
    let registry = ContractRegistry::new();
    let text = registry.resolve("Text").expect("builtin text");
    let contract = Contract::record(
        "RecordingSetup",
        ["sourceNode", "phase", "remainingMs"]
            .into_iter()
            .map(|name| {
                (
                    name.to_owned(),
                    ContractField {
                        contract: text.clone(),
                        optional: false,
                    },
                )
            })
            .collect(),
    )
    .expect("setup contract");
    Value::new(
        contract.shape(),
        Data::Record(
            [
                (
                    "sourceNode".into(),
                    Data::Text(snapshot.source.as_str().into()),
                ),
                ("phase".into(), Data::Text(snapshot.phase.name().into())),
                (
                    "remainingMs".into(),
                    Data::Text(snapshot.remaining_ms.to_string().into()),
                ),
            ]
            .into(),
        ),
        Provenance::default().with_policy(policy),
    )
    .expect("bounded setup receipt")
    .with_metadata(Some(ValueMetadata::capture(&contract)))
}
