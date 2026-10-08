//! Synthetic source, real shared storage, independent recording lifetime.
use std::{
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};
use tokio::sync::{mpsc, oneshot, watch};
use wes_adapters::{
    datasets::{DatasetStore, StoreLimits},
    storage::{Durability, FileValues},
};
use wes_core::{
    Data, Primitive, Provenance, Shape, Value,
    capability::{Capability, Safety},
    contracts::{ContractRegistry, ResolvedContractBundle},
    flow::FlowPolicy,
};
use wes_engine::{
    driver::CancellationToken,
    eventlog,
    providers::{Call, InvocationError},
    runtime::{Effect, ExecutionTraits, Runtime},
    storage::{
        StoreWorker, StoreWorkerLimits,
        datasets::{DatasetLifecycle, PageRequest, RecordingEnd},
        spawn_storage,
    },
    streams::{self, StreamFuture, StreamSink, StreamingInvoker},
};

fn home() -> tempfile::TempDir {
    let mut builder = tempfile::Builder::new();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        builder.permissions(std::fs::Permissions::from_mode(0o700));
    }
    builder.tempdir().unwrap()
}
fn value(n: i64) -> Value {
    Value::new(
        Shape::Primitive(Primitive::Int),
        Data::Int(n),
        Provenance::default(),
    )
    .unwrap()
}
fn call() -> Call {
    let mut runtime = Runtime::new();
    runtime
        .add(
            (),
            vec![],
            ExecutionTraits {
                pure: false,
                repeatable: false,
                bounded: true,
            },
        )
        .unwrap();
    let run = runtime
        .start(Duration::ZERO)
        .into_iter()
        .find_map(|effect| match effect {
            Effect::Spawn(ticket) => Some(ticket.run),
            _ => None,
        })
        .unwrap();
    let mut cap = Capability::new(
        ["synthetic-events"],
        Shape::Primitive(Primitive::Int),
        Safety::Safe,
    );
    cap.streaming = true;
    Call {
        authority: Default::default(),
        run,
        capability: Arc::new(cap),
        arguments: Default::default(),
    }
}
fn schema() -> ResolvedContractBundle {
    ResolvedContractBundle::capture(
        ContractRegistry::new().resolve("Int").unwrap(),
        Default::default(),
    )
    .unwrap()
}
fn worker(home: &tempfile::TempDir) -> (StoreWorker, wes_engine::storage::StoreWorkerTask) {
    let datasets = DatasetStore::open(
        &home.path().join("datasets"),
        Durability::FileAndDirectory,
        StoreLimits::default(),
    )
    .unwrap();
    let values = FileValues::open(
        &home.path().join("values"),
        Default::default(),
        Durability::FileAndDirectory,
        None,
    )
    .unwrap();
    spawn_storage(values, datasets, StoreWorkerLimits::default()).unwrap()
}
struct Source {
    entered: mpsc::UnboundedSender<(StreamSink, oneshot::Sender<()>)>,
    calls: Arc<AtomicUsize>,
}
impl StreamingInvoker for Source {
    fn subscribe(&self, _: Call, sink: StreamSink, token: CancellationToken) -> StreamFuture {
        self.calls.fetch_add(1, Ordering::SeqCst);
        sink.opened().unwrap();
        let (finish, receive) = oneshot::channel();
        self.entered.send((sink, finish)).unwrap();
        Box::pin(async move {
            tokio::select! { _ = token.cancelled() => Err(InvocationError::Cancelled), _ = receive => Ok(()) }
        })
    }
}
async fn until(
    updates: &mut watch::Receiver<eventlog::Status>,
    predicate: impl Fn(&eventlog::Status) -> bool,
) -> eventlog::Status {
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            let current = updates.borrow_and_update().clone();
            if predicate(&current) {
                return current;
            }
            updates.changed().await.unwrap();
        }
    })
    .await
    .expect("recording must reach bounded acknowledgement")
}

