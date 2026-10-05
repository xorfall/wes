#[path = "session/calculation.rs"]
mod calculation;
#[path = "session/checkpoint.rs"]
mod checkpoint;
#[path = "session/cooperative.rs"]
mod cooperative;
#[path = "session/history_pages.rs"]
mod history_pages;
#[path = "session/observation.rs"]
mod observation;
#[path = "session/ordered_pipelines.rs"]
mod ordered_pipelines;
#[path = "session/refresh_downstream.rs"]
mod refresh_downstream;
#[path = "session/repeat.rs"]
mod repeat;
#[path = "session/requests.rs"]
mod requests;
#[path = "session/sandbox.rs"]
mod sandbox;
#[path = "session/sequential_requests.rs"]
mod sequential_requests;
#[path = "session/typed_calculations.rs"]
mod typed_calculations;
#[path = "session/view_packages.rs"]
mod view_packages;
use std::{
    num::NonZeroUsize,
    sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
        mpsc,
    },
    time::Duration,
};
use tokio::sync::{Notify, oneshot};
use wes_core::{
    Data, Shape,
    capability::{Capability, Parameter, ProviderDescription, Safety},
};
use wes_engine::{
    calls::{CallJournal, RequiredPersistence},
    driver::CancellationToken,
    graph::NodeState,
    history::{
        AppendReceipt, JournalEntry, JournalSink, Persistence, Record, RecordError, RecoveryEntry,
    },
    log::LogStatus,
    providers::{Call, InvocationFuture, Invoker},
    recording::{RecorderLimits, spawn_recorder},
    session::{self, RecordingMode, SessionError, SessionHandle},
    source::SourceInput,
    type_sources::{TypeSourceError, TypeSourceReader},
    workspace::Workspace,
};
#[path = "session/imports.rs"]
mod imports;
#[path = "session/restore.rs"]
mod restoration;
#[path = "session/values.rs"]
mod value_publication;

struct CalledNoticeSink {
    records: Arc<Mutex<Vec<Record>>>,
    gate: Option<Gate>,
}
impl JournalSink for CalledNoticeSink {
    fn append(&mut self, record: &Record) -> Result<AppendReceipt, RecordError> {
        if matches!(record, Record::Recovery(RecoveryEntry::Called { .. })) {
            if let Some((entered, released)) = self.gate.take() {
                entered.send(()).unwrap();
                released.recv_timeout(Duration::from_secs(5)).unwrap();
            }
            return Err(RecordError::backend(
                "synthetic",
                false,
                std::io::Error::other("private called failure"),
            ));
        }
        self.records.lock().unwrap().push(record.clone());
        Ok(AppendReceipt {
            persistence: Persistence::FileSynced,
            end_offset: 1,
        })
    }
}
#[tokio::test]
async fn execution_recording_notices_enter_log_before_broadcast_and_survive_obsolete_cancelled_runs()
 {
    use wes_engine::history::NoticeContext;
    for cancelled in [false, true] {
        let (base, calls) = workspace(None, None);
        let records = Arc::new(Mutex::new(vec![]));
        let (entered, blocked) = oneshot::channel();
        let (release, released) = mpsc::channel();
        let (recorder, writer) = spawn_recorder(
            CalledNoticeSink {
                records: records.clone(),
                gate: Some((entered, released)),
            },
            RecorderLimits::default(),
        )
        .unwrap();
        let (handle, task) = session::spawn(
            base,
            RecordingMode::Required(CallJournal::new(
                recorder.clone(),
                RequiredPersistence::FileSynced,
            )),
            no_files(),
            NonZeroUsize::new(1).unwrap(),
        )
        .unwrap();
        let mut live = handle.subscribe_notices().unwrap();
        let reply = submit(&handle, "source", "catalog echo value:hello > answer").await;
        let node = reply.nodes[0].clone();
        blocked.await.unwrap();
        let run = handle.snapshot().await.unwrap().execution.runs[&node].clone();
        let original_error = if cancelled {
            submit(&handle, "cancel", ":cancel $answer").await;
            Some(handle.snapshot().await.unwrap().execution.errors[&node].clone())
        } else {
            None
        };
        release.send(()).unwrap();
        let notice = tokio::time::timeout(Duration::from_secs(5), live.recv())
            .await
            .unwrap()
            .unwrap();
        let captured = handle.log().await.unwrap();
        assert!(captured.entries.iter().any(|entry| matches!(entry.entry(), JournalEntry::Noticed(record) if record.error().id() == notice.error.id())));
        handle.wait_idle().await.unwrap();
        let log = handle.log().await.unwrap();
        let entries: Vec<_> = log
            .entries
            .iter()
            .filter(|entry| matches!(entry.entry(), JournalEntry::Noticed(_)))
            .collect();
        assert_eq!(entries.len(), 1);
        assert!(entries[0].durable());
        let JournalEntry::Noticed(record) = entries[0].entry() else {
            panic!()
        };
        assert_eq!(record.error(), &notice.error);
        assert_eq!(
            record.context(),
            &NoticeContext::Execution {
                node: node.clone(),
                run
            }
        );
        assert_ne!(record.id(), record.error().id().as_str());
        assert!(!record.error().message().contains("private"));
        let snapshot = handle.snapshot().await.unwrap();
        assert_eq!(
            snapshot.execution.graph.node(&node).unwrap().state(),
            if cancelled {
                NodeState::Cancelled
            } else {
                NodeState::Ready
            }
        );
        if let Some(error) = original_error {
            assert_eq!(snapshot.execution.errors[&node], error);
        }
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        assert!(records.lock().unwrap().iter().any(|entry| matches!(entry, Record::Journal(JournalEntry::Noticed(saved)) if saved == record)));
        stop(handle, task).await;
        recorder.shutdown().await.unwrap();
        writer.join().await.unwrap();
    }
}

struct Echo {
    calls: Arc<AtomicUsize>,
    gate: Option<Arc<Notify>>,
    entered: Mutex<Option<oneshot::Sender<()>>>,
}

