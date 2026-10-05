use indexmap::IndexMap;
use std::{
    num::NonZeroUsize,
    sync::{Arc, Mutex, mpsc},
    time::Duration,
};
use tokio::sync::oneshot;
use wes_core::{
    Data, ErrorId, ErrorValue, Provenance, Shape, Value,
    capability::{Capability, Parameter, ProviderDescription, Safety},
    contracts::ContractRegistry,
};
use wes_engine::{
    calls::{CallJournal, CallJournalError, RequiredPersistence},
    driver::{self, CancellationToken, Command, Executor, Reply},
    graph::{NodeId, NodeState, OutputRef},
    history::{
        AppendReceipt, CommandRecord, JournalEntry, JournalSink, Persistence, Record, RecordError,
        RecoveryEntry,
    },
    plan::{self, Guards, Plan, Task},
    providers::{
        BoundCall, Call, CallExecutor, InvocationError, InvocationFuture, Invoker, Providers,
    },
    recording::{Recorder, RecorderLimits, RecorderTask, spawn_recorder},
    runtime::{Effect, Outcome, RunTicket, Runtime},
};
use wes_language::{Expression, SourceText, parse, resolve::resolve};
#[path = "calls/batches.rs"]
mod batches;

type Timeline = Arc<Mutex<Vec<&'static str>>>;
type Gate = (&'static str, oneshot::Sender<()>, mpsc::Receiver<()>);
struct Sink {
    records: Arc<Mutex<Vec<Record>>>,
    timeline: Timeline,
    gate: Option<Gate>,
    fail: Option<&'static str>,
    persistence: Persistence,
}
fn kind(record: &Record) -> &'static str {
    match record {
        Record::Journal(JournalEntry::Command(_)) => "command",
        Record::Recovery(RecoveryEntry::Accepted { .. }) => "accepted",
        Record::Recovery(RecoveryEntry::Calling(_)) => "calling",
        Record::Recovery(RecoveryEntry::Called { .. }) => "called",
        _ => panic!("unexpected test record"),
    }
}
impl JournalSink for Sink {
    fn append(&mut self, record: &Record) -> Result<AppendReceipt, RecordError> {
        let phase = kind(record);
        if self.gate.as_ref().is_some_and(|gate| gate.0 == phase) {
            let (_, entered, release) = self.gate.take().unwrap();
            entered.send(()).unwrap();
            release
                .recv_timeout(Duration::from_secs(5))
                .expect("release test writer");
        }
        self.records.lock().unwrap().push(record.clone());
        self.timeline.lock().unwrap().push(phase);
        if self.fail == Some(phase) {
            return Err(RecordError::backend(
                "test append",
                true,
                std::io::Error::other("private disk detail"),
            ));
        }
        Ok(AppendReceipt {
            persistence: self.persistence,
            end_offset: self.records.lock().unwrap().len() as u64,
        })
    }
}
fn sink() -> Sink {
    Sink {
        records: Arc::default(),
        timeline: Arc::default(),
        gate: None,
        fail: None,
        persistence: Persistence::FileSynced,
    }
}
struct Recording {
    journal: CallJournal,
    recorder: Recorder,
    task: RecorderTask,
    records: Arc<Mutex<Vec<Record>>>,
    timeline: Timeline,
}
impl Recording {
    fn new(sink: Sink) -> Self {
        let records = sink.records.clone();
        let timeline = sink.timeline.clone();
        let (recorder, task) = spawn_recorder(sink, RecorderLimits::default()).unwrap();
        let journal = CallJournal::new(recorder.clone(), RequiredPersistence::FileSynced);
        Self {
            journal,
            recorder,
            task,
            records,
            timeline,
        }
    }
    async fn admit(&self, mut ticket: RunTicket<BoundCall>) -> RunTicket<BoundCall> {
        let receipt = self
            .journal
            .admit(command(ticket.run.node().clone()))
            .await
            .unwrap();
        ticket.payload = ticket.payload.with_admission(receipt);
        ticket
    }
    async fn finish(self) {
        self.recorder.shutdown().await.unwrap();
        self.task.join().await.unwrap();
    }
}
fn command(node: NodeId) -> CommandRecord {
    CommandRecord {
        source_name: "fixture.wes".into(),
        source_start: wes_language::Position { line: 1, column: 1 },
        changed_nodes: vec![],
        document: None,
        revision_of: None,
        environments: None,
        cell: "cell-1".into(),
        text: "catalog echo value:hello".into(),
        nodes: vec![node],
        type_sources: IndexMap::new(),
        calculation_package: None,
        imports: vec![],
        replay: "catalog echo value:hello".into(),
    }
}
struct Echo {
    timeline: Timeline,
    result: Option<Result<Value, InvocationError>>,
    panic: bool,
}
impl Invoker for Echo {
    fn invoke(&self, call: Call, _: CancellationToken) -> InvocationFuture {
        self.timeline.lock().unwrap().push("invoke");
        assert!(!self.panic, "private provider panic payload");
        let result = self
            .result
            .clone()
            .unwrap_or_else(|| Ok(call.arguments["value"].clone()));
        Box::pin(async move { result })
    }
}
fn ticket_with(echo: Echo, guarded: bool) -> RunTicket<BoundCall> {
    let mut capability = Capability::new(["echo"], Shape::Unknown, Safety::Safe);
    capability.parameters = vec![Parameter::new("value", Shape::Unknown, true)];
    let mut providers =
        Providers::local(wes_engine::providers::LocalScope::new("fixture").unwrap());
    providers.register(
        ProviderDescription::new("catalog", [capability], vec![]).unwrap(),
        Arc::new(echo),
    );
    let source = if guarded {
        "catalog echo value:$source"
    } else {
        "catalog echo value:hello"
    };
    let parsed = parse(&SourceText::new("test", source));
    let statement = &parsed.script.statements[0];
    let Expression::Call(call) = &statement.expression else {
        panic!("call")
    };
    let resolution = resolve(call, providers.catalogue()).unwrap();
    let Plan::NewNode(node) = plan::plan(&resolution, statement, &Guards::new(), &|_| {
        Some(OutputRef::data(NodeId::new("source").unwrap()))
    })
    .unwrap() else {
        panic!("node")
    };
    let Task::Invoke(mut invocation) = node.task else {
        panic!("invoke")
    };
    if guarded {
        let mut registry = ContractRegistry::new();
        registry
            .load("types: {Positive: {base: Int, min: 1}}")
            .unwrap();
        invocation
            .guards
            .insert("value".into(), vec![registry.resolve("Positive").unwrap()]);
    }
    let bound = providers.bind_finite(invocation).unwrap();
    let mut runtime = Runtime::new();
    runtime.add(bound.clone(), [], bound.traits()).unwrap();
    let mut ticket = runtime
        .start(Duration::ZERO)
        .into_iter()
        .find_map(|effect| match effect {
            Effect::Spawn(ticket) => Some(ticket),
            _ => None,
        })
        .unwrap();
    if guarded {
        ticket.inputs.insert(
            NodeId::new("source").unwrap(),
            Value::new(
                Shape::Unknown,
                Data::Text("3".into()),
                Provenance::default(),
            )
            .unwrap(),
        );
    }
    ticket
}
fn ticket(recording: &Recording) -> RunTicket<BoundCall> {
    ticket_with(
        Echo {
            timeline: recording.timeline.clone(),
            result: None,
            panic: false,
        },
        false,
    )
}
fn failed(outcome: &Outcome, code: &str) {
    let Outcome::Failed(error) = outcome else {
        panic!("failed: {outcome:?}")
    };
    assert_eq!(error.code(), code);
    assert!(!error.message().contains("private"));
}

