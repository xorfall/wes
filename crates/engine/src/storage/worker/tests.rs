use super::*;
use std::{
    collections::{HashMap, HashSet},
    sync::{
        Mutex,
        atomic::{AtomicBool, Ordering},
        mpsc as sync_mpsc,
    },
    time::Duration,
};
use wes_core::{Data, Provenance, Shape};

#[derive(Default)]
struct State {
    values: HashMap<ValueHandle, Value>,
    kept: HashSet<ValueHandle>,
    evicted: Vec<ValueHandle>,
    calls: usize,
    encoded_bytes: usize,
    fail_size: bool,
    reject_keep: bool,
}
type Gate = (oneshot::Sender<()>, sync_mpsc::Receiver<()>);
struct Memory {
    state: Arc<Mutex<State>>,
    dropped: Arc<AtomicBool>,
    gate: Option<Gate>,
    panic: bool,
    published_error: bool,
}
impl Drop for Memory {
    fn drop(&mut self) {
        self.dropped.store(true, Ordering::SeqCst);
    }
}
fn memory() -> Memory {
    Memory {
        state: Arc::default(),
        dropped: Arc::new(AtomicBool::new(false)),
        gate: None,
        panic: false,
        published_error: false,
    }
}
fn value(text: &str) -> Value {
    Value::new(
        Shape::Unknown,
        Data::Text(text.into()),
        Provenance::default(),
    )
    .unwrap()
}
#[tokio::test]
async fn recovery_requires_retention_and_bounds_decoded_replies() {
    let backend = memory();
    let state = backend.state.clone();
    let small = ValueHandle::fresh();
    let large = ValueHandle::fresh();
    {
        let mut state = state.lock().unwrap();
        state.values.insert(small.clone(), value("small"));
        state
            .values
            .insert(large.clone(), value(&"x".repeat(10_000)));
        state.kept.insert(large.clone());
    }
    let (store, task) = spawn_store(
        backend,
        StoreWorkerLimits {
            bytes: NonZeroU32::new(4096).unwrap(),
            ..StoreWorkerLimits::default()
        },
    )
    .unwrap();
    assert!(store.recover(small.clone()).await.unwrap().is_none());
    state.lock().unwrap().kept.insert(small.clone());
    let recovered = store.recover(small).await.unwrap().unwrap();
    assert_eq!(recovered.loaded.value, value("small"));

    assert!(matches!(
        store.recover(large).await,
        Err(StoreError::Limit(_))
    ));
    assert_eq!(state.lock().unwrap().calls, 0);
    store.shutdown().await.unwrap();
    task.join().await.unwrap();
}
#[tokio::test]
async fn retaining_an_existing_handle_never_republishes_and_preserves_partial_failure_identity() {
    let backend = memory();
    let state = backend.state.clone();
    let handle = ValueHandle::fresh();
    state
        .lock()
        .unwrap()
        .values
        .insert(handle.clone(), value("existing"));
    state.lock().unwrap().encoded_bytes = 123;
    let (store, task) = spawn_store(backend, StoreWorkerLimits::default()).unwrap();
    let retained = store.retain(handle.clone()).await.unwrap();
    assert_eq!(retained.handle, handle);
    assert_eq!(retained.bytes, 123);
    assert!(retained.kept);
    assert_eq!(
        retained.retained_persistence,
        crate::history::Persistence::Volatile
    );
    state.lock().unwrap().fail_size = true;
    assert!(
        matches!(store.retain(handle.clone()).await, Err(StoreError::Published { handle: failed, .. }) if failed == handle)
    );
    assert_eq!(state.lock().unwrap().calls, 0);
    assert!(state.lock().unwrap().kept.contains(&handle));
    store.shutdown().await.unwrap();
    task.join().await.unwrap();
}
impl ValueStore for Memory {
    fn store(&mut self, value: &Value) -> Result<ValueHandle, StoreError> {
        self.state.lock().unwrap().calls += 1;
        if let Some((entered, release)) = self.gate.take() {
            entered.send(()).unwrap();
            release.recv_timeout(Duration::from_secs(5)).unwrap();
        }
        assert!(!self.panic, "synthetic private backend payload");
        let handle = ValueHandle::fresh();
        self.state
            .lock()
            .unwrap()
            .values
            .insert(handle.clone(), value.clone());
        if self.published_error {
            return Err(StoreError::Published {
                handle,
                source: Box::new(std::io::Error::other("synthetic private sync detail")),
            });
        }
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
        Ok(Some(vec![1; self.state.lock().unwrap().encoded_bytes]))
    }
    fn size(&self, _: &ValueHandle) -> Result<Option<u64>, StoreError> {
        let state = self.state.lock().unwrap();
        if state.fail_size {
            return Err(StoreError::backend(
                "size",
                std::io::Error::other("synthetic private metadata"),
            ));
        }
        Ok(Some(state.encoded_bytes as u64))
    }
    fn release(&mut self, handle: &ValueHandle) -> Result<bool, StoreError> {
        let mut state = self.state.lock().unwrap();
        state.kept.remove(handle);
        Ok(state.values.remove(handle).is_some())
    }
    fn keep(&mut self, handle: &ValueHandle) -> Result<bool, StoreError> {
        let mut state = self.state.lock().unwrap();
        if state.reject_keep || !state.values.contains_key(handle) {
            return Ok(false);
        }
        state.kept.insert(handle.clone());
        Ok(true)
    }
    fn is_kept(&self, handle: &ValueHandle) -> Result<bool, StoreError> {
        Ok(self.state.lock().unwrap().kept.contains(handle))
    }
    fn take_evicted(&mut self, maximum: usize) -> Result<EvictionBatch, StoreError> {
        let mut state = self.state.lock().unwrap();
        let count = maximum.min(state.evicted.len());
        let handles = state.evicted.drain(..count).collect();
        Ok(EvictionBatch {
            handles,
            more: !state.evicted.is_empty(),
        })
    }
}

