use super::*;
use tokio::sync::mpsc as channel;
use wes_core::{Primitive, Provenance};
use wes_engine::{
    graph::NodeId,
    history::{HistoryCapture, HistoryCaptureLimits, HistoryCheckpoint, HistoryImage},
    recording::{Recorder, RecorderTask},
    runtime::Run,
    streams::{StreamFuture, StreamSink, StreamingInvoker},
};

struct Started {
    run: Run,
    sink: StreamSink,
    token: CancellationToken,
    finish: oneshot::Sender<()>,
}
struct Stream(channel::UnboundedSender<Started>);
impl StreamingInvoker for Stream {
    fn subscribe(&self, call: Call, sink: StreamSink, token: CancellationToken) -> StreamFuture {
        sink.push(item(0)).unwrap();
        sink.opened().unwrap();
        let (finish, ended) = oneshot::channel();
        self.0
            .send(Started {
                run: call.run,
                sink,
                token,
                finish,
            })
            .unwrap();
        Box::pin(async {
            ended.await.unwrap();
            Ok(())
        })
    }
}
fn item(n: i64) -> Value {
    Value::new(
        Shape::Primitive(Primitive::Int),
        Data::Int(n),
        Provenance::default(),
    )
    .unwrap()
}
struct NoFinite;
impl Invoker for NoFinite {
    fn invoke(&self, _: Call, _: CancellationToken) -> InvocationFuture {
        panic!("wrong stream port")
    }
}
fn base() -> (Workspace, channel::UnboundedReceiver<Started>) {
    let (mut base, _) = workspace(None, None);
    let (entered, receive) = channel::unbounded_channel();
    let mut capability = Capability::new(["watch"], Shape::Primitive(Primitive::Int), Safety::Safe);
    capability.streaming = true;
    capability.parameters = vec![Parameter::new("label", Shape::Unknown, false)];
    base.register_provider_ports(
        ProviderDescription::new("events", [capability], vec![]).unwrap(),
        Arc::new(NoFinite),
        Some(Arc::new(Stream(entered))),
    )
    .unwrap();
    (base, receive)
}
#[derive(Default)]
struct Journal {
    records: Arc<Mutex<Vec<Record>>>,
    capture_gate: Option<Gate>,
    fail_capture: bool,
    fail_snapshot: bool,
}
impl JournalSink for Journal {
    fn append(&mut self, record: &Record) -> Result<AppendReceipt, RecordError> {
        if self.fail_snapshot && matches!(record, Record::Journal(JournalEntry::Snapshot(_))) {
            return Err(RecordError::backend(
                "synthetic",
                false,
                std::io::Error::other("snapshot write failure"),
            ));
        }
        let mut records = self.records.lock().unwrap();
        records.push(record.clone());
        Ok(AppendReceipt {
            persistence: Persistence::FileSynced,
            end_offset: records.len() as u64,
        })
    }
    fn capture(&mut self, limits: HistoryCaptureLimits) -> Result<HistoryImage, RecordError> {
        if let Some((entered, release)) = self.capture_gate.take() {
            entered.send(()).unwrap();
            release.recv_timeout(Duration::from_secs(5)).unwrap();
        }
        if self.fail_capture {
            return Err(RecordError::CaptureUnsupported);
        }
        let records = self.records.lock().unwrap();
        let mut capture = HistoryCapture::new(limits);
        for record in &*records {
            capture.push(record.clone())?;
        }
        let receipt = AppendReceipt {
            persistence: Persistence::FileSynced,
            end_offset: records.len() as u64,
        };
        Ok(capture.finish(HistoryCheckpoint {
            journal: receipt,
            recovery: receipt,
        }))
    }
}
struct Fixture {
    handle: SessionHandle,
    task: session::SessionTask,
    store: StoreWorker,
    storage_task: wes_engine::storage::StoreWorkerTask,
    state: Arc<Mutex<State>>,
    recorder: Recorder,
    writer: RecorderTask,
    records: Arc<Mutex<Vec<Record>>>,
    entered: channel::UnboundedReceiver<Started>,
}
impl Fixture {
    fn new(backend: Store, journal: Journal) -> Self {
        Self::with_keep(backend, journal, AutoKeep::Never)
    }
    fn with_keep(backend: Store, journal: Journal, auto_keep: AutoKeep) -> Self {
        let state = backend.state.clone();
        let records = journal.records.clone();
        let (store, storage_task) = spawn_store(backend, StoreWorkerLimits::default()).unwrap();
        let (recorder, writer) = spawn_recorder(journal, RecorderLimits::default()).unwrap();
        let (base, entered) = base();
        let (handle, task) = session::spawn_with_storage(
            base,
            RecordingMode::Required(CallJournal::new(
                recorder.clone(),
                RequiredPersistence::FileSynced,
            )),
            no_files(),
            NonZeroUsize::new(1).unwrap(),
            SessionStorage {
                worker: store.clone(),
                auto_keep,
            },
        )
        .unwrap();
        Self {
            handle,
            task,
            store,
            storage_task,
            state,
            recorder,
            writer,
            records,
            entered,
        }
    }
    async fn open(&mut self) -> Started {
        submit(&self.handle, "stream", "events watch > live").await;
        let stream = tokio::time::timeout(Duration::from_secs(3), self.entered.recv())
            .await
            .unwrap()
            .unwrap();
        tokio::time::timeout(Duration::from_secs(3), self.handle.wait_idle())
            .await
            .unwrap()
            .unwrap();
        stream
    }
    async fn finish(self, stream: Started) {
        self.handle.shutdown().await.unwrap();
        stream.finish.send(()).unwrap();
        tokio::time::timeout(Duration::from_secs(5), self.task.join())
            .await
            .unwrap()
            .unwrap();
        shutdown_store(self.store, self.storage_task).await;
        self.recorder.shutdown().await.unwrap();
        self.writer.join().await.unwrap();
    }
}
#[tokio::test]
async fn event_stage_publication_is_transient_until_checkpoint_even_with_default_auto_keep() {
    let mut fixture = Fixture::with_keep(store(), Journal::default(), AutoKeep::default());
    let accepted = submit(
        &fixture.handle,
        "pipeline",
        include_str!("../../../../examples/live-stream-persistence/state.wes"),
    )
    .await;
    assert_eq!(accepted.nodes.len(), 3, "{:?}", accepted.diagnostics);
    let stream = fixture.entered.recv().await.unwrap();
    for n in 1..1000 {
        stream.sink.send(item(n)).await.unwrap();
    }
    tokio::time::timeout(Duration::from_secs(5), fixture.handle.wait_idle())
        .await
        .unwrap()
        .unwrap();
    let snapshot = fixture.handle.snapshot().await.unwrap();
    assert_eq!(
        snapshot.execution.values[&accepted.nodes[1]].data(),
        &Data::Int(1000)
    );
    assert!(fixture.state.lock().unwrap().kept.is_empty());
    {
        let records = fixture.records.lock().unwrap();
        assert!(!records.iter().any(|record| matches!(record,
            Record::Journal(JournalEntry::Observed(observed))
                if accepted.nodes[1..].contains(observed.node()) && observed.run().is_some())));
        assert!(
            records.len() < 40,
            "per-event required evidence: {}",
            records.len()
        );
        assert!(results(&records, &accepted.nodes[2]).is_empty());
    }
    assert!(!snapshot.recording_blocked);
    let log = fixture.handle.log().await.unwrap();
    assert_eq!(log.unconfirmed, 0);
    assert!(
        log.entries
            .iter()
            .any(|e| matches!(e.entry(), JournalEntry::Observed(o)
        if o.node() == &accepted.nodes[1] && o.state() == wes_engine::graph::NodeState::Ready)
                && matches!(e.status(), wes_engine::log::LogStatus::Memory))
    );
    let checkpoint = fixture.handle.checkpoint().await.unwrap();
    assert_eq!(
        results(&fixture.records.lock().unwrap(), &accepted.nodes[1]).len(),
        1
    );
    assert_eq!(
        results(&fixture.records.lock().unwrap(), &accepted.nodes[2]).len(),
        1
    );
    let (history, pause) = checkpoint.into_parts();
    let (base, mut entered) = base();
    let restored = session::restore(
        base,
        RecordingMode::Ephemeral,
        Some(SessionStorage {
            worker: fixture.store.clone(),
            auto_keep: AutoKeep::Never,
        }),
        history,
        CancellationToken::new(),
    )
    .await
    .unwrap();
    assert_eq!(restored.report().values, 3);
    assert_eq!(
        restored
            .workspace()
            .runtime()
            .value_of(&accepted.nodes[2])
            .unwrap()
            .data(),
        snapshot.execution.values[&accepted.nodes[2]].data()
    );
    let (handle, task) = restored
        .spawn(no_files(), NonZeroUsize::new(1).unwrap())
        .unwrap();
    handle.wait_idle().await.unwrap();
    assert!(entered.try_recv().is_err());
    stop(handle, task).await;
    drop(pause);
    fixture.finish(stream).await;
}
async fn ready(handle: &SessionHandle, node: &NodeId, last: i64) -> ValueHandle {
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let observation = handle.observe().await.unwrap();
            let matches = observation.state.execution.values.get(node).is_some_and(|value| matches!(value.data(), Data::List(items) if items.last() == Some(&Data::Int(last))));
            if matches && let Some(ValuePublication::Complete(published)) = observation.values.as_ref().and_then(|v| v.outputs.get(node)) && let Some(stored) = &published.stored {
                return stored.handle.clone();
            }
            tokio::task::yield_now().await;
        }
    }).await.unwrap()
}
fn results(records: &[Record], node: &NodeId) -> Vec<ValueHandle> {
    records
        .iter()
        .filter_map(|record| match record {
            Record::Journal(entry) => entry
                .retained_result()
                .filter(|r| &r.node == node)
                .map(|r| r.handle.clone()),
            _ => None,
        })
        .collect()
}