#[tokio::test]
async fn command_and_run_receipts_precede_provider_entry() {
    let recording = Recording::new(sink());
    let ticket = recording.admit(ticket(&recording)).await;
    let run = ticket.run.clone();
    let report = CallExecutor::recorded(recording.journal.clone())
        .execute(ticket, CancellationToken::new())
        .await;
    assert!(matches!(report.outcome, Outcome::Produced(_)));
    assert!(report.notices.is_empty());
    assert_eq!(
        *recording.timeline.lock().unwrap(),
        ["command", "accepted", "calling", "invoke", "called"]
    );
    let records = recording.records.lock().unwrap().clone();
    let Record::Recovery(RecoveryEntry::Calling(call)) = &records[2] else {
        panic!("calling")
    };
    assert_eq!(&call.run, run.id());
    assert_eq!(&call.node, run.node());
    assert_eq!(call.cell, "cell-1");
    assert_eq!(call.capability, "catalog echo");
    assert!(call.safe);
    assert!(
        matches!(&records[3], Record::Recovery(RecoveryEntry::Called { run: id, produced: true, .. }) if id == run.id())
    );
    recording.finish().await;
}

#[tokio::test]
async fn input_validation_precedes_calling_and_never_converts_reference_text() {
    let recording = Recording::new(sink());
    let work = ticket_with(
        Echo {
            timeline: recording.timeline.clone(),
            result: None,
            panic: false,
        },
        true,
    );
    let work = recording.admit(work).await;
    let report = CallExecutor::recorded(recording.journal.clone())
        .execute(work, CancellationToken::new())
        .await;
    failed(&report.outcome, "TYP005");
    assert_eq!(*recording.timeline.lock().unwrap(), ["command", "accepted"]);
    recording.finish().await;
}

