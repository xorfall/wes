use super::*;
use tokio::sync::{broadcast, mpsc as channel};
use wes_core::{Primitive, Provenance};
use wes_engine::{
    conversations::{Channel, ConversationIo, Input, InteractiveInvoker},
    driver::ConversationEvent,
    graph::NodeId,
    history::{HistoryCapture, HistoryCaptureLimits, HistoryCheckpoint, HistoryImage},
    recording::{Recorder, RecorderTask},
    runtime::Run,
};

struct Started {
    run: Run,
    io: ConversationIo,
    token: CancellationToken,
    finish: oneshot::Sender<()>,
}
struct Conversation(channel::UnboundedSender<Started>);
impl InteractiveInvoker for Conversation {
    fn start(&self, call: Call, io: ConversationIo, token: CancellationToken) -> InvocationFuture {
        io.output.write(Channel::Stdout, b"question: ").unwrap();
        io.output.opened().unwrap();
        let (finish, receive) = oneshot::channel();
        self.0
            .send(Started {
                run: call.run,
                io,
                token,
                finish,
            })
            .ok()
            .unwrap();
        Box::pin(async move {
            receive.await.unwrap();
            Ok(transcript())
        })
    }
}
fn transcript() -> Value {
    Value::new(
        Shape::Primitive(Primitive::Text),
        Data::Text("synthetic-private-transcript".into()),
        Provenance::default(),
    )
    .unwrap()
}
struct NoFinite;
impl Invoker for NoFinite {
    fn invoke(&self, _: Call, _: CancellationToken) -> InvocationFuture {
        panic!("wrong conversation port")
    }
}
fn base() -> (Workspace, channel::UnboundedReceiver<Started>) {
    let (mut base, _) = workspace(None, None);
    let (entered, receive) = channel::unbounded_channel();
    base.register_provider_all_ports(
        ProviderDescription::new(
            "dialog",
            [Capability::new(
                ["ask"],
                Shape::Primitive(Primitive::Text),
                Safety::Unsafe,
            )],
            vec![],
        )
        .unwrap(),
        Arc::new(NoFinite),
        None,
        Some(Arc::new(Conversation(entered))),
    )
    .unwrap();
    (base, receive)
}
#[derive(Default)]
struct Journal {
    records: Arc<Mutex<Vec<Record>>>,
}
impl JournalSink for Journal {
    fn append(&mut self, record: &Record) -> Result<AppendReceipt, RecordError> {
        let mut records = self.records.lock().unwrap();
        records.push(record.clone());
        Ok(AppendReceipt {
            persistence: Persistence::FileSynced,
            end_offset: records.len() as u64,
        })
    }
    fn capture(&mut self, limits: HistoryCaptureLimits) -> Result<HistoryImage, RecordError> {
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
    records: Arc<Mutex<Vec<Record>>>,
    recorder: Recorder,
    writer: RecorderTask,
    entered: channel::UnboundedReceiver<Started>,
}
impl Fixture {
    fn new() -> Self {
        let backend = store();
        let state = backend.state.clone();
        let journal = Journal::default();
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
            NonZeroUsize::new(2).unwrap(),
            SessionStorage {
                worker: store.clone(),
                auto_keep: AutoKeep::default(),
            },
        )
        .unwrap();
        Self {
            handle,
            task,
            store,
            storage_task,
            state,
            records,
            recorder,
            writer,
            entered,
        }
    }
    async fn open(&mut self) -> (Started, broadcast::Receiver<Arc<ConversationEvent>>) {
        let mut events = self.handle.subscribe_conversations().unwrap();
        let reply = submit(
            &self.handle,
            "conversation",
            "@interactive dialog ask > talking",
        )
        .await;
        assert!(reply.diagnostics.diagnostics.is_empty());
        let active = tokio::time::timeout(Duration::from_secs(3), self.entered.recv())
            .await
            .unwrap()
            .unwrap();
        tokio::time::timeout(Duration::from_secs(3), async {
            loop { if matches!(events.recv().await.unwrap().as_ref(), ConversationEvent::Started(run) if *run == active.run) { break; } }
        }).await.unwrap();
        (active, events)
    }
    async fn finish(self) {
        self.handle.shutdown().await.unwrap();
        tokio::time::timeout(Duration::from_secs(5), self.task.join())
            .await
            .unwrap()
            .unwrap();
        shutdown_store(self.store, self.storage_task).await;
        self.recorder.shutdown().await.unwrap();
        self.writer.join().await.unwrap();
    }
}
async fn output(events: &mut broadcast::Receiver<Arc<ConversationEvent>>) -> String {
    tokio::time::timeout(Duration::from_secs(3), async {
        loop {
            if let ConversationEvent::Output { batch, .. } = events.recv().await.unwrap().as_ref() {
                break batch.text.clone();
            }
        }
    })
    .await
    .unwrap()
}
async fn result(handle: &SessionHandle, node: &NodeId) -> ValueHandle {
    tokio::time::timeout(Duration::from_secs(5), handle.wait_idle())
        .await
        .unwrap()
        .unwrap();
    let values = handle.values().await.unwrap().unwrap();
    let ValuePublication::Complete(value) = &values.outputs[node] else {
        panic!("complete value")
    };
    assert!(value.problem.is_none());
    let stored = value.stored.as_ref().unwrap();
    assert!(!stored.kept);
    stored.handle.clone()
}