#[tokio::test]
async fn session_log_captures_source_diagnostics_once_and_is_independent_of_subscribers() {
    let (workspace, _) = workspace(None, None);
    let (handle, task) = session::spawn(
        workspace,
        RecordingMode::Ephemeral,
        no_files(),
        NonZeroUsize::new(2).unwrap(),
    )
    .unwrap();
    let text = "catalog echo value:🦀 > answer\nmissing call";
    let report = submit(&handle, "unicode", text).await;
    handle.wait_idle().await.unwrap();
    let first = handle.log().await.unwrap();
    assert_eq!(first.pending, 0);
    assert_eq!(first.unconfirmed, 0);
    assert_eq!(first.capture_failures, 0);
    let diagnostics: Vec<_> = first
        .entries
        .iter()
        .filter_map(|entry| match entry.entry() {
            JournalEntry::Diagnosed(diagnostic) => Some(diagnostic),
            _ => None,
        })
        .collect();
    assert_eq!(diagnostics.len(), report.diagnostics.diagnostics.len());
    assert_eq!(diagnostics[0].source(), text);
    assert_eq!(diagnostics[0].cell(), "unicode");
    assert_eq!(
        diagnostics[0].diagnostic(),
        &report.diagnostics.diagnostics[0]
    );
    assert!(
        first
            .entries
            .iter()
            .all(|entry| matches!(entry.status(), LogStatus::Memory) && !entry.durable())
    );
    assert!(first.entries.iter().any(|entry| matches!(entry.entry(), JournalEntry::Observed(record) if record.state() == NodeState::Ready)));
    let mut wakeups = handle.subscribe_log().unwrap();
    assert!(Arc::ptr_eq(
        &report,
        &submit(&handle, "unicode", text).await
    ));
    assert_eq!(
        handle.log().await.unwrap().entries.len(),
        first.entries.len()
    );
    assert!(matches!(
        wakeups.try_recv(),
        Err(tokio::sync::broadcast::error::TryRecvError::Empty)
    ));
    // One-slot wakeups deliberately coalesce. A fresh snapshot remains the source of truth.
    submit(&handle, "next", "catalog echo value:new > next").await;
    handle.wait_idle().await.unwrap();
    assert!(matches!(
        wakeups.try_recv(),
        Err(tokio::sync::broadcast::error::TryRecvError::Lagged(_))
    ));
    assert!(handle.log().await.unwrap().entries.len() > first.entries.len());
    stop(handle, task).await;
}

struct LogSink {
    records: Arc<Mutex<Vec<Record>>>,
    gate: Option<Gate>,
    fail_observation: bool,
}
impl JournalSink for LogSink {
    fn append(&mut self, record: &Record) -> Result<AppendReceipt, RecordError> {
        if matches!(record, Record::Journal(JournalEntry::Observed(_))) {
            if let Some((entered, release)) = self.gate.take() {
                let _ = entered.send(());
                release.recv_timeout(Duration::from_secs(5)).unwrap();
            }
            if self.fail_observation {
                return Err(RecordError::backend(
                    "test",
                    false,
                    std::io::Error::other("private log backend detail"),
                ));
            }
        }
        let mut records = self.records.lock().unwrap();
        records.push(record.clone());
        Ok(AppendReceipt {
            persistence: Persistence::FileSynced,
            end_offset: records.len() as u64,
        })
    }
}

#[tokio::test]
async fn blocked_log_never_blocks_cancel_and_session_shutdown_joins_its_receipts() {
    let (workspace, calls) = workspace(None, None);
    let records = Arc::new(Mutex::new(vec![]));
    let (entered, disk_entered) = oneshot::channel();
    let (release, released) = mpsc::channel();
    let (recorder, recorder_task) = spawn_recorder(
        LogSink {
            records: records.clone(),
            gate: Some((entered, released)),
            fail_observation: false,
        },
        RecorderLimits::default(),
    )
    .unwrap();
    let (handle, task) = session::spawn(
        workspace,
        RecordingMode::Required(CallJournal::new(
            recorder.clone(),
            RequiredPersistence::FileSynced,
        )),
        no_files(),
        NonZeroUsize::new(1).unwrap(),
    )
    .unwrap();
    submit(&handle, "run", "catalog echo value:hello > answer").await;
    disk_entered.await.unwrap();
    assert_eq!(calls.load(Ordering::SeqCst), 0); // Calling must queue behind the blocked observation.
    let initial = handle.log().await.unwrap();
    assert!(initial.pending > 0);
    let first_id = initial.entries[0].id().to_owned();
    submit(&handle, "cancel", ":cancel $answer").await;
    let log = handle.log().await.unwrap();
    let cancelled = log
        .entries
        .iter()
        .find_map(|entry| match entry.entry() {
            JournalEntry::Observed(record) if record.state() == NodeState::Cancelled => {
                Some(record.clone())
            }
            _ => None,
        })
        .unwrap();
    assert!(cancelled.error().is_some());
    handle.shutdown().await.unwrap();
    let joined = tokio::spawn(async move { task.join().await.unwrap() });
    assert!(
        tokio::time::timeout(Duration::from_millis(20), handle.wait_idle())
            .await
            .is_err()
    );
    assert!(!joined.is_finished());
    release.send(()).unwrap();
    tokio::time::timeout(Duration::from_secs(5), joined)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(calls.load(Ordering::SeqCst), 0);
    let records = records.lock().unwrap().clone();
    assert!(records.iter().any(|entry| matches!(entry, Record::Journal(JournalEntry::Observed(record)) if record.id() == first_id)));
    assert!(records.iter().any(|entry| matches!(entry, Record::Journal(JournalEntry::Observed(record)) if record == &cancelled)));
    recorder.shutdown().await.unwrap();
    recorder_task.join().await.unwrap();
}

