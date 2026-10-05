use super::*;
use std::collections::{HashMap, HashSet, VecDeque};
use wes_core::Value;
use wes_engine::{
    session::{SessionStorage, ValuePublication},
    storage::{
        AutoKeep, EvictionBatch, LoadedValue, StoreError, StoreWorker, StoreWorkerLimits,
        ValueHandle, ValueStore, spawn_store,
    },
};

#[derive(Default)]
struct State {
    values: HashMap<ValueHandle, Value>,
    reasons: HashMap<ValueHandle, wes_engine::storage::Retention>,
    kept: HashSet<ValueHandle>,
    order: Vec<ValueHandle>,
    evicted: VecDeque<ValueHandle>,
    keeps: usize,
    releases: usize,
    eviction_reads: usize,
}
struct Store {
    state: Arc<Mutex<State>>,
    gate: Option<Gate>,
    evict: bool,
    fail_keep: bool,
    persistence: Persistence,
    keep_gate: Option<Gate>,
    release_gate: Option<Gate>,
    fail_release: bool,
    fail_evictions: bool,
}
impl ValueStore for Store {
    fn retained_persistence(&self) -> Persistence {
        self.persistence
    }
    fn store(&mut self, value: &Value) -> Result<ValueHandle, StoreError> {
        if let Some((entered, release)) = self.gate.take() {
            let _ = entered.send(());
            release.recv_timeout(Duration::from_secs(5)).unwrap();
        }
        let handle = ValueHandle::fresh();
        let mut state = self.state.lock().unwrap();
        if self.evict {
            let unkept: Vec<_> = state
                .values
                .keys()
                .filter(|id| !state.kept.contains(*id))
                .cloned()
                .collect();
            for id in unkept {
                state.values.remove(&id);
                state.evicted.push_back(id);
            }
        }
        state.values.insert(handle.clone(), value.clone());
        state.order.push(handle.clone());
        Ok(handle)
    }
    fn read(&self, handle: &ValueHandle) -> Result<Option<LoadedValue>, StoreError> {
        Ok(self
            .state
            .lock()
            .unwrap()
            .values
            .get(handle)
            .cloned()
            .map(|value| LoadedValue { value }))
    }
    fn encoded(&self, _: &ValueHandle) -> Result<Option<Vec<u8>>, StoreError> {
        Ok(None)
    }
    fn size(&self, handle: &ValueHandle) -> Result<Option<u64>, StoreError> {
        Ok(self
            .state
            .lock()
            .unwrap()
            .values
            .contains_key(handle)
            .then_some(128))
    }
    fn keep(&mut self, handle: &ValueHandle) -> Result<bool, StoreError> {
        self.state.lock().unwrap().keeps += 1;
        if let Some((entered, released)) = self.keep_gate.take() {
            entered.send(()).unwrap();
            released.recv_timeout(Duration::from_secs(5)).unwrap();
        }
        if self.fail_keep {
            return Err(StoreError::backend(
                "archive",
                std::io::Error::other("private archive detail"),
            ));
        }
        self.state.lock().unwrap().kept.insert(handle.clone());
        Ok(true)
    }
    fn keep_with_reason(
        &mut self,
        handle: &ValueHandle,
        reason: wes_engine::storage::Retention,
    ) -> Result<bool, StoreError> {
        let kept = self.keep(handle)?;
        if kept {
            self.state
                .lock()
                .unwrap()
                .reasons
                .insert(handle.clone(), reason);
        }
        Ok(kept)
    }
    fn retention(
        &self,
        handle: &ValueHandle,
    ) -> Result<wes_engine::storage::Retention, StoreError> {
        let state = self.state.lock().unwrap();
        Ok(state
            .reasons
            .get(handle)
            .copied()
            .unwrap_or(if state.kept.contains(handle) {
                wes_engine::storage::Retention::Unknown
            } else {
                wes_engine::storage::Retention::Temporary
            }))
    }
    fn release(&mut self, handle: &ValueHandle) -> Result<bool, StoreError> {
        self.state.lock().unwrap().releases += 1;
        if let Some((entered, released)) = self.release_gate.take() {
            entered.send(()).unwrap();
            released.recv_timeout(Duration::from_secs(5)).unwrap();
        }
        let mut state = self.state.lock().unwrap();
        state.kept.remove(handle);
        let removed = state.values.remove(handle).is_some();
        if self.fail_release {
            return Err(StoreError::Released {
                handle: handle.clone(),
                source: Box::new(std::io::Error::other("private release failure")),
            });
        }
        Ok(removed)
    }
    fn take_evicted(&mut self, maximum: usize) -> Result<EvictionBatch, StoreError> {
        let mut state = self.state.lock().unwrap();
        state.eviction_reads += 1;
        if self.fail_evictions {
            return Err(StoreError::backend(
                "synthetic",
                std::io::Error::other("private eviction failure"),
            ));
        }
        let count = maximum.min(state.evicted.len());
        let handles = state.evicted.drain(..count).collect();
        Ok(EvictionBatch {
            handles,
            more: !state.evicted.is_empty(),
        })
    }
    fn is_kept(&self, handle: &ValueHandle) -> Result<bool, StoreError> {
        Ok(self.state.lock().unwrap().kept.contains(handle))
    }
}
fn store() -> Store {
    Store {
        state: Arc::default(),
        gate: None,
        evict: false,
        fail_keep: false,
        persistence: Persistence::FileSynced,
        keep_gate: None,
        release_gate: None,
        fail_release: false,
        fail_evictions: false,
    }
}
async fn shutdown_store(store: StoreWorker, task: wes_engine::storage::StoreWorkerTask) {
    store.shutdown().await.unwrap();
    task.join().await.unwrap();
}