#[tokio::test]
async fn source_conversation_is_busy_input_is_private_and_live_output_does_not_create_archives_or_logs()
 {
    let mut fixture = Fixture::new();
    let (mut active, mut events) = fixture.open().await;
    assert_eq!(output(&mut events).await, "question: ");
    let node = active.run.node().clone();
    let snapshot = fixture.handle.snapshot().await.unwrap();
    assert_eq!(snapshot.conversations[&node], *active.run.id());
    assert_eq!(
        snapshot.execution.graph.node(&node).unwrap().state(),
        NodeState::Running
    );
    assert!(!snapshot.execution.idle);
    assert!(
        tokio::time::timeout(Duration::from_millis(20), fixture.handle.wait_idle())
            .await
            .is_err()
    );
    let duplicate = submit(
        &fixture.handle,
        "conversation",
        "@interactive dialog ask > talking",
    )
    .await;
    assert_eq!(duplicate.nodes, vec![node.clone()]);
    assert!(fixture.entered.try_recv().is_err());
    let reconnected = fixture.handle.subscribe_conversations().unwrap();
    assert!(fixture.entered.try_recv().is_err());
    drop(reconnected);
    for n in 0..4 {
        active
            .io
            .output
            .write(Channel::Stdout, n.to_string().as_bytes())
            .unwrap();
        assert_eq!(output(&mut events).await, n.to_string());
    }
    fixture
        .handle
        .input(
            node.clone(),
            active.run.id().clone(),
            b"synthetic-private-answer",
        )
        .await
        .unwrap();
    let Some(Input::Bytes(bytes)) = active.io.receive().await else {
        panic!("answer")
    };
    assert_eq!(bytes, b"synthetic-private-answer");
    let finite = submit(&fixture.handle, "finite", "catalog echo value:7")
        .await
        .nodes[0]
        .clone();
    let mut updates = fixture.handle.subscribe_updates().unwrap();
    tokio::time::timeout(Duration::from_secs(3), async {
        while !fixture
            .handle
            .snapshot()
            .await
            .unwrap()
            .execution
            .values
            .contains_key(&finite)
        {
            assert!(!matches!(
                updates.recv().await,
                Err(broadcast::error::RecvError::Closed)
            ));
        }
    })
    .await
    .unwrap();
    active.finish.send(()).unwrap();
    let stored = result(&fixture.handle, &node).await;
    assert_eq!(
        fixture.state.lock().unwrap().values[&stored],
        transcript().with_provenance(
            wes_core::Provenance::default()
                .with_policy(&wes_core::flow::FlowPolicy::default().from_origin("local:fixture"))
        )
    );
    assert!(!fixture.state.lock().unwrap().kept.contains(&stored));
    {
        let records = fixture.records.lock().unwrap();
        assert!(!format!("{records:?}").contains("synthetic-private"));
        assert!(records.iter().filter(|record| matches!(record, Record::Journal(JournalEntry::Observed(observed)) if observed.node() == &node)).count() <= 3);
        assert!(!records.iter().any(|record| matches!(record, Record::Journal(JournalEntry::Result(result)) if result.node == node)));
        assert_eq!(records.iter().filter(|record| matches!(record, Record::Recovery(RecoveryEntry::Calling(call)) if call.node == node)).count(), 1);
    }
    fixture.finish().await;
}