#[tokio::test]
async fn log_failure_preserves_success_but_blocks_new_recorded_admission() {
    let (workspace, calls) = workspace(None, None);
    let (recorder, recorder_task) = spawn_recorder(
        LogSink {
            records: Arc::default(),
            gate: None,
            fail_observation: true,
        },
        RecorderLimits::default(),
    )
    .unwrap();
    let (handle, task) = session::spawn(
        workspace,
        RecordingMode::Required(CallJournal::new(
            recorder.clone(),
            RequiredPersistence::FileSynced,
        )),
        no_files(),
        NonZeroUsize::new(1).unwrap(),
    )
    .unwrap();
    submit(&handle, "run", "catalog echo value:hello > answer").await;
    handle.wait_idle().await.unwrap();
    let snapshot = handle.snapshot().await.unwrap();
    assert!(snapshot.recording_blocked);
    assert_eq!(
        snapshot.execution.values[&snapshot.names["answer"].node].data(),
        &Data::Text("hello".into())
    );
    let log = handle.log().await.unwrap();
    assert_eq!(log.pending, 0);
    assert!(log.unconfirmed > 0);
    assert!(!format!("{log:?}").contains("private log backend detail"));
    assert!(log.entries.iter().any(|entry| matches!(entry.entry(), JournalEntry::Observed(record) if record.state() == NodeState::Ready) && matches!(entry.status(), LogStatus::Unconfirmed { may_have_appended: false, .. })));
    assert!(matches!(
        handle
            .submit(input("blocked", "catalog echo value:again"))
            .await,
        Err(SessionError::Recording)
    ));
    assert!(matches!(
        handle
            .submit(input(
                "blocked-refresh",
                ":refresh $answer scope:downstream"
            ))
            .await,
        Err(SessionError::Recording)
    ));
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    // Controls and diagnostics remain available; no secondary failure becomes the node error.
    submit(&handle, "cancel", ":cancel $answer").await;
    stop(handle, task).await;
    recorder.shutdown().await.unwrap();
    recorder_task.join().await.unwrap();
}

#[tokio::test]
async fn recorded_log_acknowledges_diagnostics_and_final_states_in_capture_order() {
    let (workspace, _) = workspace(None, None);
    let records = Arc::new(Mutex::new(vec![]));
    let (recorder, recorder_task) = spawn_recorder(
        LogSink {
            records: records.clone(),
            gate: None,
            fail_observation: false,
        },
        RecorderLimits::default(),
    )
    .unwrap();
    let (handle, task) = session::spawn(
        workspace,
        RecordingMode::Required(CallJournal::new(
            recorder.clone(),
            RequiredPersistence::FileSynced,
        )),
        no_files(),
        NonZeroUsize::new(2).unwrap(),
    )
    .unwrap();
    submit(
        &handle,
        "run",
        "catalog echo value:hello > answer\nmissing call",
    )
    .await;
    handle.wait_idle().await.unwrap();
    let log = handle.log().await.unwrap();
    assert_eq!(log.pending, 0);
    assert_eq!(log.unconfirmed, 0);
    assert!(log.entries.iter().all(|entry| entry.durable()));
    let disk: Vec<_> = records
        .lock()
        .unwrap()
        .iter()
        .filter_map(|entry| match entry {
            Record::Journal(JournalEntry::Observed(record)) => Some(record.id().to_owned()),
            Record::Journal(JournalEntry::Diagnosed(record)) => Some(record.id().to_owned()),
            _ => None,
        })
        .collect();
    assert_eq!(
        log.entries
            .iter()
            .map(|entry| entry.id())
            .collect::<Vec<_>>(),
        disk
    );
    stop(handle, task).await;
    recorder.shutdown().await.unwrap();
    recorder_task.join().await.unwrap();
}
impl Invoker for Echo {
    fn invoke(&self, call: Call, _: CancellationToken) -> InvocationFuture {
        self.calls.fetch_add(1, Ordering::SeqCst);
        let gate = if call.arguments["value"].data() == &Data::Text("blocked".into()) {
            self.gate.clone()
        } else {
            None
        };
        if gate.is_some()
            && let Some(entered) = self.entered.lock().unwrap().take()
        {
            let _ = entered.send(());
        }
        Box::pin(async move {
            if let Some(gate) = gate {
                gate.notified().await;
            }
            Ok(call.arguments["value"].clone())
        })
    }
}
fn workspace(
    gate: Option<Arc<Notify>>,
    entered: Option<oneshot::Sender<()>>,
) -> (Workspace, Arc<AtomicUsize>) {
    let calls = Arc::new(AtomicUsize::new(0));
    let mut workspace =
        Workspace::local(wes_engine::providers::LocalScope::new("fixture").unwrap());
    let mut echo = Capability::new(["echo"], Shape::Unknown, Safety::Safe);
    echo.parameters = vec![Parameter::new("value", Shape::Unknown, true)];
    workspace
        .register_provider(
            ProviderDescription::new("catalog", [echo], vec![]).unwrap(),
            Arc::new(Echo {
                calls: calls.clone(),
                gate,
                entered: Mutex::new(entered),
            }),
        )
        .unwrap();
    (workspace, calls)
}
struct Reader<F>(F);
impl<F: Fn(&str, usize) -> Result<String, TypeSourceError> + Send + Sync + 'static> TypeSourceReader
    for Reader<F>
{
    fn read(&self, path: &str, max: usize) -> Result<String, TypeSourceError> {
        (self.0)(path, max)
    }
}
fn reader(
    f: impl Fn(&str, usize) -> Result<String, TypeSourceError> + Send + Sync + 'static,
) -> Arc<dyn TypeSourceReader> {
    Arc::new(Reader(f))
}
fn no_files() -> Arc<dyn TypeSourceReader> {
    reader(|_, _| panic!("unexpected file read"))
}
fn input(cell: &str, text: &str) -> SourceInput {
    SourceInput::new(cell.into(), text.into()).unwrap()
}
async fn submit(handle: &SessionHandle, cell: &str, text: &str) -> Arc<session::SubmissionResult> {
    tokio::time::timeout(Duration::from_secs(5), handle.submit(input(cell, text)))
        .await
        .unwrap()
        .unwrap()
}
async fn stop(handle: SessionHandle, task: session::SessionTask) {
    handle.shutdown().await.unwrap();
    task.join().await.unwrap();
}