#[tokio::test]
async fn publication_uses_encoded_size_and_explicit_keep_policy_without_claiming_memory_durability()
{
    let backend = memory();
    backend.state.lock().unwrap().encoded_bytes = 128;
    let (store, task) = spawn_store(backend, StoreWorkerLimits::default()).unwrap();
    for (policy, kept) in [
        (AutoKeep::Never, false),
        (AutoKeep::UpToBytes(127), false),
        (AutoKeep::UpToBytes(128), true),
        (AutoKeep::default(), true),
    ] {
        let output = store.publish(value("x"), policy.into()).await.unwrap();
        assert_eq!(output.bytes, 128);
        assert_eq!(output.kept, kept);
        assert_eq!(store.is_kept(output.handle).await.unwrap(), kept);
        assert_eq!(
            output.retained_persistence,
            crate::history::Persistence::Volatile
        );
    }
    store.shutdown().await.unwrap();
    task.join().await.unwrap();
}

#[tokio::test]
async fn metadata_and_keep_failures_preserve_the_already_published_handle_without_retry() {
    for size_failure in [false, true] {
        let backend = memory();
        let state = backend.state.clone();
        {
            let mut state = state.lock().unwrap();
            state.fail_size = size_failure;
            state.reject_keep = !size_failure;
        }
        let (store, task) = spawn_store(backend, StoreWorkerLimits::default()).unwrap();
        let Err(StoreError::Published { handle, .. }) = store
            .publish(value("hello"), AutoKeep::default().into())
            .await
        else {
            panic!("preserved publication")
        };
        assert_eq!(state.lock().unwrap().calls, 1);
        assert_eq!(
            store.read(handle).await.unwrap().unwrap().value,
            value("hello")
        );
        store.shutdown().await.unwrap();
        task.join().await.unwrap();
    }
}