#[tokio::test]
async fn ordered_accumulation_retains_events_beyond_the_rolling_source_window() {
    let (base, mut entered) = base();
    let (handle, task) = session::spawn(
        base,
        RecordingMode::Ephemeral,
        no_files(),
        NonZeroUsize::new(1).unwrap(),
    )
    .unwrap();
    let accepted = submit(
        &handle,
        "open",
        "events watch > live | :accumulate limit:1000 > history",
    )
    .await;
    assert_eq!(accepted.nodes.len(), 2, "{:?}", accepted.diagnostics);
    let stream = entered.recv().await.unwrap();
    for n in 1..600 {
        stream.sink.push(item(n)).unwrap();
        if n % 50 == 0 || n == 599 {
            tokio::time::timeout(Duration::from_secs(5), async {
                loop {
                    let s = handle.snapshot().await.unwrap();
                    if let Some(Data::Record(value)) =
                        s.execution.values.get(&accepted.nodes[1]).map(Value::data)
                    {
                        if let Some(Data::List(items)) = value.get("items") {
                            if items.len() == n as usize + 1 {
                                break;
                            }
                        }
                    }
                    tokio::task::yield_now().await;
                }
            })
            .await
            .unwrap();
        }
    }
    stream.finish.send(()).unwrap();
    handle.wait_idle().await.unwrap();
    let s = handle.snapshot().await.unwrap();
    let Data::Record(value) = s.execution.values[&accepted.nodes[1]].data() else {
        panic!()
    };
    assert_eq!(
        value["items"],
        Data::List((0..600).map(Data::Int).collect())
    );
    assert!(entered.try_recv().is_err());
    stop(handle, task).await;
}