#[tokio::test]
async fn source_submission_executes_and_exact_identity_retries_share_the_same_reply() {
    let (workspace, calls) = workspace(None, None);
    let (handle, task) = session::spawn(
        workspace,
        RecordingMode::Ephemeral,
        no_files(),
        NonZeroUsize::new(2).unwrap(),
    )
    .unwrap();
    let text = ":def named as catalog echo value:?value\nnamed value:hello > first\nmissing call\ncatalog echo value:$first > second";
    let first = submit(&handle, "cell", text).await;
    assert_eq!(first.nodes.len(), 2);
    assert_eq!(first.accepted.len(), 3);
    assert_eq!(first.diagnostics.diagnostics.len(), 2); // definition success + missing provider
    let mut duplicates = vec![];
    for _ in 0..8 {
        let handle = handle.clone();
        duplicates.push(tokio::spawn(
            async move { submit(&handle, "cell", text).await },
        ));
    }
    for duplicate in duplicates {
        assert!(Arc::ptr_eq(&first, &duplicate.await.unwrap()));
    }
    handle.wait_idle().await.unwrap();
    assert_eq!(calls.load(Ordering::SeqCst), 2);
    let snapshot = handle.snapshot().await.unwrap();
    assert_eq!(
        snapshot.execution.values[&snapshot.names["second"].node].data(),
        &Data::Text("hello".into())
    );
    assert!(matches!(
        handle
            .submit(input("cell", "catalog echo value:different"))
            .await,
        Err(SessionError::Conflict)
    ));
    assert_eq!(calls.load(Ordering::SeqCst), 2);
    stop(handle, task).await;
}

#[tokio::test]
async fn rejected_and_empty_submissions_remain_idempotent_without_starting_a_provider() {
    let (workspace, calls) = workspace(None, None);
    let (handle, task) = session::spawn(
        workspace,
        RecordingMode::Ephemeral,
        no_files(),
        NonZeroUsize::new(1).unwrap(),
    )
    .unwrap();
    for (cell, text) in [
        ("syntax", "catalog echo value:\"unclosed"),
        ("mixed", "catalog echo value:x\n:cancel $missing"),
        ("empty", " \n"),
        ("unknown", "missing call"),
    ] {
        let first = submit(&handle, cell, text).await;
        assert!(first.nodes.is_empty());
        assert!(!first.recorded);
        assert!(Arc::ptr_eq(&first, &submit(&handle, cell, text).await));
    }
    assert_eq!(calls.load(Ordering::SeqCst), 0);
    stop(handle, task).await;
}

type Gate = (oneshot::Sender<()>, mpsc::Receiver<()>);

#[tokio::test]
async fn recorded_session_captures_local_queries_without_fabricating_external_call_recovery() {
    let records = Arc::new(Mutex::new(vec![]));
    let (recorder, recorder_task) = spawn_recorder(
        Sink {
            records: records.clone(),
            block_cell: None,
            gate: None,
            fail: false,
        },
        RecorderLimits::default(),
    )
    .unwrap();
    let (workspace, calls) = workspace(None, None);
    let (handle, task) = session::spawn(
        workspace,
        RecordingMode::Required(CallJournal::new(
            recorder.clone(),
            RequiredPersistence::FileSynced,
        )),
        no_files(),
        NonZeroUsize::new(2).unwrap(),
    )
    .unwrap();
    let report = submit(&handle, "queries", ":help > help\n:list capabilities provider:catalog > capabilities\n:inspect catalog echo > description\ncatalog echo value:$capabilities > copied\n:list names > names\n:list nodes > nodes").await;
    assert_eq!(report.accepted.len(), 6);
    assert!(report.diagnostics.diagnostics.is_empty());
    handle.wait_idle().await.unwrap();
    let snapshot = handle.snapshot().await.unwrap();
    let values = &snapshot.execution.values;
    assert_eq!(
        values[&snapshot.names["capabilities"].node]
            .clone()
            .with_provenance(
                wes_core::Provenance::default().with_policy(
                    &wes_core::flow::FlowPolicy::default().from_origin("local:fixture")
                )
            ),
        values[&snapshot.names["copied"].node]
    );
    assert!(matches!(
        values[&snapshot.names["description"].node].data(),
        Data::Record(_)
    ));
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    stop(handle, task).await;
    recorder.shutdown().await.unwrap();
    recorder_task.join().await.unwrap();
    let records = records.lock().unwrap();
    assert!(
        records
            .iter()
            .any(|record| matches!(record, Record::Journal(JournalEntry::Observed(_))))
    );
    let records = command_records(&records);
    assert_eq!(records.len(), 4);
    assert!(
        matches!(&records[0], Record::Journal(JournalEntry::Command(command)) if command.nodes == report.nodes)
    );
    assert!(
        matches!(&records[1], Record::Recovery(RecoveryEntry::Accepted { cell }) if cell == "queries")
    );
    assert!(
        matches!(&records[2], Record::Recovery(RecoveryEntry::Calling(call)) if call.node == snapshot.names["copied"].node)
    );
    assert!(matches!(
        &records[3],
        Record::Recovery(RecoveryEntry::Called { .. })
    ));
    for name in ["names", "nodes"] {
        assert!(matches!(
            values[&snapshot.names[name].node].data(),
            Data::List(_)
        ));
    }
}

struct Sink {
    records: Arc<Mutex<Vec<Record>>>,
    block_cell: Option<String>,
    gate: Option<Gate>,
    fail: bool,
}
// Observations may interleave with recovery writes, but the write-ahead ordering must not change.
fn command_records(records: &[Record]) -> Vec<&Record> {
    records
        .iter()
        .filter(|record| {
            !matches!(
                record,
                Record::Journal(
                    JournalEntry::Observed(_)
                        | JournalEntry::Diagnosed(_)
                        | JournalEntry::Submitted(_)
                )
            )
        })
        .collect()
}
impl JournalSink for Sink {
    fn append(&mut self, record: &Record) -> Result<AppendReceipt, RecordError> {
        if matches!(record, Record::Journal(JournalEntry::Command(command)) if self.block_cell.as_deref() == Some(&command.cell))
            && let Some((entered, release)) = self.gate.take()
        {
            entered.send(()).unwrap();
            release.recv_timeout(Duration::from_secs(5)).unwrap();
        }
        self.records.lock().unwrap().push(record.clone());
        if self.fail && matches!(record, Record::Recovery(RecoveryEntry::Accepted { .. })) {
            return Err(RecordError::backend(
                "test",
                true,
                std::io::Error::other("private disk detail"),
            ));
        }
        Ok(AppendReceipt {
            persistence: Persistence::FileSynced,
            end_offset: self.records.lock().unwrap().len() as u64,
        })
    }
}