#[tokio::test]
async fn checkpoint_waits_for_conversation_but_allows_answer_and_never_implicitly_keeps_transcript()
{
    let mut fixture = Fixture::new();
    let (mut active, _) = fixture.open().await;
    let handle = fixture.handle.clone();
    let checkpoint = tokio::spawn(async move { handle.checkpoint().await });
    let mut updates = fixture.handle.subscribe_updates().unwrap();
    tokio::time::timeout(Duration::from_secs(3), async {
        while !fixture.handle.snapshot().await.unwrap().checkpoint_pending {
            assert!(!matches!(
                updates.recv().await,
                Err(broadcast::error::RecvError::Closed)
            ));
        }
    })
    .await
    .unwrap();
    assert!(!checkpoint.is_finished());
    fixture
        .handle
        .input(
            active.run.node().clone(),
            active.run.id().clone(),
            b"answer while saving",
        )
        .await
        .unwrap();
    assert!(matches!(active.io.receive().await, Some(Input::Bytes(_))));
    fixture
        .handle
        .eof(active.run.node().clone(), active.run.id().clone())
        .await
        .unwrap();
    assert!(matches!(active.io.receive().await, Some(Input::Eof)));
    active.finish.send(()).unwrap();
    let checkpoint = tokio::time::timeout(Duration::from_secs(3), checkpoint)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert_eq!(fixture.state.lock().unwrap().keeps, 0);
    drop(checkpoint);
    let state = fixture.state.clone();
    fixture.finish().await;
    assert_eq!(state.lock().unwrap().keeps, 0);
}

#[tokio::test]
async fn explicit_keep_restores_held_conversation_and_refresh_needs_new_run_input() {
    let mut fixture = Fixture::new();
    let (active, _) = fixture.open().await;
    let old = active.run.clone();
    active.finish.send(()).unwrap();
    let kept = result(&fixture.handle, old.node()).await;
    fixture.handle.keep(kept.clone()).await.unwrap();
    let checkpoint = fixture.handle.checkpoint().await.unwrap();
    let (history, pause) = checkpoint.into_parts();
    let (base, mut entered) = base();
    let restored = session::restore(
        base,
        RecordingMode::Ephemeral,
        Some(SessionStorage {
            worker: fixture.store.clone(),
            auto_keep: AutoKeep::default(),
        }),
        history,
        CancellationToken::new(),
    )
    .await
    .unwrap();
    assert_eq!(restored.report().values, 1);
    let (handle, task) = restored
        .spawn(no_files(), NonZeroUsize::new(1).unwrap())
        .unwrap();
    handle.wait_idle().await.unwrap();
    assert!(entered.try_recv().is_err());
    assert!(handle.snapshot().await.unwrap().conversations.is_empty());
    assert!(
        handle
            .input(old.node().clone(), old.id().clone(), b"stale answer")
            .await
            .is_err()
    );
    let mut events = handle.subscribe_conversations().unwrap();
    submit(&handle, "refresh", ":refresh $talking").await;
    let mut active = entered.recv().await.unwrap();
    tokio::time::timeout(Duration::from_secs(3), async {
        loop {
            if matches!(
                events.recv().await.unwrap().as_ref(),
                ConversationEvent::Started(_)
            ) {
                break;
            }
        }
    })
    .await
    .unwrap();
    assert_ne!(old, active.run);
    assert!(
        handle
            .input(old.node().clone(), old.id().clone(), b"old answer")
            .await
            .is_err()
    );
    handle
        .input(
            active.run.node().clone(),
            active.run.id().clone(),
            b"new answer",
        )
        .await
        .unwrap();
    assert!(matches!(active.io.receive().await, Some(Input::Bytes(_))));
    active.finish.send(()).unwrap();
    let fresh = result(&handle, old.node()).await;
    assert_ne!(fresh, kept);
    stop(handle, task).await;
    drop(pause);
    fixture.finish().await;
}