#[tokio::test]
async fn single_event_flushes_and_600_events_survive_window_eviction_without_second_producer() {
    let home = home();
    let (store, storage) = worker(&home);
    let call = call();
    let (prepared, archive) = eventlog::Prepared::prepare(
        store.clone(),
        call.run.id().to_string(),
        uuid::Uuid::new_v4().to_string(),
        schema(),
        FlowPolicy::default(),
        Default::default(),
        1,
    )
    .await
    .unwrap();
    let (entered, mut entries) = mpsc::unbounded_channel();
    let calls = Arc::new(AtomicUsize::new(0));
    let (stream, task) = eventlog::spawn(
        call,
        Arc::new(Source {
            entered,
            calls: calls.clone(),
        }),
        Default::default(),
        Default::default(),
        CancellationToken::new(),
        prepared,
    )
    .unwrap();
    let (sink, finish) = entries.recv().await.unwrap();
    let mut updates = archive.subscribe();
    for ordinal in 1..=600 {
        sink.send(value(ordinal)).await.unwrap();
        until(&mut updates, |s| s.reference.records() == ordinal as u64).await;
    }
    finish.send(()).unwrap();
    task.join().await.unwrap();
    let final_ = archive.snapshot();
    assert_eq!(final_.phase, eventlog::Phase::Stopped);
    assert_eq!(final_.coverage.termination, Some(RecordingEnd::Natural));
    assert_eq!(final_.coverage.pending, Some(0));
    assert_eq!(final_.reference.records(), 600);
    assert_eq!(stream.snapshot().omitted, 100);
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    let page = store
        .dataset_page(
            final_.reference.clone(),
            PageRequest {
                from: 0,
                rows: 1,
                bytes: 65536,
                segments: 1,
                work: None,
            },
        )
        .await
        .unwrap();
    assert_eq!(page.rows[0].value.data(), &Data::Int(1));
    drop(page);
    drop(store);
    storage.join().await.unwrap();
}

#[tokio::test]
async fn stop_recording_drains_accepted_boundary_and_source_continues() {
    let home = home();
    let (store, storage) = worker(&home);
    let call = call();
    let (prepared, archive) = eventlog::Prepared::prepare(
        store.clone(),
        call.run.id().to_string(),
        uuid::Uuid::new_v4().to_string(),
        schema(),
        FlowPolicy::default(),
        Default::default(),
        1,
    )
    .await
    .unwrap();
    let (entered, mut entries) = mpsc::unbounded_channel();
    let calls = Arc::new(AtomicUsize::new(0));
    let (stream, task) = eventlog::spawn(
        call,
        Arc::new(Source { entered, calls }),
        Default::default(),
        Default::default(),
        CancellationToken::new(),
        prepared,
    )
    .unwrap();
    let (sink, finish) = entries.recv().await.unwrap();
    let mut updates = archive.subscribe();
    sink.send(value(1)).await.unwrap();
    until(&mut updates, |s| s.reference.records() == 1).await;
    archive.stop();
    archive.stop();
    let stopped = until(&mut updates, |s| s.phase == eventlog::Phase::Stopped).await;
    assert_eq!(stopped.coverage.termination, Some(RecordingEnd::Manual));
    sink.send(value(2)).await.unwrap();
    assert!(!matches!(
        stream.snapshot().phase,
        streams::Phase::Cancelled | streams::Phase::Failed(_)
    ));
    finish.send(()).unwrap();
    task.join().await.unwrap();
    assert_eq!(archive.snapshot().reference.records(), 1);
    let info = store
        .dataset_inspect(stopped.reference.clone())
        .await
        .unwrap();
    assert_eq!(info.lifecycle, DatasetLifecycle::Sealed);
    assert!(
        info.protected,
        "Record protects the acknowledged terminal prefix"
    );
    let plan = store
        .dataset_plan_delete(stopped.reference.clone())
        .await
        .unwrap();
    assert!(!plan.active_writer);
    assert_eq!(plan.references.len(), 1);
    assert_eq!(
        plan.references[0].kind,
        wes_engine::storage::datasets::DatasetRootKind::Recording
    );
    assert!(store.dataset_delete(plan.token, true, false).await.is_err());
    drop(store);
    storage.join().await.unwrap();
    let (store, storage) = worker(&home);
    assert!(
        store
            .dataset_inspect(stopped.reference.clone())
            .await
            .unwrap()
            .protected
    );
    assert!(store.dataset_collect().await.unwrap().complete);
    let page = store
        .dataset_page(
            stopped.reference.clone(),
            PageRequest {
                from: 0,
                rows: 1,
                bytes: 65536,
                segments: 1,
                work: None,
            },
        )
        .await
        .unwrap();
    assert_eq!(page.rows[0].value.data(), &Data::Int(1));
    drop(page);
    let plan = store
        .dataset_plan_delete(stopped.reference.clone())
        .await
        .unwrap();
    assert!(store.dataset_delete(plan.token, true, true).await.is_ok());
    assert!(store.dataset_inspect(stopped.reference).await.is_err());
    drop(store);
    storage.join().await.unwrap();
}