#[tokio::test]
async fn source_stream_is_idle_finite_commands_progress_and_windows_do_not_flood_history_or_archive()
 {
    let mut fixture = Fixture::new(store(), Journal::default());
    let stream = fixture.open().await;
    let node = stream.run.node().clone();
    assert_eq!(
        fixture.handle.snapshot().await.unwrap().execution.streaming,
        vec![node.clone()]
    );
    let initial = ready(&fixture.handle, &node, 0).await;
    let logs = fixture.records.lock().unwrap().len();
    stream.sink.push(item(1)).unwrap();
    stream.sink.reject_item().unwrap();
    let latest = ready(&fixture.handle, &node, 1).await;
    assert_ne!(initial, latest);
    fixture.handle.wait_idle().await.unwrap();
    assert_eq!(fixture.records.lock().unwrap().len(), logs);
    assert!(results(&fixture.records.lock().unwrap(), &node).is_empty());
    assert_eq!(fixture.state.lock().unwrap().keeps, 0);
    let observation = fixture.handle.observe().await.unwrap();
    assert!(
        observation.state.execution.values[&node]
            .provenance()
            .cautions()
            .contains("Stream rejected 1 invalid items.")
    );
    submit(
        &fixture.handle,
        "finite",
        "catalog echo value:ok > ordinary",
    )
    .await;
    tokio::time::timeout(Duration::from_secs(3), fixture.handle.wait_idle())
        .await
        .unwrap()
        .unwrap();
    assert!(!fixture.records.lock().unwrap().iter().any(|record| matches!(record, Record::Recovery(RecoveryEntry::Calling(call)) if call.node == node)));
    fixture.finish(stream).await;
}
#[tokio::test]
async fn checkpoint_retains_one_window_pauses_later_windows_and_resumes_without_reopening() {
    let mut fixture = Fixture::new(store(), Journal::default());
    let stream = fixture.open().await;
    let node = stream.run.node().clone();
    let before = ready(&fixture.handle, &node, 0).await;
    let checkpoint = tokio::time::timeout(Duration::from_secs(3), fixture.handle.checkpoint())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        results(&fixture.records.lock().unwrap(), &node),
        vec![before.clone()]
    );
    assert!(fixture.state.lock().unwrap().kept.contains(&before));
    stream.sink.push(item(1)).unwrap();
    tokio::time::sleep(Duration::from_millis(300)).await;
    let snapshot = fixture.handle.snapshot().await.unwrap();
    assert!(snapshot.checkpoint_pending);
    assert_eq!(
        snapshot.execution.values[&node].data(),
        &Data::List(vec![Data::Int(0)])
    );
    assert!(!stream.token.is_cancelled());
    drop(checkpoint);
    let after = ready(&fixture.handle, &node, 1).await;
    assert_ne!(before, after);
    assert!(fixture.state.lock().unwrap().kept.contains(&before));
    assert!(fixture.entered.try_recv().is_err());
    fixture.finish(stream).await;
}
#[tokio::test]
async fn clean_shutdown_joins_provider_then_keeps_last_visible_window_once() {
    let mut fixture = Fixture::new(store(), Journal::default());
    let stream = fixture.open().await;
    let node = stream.run.node().clone();
    let last = ready(&fixture.handle, &node, 0).await;
    fixture.handle.shutdown().await.unwrap();
    tokio::time::timeout(Duration::from_secs(3), stream.token.cancelled())
        .await
        .unwrap();
    let joining = tokio::spawn(fixture.task.join());
    tokio::task::yield_now().await;
    assert!(!joining.is_finished());
    assert_eq!(fixture.state.lock().unwrap().keeps, 0);
    stream.finish.send(()).unwrap();
    tokio::time::timeout(Duration::from_secs(3), joining)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert_eq!(
        results(&fixture.records.lock().unwrap(), &node),
        vec![last.clone()]
    );
    assert!(fixture.state.lock().unwrap().kept.contains(&last));
    assert_eq!(fixture.state.lock().unwrap().keeps, 1);
    shutdown_store(fixture.store, fixture.storage_task).await;
    fixture.recorder.shutdown().await.unwrap();
    fixture.writer.join().await.unwrap();
}
#[tokio::test]
async fn shutdown_releases_checkpoint_deferred_windows_before_waiting_for_physical_cleanup() {
    let mut fixture = Fixture::new(store(), Journal::default());
    let stream = fixture.open().await;
    let checkpoint = fixture.handle.checkpoint().await.unwrap();
    stream.sink.push(item(1)).unwrap();
    tokio::time::sleep(Duration::from_millis(300)).await;
    fixture.handle.shutdown().await.unwrap();
    tokio::time::timeout(Duration::from_secs(3), stream.token.cancelled())
        .await
        .unwrap();
    stream.finish.send(()).unwrap();
    tokio::time::timeout(Duration::from_secs(3), fixture.task.join())
        .await
        .unwrap()
        .unwrap();
    drop(checkpoint);
    shutdown_store(fixture.store, fixture.storage_task).await;
    fixture.recorder.shutdown().await.unwrap();
    fixture.writer.join().await.unwrap();
}
#[tokio::test]
async fn failed_capture_resumes_window_delivery_and_does_not_reopen_provider() {
    let mut fixture = Fixture::new(
        store(),
        Journal {
            fail_capture: true,
            ..Journal::default()
        },
    );
    let stream = fixture.open().await;
    assert!(matches!(
        fixture.handle.checkpoint().await,
        Err(SessionError::Recording)
    ));
    stream.sink.push(item(1)).unwrap();
    ready(&fixture.handle, stream.run.node(), 1).await;
    assert!(fixture.entered.try_recv().is_err());
    assert!(!stream.token.is_cancelled());
    fixture.finish(stream).await;
}
#[tokio::test]
async fn retention_failure_refuses_checkpoint_without_retry_and_shutdown_still_joins() {
    let mut backend = store();
    backend.fail_keep = true;
    let mut fixture = Fixture::new(backend, Journal::default());
    let stream = fixture.open().await;
    assert!(matches!(
        tokio::time::timeout(Duration::from_secs(3), fixture.handle.checkpoint())
            .await
            .unwrap(),
        Err(SessionError::Recording)
    ));
    assert_eq!(fixture.state.lock().unwrap().keeps, 1);
    fixture.handle.shutdown().await.unwrap();
    stream.finish.send(()).unwrap();
    tokio::time::timeout(Duration::from_secs(3), fixture.task.join())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(fixture.state.lock().unwrap().keeps, 1);
    assert!(
        fixture
            .records
            .lock()
            .unwrap()
            .iter()
            .any(|record| matches!(record, Record::Journal(JournalEntry::Noticed(_))))
    );
    shutdown_store(fixture.store, fixture.storage_task).await;
    fixture.recorder.shutdown().await.unwrap();
    fixture.writer.join().await.unwrap();
}