#[tokio::test]
async fn waiting_for_calling_keeps_async_timers_alive_and_cancellation_prevents_entry() {
    let mut sink = sink();
    let (entered, receive) = oneshot::channel();
    let (release, gate) = mpsc::channel();
    sink.gate = Some(("calling", entered, gate));
    let recording = Recording::new(sink);
    let work = recording.admit(ticket(&recording)).await;
    let token = CancellationToken::new();
    let mut executing = tokio::spawn(
        CallExecutor::recorded(recording.journal.clone()).execute(work, token.clone()),
    );
    receive.await.unwrap();
    token.cancel();
    assert!(
        tokio::time::timeout(Duration::from_millis(5), &mut executing)
            .await
            .is_err()
    );
    assert_eq!(*recording.timeline.lock().unwrap(), ["command", "accepted"]);
    release.send(()).unwrap();
    let report = executing.await.unwrap();
    assert!(matches!(report.outcome, Outcome::Cancelled(_)));
    assert!(report.notices.is_empty());
    assert_eq!(
        *recording.timeline.lock().unwrap(),
        ["command", "accepted", "calling", "called"]
    );
    assert!(matches!(
        recording.records.lock().unwrap().last(),
        Some(Record::Recovery(RecoveryEntry::Called {
            produced: false,
            ..
        }))
    ));
    recording.finish().await;
}

#[tokio::test]
async fn calling_failure_blocks_provider_and_redacts_backend_details() {
    let mut sink = sink();
    sink.fail = Some("calling");
    let recording = Recording::new(sink);
    let work = recording.admit(ticket(&recording)).await;
    let report = CallExecutor::recorded(recording.journal.clone())
        .execute(work, CancellationToken::new())
        .await;
    failed(&report.outcome, "RUN005");
    assert_eq!(
        *recording.timeline.lock().unwrap(),
        ["command", "accepted", "calling"]
    );
    recording.finish().await;
}

#[tokio::test]
async fn missing_foreign_and_wrong_node_receipts_cannot_authorize_calls_or_downgrade_mode() {
    let recording = Recording::new(sink());
    let foreign = Recording::new(sink());
    let work = ticket(&recording);
    let report = CallExecutor::recorded(recording.journal.clone())
        .execute(work.clone(), CancellationToken::new())
        .await;
    failed(&report.outcome, "RUN005");
    let wrong = recording
        .journal
        .admit(command(NodeId::new("other").unwrap()))
        .await
        .unwrap();
    let mut wrong_work = work.clone();
    wrong_work.payload = wrong_work.payload.with_admission(wrong);
    let report = CallExecutor::recorded(recording.journal.clone())
        .execute(wrong_work, CancellationToken::new())
        .await;
    failed(&report.outcome, "RUN005");
    let foreign_work = foreign.admit(work.clone()).await;
    let report = CallExecutor::recorded(recording.journal.clone())
        .execute(foreign_work, CancellationToken::new())
        .await;
    failed(&report.outcome, "RUN005");
    let work = recording.admit(work).await;
    let report = CallExecutor::ephemeral()
        .execute(work, CancellationToken::new())
        .await;
    failed(&report.outcome, "RUN005");
    assert!(
        !recording
            .timeline
            .lock()
            .unwrap()
            .iter()
            .any(|phase| matches!(*phase, "calling" | "invoke"))
    );
    foreign.finish().await;
    recording.finish().await;
}