#[tokio::test]
async fn required_admission_does_not_install_early_or_block_cancellation_and_waits() {
    let gate = Arc::new(Notify::new());
    let (entered, entered_rx) = oneshot::channel();
    let (workspace, calls) = workspace(Some(gate.clone()), Some(entered));
    let (disk_entered, disk_entered_rx) = oneshot::channel();
    let (release_disk, released_disk) = mpsc::channel();
    let records = Arc::new(Mutex::new(vec![]));
    let (recorder, recorder_task) = spawn_recorder(
        Sink {
            records: records.clone(),
            block_cell: Some("second".into()),
            gate: Some((disk_entered, released_disk)),
            fail: false,
        },
        RecorderLimits::default(),
    )
    .unwrap();
    let journal = CallJournal::new(recorder.clone(), RequiredPersistence::FileSynced);
    let (handle, task) = session::spawn(
        workspace,
        RecordingMode::Required(journal),
        no_files(),
        NonZeroUsize::new(2).unwrap(),
    )
    .unwrap();
    let old = submit(&handle, "first", "catalog echo value:blocked > old")
        .await
        .nodes[0]
        .clone();
    tokio::time::timeout(Duration::from_secs(5), entered_rx)
        .await
        .unwrap()
        .unwrap();
    let h = handle.clone();
    let second =
        tokio::spawn(async move { submit(&h, "second", "catalog echo value:new > new").await });
    tokio::time::timeout(Duration::from_secs(5), disk_entered_rx)
        .await
        .unwrap()
        .unwrap();
    let cancelled = submit(&handle, "cancel", ":cancel $old").await;
    assert_eq!(cancelled.accepted.len(), 1);
    let observed = submit(&handle, "wait-cancel", ":wait $old::cancel").await;
    assert!(observed.diagnostics.diagnostics.is_empty());
    let snapshot = handle.snapshot().await.unwrap();
    assert!(snapshot.admission_pending);
    assert!(!snapshot.names.contains_key("new"));
    assert_eq!(
        snapshot.execution.graph.node(&old).unwrap().state(),
        NodeState::Cancelled
    );
    assert!(snapshot.execution.executing.contains(&old));
    assert!(!second.is_finished());
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    release_disk.send(()).unwrap();
    second.await.unwrap();
    gate.notify_one();
    handle.wait_idle().await.unwrap();
    assert_eq!(calls.load(Ordering::SeqCst), 2);
    assert!(
        handle
            .snapshot()
            .await
            .unwrap()
            .execution
            .values
            .contains_key(&handle.snapshot().await.unwrap().names["new"].node)
    );
    {
        let records = records.lock().unwrap();
        let records = command_records(&records);
        assert!(
            matches!(&records[0], Record::Journal(JournalEntry::Command(command)) if command.cell == "first")
        );
        assert!(
            matches!(&records[1], Record::Recovery(RecoveryEntry::Accepted {cell}) if cell == "first")
        );
        assert!(
            matches!(&records[2], Record::Recovery(RecoveryEntry::Calling(call)) if call.cell == "first")
        );
    }
    stop(handle, task).await;
    recorder.shutdown().await.unwrap();
    recorder_task.join().await.unwrap();
}

#[tokio::test]
async fn uncertain_admission_is_cached_blocks_new_recorded_work_and_keeps_controls_available() {
    let (workspace, calls) = workspace(None, None);
    let records = Arc::new(Mutex::new(vec![]));
    let (recorder, recorder_task) = spawn_recorder(
        Sink {
            records: records.clone(),
            block_cell: None,
            gate: None,
            fail: true,
        },
        RecorderLimits::default(),
    )
    .unwrap();
    let (handle, task) = session::spawn(
        workspace,
        RecordingMode::Required(CallJournal::new(
            recorder.clone(),
            RequiredPersistence::FileSynced,
        )),
        no_files(),
        NonZeroUsize::new(1).unwrap(),
    )
    .unwrap();
    for cell in ["failed", "failed", "different"] {
        assert!(matches!(
            handle
                .submit(input(cell, "catalog echo value:hello > result"))
                .await,
            Err(SessionError::Recording)
        ));
    }
    assert_eq!(command_records(&records.lock().unwrap()).len(), 2);
    assert_eq!(calls.load(Ordering::SeqCst), 0);
    let snapshot = handle.snapshot().await.unwrap();
    assert!(snapshot.recording_blocked);
    assert!(snapshot.execution.graph.is_empty());
    let cancellation = submit(&handle, "control", ":cancel $missing").await;
    assert!(!cancellation.diagnostics.diagnostics.is_empty());
    stop(handle, task).await;
    recorder.shutdown().await.unwrap();
    recorder_task.join().await.unwrap();
}

#[tokio::test]
async fn waits_observe_selected_outputs_and_do_not_repeat_successful_work() {
    let (workspace, calls) = workspace(None, None);
    let (handle, task) = session::spawn(
        workspace,
        RecordingMode::Ephemeral,
        no_files(),
        NonZeroUsize::new(1).unwrap(),
    )
    .unwrap();
    submit(&handle, "source", "catalog echo value:ok > result").await;
    let available = submit(&handle, "wait", ":wait $result").await;
    assert!(available.diagnostics.diagnostics.is_empty());
    let closed = submit(&handle, "wait-error", ":wait $result::error").await;
    assert_eq!(closed.diagnostics.diagnostics[0].code, "MET006");
    assert!(Arc::ptr_eq(
        &closed,
        &submit(&handle, "wait-error", ":wait $result::error").await
    ));
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    stop(handle, task).await;
}

#[tokio::test]
async fn shutdown_joins_preparation_and_does_not_install_its_late_type_package() {
    let (entered, entered_rx) = oneshot::channel();
    let entered = Mutex::new(Some(entered));
    let (release, released) = mpsc::channel();
    let released = Mutex::new(released);
    let type_reader = reader(move |_, _| {
        entered.lock().unwrap().take().unwrap().send(()).unwrap();
        released
            .lock()
            .unwrap()
            .recv_timeout(Duration::from_secs(5))
            .unwrap();
        Ok("types: {Late: {base: Text}}".into())
    });
    let (workspace, _) = workspace(None, None);
    let (handle, task) = session::spawn(
        workspace,
        RecordingMode::Ephemeral,
        type_reader,
        NonZeroUsize::new(1).unwrap(),
    )
    .unwrap();
    let h = handle.clone();
    let pending = tokio::spawn(async move {
        h.submit(input("source", ":package load path:blocked.yaml"))
            .await
    });
    tokio::time::timeout(Duration::from_secs(5), entered_rx)
        .await
        .unwrap()
        .unwrap();
    handle.shutdown().await.unwrap();
    let joined = tokio::spawn(task.join());
    tokio::task::yield_now().await;
    assert!(!joined.is_finished());
    assert!(!pending.is_finished());
    assert!(handle.snapshot().await.unwrap().execution.closed);
    release.send(()).unwrap();
    assert!(matches!(pending.await.unwrap(), Err(SessionError::Stopped)));
    joined.await.unwrap().unwrap();
}