#[tokio::test]
async fn shutdown_revokes_conversation_and_joins_before_stopping_without_auto_retention() {
    let mut fixture = Fixture::new();
    let (active, _) = fixture.open().await;
    fixture.handle.shutdown().await.unwrap();
    active.token.cancelled().await;
    assert!(
        fixture
            .handle
            .input(
                active.run.node().clone(),
                active.run.id().clone(),
                b"late answer"
            )
            .await
            .is_err()
    );
    let mut joined = Box::pin(fixture.task.join());
    assert!(
        tokio::time::timeout(Duration::from_millis(20), &mut joined)
            .await
            .is_err()
    );
    assert_eq!(fixture.state.lock().unwrap().keeps, 0);
    active.finish.send(()).unwrap();
    tokio::time::timeout(Duration::from_secs(3), joined)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(fixture.state.lock().unwrap().keeps, 0);
    shutdown_store(fixture.store, fixture.storage_task).await;
    fixture.recorder.shutdown().await.unwrap();
    fixture.writer.join().await.unwrap();
}

#[tokio::test]
async fn source_refuses_interaction_without_a_captured_provider_port() {
    let fixture = Fixture::new();
    let refused = submit(
        &fixture.handle,
        "unsupported",
        "@interactive catalog echo value:7",
    )
    .await;
    assert!(refused.nodes.is_empty());
    assert!(
        refused
            .diagnostics
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.code == "ENG006")
    );
    assert!(
        fixture
            .handle
            .snapshot()
            .await
            .unwrap()
            .execution
            .graph
            .is_empty()
    );
    fixture.finish().await;
}

#[tokio::test]
async fn unsaved_transcript_does_not_reappear_from_checkpoint_or_rerun_the_dialogue() {
    let mut fixture = Fixture::new();
    let (active, _) = fixture.open().await;
    let node = active.run.node().clone();
    active.finish.send(()).unwrap();
    let stored = result(&fixture.handle, &node).await;
    let (history, pause) = fixture.handle.checkpoint().await.unwrap().into_parts();
    assert!(!fixture.state.lock().unwrap().kept.contains(&stored));
    let (base, mut entered) = base();
    let restored = session::restore(
        base,
        RecordingMode::Ephemeral,
        Some(SessionStorage {
            worker: fixture.store.clone(),
            auto_keep: AutoKeep::default(),
        }),
        history,
        CancellationToken::new(),
    )
    .await
    .unwrap();
    assert_eq!(restored.report().values, 0);
    let (handle, task) = restored
        .spawn(no_files(), NonZeroUsize::new(1).unwrap())
        .unwrap();
    handle.wait_idle().await.unwrap();
    assert!(entered.try_recv().is_err());
    let snapshot = handle.snapshot().await.unwrap();
    assert!(snapshot.execution.values.is_empty());
    assert_eq!(
        snapshot.execution.graph.node(&node).unwrap().state(),
        NodeState::Stale
    );
    assert!(snapshot.conversations.is_empty());
    stop(handle, task).await;
    drop(pause);
    fixture.finish().await;
}