#[tokio::test]
async fn admission_requires_both_receipts_at_the_configured_persistence_level() {
    for phase in ["command", "accepted"] {
        let mut sink = sink();
        sink.fail = Some(phase);
        let recording = Recording::new(sink);
        assert!(matches!(
            recording
                .journal
                .admit(command(NodeId::new("id1000").unwrap()))
                .await,
            Err(CallJournalError::Recording(_))
        ));
        assert_eq!(
            recording.records.lock().unwrap().len(),
            if phase == "command" { 1 } else { 2 }
        );
        recording.finish().await;
    }
    for (persistence, required, accepted) in [
        (
            Persistence::Volatile,
            RequiredPersistence::FileSynced,
            false,
        ),
        (
            Persistence::FileSynced,
            RequiredPersistence::FileAndDirectorySynced,
            false,
        ),
        (
            Persistence::FileAndDirectorySynced,
            RequiredPersistence::FileSynced,
            true,
        ),
        (Persistence::Volatile, RequiredPersistence::Volatile, true),
    ] {
        let mut sink = sink();
        sink.persistence = persistence;
        let recording = Recording::new(sink);
        let journal = CallJournal::new(recording.recorder.clone(), required);
        let result = journal.admit(command(NodeId::new("id1000").unwrap())).await;
        assert_eq!(result.is_ok(), accepted);
        if !accepted {
            assert!(matches!(
                result,
                Err(CallJournalError::InsufficientPersistence)
            ));
        }
        recording.finish().await;
    }
}

#[tokio::test]
async fn completion_record_failure_preserves_success_error_identity_and_cancellation() {
    let original = ErrorValue::new(
        ErrorId::new("original-error").unwrap(),
        "API001",
        "provider rejected",
        vec![],
        None,
    )
    .unwrap();
    for result in [
        None,
        Some(Err(InvocationError::Failed(original.clone()))),
        Some(Err(InvocationError::Cancelled)),
    ] {
        let mut sink = sink();
        sink.fail = Some("called");
        let recording = Recording::new(sink);
        let work = ticket_with(
            Echo {
                timeline: recording.timeline.clone(),
                result: result.clone(),
                panic: false,
            },
            false,
        );
        let work = recording.admit(work).await;
        let report = CallExecutor::recorded(recording.journal.clone())
            .execute(work, CancellationToken::new())
            .await;
        match result {
            None => assert!(matches!(report.outcome, Outcome::Produced(_))),
            Some(Err(InvocationError::Failed(_))) => {
                assert!(
                    matches!(&report.outcome, Outcome::Failed(error) if error == &original.clone().with_policy(&wes_core::flow::FlowPolicy::default().from_origin("local:fixture")))
                )
            }
            Some(Err(InvocationError::Cancelled)) => {
                assert!(matches!(report.outcome, Outcome::Cancelled(_)))
            }
            _ => unreachable!(),
        }
        assert_eq!(report.notices.len(), 1);
        assert_eq!(report.notices[0].code(), "RUN005");
        assert!(!report.notices[0].message().contains("private"));
        recording.finish().await;
    }
}

#[tokio::test]
async fn remote_uncertainty_has_an_operational_notice_independent_of_node_publication() {
    let original = ErrorValue::new(
        ErrorId::new("remote-unknown").unwrap(),
        "ENV036",
        "remote exec may still be running",
        vec![],
        None,
    )
    .unwrap();
    let recording = Recording::new(sink());
    let work = ticket_with(
        Echo {
            timeline: recording.timeline.clone(),
            result: Some(Err(InvocationError::Failed(original))),
            panic: false,
        },
        false,
    );
    let work = recording.admit(work).await;
    let report = CallExecutor::recorded(recording.journal.clone())
        .execute(work, CancellationToken::new())
        .await;
    assert!(matches!(&report.outcome, Outcome::Failed(error) if error.code() == "ENV036"));
    assert_eq!(report.notices.len(), 1);
    assert_eq!(report.notices[0].code(), "ENV036");
    recording.finish().await;
}

#[tokio::test]
async fn synchronous_provider_panic_closes_the_local_attempt_without_exposing_its_payload() {
    let recording = Recording::new(sink());
    let work = ticket_with(
        Echo {
            timeline: recording.timeline.clone(),
            result: None,
            panic: true,
        },
        false,
    );
    let work = recording.admit(work).await;
    let report = CallExecutor::recorded(recording.journal.clone())
        .execute(work, CancellationToken::new())
        .await;
    failed(&report.outcome, "RUN001");
    assert!(report.notices.is_empty());
    assert_eq!(
        *recording.timeline.lock().unwrap(),
        ["command", "accepted", "calling", "invoke", "called"]
    );
    recording.finish().await;
}