#[tokio::test]
async fn client_disconnect_does_not_retract_an_enqueued_source_or_duplicate_the_type_read() {
    let (entered, entered_rx) = oneshot::channel();
    let entered = Mutex::new(Some(entered));
    let (release, released) = mpsc::channel();
    let released = Mutex::new(released);
    let reads = Arc::new(AtomicUsize::new(0));
    let counted = reads.clone();
    let type_reader = reader(move |_, _| {
        counted.fetch_add(1, Ordering::SeqCst);
        entered.lock().unwrap().take().unwrap().send(()).unwrap();
        released
            .lock()
            .unwrap()
            .recv_timeout(Duration::from_secs(5))
            .unwrap();
        Ok("types: {Category: {base: Text, enum: [books]}}".into())
    });
    let (workspace, _) = workspace(None, None);
    let (handle, task) = session::spawn(
        workspace,
        RecordingMode::Ephemeral,
        type_reader,
        NonZeroUsize::new(1).unwrap(),
    )
    .unwrap();
    let text = ":package load path:blocked.yaml\n:type check \"books\" as:Category > checked";
    let h = handle.clone();
    let pending = tokio::spawn(async move { h.submit(input("cell", text)).await });
    tokio::time::timeout(Duration::from_secs(5), entered_rx)
        .await
        .unwrap()
        .unwrap();
    pending.abort();
    assert!(pending.await.unwrap_err().is_cancelled());
    release.send(()).unwrap();
    let report = submit(&handle, "cell", text).await;
    assert_eq!(report.nodes.len(), 1);
    assert_eq!(report.accepted.len(), 2);
    handle.wait_idle().await.unwrap();
    assert_eq!(reads.load(Ordering::SeqCst), 1);
    assert_eq!(
        handle.snapshot().await.unwrap().execution.values[&report.nodes[0]].data(),
        &Data::Text("books".into())
    );
    stop(handle, task).await;
}