#[tokio::test]
async fn public_operations_are_serial_and_shutdown_releases_the_owned_store() {
    let memory = memory();
    let state = memory.state.clone();
    let dropped = memory.dropped.clone();
    let (store, task) = spawn_store(memory, StoreWorkerLimits::default()).unwrap();
    let stored = store.store(value("hello")).await.unwrap();
    let loaded = store.read(stored.clone()).await.unwrap().unwrap();
    assert_eq!(loaded.value, value("hello"));

    assert!(store.keep(stored.clone()).await.unwrap());
    assert!(store.is_kept(stored.clone()).await.unwrap());
    state.lock().unwrap().encoded_bytes = 3;
    assert_eq!(
        store.encoded(stored.clone()).await.unwrap(),
        Some(vec![1, 1, 1])
    );
    assert_eq!(store.size(stored.clone()).await.unwrap(), Some(3));
    let first = ValueHandle::fresh();
    let second = ValueHandle::fresh();
    state.lock().unwrap().evicted = vec![first.clone(), second.clone()];
    let batch = store
        .take_evicted(NonZeroUsize::new(1).unwrap())
        .await
        .unwrap();
    assert_eq!(batch.handles, [first]);
    assert!(batch.more);
    let batch = store
        .take_evicted(NonZeroUsize::new(1).unwrap())
        .await
        .unwrap();
    assert_eq!(batch.handles, [second]);
    assert!(!batch.more);
    assert!(store.release(stored.clone()).await.unwrap());
    assert!(store.read(stored.clone()).await.unwrap().is_none());
    assert!(!store.is_kept(stored).await.unwrap());
    let report = store.shutdown().await.unwrap();
    assert_eq!(report.failed, 0);
    assert_eq!(report.attempted, 11);
    assert!(dropped.load(Ordering::SeqCst));
    task.join().await.unwrap();
    assert!(matches!(
        store.store(value("late")).await,
        Err(StoreError::Closed)
    ));
}

#[tokio::test]
async fn blocked_storage_does_not_block_timers_and_dropped_receipts_do_not_retract_writes() {
    let mut memory = memory();
    let state = memory.state.clone();
    let dropped = memory.dropped.clone();
    let (entered, entered_rx) = oneshot::channel();
    let (release, release_rx) = sync_mpsc::channel();
    memory.gate = Some((entered, release_rx));
    let (store, task) = spawn_store(memory, StoreWorkerLimits::default()).unwrap();
    let first = store.enqueue_store(value("first")).await.unwrap();
    tokio::time::timeout(Duration::from_secs(5), entered_rx)
        .await
        .unwrap()
        .unwrap();
    let second = store.enqueue_store(value("second")).await.unwrap();
    drop(first);
    drop(second);
    drop(store);
    let join = tokio::spawn(task.join());
    tokio::time::timeout(
        Duration::from_secs(1),
        tokio::time::sleep(Duration::from_millis(10)),
    )
    .await
    .unwrap();
    assert!(!join.is_finished());
    assert!(!dropped.load(Ordering::SeqCst));
    release.send(()).unwrap();
    join.await.unwrap().unwrap();
    assert!(dropped.load(Ordering::SeqCst));
    assert_eq!(state.lock().unwrap().values.len(), 2);
}

#[tokio::test]
async fn reply_credit_survives_completion_and_shutdown_wakes_unadmitted_waiters() {
    let payload = value("hello");
    let cost = value_charge(&payload, u64::MAX).unwrap() as u32;
    let (store, task) = spawn_store(
        memory(),
        StoreWorkerLimits {
            operations: NonZeroUsize::new(2).unwrap(),
            bytes: NonZeroU32::new(cost).unwrap(),
        },
    )
    .unwrap();
    let first = store.enqueue_store(payload.clone()).await.unwrap();
    assert_eq!(store.flush().await.unwrap().attempted, 1);
    assert_eq!(store.budget.available_permits(), 0); // the unread reply still owns its credit
    let s = store.clone();
    let waiting = tokio::spawn(async move { s.store(payload).await });
    tokio::task::yield_now().await;
    assert!(!waiting.is_finished());
    assert_eq!(store.shutdown().await.unwrap().attempted, 1);
    task.join().await.unwrap();
    assert!(matches!(waiting.await.unwrap(), Err(StoreError::Closed)));
    first.wait().await.unwrap();
}