#[tokio::test]
async fn driver_reports_recording_problem_separately_from_ready_value() {
    let mut sink = sink();
    sink.fail = Some("called");
    let recording = Recording::new(sink);
    let work = recording.admit(ticket(&recording)).await;
    let mut runtime = Runtime::new();
    let node = runtime
        .add(work.payload.clone(), [], work.payload.traits())
        .unwrap();
    let (handle, driver_task) = driver::spawn(
        runtime,
        Arc::new(CallExecutor::recorded(recording.journal.clone())),
        NonZeroUsize::new(1).unwrap(),
    )
    .unwrap();
    let mut notices = handle.subscribe_notices().unwrap();
    handle.command(Command::Start).await.unwrap();
    handle.wait_idle().await.unwrap();
    let notice = notices.recv().await.unwrap();
    assert_eq!(notice.run.node(), &node);
    assert_eq!(notice.error.code(), "RUN005");
    let Reply::Snapshot(snapshot) = handle.command(Command::Snapshot).await.unwrap() else {
        panic!("snapshot")
    };
    assert_eq!(
        snapshot.graph.node(&node).unwrap().state(),
        NodeState::Ready
    );
    assert!(snapshot.values.contains_key(&node));
    assert!(!snapshot.errors.contains_key(&node));
    assert_eq!(snapshot.runs.get(&node), Some(notice.run.id()));
    handle.command(Command::Shutdown).await.unwrap();
    driver_task.join().await.unwrap();
    recording.finish().await;
}

#[tokio::test]
async fn cancellation_while_finishing_recording_retains_lease_and_reports_late_recording_fault() {
    let mut sink = sink();
    sink.fail = Some("called");
    let (entered, receive) = oneshot::channel();
    let (release, gate) = mpsc::channel();
    sink.gate = Some(("called", entered, gate));
    let recording = Recording::new(sink);
    let work = recording.admit(ticket(&recording)).await;
    let mut runtime = Runtime::new();
    let node = runtime
        .add(work.payload.clone(), [], work.payload.traits())
        .unwrap();
    let (handle, driver_task) = driver::spawn(
        runtime,
        Arc::new(CallExecutor::recorded(recording.journal.clone())),
        NonZeroUsize::new(1).unwrap(),
    )
    .unwrap();
    let mut notices = handle.subscribe_notices().unwrap();
    handle.command(Command::Start).await.unwrap();
    receive.await.unwrap(); // The provider has returned, but the accepted completion write has not.
    handle.command(Command::Cancel(node.clone())).await.unwrap();
    assert!(
        tokio::time::timeout(Duration::from_millis(5), handle.wait_idle())
            .await
            .is_err()
    );
    let Reply::Snapshot(snapshot) = handle.command(Command::Snapshot).await.unwrap() else {
        panic!("snapshot")
    };
    assert_eq!(
        snapshot.graph.node(&node).unwrap().state(),
        NodeState::Cancelled
    );
    assert_eq!(snapshot.executing.as_slice(), std::slice::from_ref(&node));
    assert!(matches!(
        handle.command(Command::Refresh(node.clone())).await,
        Err(wes_engine::driver::DriverError::Runtime(
            wes_engine::runtime::RuntimeError::Busy(_)
        ))
    ));
    release.send(()).unwrap();
    handle.wait_idle().await.unwrap();
    let notice = notices.recv().await.unwrap();
    assert_eq!(notice.run.node(), &node);
    assert_eq!(notice.error.code(), "RUN005");
    let Reply::Snapshot(snapshot) = handle.command(Command::Snapshot).await.unwrap() else {
        panic!("snapshot")
    };
    assert_eq!(
        snapshot.graph.node(&node).unwrap().state(),
        NodeState::Cancelled
    );
    assert_eq!(snapshot.errors[&node].code(), "RUN003");
    assert!(!snapshot.values.contains_key(&node));
    assert!(!snapshot.actual_typings.contains_key(&node));
    handle.command(Command::Shutdown).await.unwrap();
    driver_task.join().await.unwrap();
    recording.finish().await;
}