#[tokio::test]
async fn explicit_deadline_cancels_a_checkpoint_deferred_stream_without_waiting_for_pause_release()
{
    let mut fixture = Fixture::new(store(), Journal::default());
    let stream = fixture.open().await;
    submit(&fixture.handle, "deadline", ":timeout $live after:PT0.7S").await;
    let checkpoint = fixture.handle.checkpoint().await.unwrap();
    stream.sink.push(item(1)).unwrap();
    tokio::time::sleep(Duration::from_millis(300)).await;
    tokio::time::timeout(Duration::from_secs(3), stream.token.cancelled())
        .await
        .unwrap();
    assert_eq!(
        fixture
            .handle
            .snapshot()
            .await
            .unwrap()
            .execution
            .graph
            .node(stream.run.node())
            .unwrap()
            .state(),
        NodeState::Cancelled
    );
    stream.finish.send(()).unwrap();
    tokio::time::timeout(Duration::from_secs(3), async {
        loop {
            if fixture.handle.snapshot().await.unwrap().execution.idle {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    assert!(fixture.handle.snapshot().await.unwrap().checkpoint_pending);
    drop(checkpoint);
    fixture.handle.shutdown().await.unwrap();
    fixture.task.join().await.unwrap();
    shutdown_store(fixture.store, fixture.storage_task).await;
    fixture.recorder.shutdown().await.unwrap();
    fixture.writer.join().await.unwrap();
}

#[tokio::test]
async fn retained_stream_checkpoint_reconstructs_held_window_and_only_explicit_refresh_subscribes()
{
    let mut fixture = Fixture::new(store(), Journal::default());
    let stream = fixture.open().await;
    let node = stream.run.node().clone();
    let original = ready(&fixture.handle, &node, 0).await;
    let checkpoint = fixture.handle.checkpoint().await.unwrap();
    let (history, pause) = checkpoint.into_parts();
    let (base, mut entered) = base();
    let restored = session::restore(
        base,
        RecordingMode::Ephemeral,
        Some(SessionStorage {
            worker: fixture.store.clone(),
            auto_keep: AutoKeep::Never,
        }),
        history,
        CancellationToken::new(),
    )
    .await
    .unwrap();
    assert_eq!(restored.report().values, 1);
    assert_eq!(
        restored
            .workspace()
            .runtime()
            .value_of(&node)
            .unwrap()
            .shape(),
        &Shape::List(Box::new(Shape::Primitive(Primitive::Int)))
    );
    let (handle, task) = restored
        .spawn(no_files(), NonZeroUsize::new(1).unwrap())
        .unwrap();
    handle.wait_idle().await.unwrap();
    assert!(entered.try_recv().is_err());
    let values = handle.values().await.unwrap().unwrap();
    let ValuePublication::Recovered(recovered) = &values.outputs[&node] else {
        panic!("restored window")
    };
    assert_eq!(recovered.handle, original);
    submit(&handle, "explicit-refresh", ":refresh $live").await;
    let next = tokio::time::timeout(Duration::from_secs(3), entered.recv())
        .await
        .unwrap()
        .unwrap();
    assert_ne!(next.run, stream.run);
    handle.wait_idle().await.unwrap();
    handle.shutdown().await.unwrap();
    next.finish.send(()).unwrap();
    task.join().await.unwrap();
    drop(pause);
    fixture.finish(stream).await;
}

#[tokio::test]
async fn cancelled_stream_and_derived_last_values_remain_readable_without_keep() {
    let mut fixture = Fixture::new(store(), Journal::default());
    let stream = fixture.open().await;
    let node = stream.run.node().clone();
    let original = ready(&fixture.handle, &node, 0).await;
    submit(
        &fixture.handle,
        "count",
        ":calc { return length($live); } > count",
    )
    .await;
    fixture.handle.wait_idle().await.unwrap();
    let before = fixture.handle.observe().await.unwrap();
    let count = before.state.names["count"].node.clone();
    let count_run = before.state.execution.runs[&count].clone();
    // Resolve the stream's actual name from the fixture rather than assuming a public alias.
    let name = before
        .state
        .names
        .iter()
        .find(|(_, output)| output.node == node)
        .unwrap()
        .0
        .clone();
    submit(&fixture.handle, "cancel", &format!(":cancel ${name}")).await;
    tokio::time::timeout(Duration::from_secs(3), stream.token.cancelled())
        .await
        .unwrap();
    stream.finish.send(()).unwrap();
    fixture.handle.wait_idle().await.unwrap();
    let after = fixture.handle.observe().await.unwrap();
    assert_eq!(
        after.state.execution.graph.node(&node).unwrap().state(),
        wes_engine::graph::NodeState::Cancelled
    );
    assert!(after.state.execution.values.get(&node).is_none());
    assert_eq!(
        after.state.execution.stopped_values[&node].value.data(),
        &Data::List(vec![Data::Int(0)])
    );
    assert_eq!(
        after.state.execution.stopped_values[&count].value.data(),
        &Data::Int(1)
    );
    assert_eq!(after.state.execution.stopped_values[&count].run, count_run);
    let count_handle = after.values.as_ref().unwrap().outputs[&count]
        .handle()
        .unwrap()
        .clone();
    assert!(fixture.store.read(count_handle).await.unwrap().is_some());
    let publication = &after.values.as_ref().unwrap().outputs[&node];
    assert_eq!(publication.handle(), Some(&original));
    assert!(fixture.store.read(original).await.unwrap().is_some());
    assert_eq!(fixture.state.lock().unwrap().keeps, 0);
    fixture.handle.shutdown().await.unwrap();
    fixture.task.join().await.unwrap();
    shutdown_store(fixture.store, fixture.storage_task).await;
    fixture.recorder.shutdown().await.unwrap();
    fixture.writer.join().await.unwrap();
}

#[tokio::test]
async fn cancellation_accepts_pending_last_publication_and_eviction_withdraws_it() {
    let (entered, blocked) = oneshot::channel();
    let (release, released) = mpsc::channel();
    let mut backend = store();
    backend.gate = Some((entered, released));
    backend.evict = true;
    let mut fixture = Fixture::new(backend, Journal::default());
    submit(&fixture.handle, "stream", "events watch > live").await;
    let stream = fixture.entered.recv().await.unwrap();
    blocked.await.unwrap();
    submit(&fixture.handle, "cancel", ":cancel $live").await;
    tokio::time::timeout(Duration::from_secs(3), stream.token.cancelled())
        .await
        .unwrap();
    let node = stream.run.node().clone();
    assert!(
        fixture
            .handle
            .observe()
            .await
            .unwrap()
            .state
            .execution
            .stopped_values
            .contains_key(&node)
    );
    release.send(()).unwrap();
    stream.finish.send(()).unwrap();
    fixture.handle.wait_idle().await.unwrap();
    let observation = fixture.handle.observe().await.unwrap();
    let handle = observation.values.as_ref().unwrap().outputs[&node]
        .handle()
        .unwrap()
        .clone();
    assert!(fixture.store.read(handle).await.unwrap().is_some());
    submit(&fixture.handle, "evict", ":calc { return 99; } > fresh").await;
    fixture.handle.wait_idle().await.unwrap();
    let observation = fixture.handle.observe().await.unwrap();
    assert!(
        !observation
            .state
            .execution
            .stopped_values
            .contains_key(&node)
    );
    assert!(
        !observation
            .values
            .as_ref()
            .unwrap()
            .outputs
            .contains_key(&node)
    );
    fixture.handle.shutdown().await.unwrap();
    fixture.task.join().await.unwrap();
    shutdown_store(fixture.store, fixture.storage_task).await;
    fixture.recorder.shutdown().await.unwrap();
    fixture.writer.join().await.unwrap();
}

/// A crash image is the durable journal prefix, not checkpoint(): taking a checkpoint
/// would deliberately retain the progress whose loss this test exercises.
fn crash_image(records: &[Record]) -> HistoryImage {
    let mut capture = HistoryCapture::new(HistoryCaptureLimits::default());
    for record in records {
        capture.push(record.clone()).unwrap();
    }
    let receipt = AppendReceipt {
        persistence: Persistence::FileSynced,
        end_offset: records.len() as u64,
    };
    capture.finish(HistoryCheckpoint {
        journal: receipt,
        recovery: receipt,
    })
}
#[tokio::test]
async fn live_keep_survives_later_unrecorded_progress_and_restore_never_replays() {
    let mut fixture = Fixture::new(store(), Journal::default());
    let accepted = submit(
        &fixture.handle,
        "pipeline",
        "events watch | :stream accumulate limit:3 overflow:drop-oldest > collected",
    )
    .await;
    let stream = fixture.entered.recv().await.unwrap();
    fixture.handle.wait_idle().await.unwrap();
    let saved =
        fixture.handle.snapshot().await.unwrap().execution.values[&accepted.nodes[1]].clone();
    let values = fixture.handle.values().await.unwrap().unwrap();
    let handle = values.outputs[&accepted.nodes[1]].handle().unwrap().clone();
    assert!(fixture.handle.keep(handle).await.unwrap().problem.is_none());
    for n in 1..25 {
        stream.sink.send(item(n)).await.unwrap();
    }
    fixture.handle.wait_idle().await.unwrap();
    assert_ne!(
        fixture.handle.snapshot().await.unwrap().execution.values[&accepted.nodes[1]],
        saved
    );
    let image = crash_image(&fixture.records.lock().unwrap());
    let (base, mut entered) = base();
    let restored = session::restore(
        base,
        RecordingMode::Ephemeral,
        Some(SessionStorage {
            worker: fixture.store.clone(),
            auto_keep: AutoKeep::Never,
        }),
        image,
        CancellationToken::new(),
    )
    .await
    .unwrap();
    assert_eq!(
        restored.workspace().runtime().value_of(&accepted.nodes[1]),
        Some(&saved)
    );
    let (handle, task) = restored
        .spawn(no_files(), NonZeroUsize::new(1).unwrap())
        .unwrap();
    handle.wait_idle().await.unwrap();
    assert!(entered.try_recv().is_err());
    stop(handle, task).await;
    fixture.finish(stream).await;
}

#[tokio::test]
async fn branch_failure_is_checkpointed_without_stopping_sibling_or_replaying_handlers() {
    let mut fixture = Fixture::new(store(), Journal::default());
    let accepted = submit(
        &fixture.handle,
        "pipeline",
        include_str!("../../../../examples/live-stream-persistence/branches.wes"),
    )
    .await;
    assert_eq!(accepted.nodes.len(), 3, "{:?}", accepted.diagnostics);
    let stream = fixture.entered.recv().await.unwrap();
    for n in 1..10 {
        stream.sink.send(item(n)).await.unwrap();
    }
    fixture.handle.wait_idle().await.unwrap();
    let before = fixture.handle.snapshot().await.unwrap();
    let checkpoint = fixture.handle.checkpoint().await.unwrap();
    let (image, pause) = checkpoint.into_parts();
    let (base, mut entered) = base();
    let restored = session::restore(
        base,
        RecordingMode::Ephemeral,
        Some(SessionStorage {
            worker: fixture.store.clone(),
            auto_keep: AutoKeep::Never,
        }),
        image,
        CancellationToken::new(),
    )
    .await
    .unwrap();
    assert_eq!(
        restored
            .workspace()
            .runtime()
            .graph()
            .node(&accepted.nodes[1])
            .unwrap()
            .state(),
        wes_engine::graph::NodeState::Failed
    );
    assert_eq!(
        restored.workspace().runtime().value_of(&accepted.nodes[2]),
        before.execution.values.get(&accepted.nodes[2])
    );
    let (handle, task) = restored
        .spawn(no_files(), NonZeroUsize::new(1).unwrap())
        .unwrap();
    handle.wait_idle().await.unwrap();
    assert!(entered.try_recv().is_err());
    stop(handle, task).await;
    drop(pause);
    fixture.finish(stream).await;
}

#[tokio::test]
async fn late_live_snapshot_cannot_hydrate_changed_source_or_claim_an_unknown_epoch() {
    let mut fixture = Fixture::new(store(), Journal::default());
    let accepted = submit(
        &fixture.handle,
        "pipeline",
        "events watch > source | :stream accumulate limit:3 overflow:drop-oldest > collected",
    )
    .await;
    let stream = fixture.entered.recv().await.unwrap();
    fixture.handle.wait_idle().await.unwrap();
    let checkpoint = fixture.handle.checkpoint().await.unwrap();
    drop(checkpoint);
    for case in 0..3 {
        let unknown_epoch = case == 1;
        let mut records = fixture.records.lock().unwrap().clone();
        let mut snapshot = records
            .iter()
            .find_map(|r| match r {
                Record::Journal(JournalEntry::Snapshot(s))
                    if s.observation.node() == &accepted.nodes[1] =>
                {
                    Some(s.clone())
                }
                _ => None,
            })
            .unwrap();
        if unknown_epoch {
            snapshot.epoch = wes_engine::runtime::RunId::new("never-started").unwrap();
        } else {
            let mut change = records
                .iter()
                .find_map(|r| match r {
                    Record::Journal(JournalEntry::Command(c)) if c.cell == "pipeline" => {
                        Some(c.clone())
                    }
                    _ => None,
                })
                .unwrap();
            change.cell = "change".into();
            change.text = if case == 2 {
                ":node remove $collected scope:downstream"
            } else {
                ":change $source label:new"
            }
            .into();
            change.replay = change.text.clone();
            change.nodes.clear();
            change.changed_nodes = if case == 2 {
                vec![]
            } else {
                vec![accepted.nodes[0].clone()]
            };
            records.push(Record::Journal(JournalEntry::Command(change)));
        }
        records.push(Record::Journal(JournalEntry::Snapshot(snapshot)));
        let (base, _) = base();
        let restored = session::restore(
            base,
            RecordingMode::Ephemeral,
            Some(SessionStorage {
                worker: fixture.store.clone(),
                auto_keep: AutoKeep::Never,
            }),
            crash_image(&records),
            CancellationToken::new(),
        )
        .await;
        if unknown_epoch {
            assert!(matches!(restored, Err(session::RestoreError::LiveSnapshot)));
        } else {
            let restored = restored.unwrap();
            assert!(
                restored
                    .workspace()
                    .runtime()
                    .value_of(&accepted.nodes[1])
                    .is_none()
            );
            if case == 2 {
                assert!(
                    restored
                        .workspace()
                        .runtime()
                        .graph()
                        .node(&accepted.nodes[1])
                        .is_none()
                );
            } else {
                assert!(
                    restored
                        .report()
                        .unconfirmed_changes
                        .contains(&accepted.nodes[1])
                );
            }
        }
    }
    fixture.finish(stream).await;
}

#[tokio::test]
async fn failed_live_snapshot_receipt_is_not_a_successful_keep_or_checkpoint() {
    let mut fixture = Fixture::new(
        store(),
        Journal {
            fail_snapshot: true,
            ..Journal::default()
        },
    );
    let accepted = submit(
        &fixture.handle,
        "pipeline",
        "events watch | :calc { return input + 1; } > latest",
    )
    .await;
    let stream = fixture.entered.recv().await.unwrap();
    fixture.handle.wait_idle().await.unwrap();
    let values = fixture.handle.values().await.unwrap().unwrap();
    let receipt = fixture
        .handle
        .keep(values.outputs[&accepted.nodes[1]].handle().unwrap().clone())
        .await
        .unwrap();
    assert!(receipt.problem.is_some());
    assert!(receipt.journal.is_empty());
    assert!(fixture.handle.snapshot().await.unwrap().recording_blocked);
    assert!(matches!(
        fixture.handle.checkpoint().await,
        Err(SessionError::Recording)
    ));
    assert_eq!(
        fixture.handle.snapshot().await.unwrap().execution.values[&accepted.nodes[1]].data(),
        &Data::Int(1)
    );
    fixture.finish(stream).await;
}

#[tokio::test]
async fn event_provider_calls_keep_required_recovery_records() {
    let mut fixture = Fixture::new(store(), Journal::default());
    let accepted = submit(
        &fixture.handle,
        "pipeline",
        "events watch | catalog echo value:input > forwarded",
    )
    .await;
    assert_eq!(accepted.nodes.len(), 2, "{:?}", accepted.diagnostics);
    let stream = fixture.entered.recv().await.unwrap();
    for n in 1..20 {
        stream.sink.send(item(n)).await.unwrap();
    }
    fixture.handle.wait_idle().await.unwrap();
    let records = fixture.records.lock().unwrap().clone();
    assert_eq!(records.iter().filter(|r| matches!(r, Record::Recovery(RecoveryEntry::Calling(c)) if c.node == accepted.nodes[1])).count(), 20);
    assert_eq!(records.iter().filter(|r| matches!(r, Record::Recovery(RecoveryEntry::Called { node, .. }) if node == &accepted.nodes[1])).count(), 20);
    assert!(!records.iter().any(|r| matches!(r, Record::Journal(JournalEntry::Observed(o)) if o.node() == &accepted.nodes[1] && o.run().is_some())));
    assert!(!fixture.handle.snapshot().await.unwrap().recording_blocked);
    fixture.finish(stream).await;
}