#[tokio::test]
async fn cancelled_admission_does_not_mutate_and_rejected_payloads_never_enter_the_store() {
    let memory = memory();
    let state = memory.state.clone();
    let payload = value("one");
    let cost = value_charge(&payload, u64::MAX).unwrap() as u32;
    let (store, task) = spawn_store(
        memory,
        StoreWorkerLimits {
            operations: NonZeroUsize::new(2).unwrap(),
            bytes: NonZeroU32::new(cost).unwrap(),
        },
    )
    .unwrap();
    let first = store.enqueue_store(payload.clone()).await.unwrap();
    store.flush().await.unwrap();
    let s = store.clone();
    let waiting = tokio::spawn(async move { s.store(payload).await });
    tokio::task::yield_now().await;
    waiting.abort();
    assert!(waiting.await.unwrap_err().is_cancelled());
    first.wait().await.unwrap();
    assert!(matches!(
        store.store(value(&"x".repeat(1000))).await,
        Err(StoreError::Limit(_))
    ));
    assert_eq!(store.flush().await.unwrap().attempted, 1);
    assert_eq!(state.lock().unwrap().calls, 1);
    store.shutdown().await.unwrap();
    task.join().await.unwrap();
}

#[tokio::test]
async fn publication_uncertainty_preserves_its_handle_and_is_never_automatically_retried() {
    let mut memory = memory();
    memory.published_error = true;
    let state = memory.state.clone();
    let (store, task) = spawn_store(memory, StoreWorkerLimits::default()).unwrap();
    let error = store.store(value("exists")).await.unwrap_err();
    assert!(!error.to_string().contains("private"));
    let StoreError::Published { handle, .. } = error else {
        panic!("publication identity")
    };
    assert_eq!(
        store.read(handle).await.unwrap().unwrap().value,
        value("exists")
    );
    assert_eq!(state.lock().unwrap().calls, 1);
    let report = store.shutdown().await.unwrap();
    assert_eq!(report.attempted, 2);
    assert_eq!(report.failed, 1);
    task.join().await.unwrap();
}

#[tokio::test]
async fn panicked_storage_closes_receipts_and_releases_ownership_without_exposing_panic_text() {
    let mut memory = memory();
    memory.panic = true;
    let dropped = memory.dropped.clone();
    let (store, task) = spawn_store(memory, StoreWorkerLimits::default()).unwrap();
    let error = store.store(value("input")).await.unwrap_err();
    assert!(matches!(error, StoreError::Closed));
    assert!(!error.to_string().contains("private"));
    assert!(matches!(task.join().await, Err(StoreError::Closed)));
    assert!(dropped.load(Ordering::SeqCst));
    assert!(matches!(store.flush().await, Err(StoreError::Closed)));
}

#[tokio::test]
async fn responses_and_platform_capacities_are_checked_without_consuming_eviction_notifications() {
    let memory = memory();
    let state = memory.state.clone();
    let handle = ValueHandle::fresh();
    state
        .lock()
        .unwrap()
        .values
        .insert(handle.clone(), value(&"x".repeat(4096)));
    state.lock().unwrap().encoded_bytes = 4096;
    let eviction = ValueHandle::fresh();
    state.lock().unwrap().evicted.push(eviction.clone());
    let (store, task) = spawn_store(
        memory,
        StoreWorkerLimits {
            operations: NonZeroUsize::new(2).unwrap(),
            bytes: NonZeroU32::new(1024).unwrap(),
        },
    )
    .unwrap();
    assert!(matches!(
        store.read(handle.clone()).await,
        Err(StoreError::Limit(_))
    ));
    assert!(matches!(
        store.encoded(handle).await,
        Err(StoreError::Limit(_))
    ));
    assert!(matches!(
        store.take_evicted(NonZeroUsize::new(100).unwrap()).await,
        Err(StoreError::Limit(_))
    ));
    assert_eq!(
        store
            .take_evicted(NonZeroUsize::new(1).unwrap())
            .await
            .unwrap()
            .handles,
        [eviction]
    );
    store.shutdown().await.unwrap();
    task.join().await.unwrap();
    assert!(matches!(
        spawn_store(
            self::memory(),
            StoreWorkerLimits {
                operations: NonZeroUsize::new(usize::MAX).unwrap(),
                ..StoreWorkerLimits::default()
            }
        ),
        Err(StoreError::Limit(_))
    ));
}