#[tokio::test]
async fn retention_policy_is_snapshot_visible_and_captured_at_ready_not_when_storage_unblocks() {
    use wes_engine::session::RetentionPolicy;
    let mut backend = store();
    let state = backend.state.clone();
    let (entered, storage_entered) = oneshot::channel();
    let (release, released) = mpsc::channel();
    backend.gate = Some((entered, released));
    let (store, store_task) = spawn_store(backend, StoreWorkerLimits::default()).unwrap();
    let (workspace, calls) = workspace(None, None);
    let (handle, task) = session::spawn_with_storage(
        workspace,
        RecordingMode::Ephemeral,
        no_files(),
        NonZeroUsize::new(2).unwrap(),
        SessionStorage {
            worker: store.clone(),
            auto_keep: AutoKeep::Never,
        },
    )
    .unwrap();
    let disabled = RetentionPolicy {
        automatic: false,
        under: 7,
    };
    let mut updates = handle.subscribe_values().unwrap().unwrap();
    handle.set_retention_policy(disabled).await.unwrap();
    updates.try_recv().unwrap();
    handle.set_retention_policy(disabled).await.unwrap();
    assert!(matches!(
        updates.try_recv(),
        Err(tokio::sync::broadcast::error::TryRecvError::Empty)
    ));
    assert_eq!(handle.values().await.unwrap().unwrap().policy, disabled);
    assert_eq!(calls.load(Ordering::SeqCst), 0);
    submit(&handle, "first", "catalog echo value:first > first").await;
    storage_entered.await.unwrap();
    submit(&handle, "second", "catalog echo value:second > second").await;
    tokio::time::timeout(Duration::from_secs(5), async {
        while handle.values().await.unwrap().unwrap().pending != 2 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    handle
        .set_retention_policy(RetentionPolicy {
            automatic: true,
            under: 128,
        })
        .await
        .unwrap();
    submit(&handle, "third", "catalog echo value:third > third").await;
    tokio::time::timeout(Duration::from_secs(5), async {
        while handle.values().await.unwrap().unwrap().pending != 3 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    let disabled = RetentionPolicy {
        automatic: false,
        under: 127,
    };
    handle.set_retention_policy(disabled).await.unwrap();
    assert_eq!(handle.values().await.unwrap().unwrap().policy, disabled);
    release.send(()).unwrap();
    handle.wait_idle().await.unwrap();
    {
        let state = state.lock().unwrap();
        assert_eq!(state.order.len(), 3);
        assert!(!state.kept.contains(&state.order[0]));
        assert!(!state.kept.contains(&state.order[1]));
        assert!(state.kept.contains(&state.order[2])); // Inclusive 128-byte policy at Ready.
    }
    handle
        .set_retention_policy(RetentionPolicy {
            automatic: true,
            under: 127,
        })
        .await
        .unwrap();
    submit(&handle, "fourth", "catalog echo value:fourth > fourth").await;
    handle.wait_idle().await.unwrap();
    {
        let state = state.lock().unwrap();
        assert_eq!(state.order.len(), 4);
        assert!(!state.kept.contains(&state.order[3]));
        assert_eq!(state.kept.len(), 1); // No retroactive archive or release.
    }
    stop(handle.clone(), task).await;
    assert!(matches!(
        handle.set_retention_policy(disabled).await,
        Err(SessionError::Stopped)
    ));
    shutdown_store(store, store_task).await;
}

#[tokio::test]
async fn retention_preference_without_a_store_is_an_explicit_non_mutating_refusal() {
    use wes_engine::session::RetentionPolicy;
    let (workspace, calls) = workspace(None, None);
    let (handle, task) = session::spawn(
        workspace,
        RecordingMode::Ephemeral,
        no_files(),
        NonZeroUsize::new(1).unwrap(),
    )
    .unwrap();
    assert!(matches!(
        handle
            .set_retention_policy(RetentionPolicy {
                automatic: true,
                under: u64::MAX
            })
            .await,
        Err(SessionError::NoStorage)
    ));
    assert!(handle.values().await.unwrap().is_none());
    assert!(matches!(
        handle.keep(ValueHandle::fresh()).await,
        Err(SessionError::NoStorage)
    ));
    assert!(matches!(
        handle.release(ValueHandle::fresh()).await,
        Err(SessionError::NoStorage)
    ));
    assert!(handle.log().await.unwrap().entries.is_empty());
    assert_eq!(calls.load(Ordering::SeqCst), 0);
    stop(handle, task).await;
}

async fn current(
    handle: &SessionHandle,
    name: &str,
) -> (wes_engine::graph::NodeId, session::PublishedValue) {
    let node = handle.snapshot().await.unwrap().names[name].node.clone();
    let snapshot = handle.values().await.unwrap().unwrap();
    let ValuePublication::Complete(output) = &snapshot.outputs[&node] else {
        panic!("complete publication")
    };
    (node, output.clone())
}

#[tokio::test]
async fn manual_keep_acknowledges_archive_and_exact_run_then_release_forgets_only_that_output() {
    let backend = store();
    let state = backend.state.clone();
    let (store, storage_task) = spawn_store(backend, StoreWorkerLimits::default()).unwrap();
    let results = Arc::new(AtomicUsize::new(0));
    let (recorder, writer) = spawn_recorder(
        ResultSink {
            state: state.clone(),
            results: results.clone(),
            gate: None,
            fail: false,
        },
        RecorderLimits::default(),
    )
    .unwrap();
    let (workspace, calls) = workspace(None, None);
    let (handle, task) = session::spawn_with_storage(
        workspace,
        RecordingMode::Required(CallJournal::new(
            recorder.clone(),
            RequiredPersistence::FileSynced,
        )),
        no_files(),
        NonZeroUsize::new(1).unwrap(),
        SessionStorage {
            worker: store.clone(),
            auto_keep: AutoKeep::Never,
        },
    )
    .unwrap();
    assert!(matches!(
        handle.keep(ValueHandle::fresh()).await,
        Err(SessionError::UnknownValue)
    ));
    submit(&handle, "source", "catalog echo value:hello > answer").await;
    handle.wait_idle().await.unwrap();
    let (node, output) = current(&handle, "answer").await;
    let value_handle = output.stored.as_ref().unwrap().handle.clone();
    assert!(!output.durably_retained());
    let receipt = handle.keep(value_handle.clone()).await.unwrap();
    assert!(receipt.problem.is_none());
    assert!(receipt.affected);
    assert_eq!(receipt.handle, value_handle);
    assert_eq!(receipt.journal.len(), 1);
    assert_eq!(receipt.journal[0].node, node);
    assert_eq!(receipt.journal[0].run, output.run);
    assert_eq!(results.load(Ordering::SeqCst), 1);
    assert!(current(&handle, "answer").await.1.durably_retained());
    assert_eq!(state.lock().unwrap().order.len(), 1); // Retention never publishes another identity.
    let receipt = handle.release(value_handle.clone()).await.unwrap();
    assert!(receipt.problem.is_none());
    assert!(receipt.affected);
    let snapshot = handle.snapshot().await.unwrap();
    assert_eq!(
        snapshot.execution.graph.node(&node).unwrap().state(),
        NodeState::Stale
    );
    assert!(!snapshot.execution.values.contains_key(&node));
    assert!(handle.values().await.unwrap().unwrap().outputs.is_empty());
    assert!(snapshot.execution.errors.is_empty());
    assert!(!handle.release(value_handle).await.unwrap().affected);
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    stop(handle, task).await;
    shutdown_store(store, storage_task).await;
    recorder.shutdown().await.unwrap();
    writer.join().await.unwrap();
}

#[tokio::test]
async fn manual_keep_survives_disconnect_drop_and_shutdown_without_replacing_a_new_run() {
    let mut backend = store();
    let state = backend.state.clone();
    let (entered, blocked) = oneshot::channel();
    let (release, released) = mpsc::channel();
    backend.keep_gate = Some((entered, released));
    let (store, storage_task) = spawn_store(backend, StoreWorkerLimits::default()).unwrap();
    let results = Arc::new(AtomicUsize::new(0));
    let (recorder, writer) = spawn_recorder(
        ResultSink {
            state: state.clone(),
            results: results.clone(),
            gate: None,
            fail: false,
        },
        RecorderLimits::default(),
    )
    .unwrap();
    let (workspace, calls) = workspace(None, None);
    let (handle, task) = session::spawn_with_storage(
        workspace,
        RecordingMode::Required(CallJournal::new(
            recorder.clone(),
            RequiredPersistence::FileSynced,
        )),
        no_files(),
        NonZeroUsize::new(2).unwrap(),
        SessionStorage {
            worker: store.clone(),
            auto_keep: AutoKeep::Never,
        },
    )
    .unwrap();
    submit(&handle, "source", "catalog echo value:old > answer").await;
    handle.wait_idle().await.unwrap();
    let (_, old) = current(&handle, "answer").await;
    let selected = old.stored.unwrap().handle;
    let caller = {
        let handle = handle.clone();
        let selected = selected.clone();
        tokio::spawn(async move { handle.keep(selected).await })
    };
    blocked.await.unwrap();
    caller.abort();
    submit(&handle, "change", ":change $answer value:new").await;
    submit(&handle, "refresh", ":refresh $answer").await;
    tokio::time::timeout(Duration::from_secs(5), async {
        while calls.load(Ordering::SeqCst) != 2
            || handle.values().await.unwrap().unwrap().pending != 2
        {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    submit(&handle, "drop", ":node remove $answer scope:downstream").await;
    handle.shutdown().await.unwrap();
    let joined = tokio::spawn(task.join());
    assert!(!joined.is_finished());
    release.send(()).unwrap();
    joined.await.unwrap().unwrap();
    assert!(state.lock().unwrap().kept.contains(&selected));
    assert_eq!(results.load(Ordering::SeqCst), 1);
    assert_eq!(state.lock().unwrap().order.len(), 2); // Already accepted newer publication was joined.
    assert_eq!(state.lock().unwrap().keeps, 1);
    shutdown_store(store, storage_task).await;
    recorder.shutdown().await.unwrap();
    writer.join().await.unwrap();
}

#[tokio::test]
async fn partial_release_preserves_memory_and_error_identity_without_claiming_retention_or_retrying()
 {
    let mut backend = store();
    backend.fail_release = true;
    let state = backend.state.clone();
    let (store, storage_task) = spawn_store(backend, StoreWorkerLimits::default()).unwrap();
    let (workspace, _) = workspace(None, None);
    let (handle, task) = session::spawn_with_storage(
        workspace,
        RecordingMode::Ephemeral,
        no_files(),
        NonZeroUsize::new(1).unwrap(),
        SessionStorage {
            worker: store.clone(),
            auto_keep: AutoKeep::default(),
        },
    )
    .unwrap();
    submit(&handle, "source", "catalog echo value:hello > answer").await;
    handle.wait_idle().await.unwrap();
    let (node, before) = current(&handle, "answer").await;
    let selected = before.stored.unwrap().handle;
    let receipt = handle.release(selected.clone()).await.unwrap();
    assert!(receipt.may_have_applied);
    assert!(receipt.problem.is_some());
    assert!(!format!("{receipt:?}").contains("private release failure"));
    let (_, after) = current(&handle, "answer").await;
    assert_eq!(
        after.problem.as_ref().unwrap().id(),
        receipt.problem.as_ref().unwrap().id()
    );
    assert_eq!(after.uncertain_handle.as_ref(), Some(&selected));
    assert!(!after.durably_retained());
    let snapshot = handle.snapshot().await.unwrap();
    assert_eq!(
        snapshot.execution.graph.node(&node).unwrap().state(),
        NodeState::Ready
    );
    assert!(snapshot.execution.values.contains_key(&node));
    assert!(snapshot.execution.errors.is_empty());
    assert_eq!(state.lock().unwrap().releases, 1);
    assert_eq!(handle.values().await.unwrap().unwrap().failures, 1);
    let log = handle.log().await.unwrap();
    assert!(log.entries.iter().any(|entry| matches!(entry.entry(), JournalEntry::Noticed(notice)
        if notice.error() == receipt.problem.as_ref().unwrap()
            && notice.context() == &wes_engine::history::NoticeContext::Release { handle: selected.clone(), may_have_applied: true })));
    stop(handle, task).await;
    shutdown_store(store, storage_task).await;
}

#[tokio::test]
async fn manual_keep_result_failure_keeps_archive_blocks_new_keeps_and_still_allows_explicit_release()
 {
    let backend = store();
    let state = backend.state.clone();
    let (store, storage_task) = spawn_store(backend, StoreWorkerLimits::default()).unwrap();
    let results = Arc::new(AtomicUsize::new(0));
    let (recorder, writer) = spawn_recorder(
        ResultSink {
            state: state.clone(),
            results: results.clone(),
            gate: None,
            fail: true,
        },
        RecorderLimits::default(),
    )
    .unwrap();
    let (workspace, _) = workspace(None, None);
    let (handle, task) = session::spawn_with_storage(
        workspace,
        RecordingMode::Required(CallJournal::new(
            recorder.clone(),
            RequiredPersistence::FileSynced,
        )),
        no_files(),
        NonZeroUsize::new(1).unwrap(),
        SessionStorage {
            worker: store.clone(),
            auto_keep: AutoKeep::Never,
        },
    )
    .unwrap();
    submit(&handle, "source", "catalog echo value:hello > answer").await;
    handle.wait_idle().await.unwrap();
    let (_, output) = current(&handle, "answer").await;
    let selected = output.stored.unwrap().handle;
    let receipt = handle.keep(selected.clone()).await.unwrap();
    assert!(receipt.problem.is_some());
    assert!(receipt.may_have_applied);
    assert!(receipt.stored.as_ref().unwrap().kept);
    assert!(receipt.journal.is_empty());
    assert!(handle.snapshot().await.unwrap().recording_blocked);
    assert!(matches!(
        handle.keep(selected.clone()).await,
        Err(SessionError::Recording)
    ));
    assert!(state.lock().unwrap().kept.contains(&selected));
    assert_eq!(results.load(Ordering::SeqCst), 1);
    assert_eq!(state.lock().unwrap().keeps, 1);
    assert!(!current(&handle, "answer").await.1.durably_retained());
    assert!(handle.release(selected).await.unwrap().affected);
    stop(handle, task).await;
    shutdown_store(store, storage_task).await;
    recorder.shutdown().await.unwrap();
    writer.join().await.unwrap();
}

#[tokio::test]
async fn manual_storage_queue_is_bounded_but_snapshots_preferences_and_shutdown_remain_responsive()
{
    let mut backend = store();
    let state = backend.state.clone();
    let (entered, blocked) = oneshot::channel();
    let (release, released) = mpsc::channel();
    backend.release_gate = Some((entered, released));
    let (store, storage_task) = spawn_store(backend, StoreWorkerLimits::default()).unwrap();
    let (workspace, _) = workspace(None, None);
    let (handle, task) = session::spawn_with_storage(
        workspace,
        RecordingMode::Ephemeral,
        no_files(),
        NonZeroUsize::new(1).unwrap(),
        SessionStorage {
            worker: store.clone(),
            auto_keep: AutoKeep::Never,
        },
    )
    .unwrap();
    let mut requests = tokio::task::JoinSet::new();
    let caller = handle.clone();
    requests.spawn(async move { caller.release(ValueHandle::fresh()).await });
    blocked.await.unwrap();
    for _ in 1..256 {
        let caller = handle.clone();
        requests.spawn(async move { caller.release(ValueHandle::fresh()).await });
    }
    tokio::time::timeout(Duration::from_secs(5), async {
        while handle.values().await.unwrap().unwrap().pending != 256 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    assert!(matches!(
        handle.release(ValueHandle::fresh()).await,
        Err(SessionError::Capacity)
    ));
    handle
        .set_retention_policy(session::RetentionPolicy {
            automatic: false,
            under: 123,
        })
        .await
        .unwrap();
    assert_eq!(handle.values().await.unwrap().unwrap().policy.under, 123);
    assert_eq!(state.lock().unwrap().releases, 1);
    handle.shutdown().await.unwrap();
    let joined = tokio::spawn(task.join());
    assert!(!joined.is_finished());
    release.send(()).unwrap();
    while let Some(reply) = requests.join_next().await {
        assert!(!reply.unwrap().unwrap().affected);
    }
    joined.await.unwrap().unwrap();
    assert_eq!(state.lock().unwrap().releases, 256);
    shutdown_store(store, storage_task).await;
}

#[tokio::test]
async fn release_waits_for_the_preceding_keep_result_receipt_even_when_its_client_disconnects() {
    let backend = store();
    let state = backend.state.clone();
    let (store, storage_task) = spawn_store(backend, StoreWorkerLimits::default()).unwrap();
    let results = Arc::new(AtomicUsize::new(0));
    let (entered, blocked) = oneshot::channel();
    let (release, released) = mpsc::channel();
    let (recorder, writer) = spawn_recorder(
        ResultSink {
            state: state.clone(),
            results: results.clone(),
            gate: Some((entered, released)),
            fail: false,
        },
        RecorderLimits::default(),
    )
    .unwrap();
    let (workspace, _) = workspace(None, None);
    let (handle, task) = session::spawn_with_storage(
        workspace,
        RecordingMode::Required(CallJournal::new(
            recorder.clone(),
            RequiredPersistence::FileSynced,
        )),
        no_files(),
        NonZeroUsize::new(1).unwrap(),
        SessionStorage {
            worker: store.clone(),
            auto_keep: AutoKeep::Never,
        },
    )
    .unwrap();
    submit(&handle, "source", "catalog echo value:hello > answer").await;
    handle.wait_idle().await.unwrap();
    let (_, output) = current(&handle, "answer").await;
    let selected = output.stored.unwrap().handle;
    let keep = {
        let handle = handle.clone();
        let selected = selected.clone();
        tokio::spawn(async move { handle.keep(selected).await })
    };
    blocked.await.unwrap();
    keep.abort();
    assert!(state.lock().unwrap().kept.contains(&selected));
    let remove = {
        let handle = handle.clone();
        tokio::spawn(async move { handle.release(selected).await })
    };
    tokio::time::timeout(Duration::from_secs(5), async {
        while handle.values().await.unwrap().unwrap().pending != 2 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    assert_eq!(state.lock().unwrap().releases, 0);
    handle.shutdown().await.unwrap();
    let joined = tokio::spawn(task.join());
    assert!(!joined.is_finished());
    release.send(()).unwrap();
    assert!(remove.await.unwrap().unwrap().affected);
    joined.await.unwrap().unwrap();
    assert_eq!(results.load(Ordering::SeqCst), 1);
    assert_eq!(state.lock().unwrap().keeps, 1);
    assert_eq!(state.lock().unwrap().releases, 1);
    shutdown_store(store, storage_task).await;
    recorder.shutdown().await.unwrap();
    writer.join().await.unwrap();
}

struct SharedResultSink {
    state: Arc<Mutex<State>>,
    results: Arc<AtomicUsize>,
    fail_second: bool,
}
impl JournalSink for SharedResultSink {
    fn append(&mut self, record: &Record) -> Result<AppendReceipt, RecordError> {
        if let Record::Journal(JournalEntry::Result(result)) = record {
            assert!(self.state.lock().unwrap().kept.contains(&result.handle));
            assert_eq!(result.run.as_str(), result.node.as_str());
            let number = self.results.fetch_add(1, Ordering::SeqCst) + 1;
            if self.fail_second && number == 2 {
                return Err(RecordError::backend(
                    "synthetic",
                    true,
                    std::io::Error::other("private second receipt failure"),
                ));
            }
        }
        Ok(AppendReceipt {
            persistence: Persistence::FileSynced,
            end_offset: 1,
        })
    }
}
#[tokio::test]
async fn keep_preserves_distinct_runs_and_each_shared_handles_successful_journal_receipt() {
    use wes_engine::{
        graph::NodeId,
        history::{
            CommandRecord, HistoryCapture, HistoryCaptureLimits, HistoryCheckpoint, RetainedResult,
        },
    };
    for fail_second in [false, true] {
        let backend = store();
        let state = backend.state.clone();
        let selected = ValueHandle::fresh();
        state.lock().unwrap().values.insert(
            selected.clone(),
            Value::new(
                Shape::Unknown,
                Data::Text("retained".into()),
                Default::default(),
            )
            .unwrap(),
        );
        state.lock().unwrap().kept.insert(selected.clone());
        let nodes = [NodeId::new("id80").unwrap(), NodeId::new("id81").unwrap()];
        let text = "catalog echo value:old > first\ncatalog echo value:old > second";
        let mut capture = HistoryCapture::new(HistoryCaptureLimits::default());
        capture
            .push(Record::Journal(JournalEntry::Command(CommandRecord {
                source_name: "fixture.wes".into(),
                source_start: wes_language::Position { line: 1, column: 1 },
                changed_nodes: vec![],
                document: None,
                revision_of: None,
                environments: None,
                cell: "old".into(),
                text: text.into(),
                replay: text.into(),
                nodes: nodes.to_vec(),
                type_sources: Default::default(),
                calculation_package: None,
                imports: vec![],
            })))
            .unwrap();
        for node in &nodes {
            capture
                .push(Record::Journal(JournalEntry::Result(RetainedResult {
                    retention: wes_engine::storage::Retention::Unknown,
                    node: node.clone(),
                    handle: selected.clone(),
                    run: wes_engine::runtime::RunId::new(node.as_str()).unwrap(),
                })))
                .unwrap();
        }
        let checkpoint = AppendReceipt {
            persistence: Persistence::FileSynced,
            end_offset: 100,
        };
        let image = capture.finish(HistoryCheckpoint {
            journal: checkpoint,
            recovery: checkpoint,
        });
        let (store, storage_task) = spawn_store(backend, StoreWorkerLimits::default()).unwrap();
        let results = Arc::new(AtomicUsize::new(0));
        let (recorder, writer) = spawn_recorder(
            SharedResultSink {
                state: state.clone(),
                results: results.clone(),
                fail_second,
            },
            RecorderLimits::default(),
        )
        .unwrap();
        let (workspace, calls) = workspace(None, None);
        let restored = session::restore(
            workspace,
            RecordingMode::Required(CallJournal::new(
                recorder.clone(),
                RequiredPersistence::FileSynced,
            )),
            Some(SessionStorage {
                worker: store.clone(),
                auto_keep: AutoKeep::Never,
            }),
            image,
            wes_engine::driver::CancellationToken::new(),
        )
        .await
        .unwrap();
        let (handle, task) = restored
            .spawn(no_files(), NonZeroUsize::new(1).unwrap())
            .unwrap();
        let receipt = handle.keep(selected).await.unwrap();
        assert_eq!(receipt.problem.is_some(), fail_second);
        assert_eq!(receipt.journal.len(), if fail_second { 1 } else { 2 });
        assert!(
            receipt
                .journal
                .iter()
                .all(|ack| ack.run.as_str() == ack.node.as_str())
        );
        let snapshot = handle.values().await.unwrap().unwrap();
        for node in nodes {
            let ValuePublication::Complete(output) = &snapshot.outputs[&node] else {
                panic!("manual confirmation")
            };
            assert_eq!(output.run.as_str(), node.as_str());
            assert_eq!(output.durably_retained(), !fail_second);
        }
        assert_eq!(
            handle.snapshot().await.unwrap().recording_blocked,
            fail_second
        );
        assert_eq!(state.lock().unwrap().keeps, 1);
        assert_eq!(results.load(Ordering::SeqCst), 2);
        assert_eq!(calls.load(Ordering::SeqCst), 0);
        stop(handle, task).await;
        shutdown_store(store, storage_task).await;
        recorder.shutdown().await.unwrap();
        writer.join().await.unwrap();
    }
}

#[tokio::test]
async fn failed_keep_is_logged_after_its_node_and_client_are_gone_and_shutdown_joins_the_log() {
    use wes_engine::history::NoticeContext;
    let mut backend = store();
    backend.fail_keep = true;
    let state = backend.state.clone();
    let (entered, blocked) = oneshot::channel();
    let (release, released) = mpsc::channel();
    backend.keep_gate = Some((entered, released));
    let (store, storage_task) = spawn_store(backend, StoreWorkerLimits::default()).unwrap();
    let records = Arc::new(Mutex::new(vec![]));
    let (recorder, writer) = spawn_recorder(
        LogSink {
            records: records.clone(),
            gate: None,
            fail_observation: false,
        },
        RecorderLimits::default(),
    )
    .unwrap();
    let (workspace, calls) = workspace(None, None);
    let (handle, task) = session::spawn_with_storage(
        workspace,
        RecordingMode::Required(CallJournal::new(
            recorder.clone(),
            RequiredPersistence::FileSynced,
        )),
        no_files(),
        NonZeroUsize::new(1).unwrap(),
        SessionStorage {
            worker: store.clone(),
            auto_keep: AutoKeep::Never,
        },
    )
    .unwrap();
    submit(&handle, "source", "catalog echo value:hello > answer").await;
    handle.wait_idle().await.unwrap();
    let (_, output) = current(&handle, "answer").await;
    let selected = output.stored.unwrap().handle;
    let caller = {
        let handle = handle.clone();
        let selected = selected.clone();
        tokio::spawn(async move { handle.keep(selected).await })
    };
    blocked.await.unwrap();
    caller.abort();
    submit(&handle, "drop", ":node remove $answer scope:downstream").await;
    handle.shutdown().await.unwrap();
    let joined = tokio::spawn(task.join());
    release.send(()).unwrap();
    joined.await.unwrap().unwrap();
    let recorded = records.lock().unwrap().clone();
    let notices: Vec<_> = recorded
        .iter()
        .filter_map(|record| {
            if let Record::Journal(JournalEntry::Noticed(notice)) = record {
                Some(notice)
            } else {
                None
            }
        })
        .collect();
    assert_eq!(notices.len(), 1);
    assert_eq!(
        notices[0].context(),
        &NoticeContext::Keep {
            handle: selected,
            may_have_applied: true
        }
    );
    assert!(!notices[0].error().message().contains("private"));
    assert_eq!(state.lock().unwrap().keeps, 1);
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    shutdown_store(store, storage_task).await;
    recorder.shutdown().await.unwrap();
    writer.join().await.unwrap();
}

#[tokio::test]
async fn eviction_read_failures_are_global_notices_without_tight_retry_or_node_failure() {
    use wes_engine::history::NoticeContext;
    let mut backend = store();
    backend.fail_evictions = true;
    let state = backend.state.clone();
    let (store, storage_task) = spawn_store(backend, StoreWorkerLimits::default()).unwrap();
    let (workspace, _) = workspace(None, None);
    let (handle, task) = session::spawn_with_storage(
        workspace,
        RecordingMode::Ephemeral,
        no_files(),
        NonZeroUsize::new(1).unwrap(),
        SessionStorage {
            worker: store.clone(),
            auto_keep: AutoKeep::Never,
        },
    )
    .unwrap();
    submit(&handle, "first", "catalog echo value:first > first").await;
    handle.wait_idle().await.unwrap();
    assert_eq!(state.lock().unwrap().eviction_reads, 1);
    let log = handle.log().await.unwrap();
    assert_eq!(log.entries.iter().filter(|entry| matches!(entry.entry(), JournalEntry::Noticed(notice) if notice.context() == &NoticeContext::Eviction)).count(), 1);
    assert!(handle.snapshot().await.unwrap().execution.errors.is_empty());
    submit(&handle, "second", "catalog echo value:second > second").await;
    handle.wait_idle().await.unwrap();
    assert_eq!(state.lock().unwrap().eviction_reads, 2);
    assert_eq!(handle.values().await.unwrap().unwrap().failures, 2);
    stop(handle, task).await;
    shutdown_store(store, storage_task).await;
}

struct ResultSink {
    state: Arc<Mutex<State>>,
    results: Arc<AtomicUsize>,
    gate: Option<Gate>,
    fail: bool,
}
impl JournalSink for ResultSink {
    fn append(&mut self, record: &Record) -> Result<AppendReceipt, RecordError> {
        if let Record::Journal(JournalEntry::Result(result)) = record {
            assert!(self.state.lock().unwrap().kept.contains(&result.handle));
            self.results.fetch_add(1, Ordering::SeqCst);
            if let Some((entered, released)) = self.gate.take() {
                let _ = entered.send(());
                released.recv_timeout(Duration::from_secs(5)).unwrap();
            }
            if self.fail {
                return Err(RecordError::backend(
                    "result",
                    true,
                    std::io::Error::other("private result journal detail"),
                ));
            }
        }
        Ok(AppendReceipt {
            persistence: Persistence::FileSynced,
            end_offset: 1,
        })
    }
}

#[tokio::test]
async fn failed_result_receipt_keeps_archive_and_success_but_never_retries_or_claims_recoverability()
 {
    let backend = store();
    let state = backend.state.clone();
    let (store, store_task) = spawn_store(backend, StoreWorkerLimits::default()).unwrap();
    let results = Arc::new(AtomicUsize::new(0));
    let (recorder, recorder_task) = spawn_recorder(
        ResultSink {
            state,
            results: results.clone(),
            gate: None,
            fail: true,
        },
        RecorderLimits::default(),
    )
    .unwrap();
    let (workspace, calls) = workspace(None, None);
    let (handle, task) = session::spawn_with_storage(
        workspace,
        RecordingMode::Required(CallJournal::new(
            recorder.clone(),
            RequiredPersistence::FileSynced,
        )),
        no_files(),
        NonZeroUsize::new(1).unwrap(),
        SessionStorage {
            worker: store.clone(),
            auto_keep: AutoKeep::default(),
        },
    )
    .unwrap();
    submit(&handle, "run", "catalog echo value:hello > answer").await;
    handle.wait_idle().await.unwrap();
    let values = handle.values().await.unwrap().unwrap();
    let ValuePublication::Complete(result) = values.outputs.values().next().unwrap() else {
        panic!("completed")
    };
    assert!(result.stored.as_ref().unwrap().kept);
    assert!(result.journal.is_none());
    assert!(result.problem.is_some());
    assert!(!result.durably_retained());
    assert!(!format!("{result:?}").contains("private result journal detail"));
    let snapshot = handle.snapshot().await.unwrap();
    assert!(snapshot.recording_blocked);
    assert_eq!(
        snapshot.execution.values[&snapshot.names["answer"].node].data(),
        &Data::Text("hello".into())
    );
    assert!(matches!(
        handle.submit(input("new", "catalog echo value:new")).await,
        Err(SessionError::Recording)
    ));
    assert_eq!(results.load(Ordering::SeqCst), 1);
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    stop(handle, task).await;
    shutdown_store(store, store_task).await;
    recorder.shutdown().await.unwrap();
    recorder_task.join().await.unwrap();
}

#[tokio::test]
async fn shutdown_joins_a_blocked_result_receipt_after_the_archive_operation_finished() {
    let backend = store();
    let state = backend.state.clone();
    let (store, store_task) = spawn_store(backend, StoreWorkerLimits::default()).unwrap();
    let results = Arc::new(AtomicUsize::new(0));
    let (entered, result_entered) = oneshot::channel();
    let (release, released) = mpsc::channel();
    let (recorder, recorder_task) = spawn_recorder(
        ResultSink {
            state: state.clone(),
            results: results.clone(),
            gate: Some((entered, released)),
            fail: false,
        },
        RecorderLimits::default(),
    )
    .unwrap();
    let (workspace, _) = workspace(None, None);
    let (handle, task) = session::spawn_with_storage(
        workspace,
        RecordingMode::Required(CallJournal::new(
            recorder.clone(),
            RequiredPersistence::FileSynced,
        )),
        no_files(),
        NonZeroUsize::new(1).unwrap(),
        SessionStorage {
            worker: store.clone(),
            auto_keep: AutoKeep::default(),
        },
    )
    .unwrap();
    submit(&handle, "run", "catalog echo value:hello > answer").await;
    result_entered.await.unwrap();
    assert_eq!(state.lock().unwrap().kept.len(), 1);
    assert!(matches!(
        handle
            .values()
            .await
            .unwrap()
            .unwrap()
            .outputs
            .values()
            .next()
            .unwrap(),
        ValuePublication::Pending { .. }
    ));
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
    assert_eq!(results.load(Ordering::SeqCst), 1);
    shutdown_store(store, store_task).await;
    recorder.shutdown().await.unwrap();
    recorder_task.join().await.unwrap();
}

#[tokio::test]
async fn slow_storage_has_bounded_pending_ownership_and_visible_refusal_without_failing_nodes() {
    let mut backend = store();
    let (entered, storage_entered) = oneshot::channel();
    let (release, released) = mpsc::channel();
    backend.gate = Some((entered, released));
    let (store, store_task) = spawn_store(backend, StoreWorkerLimits::default()).unwrap();
    let (workspace, calls) = workspace(None, None);
    let (handle, task) = session::spawn_with_storage(
        workspace,
        RecordingMode::Ephemeral,
        no_files(),
        NonZeroUsize::new(8).unwrap(),
        SessionStorage {
            worker: store.clone(),
            auto_keep: AutoKeep::Never,
        },
    )
    .unwrap();
    let text = (0..257)
        .map(|n| format!("catalog echo value:{n} > n{n}"))
        .collect::<Vec<_>>()
        .join("\n");
    submit(&handle, "many", &text).await;
    storage_entered.await.unwrap();
    let values = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let values = handle.values().await.unwrap().unwrap();
            if values.outputs.len() == 257 {
                break values;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    assert_eq!(values.pending, 256);
    assert_eq!(values.failures, 1);
    assert_eq!(calls.load(Ordering::SeqCst), 257);
    assert_eq!(handle.snapshot().await.unwrap().execution.values.len(), 257);
    release.send(()).unwrap();
    handle.wait_idle().await.unwrap();
    assert_eq!(handle.values().await.unwrap().unwrap().pending, 0);
    stop(handle, task).await;
    shutdown_store(store, store_task).await;
}

#[tokio::test]
async fn kept_result_is_recorded_after_archive_and_uses_the_exact_runtime_run() {
    let backend = store();
    let state = backend.state.clone();
    let (store, store_task) = spawn_store(backend, StoreWorkerLimits::default()).unwrap();
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
    let (workspace, calls) = workspace(None, None);
    let (handle, task) = session::spawn_with_storage(
        workspace,
        RecordingMode::Required(CallJournal::new(
            recorder.clone(),
            RequiredPersistence::FileSynced,
        )),
        no_files(),
        NonZeroUsize::new(1).unwrap(),
        SessionStorage {
            worker: store.clone(),
            auto_keep: AutoKeep::default(),
        },
    )
    .unwrap();
    let mut wakeups = handle.subscribe_values().unwrap().unwrap();
    submit(&handle, "run", "catalog echo value:hello > answer").await;
    handle.wait_idle().await.unwrap();
    let snapshot = handle.snapshot().await.unwrap();
    let values = handle.values().await.unwrap().unwrap();
    assert_eq!(values.pending, 0);
    assert_eq!(values.failures, 0);
    let node = &snapshot.names["answer"].node;
    let ValuePublication::Complete(published) = &values.outputs[node] else {
        panic!("stored output")
    };
    assert!(published.durably_retained());
    assert!(published.problem.is_none());
    let stored = published.stored.as_ref().unwrap();
    assert_eq!(stored.bytes, 128);
    assert!(state.lock().unwrap().kept.contains(&stored.handle));
    assert_eq!(
        store
            .read(stored.handle.clone())
            .await
            .unwrap()
            .unwrap()
            .value
            .data(),
        &Data::Text("hello".into())
    );
    let records = records.lock().unwrap().clone();
    assert!(records.iter().any(|record| matches!(record, Record::Journal(JournalEntry::Result(result)) if result.node == *node && result.run == published.run && result.handle == stored.handle)));
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    assert!(!matches!(
        wakeups.try_recv(),
        Err(tokio::sync::broadcast::error::TryRecvError::Empty)
    ));
    stop(handle, task).await;
    shutdown_store(store, store_task).await;
    recorder.shutdown().await.unwrap();
    recorder_task.join().await.unwrap();
}

#[tokio::test]
async fn slow_old_publication_never_overwrites_a_new_run_and_evictions_are_run_guarded() {
    let mut backend = store();
    backend.evict = true;
    let state = backend.state.clone();
    let (entered, storage_entered) = oneshot::channel();
    let (release, released) = mpsc::channel();
    backend.gate = Some((entered, released));
    let (store, store_task) = spawn_store(backend, StoreWorkerLimits::default()).unwrap();
    let (workspace, calls) = workspace(None, None);
    let (handle, task) = session::spawn_with_storage(
        workspace,
        RecordingMode::Ephemeral,
        no_files(),
        NonZeroUsize::new(2).unwrap(),
        SessionStorage {
            worker: store.clone(),
            auto_keep: AutoKeep::Never,
        },
    )
    .unwrap();
    submit(&handle, "first", "catalog echo value:old > answer").await;
    storage_entered.await.unwrap();
    let ValuePublication::Pending { run: old } = handle
        .values()
        .await
        .unwrap()
        .unwrap()
        .outputs
        .values()
        .next()
        .unwrap()
        .clone()
    else {
        panic!("pending first")
    };
    submit(&handle, "change", ":change $answer value:new").await;
    submit(&handle, "refresh", ":refresh $answer").await;
    let new = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let values = handle.values().await.unwrap().unwrap();
            if let Some(ValuePublication::Pending { run }) = values.outputs.values().next()
                && run != &old
            {
                break run.clone();
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    release.send(()).unwrap();
    handle.wait_idle().await.unwrap();
    let snapshot = handle.snapshot().await.unwrap();
    let node = &snapshot.names["answer"].node;
    let values = handle.values().await.unwrap().unwrap();
    let ValuePublication::Complete(result) = &values.outputs[node] else {
        panic!("latest stored output")
    };
    assert_eq!(&result.run, &new);
    assert_eq!(
        snapshot.execution.values[node].data(),
        &Data::Text("new".into())
    );
    assert_eq!(
        snapshot.execution.graph.node(node).unwrap().state(),
        NodeState::Ready
    );
    assert!(result.journal.is_none());
    assert!(!result.stored.as_ref().unwrap().kept);
    assert_eq!(state.lock().unwrap().order.len(), 2);
    assert_eq!(calls.load(Ordering::SeqCst), 2);
    // A subsequent node evicts the current handle, not an unrelated/newer run's value.
    submit(&handle, "other", "catalog echo value:other > other").await;
    handle.wait_idle().await.unwrap();
    let snapshot = handle.snapshot().await.unwrap();
    assert_eq!(
        snapshot.execution.graph.node(node).unwrap().state(),
        NodeState::Stale
    );
    assert!(!snapshot.execution.values.contains_key(node));
    assert!(
        !handle
            .values()
            .await
            .unwrap()
            .unwrap()
            .outputs
            .contains_key(node)
    );
    stop(handle, task).await;
    shutdown_store(store, store_task).await;
}

#[tokio::test]
async fn shutdown_joins_storage_but_dropped_nodes_are_not_resurrected_by_late_receipts() {
    let mut backend = store();
    let state = backend.state.clone();
    let (entered, storage_entered) = oneshot::channel();
    let (release, released) = mpsc::channel();
    backend.gate = Some((entered, released));
    let (store, store_task) = spawn_store(backend, StoreWorkerLimits::default()).unwrap();
    let (workspace, _) = workspace(None, None);
    let (handle, task) = session::spawn_with_storage(
        workspace,
        RecordingMode::Ephemeral,
        no_files(),
        NonZeroUsize::new(1).unwrap(),
        SessionStorage {
            worker: store.clone(),
            auto_keep: AutoKeep::Never,
        },
    )
    .unwrap();
    submit(&handle, "run", "catalog echo value:hello > answer").await;
    storage_entered.await.unwrap();
    submit(&handle, "drop", ":node remove $answer scope:downstream").await;
    assert!(handle.snapshot().await.unwrap().execution.graph.is_empty());
    assert!(handle.values().await.unwrap().unwrap().outputs.is_empty());
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
    assert_eq!(state.lock().unwrap().order.len(), 1); // No rollback or automatic destructive cleanup.
    shutdown_store(store, store_task).await;
}

#[tokio::test]
async fn failed_archive_preserves_the_result_and_possible_handle_without_leaking_backend_details() {
    let mut backend = store();
    backend.fail_keep = true;
    let state = backend.state.clone();
    let (store, store_task) = spawn_store(backend, StoreWorkerLimits::default()).unwrap();
    let (workspace, _) = workspace(None, None);
    let (handle, task) = session::spawn_with_storage(
        workspace,
        RecordingMode::Ephemeral,
        no_files(),
        NonZeroUsize::new(1).unwrap(),
        SessionStorage {
            worker: store.clone(),
            auto_keep: AutoKeep::default(),
        },
    )
    .unwrap();
    submit(&handle, "run", "catalog echo value:hello > answer").await;
    handle.wait_idle().await.unwrap();
    let snapshot = handle.snapshot().await.unwrap();
    let values = handle.values().await.unwrap().unwrap();
    let ValuePublication::Complete(result) = &values.outputs[&snapshot.names["answer"].node] else {
        panic!("completed publication")
    };
    assert!(result.stored.is_none());
    assert!(result.problem.is_some());
    assert!(
        state
            .lock()
            .unwrap()
            .values
            .contains_key(result.uncertain_handle.as_ref().unwrap())
    );
    assert_eq!(values.failures, 1);
    assert!(!format!("{values:?}").contains("private archive detail"));
    assert_eq!(
        snapshot.execution.values[&snapshot.names["answer"].node].data(),
        &Data::Text("hello".into())
    );
    stop(handle, task).await;
    shutdown_store(store, store_task).await;
}

#[tokio::test]
async fn volatile_retained_bytes_cannot_be_promoted_by_a_durable_journal() {
    let mut backend = store();
    backend.persistence = Persistence::Volatile;
    let (store, store_task) = spawn_store(backend, StoreWorkerLimits::default()).unwrap();
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
    let (workspace, _) = workspace(None, None);
    let (handle, task) = session::spawn_with_storage(
        workspace,
        RecordingMode::Required(CallJournal::new(
            recorder.clone(),
            RequiredPersistence::FileSynced,
        )),
        no_files(),
        NonZeroUsize::new(1).unwrap(),
        SessionStorage {
            worker: store.clone(),
            auto_keep: AutoKeep::default(),
        },
    )
    .unwrap();
    submit(&handle, "run", "catalog echo value:hello > answer").await;
    handle.wait_idle().await.unwrap();
    let values = handle.values().await.unwrap().unwrap();
    let ValuePublication::Complete(result) = values.outputs.values().next().unwrap() else {
        panic!("completed publication")
    };
    assert!(result.stored.as_ref().unwrap().kept);
    assert!(!result.durably_retained());
    assert!(result.problem.is_some());
    assert!(result.journal.is_none());
    assert!(handle.snapshot().await.unwrap().recording_blocked);
    assert!(
        !records
            .lock()
            .unwrap()
            .iter()
            .any(|record| matches!(record, Record::Journal(JournalEntry::Result(_))))
    );
    stop(handle, task).await;
    shutdown_store(store, store_task).await;
    recorder.shutdown().await.unwrap();
    recorder_task.join().await.unwrap();
}

#[path = "conversations.rs"]
mod conversations;
#[path = "streams.rs"]
mod streams;

async fn pin_fixture(
    backend: Store,
    result_gate: Option<Gate>,
    fail_result: bool,
) -> (
    SessionHandle,
    session::SessionTask,
    StoreWorker,
    wes_engine::storage::StoreWorkerTask,
    wes_engine::recording::Recorder,
    wes_engine::recording::RecorderTask,
    Arc<AtomicUsize>,
) {
    let state = backend.state.clone();
    let (store, store_task) = spawn_store(backend, StoreWorkerLimits::default()).unwrap();
    let (recorder, writer) = spawn_recorder(
        ResultSink {
            state,
            results: Arc::default(),
            gate: result_gate,
            fail: fail_result,
        },
        RecorderLimits::default(),
    )
    .unwrap();
    let (workspace, calls) = workspace(None, None);
    let (handle, task) = session::spawn_with_storage(
        workspace,
        RecordingMode::Required(CallJournal::new(
            recorder.clone(),
            RequiredPersistence::FileSynced,
        )),
        no_files(),
        NonZeroUsize::new(2).unwrap(),
        SessionStorage {
            worker: store.clone(),
            auto_keep: AutoKeep::Never,
        },
    )
    .unwrap();
    submit(&handle,"data",":calc { return {body:{view:\"metric\",value:7},metadata:\"not part of the input\"}; } > response").await;
    handle.wait_idle().await.unwrap();
    submit(
        &handle,
        "view",
        ":view create Metric input:$response.body > chart",
    )
    .await;
    handle.wait_idle().await.unwrap();
    (handle, task, store, store_task, recorder, writer, calls)
}

#[tokio::test]
async fn pin_waits_for_result_history_ack_and_retains_only_the_displayed_projection() {
    use wes_engine::{session::PinBinding, views::InputBinding};
    let backend = store();
    let state = backend.state.clone();
    let (entered, blocked) = oneshot::channel();
    let (release, released) = mpsc::channel();
    let (handle, task, store, storage_task, recorder, writer, calls) =
        pin_fixture(backend, Some((entered, released)), false).await;
    let view = handle.snapshot().await.unwrap().names["chart"].node.clone();
    let before = handle
        .view_frame(view.clone())
        .await
        .unwrap()
        .instances
        .remove(0);
    let command = format!(
        ":view pin $chart instance:{:?} revision:{} inputRevision:{} > pinned",
        before.identity, before.revision, before.input_revision
    );
    submit(&handle, "pin", &command).await;
    blocked.await.unwrap();
    let still_current = handle
        .view_frame(view.clone())
        .await
        .unwrap()
        .instances
        .remove(0);
    assert!(matches!(
        still_current.input.unwrap().binding,
        InputBinding::Current(_)
    ));
    assert_eq!(still_current.revision, before.revision);
    assert_eq!(state.lock().unwrap().keeps, 1);
    release.send(()).unwrap();
    handle.wait_idle().await.unwrap();
    let (node, published) = current(&handle, "pinned").await;
    assert!(published.durably_retained());
    assert!(matches!(published.pin, Some(PinBinding::Bound)));
    let saved = published.stored.as_ref().unwrap();
    assert_eq!(saved.retention, wes_engine::storage::Retention::Protected);
    let frame = handle.view_frame(view).await.unwrap().instances.remove(0);
    assert_eq!(frame.revision, before.revision + 1);
    assert!(!frame.observing);
    let input = frame.input.unwrap();
    let InputBinding::Retained(reference) = &input.binding else {
        panic!("retained input")
    };
    assert_eq!(reference.node(), &node);
    assert_eq!(reference.run(), &published.run);
    assert_eq!(reference.handle(), &saved.handle);
    assert_eq!(reference.origin().unwrap().fields, vec!["body"]);
    let value = store
        .read(saved.handle.clone())
        .await
        .unwrap()
        .unwrap()
        .value;
    assert_eq!(&value, input.value().unwrap());
    let Data::Record(fields) = value.data() else {
        panic!("projected record")
    };
    assert_eq!(fields["value"], Data::Int(7));
    assert!(!fields.contains_key("metadata"));
    assert_eq!(
        value.provenance(),
        before.input.as_ref().unwrap().value().unwrap().provenance()
    );
    assert!(handle.release(saved.handle.clone()).await.is_err());
    assert_eq!(state.lock().unwrap().releases, 0);
    assert_eq!(calls.load(Ordering::SeqCst), 0);
    stop(handle, task).await;
    recorder.shutdown().await.unwrap();
    writer.join().await.unwrap();
    shutdown_store(store, storage_task).await;
}

#[tokio::test]
async fn pin_keeps_its_input_without_overwriting_a_concurrent_rebind_or_revoked_grant() {
    use wes_engine::{session::PinBinding, views::InputBinding};
    for revoke in [false, true] {
        let mut backend = store();
        let state = backend.state.clone();
        let (entered, blocked) = oneshot::channel();
        let (release, released) = mpsc::channel();
        backend.keep_gate = Some((entered, released));
        let (handle, task, store, storage_task, recorder, writer, calls) =
            pin_fixture(backend, None, false).await;
        let view = handle.snapshot().await.unwrap().names["chart"].node.clone();
        if revoke {
            handle
                .observe_actor("reader".into(), "ui".into())
                .await
                .unwrap();
            let intro =
                SourceInput::new("actor".into(), ":calc { return 1; } > actor_owned".into())
                    .unwrap()
                    .with_client("reader".into())
                    .unwrap()
                    .cooperative();
            handle.submit(intro).await.unwrap();
            handle.wait_idle().await.unwrap();
            handle
                .grant_work("reader".into(), vec!["view".into()])
                .await
                .unwrap();
            let input = SourceInput::new("pin".into(), ":view pin $chart > pinned".into())
                .unwrap()
                .with_client("reader".into())
                .unwrap()
                .cooperative();
            handle.submit(input).await.unwrap();
        } else {
            submit(&handle, "pin", ":view pin $chart > pinned").await;
        }
        blocked.await.unwrap();
        if revoke {
            handle.grant_work("reader".into(), vec![]).await.unwrap();
        } else {
            submit(
                &handle,
                "edit",
                ":view bind $chart input:$response.body revision:0",
            )
            .await;
        }
        let expected = if revoke { 0 } else { 1 };
        tokio::time::timeout(Duration::from_secs(5), async {
            while handle.view_frame(view.clone()).await.unwrap().instances[0].revision != expected {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        release.send(()).unwrap();
        handle.wait_idle().await.unwrap();
        let (_, published) = current(&handle, "pinned").await;
        assert!(published.durably_retained());
        assert!(matches!(published.pin, Some(PinBinding::Refused(_))));
        let frame = handle.view_frame(view).await.unwrap().instances.remove(0);
        assert_eq!(frame.revision, expected);
        assert!(matches!(
            frame.input.unwrap().binding,
            InputBinding::Current(_)
        ));
        assert_eq!(state.lock().unwrap().keeps, 1);
        assert_eq!(calls.load(Ordering::SeqCst), 0);
        stop(handle, task).await;
        recorder.shutdown().await.unwrap();
        writer.join().await.unwrap();
        shutdown_store(store, storage_task).await;
    }
}

#[tokio::test]
async fn pin_publication_failure_never_rebinds_or_retries() {
    use wes_engine::{session::PinBinding, views::InputBinding};
    for fail_journal in [false, true] {
        let mut backend = store();
        backend.fail_keep = !fail_journal;
        let state = backend.state.clone();
        let (handle, task, store, storage_task, recorder, writer, calls) =
            pin_fixture(backend, None, fail_journal).await;
        let view = handle.snapshot().await.unwrap().names["chart"].node.clone();
        submit(&handle, "pin", ":view pin $chart > pinned").await;
        handle.wait_idle().await.unwrap();
        let (_, published) = current(&handle, "pinned").await;
        assert!(!published.durably_retained());
        assert!(published.problem.is_some());
        assert!(matches!(published.pin, Some(PinBinding::Refused(_))));
        let frame = handle.view_frame(view).await.unwrap().instances.remove(0);
        assert_eq!(frame.revision, 0);
        assert!(matches!(
            frame.input.unwrap().binding,
            InputBinding::Current(_)
        ));
        assert_eq!(state.lock().unwrap().keeps, 1);
        assert_eq!(calls.load(Ordering::SeqCst), 0);
        stop(handle, task).await;
        recorder.shutdown().await.unwrap();
        writer.join().await.unwrap();
        shutdown_store(store, storage_task).await;
    }
}

#[tokio::test]
async fn pin_refuses_mismatched_display_guards_before_storage_or_binding() {
    let backend = store();
    let state = backend.state.clone();
    let (handle, task, store, storage_task, recorder, writer, calls) =
        pin_fixture(backend, None, false).await;
    let view = handle.snapshot().await.unwrap().names["chart"].node.clone();
    for (index, guard) in ["instance:\"different\"", "revision:99", "inputRevision:99"]
        .into_iter()
        .enumerate()
    {
        let reply = submit(
            &handle,
            &format!("pin{index}"),
            &format!(":view pin $chart {guard} > pinned{index}"),
        )
        .await;
        handle.wait_idle().await.unwrap();
        assert!(
            handle
                .snapshot()
                .await
                .unwrap()
                .execution
                .errors
                .contains_key(&reply.nodes[0])
        );
    }
    assert_eq!(
        handle.view_frame(view).await.unwrap().instances[0].revision,
        0
    );
    assert_eq!(state.lock().unwrap().keeps, 0);
    assert_eq!(calls.load(Ordering::SeqCst), 0);
    stop(handle, task).await;
    recorder.shutdown().await.unwrap();
    writer.join().await.unwrap();
    shutdown_store(store, storage_task).await;
}

#[tokio::test]
async fn pin_refuses_ephemeral_recording_before_new_storage_or_bind() {
    let backend = store();
    let state = backend.state.clone();
    let (store, storage_task) = spawn_store(backend, StoreWorkerLimits::default()).unwrap();
    let (workspace, calls) = workspace(None, None);
    let (handle, task) = session::spawn_with_storage(
        workspace,
        RecordingMode::Ephemeral,
        no_files(),
        NonZeroUsize::new(1).unwrap(),
        SessionStorage {
            worker: store.clone(),
            auto_keep: AutoKeep::Never,
        },
    )
    .unwrap();
    submit(
        &handle,
        "source",
        ":calc { return {view:\"metric\",value:1}; } > sample",
    )
    .await;
    handle.wait_idle().await.unwrap();
    submit(&handle, "view", ":view create Metric input:$sample > chart").await;
    handle.wait_idle().await.unwrap();
    let before = state.lock().unwrap().order.len();
    let reply = submit(&handle, "pin", ":view pin $chart > pinned").await;
    handle.wait_idle().await.unwrap();
    let snapshot = handle.snapshot().await.unwrap();
    assert!(
        snapshot.execution.errors[&reply.nodes[0]]
            .message()
            .contains("durable workspace recording")
    );
    assert_eq!(state.lock().unwrap().order.len(), before);
    assert_eq!(state.lock().unwrap().keeps, 0);
    assert_eq!(
        handle
            .view_frame(snapshot.names["chart"].node.clone())
            .await
            .unwrap()
            .instances[0]
            .revision,
        0
    );
    assert_eq!(calls.load(Ordering::SeqCst), 0);
    stop(handle, task).await;
    shutdown_store(store, storage_task).await;
}