#[tokio::test]
async fn deadline_can_revoke_a_call_while_its_calling_receipt_is_blocked() {
    let mut sink = sink();
    let (entered, receive) = oneshot::channel();
    let (release, gate) = mpsc::channel();
    sink.gate = Some(("calling", entered, gate));
    let recording = Recording::new(sink);
    let work = recording.admit(ticket(&recording)).await;
    let mut runtime = Runtime::new();
    let node = runtime
        .add(work.payload.clone(), [], work.payload.traits())
        .unwrap();
    let (handle, driver_task) = driver::spawn(
        runtime,
        Arc::new(CallExecutor::recorded(recording.journal.clone())),
        NonZeroUsize::new(1).unwrap(),
    )
    .unwrap();
    let mut events = handle.subscribe().unwrap();
    handle.command(Command::Start).await.unwrap();
    receive.await.unwrap();
    handle
        .command(Command::Timeout {
            node: Some(node.clone()),
            budget: Duration::from_millis(1),
        })
        .await
        .unwrap();
    let cancelled = tokio::time::timeout(Duration::from_secs(1), async {
        loop {
            let event = events.recv().await.unwrap();
            if event.node == node && event.state == NodeState::Cancelled {
                break event;
            }
        }
    })
    .await
    .unwrap();
    assert_eq!(cancelled.error.unwrap().code(), "RUN002");
    assert!(!recording.timeline.lock().unwrap().contains(&"invoke"));
    release.send(()).unwrap();
    handle.wait_idle().await.unwrap();
    assert!(!recording.timeline.lock().unwrap().contains(&"invoke"));
    assert!(matches!(
        recording.records.lock().unwrap().last(),
        Some(Record::Recovery(RecoveryEntry::Called {
            produced: false,
            ..
        }))
    ));
    handle.command(Command::Shutdown).await.unwrap();
    driver_task.join().await.unwrap();
    recording.finish().await;
}

#[tokio::test]
async fn workspace_commits_acknowledged_origin_and_rejects_a_different_node_receipt() {
    use wes_engine::workspace::{Preparation, Workspace, WorkspaceError};
    let recording = Recording::new(sink());
    let mut workspace =
        Workspace::local(wes_engine::providers::LocalScope::new("fixture").unwrap());
    let mut capability = Capability::new(["echo"], Shape::Unknown, Safety::Safe);
    capability.parameters = vec![Parameter::new("value", Shape::Unknown, true)];
    workspace
        .register_provider(
            ProviderDescription::new("catalog", [capability], vec![]).unwrap(),
            Arc::new(Echo {
                timeline: recording.timeline.clone(),
                result: None,
                panic: false,
            }),
        )
        .unwrap();
    let parsed = parse(&SourceText::new("cell-1", "catalog echo value:hello"));
    let prepare = |workspace: &Workspace| {
        let Preparation::Change(prepared) =
            workspace.prepare(&parsed.script.statements[0]).unwrap()
        else {
            panic!("call")
        };
        prepared
    };
    let wrong = recording
        .journal
        .admit(command(NodeId::new("other").unwrap()))
        .await
        .unwrap();
    assert!(matches!(
        prepare(&workspace).with_admission(wrong),
        Err(WorkspaceError::AdmissionMismatch)
    ));
    let prepared = prepare(&workspace);
    let admission = recording
        .journal
        .admit(command(prepared.node().unwrap().clone()))
        .await
        .unwrap();
    workspace
        .commit(prepared.with_admission(admission).unwrap())
        .unwrap();
    assert!(!recording.timeline.lock().unwrap().contains(&"invoke"));
    let work = workspace
        .start(Duration::ZERO)
        .into_iter()
        .find_map(|effect| match effect {
            Effect::Spawn(ticket) => Some(ticket),
            _ => None,
        })
        .unwrap();
    workspace.enter(&work.run);
    let run = work.run.clone();
    let report = wes_engine::tasks::TaskExecutor::recorded(recording.journal.clone())
        .execute(work, CancellationToken::new())
        .await;
    assert!(report.notices.is_empty());
    workspace.complete(&run, report.outcome, Duration::from_secs(1));
    assert_eq!(
        workspace
            .runtime()
            .graph()
            .node(run.node())
            .unwrap()
            .state(),
        NodeState::Ready
    );
    recording.finish().await;
}

#[path = "calls/interactive.rs"]
mod interactive;
#[path = "calls/streams.rs"]
mod streams;