#[tokio::test]
async fn dropping_all_handles_closes_execution_and_waits_for_physical_provider_exit() {
    let gate = Arc::new(Notify::new());
    let (entered, entered_rx) = oneshot::channel();
    let (workspace, calls) = workspace(Some(gate.clone()), Some(entered));
    let (handle, task) = session::spawn(
        workspace,
        RecordingMode::Ephemeral,
        no_files(),
        NonZeroUsize::new(1).unwrap(),
    )
    .unwrap();
    submit(&handle, "source", "catalog echo value:blocked > result").await;
    tokio::time::timeout(Duration::from_secs(5), entered_rx)
        .await
        .unwrap()
        .unwrap();
    drop(handle);
    let joined = tokio::spawn(task.join());
    tokio::task::yield_now().await;
    assert!(!joined.is_finished());
    gate.notify_one();
    tokio::time::timeout(Duration::from_secs(5), joined)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert_eq!(calls.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn execution_deadline_remains_live_while_a_new_source_is_waiting_for_a_type_file() {
    use wes_engine::workspace::Preparation;
    use wes_language::{SourceText, parse};
    let gate = Arc::new(Notify::new());
    let (entered, entered_rx) = oneshot::channel();
    let (mut workspace, _) = workspace(Some(gate.clone()), Some(entered));
    let declaration = parse(&SourceText::new(
        "fixture",
        "catalog echo value:blocked > old",
    ));
    let Preparation::Change(change) = workspace
        .prepare(&declaration.script.statements[0])
        .unwrap()
    else {
        panic!("call")
    };
    let old = workspace.commit(change).unwrap().node.unwrap();
    let timeout = parse(&SourceText::new("fixture", ":timeout $old after:PT0.1S"));
    let Preparation::Meta(meta) = workspace.prepare(&timeout.script.statements[0]).unwrap() else {
        panic!("timeout")
    };
    workspace
        .apply_control(workspace.prepare_control(meta).unwrap(), Duration::ZERO)
        .unwrap();
    let (file_entered, file_entered_rx) = oneshot::channel();
    let file_entered = Mutex::new(Some(file_entered));
    let (release, released) = mpsc::channel();
    let released = Mutex::new(released);
    let type_reader = reader(move |_, _| {
        file_entered
            .lock()
            .unwrap()
            .take()
            .unwrap()
            .send(())
            .unwrap();
        released
            .lock()
            .unwrap()
            .recv_timeout(Duration::from_secs(5))
            .unwrap();
        Ok("types: {}".into())
    });
    let (handle, task) = session::spawn(
        workspace,
        RecordingMode::Ephemeral,
        type_reader,
        NonZeroUsize::new(1).unwrap(),
    )
    .unwrap();
    submit(&handle, "start", "").await;
    tokio::time::timeout(Duration::from_secs(5), entered_rx)
        .await
        .unwrap()
        .unwrap();
    let h = handle.clone();
    let pending =
        tokio::spawn(async move { submit(&h, "file", ":package load path:blocked.yaml").await });
    tokio::time::timeout(Duration::from_secs(5), file_entered_rx)
        .await
        .unwrap()
        .unwrap();
    let waited = submit(&handle, "wait", ":wait $old::cancel").await;
    assert!(waited.diagnostics.diagnostics.is_empty());
    let snapshot = handle.snapshot().await.unwrap();
    assert!(snapshot.admission_pending);
    assert!(snapshot.execution.executing.contains(&old));
    assert_eq!(snapshot.execution.errors[&old].code(), "RUN002");
    assert!(!pending.is_finished());
    release.send(()).unwrap();
    pending.await.unwrap();
    gate.notify_one();
    handle.wait_idle().await.unwrap();
    stop(handle, task).await;
}

#[tokio::test]
async fn same_source_reactive_changes_capture_final_arguments_and_keep_command_admission() {
    let (workspace, calls) = workspace(None, None);
    let records = Arc::new(Mutex::new(vec![]));
    let (recorder, recorder_task) = spawn_recorder(
        Sink {
            records: records.clone(),
            block_cell: None,
            gate: None,
            fail: false,
        },
        RecorderLimits::default(),
    )
    .unwrap();
    let (handle, task) = session::spawn(
        workspace,
        RecordingMode::Required(CallJournal::new(
            recorder.clone(),
            RequiredPersistence::FileSynced,
        )),
        no_files(),
        NonZeroUsize::new(2).unwrap(),
    )
    .unwrap();
    let report = submit(&handle, "changed", ":workspace policy mode:reactive\ncatalog echo value:first > result\n:change $result value:second\n:change $result value:final").await;
    assert_eq!(report.accepted.len(), 4);
    assert_eq!(report.nodes.len(), 1);
    assert!(report.diagnostics.diagnostics.is_empty());
    handle.wait_idle().await.unwrap();
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    assert_eq!(
        handle.snapshot().await.unwrap().execution.values[&report.nodes[0]].data(),
        &Data::Text("final".into())
    );
    {
        let records = records.lock().unwrap();
        assert!(
            matches!(&records[0], Record::Journal(JournalEntry::Command(command)) if command.cell == "changed" && command.nodes == report.nodes)
        );
        assert_eq!(records.iter().filter(|r| matches!(r, Record::Recovery(RecoveryEntry::Calling(call)) if call.cell == "changed")).count(), 1);
    }
    stop(handle, task).await;
    recorder.shutdown().await.unwrap();
    recorder_task.join().await.unwrap();
}

#[tokio::test]
async fn changed_existing_manual_call_keeps_original_receipt_and_only_refresh_runs_it() {
    let (workspace, calls) = workspace(None, None);
    let records = Arc::new(Mutex::new(vec![]));
    let (recorder, recorder_task) = spawn_recorder(
        Sink {
            records: records.clone(),
            block_cell: None,
            gate: None,
            fail: false,
        },
        RecorderLimits::default(),
    )
    .unwrap();
    let (handle, task) = session::spawn(
        workspace,
        RecordingMode::Required(CallJournal::new(
            recorder.clone(),
            RequiredPersistence::FileSynced,
        )),
        no_files(),
        NonZeroUsize::new(1).unwrap(),
    )
    .unwrap();
    let original = submit(&handle, "original", "catalog echo value:first > result").await;
    handle.wait_idle().await.unwrap();
    let change = submit(&handle, "change", ":change $result value:second").await;
    assert!(change.nodes.is_empty());
    assert!(change.recorded);
    assert_eq!(change.accepted.len(), 1);
    handle.wait_idle().await.unwrap();
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    assert_eq!(
        handle
            .snapshot()
            .await
            .unwrap()
            .execution
            .graph
            .node(&original.nodes[0])
            .unwrap()
            .state(),
        NodeState::Stale
    );
    submit(&handle, "refresh", ":refresh $result").await;
    handle.wait_idle().await.unwrap();
    assert_eq!(calls.load(Ordering::SeqCst), 2);
    assert_eq!(
        handle.snapshot().await.unwrap().execution.values[&original.nodes[0]].data(),
        &Data::Text("second".into())
    );
    {
        let records = records.lock().unwrap();
        assert_eq!(records.iter().filter(|r| matches!(r, Record::Recovery(RecoveryEntry::Calling(call)) if call.cell == "original")).count(), 2);
        assert!(records.iter().any(|r| matches!(r, Record::Journal(JournalEntry::Command(command)) if command.cell == "change" && command.nodes.is_empty())));
    }
    stop(handle, task).await;
    recorder.shutdown().await.unwrap();
    recorder_task.join().await.unwrap();
}

#[tokio::test]
async fn a_node_changed_and_dropped_in_one_batch_never_enters_its_obsolete_queued_work() {
    let (workspace, calls) = workspace(None, None);
    let (handle, task) = session::spawn(
        workspace,
        RecordingMode::Ephemeral,
        no_files(),
        NonZeroUsize::new(2).unwrap(),
    )
    .unwrap();
    let report = submit(&handle, "discard", ":workspace policy mode:reactive\ncatalog echo value:first > transient\n:change $transient value:second\n:node remove $transient scope:downstream").await;
    assert_eq!(report.accepted.len(), 4);
    assert_eq!(report.nodes, report.removed);
    assert_eq!(report.unbound, ["transient"]);
    handle.wait_idle().await.unwrap();
    assert!(handle.snapshot().await.unwrap().execution.graph.is_empty());
    assert_eq!(calls.load(Ordering::SeqCst), 0);
    stop(handle, task).await;
}

#[tokio::test]
async fn drop_waits_for_recording_and_reports_removed_nodes_and_names_from_the_same_batch() {
    let (workspace, calls) = workspace(None, None);
    let records = Arc::new(Mutex::new(vec![]));
    let (entered, entered_rx) = oneshot::channel();
    let (release, released) = mpsc::channel();
    let (recorder, recorder_task) = spawn_recorder(
        Sink {
            records,
            block_cell: Some("drop".into()),
            gate: Some((entered, released)),
            fail: false,
        },
        RecorderLimits::default(),
    )
    .unwrap();
    let (handle, task) = session::spawn(
        workspace,
        RecordingMode::Required(CallJournal::new(
            recorder.clone(),
            RequiredPersistence::FileSynced,
        )),
        no_files(),
        NonZeroUsize::new(1).unwrap(),
    )
    .unwrap();
    let old = submit(
        &handle,
        "old",
        "catalog echo value:first > root\ncatalog echo value:$root > child\n$root > alias",
    )
    .await;
    handle.wait_idle().await.unwrap();
    let h = handle.clone();
    let pending = tokio::spawn(async move {
        submit(
            &h,
            "drop",
            ":name unbind \"alias\"\n:node remove $root scope:downstream\ncatalog echo value:new > fresh",
        )
        .await
    });
    tokio::time::timeout(Duration::from_secs(5), entered_rx)
        .await
        .unwrap()
        .unwrap();
    let snapshot = handle.snapshot().await.unwrap();
    assert_eq!(snapshot.execution.graph.len(), 2);
    assert!(snapshot.names.contains_key("alias"));
    assert!(!snapshot.names.contains_key("fresh"));
    assert_eq!(calls.load(Ordering::SeqCst), 2);
    release.send(()).unwrap();
    let report = pending.await.unwrap();
    assert_eq!(report.removed, old.nodes);
    assert_eq!(report.unbound, ["alias", "root", "child"]);
    assert_eq!(report.nodes[0].as_str(), "id1002");
    handle.wait_idle().await.unwrap();
    let snapshot = handle.snapshot().await.unwrap();
    assert_eq!(snapshot.execution.graph.len(), 1);
    assert_eq!(snapshot.names.len(), 1);
    assert_eq!(calls.load(Ordering::SeqCst), 3);
    stop(handle, task).await;
    recorder.shutdown().await.unwrap();
    recorder_task.join().await.unwrap();
}

#[tokio::test]
async fn timeout_is_a_recordable_source_control_and_expires_the_existing_run() {
    let gate = Arc::new(Notify::new());
    let (entered, entered_rx) = oneshot::channel();
    let (workspace, _) = workspace(Some(gate.clone()), Some(entered));
    let (handle, task) = session::spawn(
        workspace,
        RecordingMode::Ephemeral,
        no_files(),
        NonZeroUsize::new(1).unwrap(),
    )
    .unwrap();
    let old = submit(&handle, "old", "catalog echo value:blocked > old").await;
    tokio::time::timeout(Duration::from_secs(5), entered_rx)
        .await
        .unwrap()
        .unwrap();
    let timeout = submit(&handle, "budget", ":timeout $old after:PT0.05S").await;
    assert!(timeout.recorded);
    assert_eq!(timeout.accepted.len(), 1);
    assert!(timeout.nodes.is_empty());
    submit(&handle, "wait", ":wait $old::cancel").await;
    assert_eq!(
        handle.snapshot().await.unwrap().execution.errors[&old.nodes[0]].code(),
        "RUN002"
    );
    gate.notify_one();
    handle.wait_idle().await.unwrap();
    stop(handle, task).await;
}

#[tokio::test]
async fn captured_document_waits_for_required_admission_and_failed_receipt_never_installs() {
    let (base, _) = workspace(None, None);
    let records = Arc::new(Mutex::new(vec![]));
    let (recorder, writer) = spawn_recorder(
        Sink {
            records: records.clone(),
            block_cell: None,
            gate: None,
            fail: true,
        },
        RecorderLimits::default(),
    )
    .unwrap();
    let (handle, task) = session::spawn(
        base,
        RecordingMode::Required(CallJournal::new(
            recorder.clone(),
            RequiredPersistence::FileSynced,
        )),
        no_files(),
        NonZeroUsize::new(1).unwrap(),
    )
    .unwrap();
    let source = "types: {NeverInstalled: {base: Text}}";
    let result = handle
        .submit(
            input("editor-record-failure", ":package load source:\"\"")
                .with_document(Some(source.into()))
                .unwrap(),
        )
        .await;
    assert!(matches!(result, Err(SessionError::Recording)));
    assert!(
        !handle
            .observe()
            .await
            .unwrap()
            .types
            .iter()
            .any(|t| t == "NeverInstalled")
    );
    assert!(records.lock().unwrap().iter().any(|r| matches!(r, Record::Journal(JournalEntry::Command(c)) if c.document.as_deref() == Some(source))));
    handle.shutdown().await.unwrap();
    task.join().await.unwrap();
    recorder.shutdown().await.unwrap();
    writer.join().await.unwrap();
}

#[tokio::test]
async fn provider_info_is_a_local_query_with_empty_and_retained_information() {
    let (mut workspace, calls) = workspace(None, None);
    let information = Data::Record(indexmap::IndexMap::from([(
        "notes".into(),
        Data::List(vec![Data::Text("budget must be less than limit".into())]),
    )]));
    workspace
        .register_provider(
            ProviderDescription::new("documented", [], vec![])
                .unwrap()
                .with_information(information.clone()),
            Arc::new(Echo {
                calls: calls.clone(),
                gate: None,
                entered: Mutex::new(None),
            }),
        )
        .unwrap();
    let info_shape = Shape::Record(
        wes_core::RecordShape::new(
            "Documentation",
            [(
                "notes".into(),
                Shape::List(Box::new(Shape::Primitive(wes_core::Primitive::Text))),
            )],
        )
        .unwrap(),
    );
    workspace
        .register_provider(
            ProviderDescription::new("typedDocs", [], vec![])
                .unwrap()
                .with_typed_information(
                    wes_core::Value::new(
                        info_shape.clone(),
                        information.clone(),
                        wes_core::Provenance::default(),
                    )
                    .unwrap(),
                ),
            Arc::new(Echo {
                calls: calls.clone(),
                gate: None,
                entered: Mutex::new(None),
            }),
        )
        .unwrap();
    let (handle, task) = session::spawn(
        workspace,
        RecordingMode::Ephemeral,
        no_files(),
        NonZeroUsize::new(1).unwrap(),
    )
    .unwrap();
    let report = submit(
        &handle,
        "info",
        ":info documented > documentation\n:info catalog > emptyInfo\n:info absent > absentInfo\n:info typedDocs > typedInfo",
    )
    .await;
    assert_eq!(report.accepted.len(), 4, "{:?}", report.diagnostics);
    handle.wait_idle().await.unwrap();
    let snapshot = handle.snapshot().await.unwrap();
    let typed = &snapshot.execution.values[&snapshot.names["typedInfo"].node];
    let Shape::Record(shape) = typed.shape() else {
        panic!("typed provider info");
    };
    assert_eq!(shape.field("information"), Some(&info_shape));
    let Data::Record(value) =
        snapshot.execution.values[&snapshot.names["documentation"].node].data()
    else {
        panic!()
    };
    assert_eq!(value["information"], information);
    let Data::Record(empty) = snapshot.execution.values[&snapshot.names["emptyInfo"].node].data()
    else {
        panic!()
    };
    let Data::Record(empty) = &empty["information"] else {
        panic!()
    };
    assert_eq!(empty["notes"], Data::List(vec![]));
    assert!(
        snapshot
            .execution
            .errors
            .contains_key(&snapshot.names["absentInfo"].node)
    );
    assert_eq!(calls.load(Ordering::SeqCst), 0);
    stop(handle, task).await;
}