#[tokio::test]
async fn attach_next_reports_actual_sequence_and_does_not_copy_old_window() {
    let home = home();
    let (store, storage) = worker(&home);
    let (entered, mut entries) = mpsc::unbounded_channel();
    let calls = Arc::new(AtomicUsize::new(0));
    let (stream, task) = streams::spawn(
        call(),
        Arc::new(Source {
            entered,
            calls: calls.clone(),
        }),
        Default::default(),
        Default::default(),
        CancellationToken::new(),
    )
    .unwrap();
    let (sink, finish) = entries.recv().await.unwrap();
    sink.send(value(1)).await.unwrap();
    sink.send(value(2)).await.unwrap();
    let (prepared, archive) = eventlog::Prepared::from_next(
        store.clone(),
        &stream,
        uuid::Uuid::new_v4().to_string(),
        schema(),
        FlowPolicy::default(),
        Default::default(),
    )
    .await
    .unwrap();
    let writer = prepared.start().unwrap();
    let mut updates = archive.subscribe();
    sink.send(value(3)).await.unwrap();
    until(&mut updates, |s| s.reference.records() == 1).await;
    finish.send(()).unwrap();
    task.join().await.unwrap();
    let final_ = writer.join().await.unwrap();
    assert_eq!(final_.coverage.first, 3);
    assert_eq!(final_.coverage.committed_through, 3);
    assert_eq!(final_.reference.records(), 1);
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    drop(store);
    storage.join().await.unwrap();
}

#[tokio::test]
async fn archive_limit_and_private_escalation_never_cancel_the_shared_source() {
    for private in [false, true] {
        let home = home();
        let (store, storage) = worker(&home);
        let call = call();
        let limits = eventlog::Limits {
            records: 1,
            bytes: 1024 * 1024,
            ..Default::default()
        };
        let (prepared, archive) = eventlog::Prepared::prepare(
            store.clone(),
            call.run.id().to_string(),
            uuid::Uuid::new_v4().to_string(),
            schema(),
            FlowPolicy::default(),
            limits,
            1,
        )
        .await
        .unwrap();
        let (entered, mut entries) = mpsc::unbounded_channel();
        let calls = Arc::new(AtomicUsize::new(0));
        let (_stream, task) = eventlog::spawn(
            call,
            Arc::new(Source { entered, calls }),
            Default::default(),
            Default::default(),
            CancellationToken::new(),
            prepared,
        )
        .unwrap();
        let (sink, finish) = entries.recv().await.unwrap();
        let mut updates = archive.subscribe();
        sink.send(value(1)).await.unwrap();
        until(&mut updates, |s| s.reference.records() == 1).await;
        let next = if private {
            value(2).with_provenance(
                Provenance::default().with_policy(&FlowPolicy::default().private()),
            )
        } else {
            value(2)
        };
        sink.send(next).await.unwrap();
        let final_ = until(&mut updates, |s| s.phase == eventlog::Phase::Incomplete).await;
        assert_eq!(final_.reference.records(), 1);
        sink.send(value(3)).await.unwrap();
        if private {
            assert!(matches!(
                store.dataset_inspect(final_.reference).await,
                Err(wes_engine::storage::StoreError::DatasetWithdrawn)
            ));
        } else {
            assert_eq!(final_.coverage.termination, Some(RecordingEnd::Limit));
        }
        finish.send(()).unwrap();
        task.join().await.unwrap();
        drop(store);
        storage.join().await.unwrap();
    }
}

#[tokio::test]
async fn abandoned_preparation_releases_only_its_writer_authority_without_io_or_subscription() {
    let home = home();
    let (store, storage) = worker(&home);
    let call = call();
    let (prepared, handle) = eventlog::Prepared::prepare(
        store.clone(),
        call.run.id().to_string(),
        uuid::Uuid::new_v4().to_string(),
        schema(),
        FlowPolicy::default(),
        Default::default(),
        1,
    )
    .await
    .unwrap();
    let reference = handle.snapshot().reference;
    assert!(
        store
            .dataset_plan_delete(reference.clone())
            .await
            .unwrap()
            .active_writer
    );
    drop(prepared);
    assert!(
        !store
            .dataset_plan_delete(reference.clone())
            .await
            .unwrap()
            .active_writer
    );
    assert_eq!(
        store.dataset_inspect(reference).await.unwrap().lifecycle,
        DatasetLifecycle::Interrupted
    );
    drop(store);
    storage.join().await.unwrap();
}