#[tokio::test]
async fn shutdown_drains_operations_admitted_behind_its_message_before_releasing_the_store() {
    let mut memory = memory();
    let state = memory.state.clone();
    let dropped = memory.dropped.clone();
    let (entered, entered_rx) = oneshot::channel();
    let (release, release_rx) = sync_mpsc::channel();
    memory.gate = Some((entered, release_rx));
    let (store, task) = spawn_store(memory, StoreWorkerLimits::default()).unwrap();
    let first = store.enqueue_store(value("first")).await.unwrap();
    entered_rx.await.unwrap();
    let (shutdown, receive) = oneshot::channel();
    store
        .sender
        .send(Request::Shutdown(shutdown))
        .await
        .unwrap_or_else(|_| panic!("shutdown admitted"));
    let second = store.enqueue_store(value("second")).await.unwrap();
    assert!(!dropped.load(Ordering::SeqCst));
    release.send(()).unwrap();
    let report = receive.await.unwrap();
    assert_eq!(report.attempted, 2);
    assert_eq!(report.failed, 0);
    assert!(dropped.load(Ordering::SeqCst));
    first.wait().await.unwrap();
    second.wait().await.unwrap();
    assert_eq!(state.lock().unwrap().values.len(), 2);
    task.join().await.unwrap();
}

#[tokio::test]
async fn iter_requires_explicit_keep_and_owns_its_source() {
    use wes_core::{IterMode, IterValue};
    let (store, task) = spawn_store(memory(), StoreWorkerLimits::default()).unwrap();
    let source = value("source snapshot");
    let iter = IterValue::new(source, IterMode::Lines, None, vec![]).unwrap();
    let v = Value::new(
        Shape::Unknown,
        Data::Iter(Arc::new(iter)),
        Provenance::default(),
    )
    .unwrap();
    let published = store
        .publish(v.clone(), AutoKeep::default().into())
        .await
        .unwrap();
    assert!(!published.kept);
    assert!(!store.is_kept(published.handle.clone()).await.unwrap());
    store.keep(published.handle.clone()).await.unwrap();
    assert!(store.is_kept(published.handle.clone()).await.unwrap());
    assert_eq!(
        store
            .recover(published.handle)
            .await
            .unwrap()
            .unwrap()
            .loaded
            .value,
        v
    );
    store.shutdown().await.unwrap();
    task.join().await.unwrap();
}

#[tokio::test]
async fn protected_publication_refuses_private_data_before_storage_and_keeps_partial_identity() {
    let backend = memory();
    let state = backend.state.clone();
    let (store, task) = spawn_store(backend, StoreWorkerLimits::default()).unwrap();
    let private = value("restricted").with_provenance(
        Provenance::default().with_policy(&wes_core::flow::FlowPolicy::default().private()),
    );
    assert!(matches!(
        store.publish(private, PublicationPolicy::Protected).await,
        Err(StoreError::Restricted)
    ));
    assert_eq!(state.lock().unwrap().calls, 0);
    state.lock().unwrap().reject_keep = true;
    let error = store
        .publish(value("selected field"), PublicationPolicy::Protected)
        .await
        .unwrap_err();
    let StoreError::Published { handle, .. } = error else {
        panic!("published ownership must survive failed retention")
    };
    assert_eq!(
        store.read(handle).await.unwrap().unwrap().value,
        value("selected field")
    );
    assert_eq!(state.lock().unwrap().calls, 1);
    store.shutdown().await.unwrap();
    task.join().await.unwrap();
}
