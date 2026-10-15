//! Synthetic read owner and real owned storage. No broker, credentials or external resources.
use std::sync::{
    Arc,
    atomic::{AtomicBool, AtomicU64, Ordering},
};
use wes_adapters::{
    codec,
    storage::{Durability, FileValues},
};
use wes_core::{
    Data, Primitive, Provenance, Shape, Value,
    flow::{FlowPolicy, ProviderReadOrigin},
};
use wes_engine::{
    provider_reads::{Changes, ReadAuthority},
    storage::{StoreError, StoreWorkerLimits, ValueStore, spawn_store},
};
struct Gate {
    origin: ProviderReadOrigin,
    open: AtomicBool,
    epoch: AtomicU64,
    observers: wes_engine::credentials::material::ChangeObservers,
}
impl Gate {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            origin: ProviderReadOrigin::new(
                "00000000-0000-4000-8000-000000000075".into(),
                "a".repeat(64),
                "b".repeat(64),
            )
            .unwrap(),
            open: AtomicBool::new(true),
            epoch: AtomicU64::new(0),
            observers: Default::default(),
        })
    }
    fn set(&self, open: bool) {
        self.open.store(open, Ordering::SeqCst);
        self.epoch.fetch_add(1, Ordering::SeqCst);
        self.observers.changed();
    }
    fn policy(&self) -> FlowPolicy {
        FlowPolicy::default().with_provider_read(self.origin.clone())
    }
    fn value(&self) -> Value {
        Value::new(
            Shape::Primitive(Primitive::Text),
            Data::Text("synthetic protected data".into()),
            Provenance::default().with_policy(&self.policy()),
        )
        .unwrap()
    }
}
impl ReadAuthority for Gate {
    fn identity(&self) -> String {
        format!("fixture:{:p}", self)
    }
    fn realm(&self) -> Option<String> {
        Some(self.origin.realm().into())
    }
    fn closed(&self) -> bool {
        false
    }
    fn revision(&self) -> String {
        self.epoch.load(Ordering::SeqCst).to_string()
    }
    fn allows(&self, origin: &ProviderReadOrigin) -> bool {
        origin == &self.origin && self.open.load(Ordering::SeqCst)
    }
    fn observe(&self, observer: std::sync::Weak<Changes>) -> Result<(), &'static str> {
        self.observers
            .register(observer)
            .map_err(|_| "fixture observer capacity exceeded")
    }
}

#[tokio::test]
async fn retention_admission_preserves_read_withdrawal_without_requiring_a_read_grant() {
    let root = tempfile::tempdir().unwrap();
    let mut backend = wes_adapters::storage::TieredValues::open(
        &root.path().join("live"),
        &root.path().join("archive"),
        codec::Limits::default(),
        Durability::File,
        None,
    )
    .unwrap();
    let gate = Gate::new();
    let handle = backend.store(&gate.value()).unwrap();
    let (worker, task) = spawn_store(backend, StoreWorkerLimits::default()).unwrap();
    worker.provider_readers().register(gate.clone()).unwrap();
    gate.set(false);
    assert!(matches!(
        worker.read(handle.clone()).await,
        Err(StoreError::ProviderReadUnavailable)
    ));
    let kept = worker.retain(handle.clone()).await.unwrap();
    assert!(kept.kept);
    assert!(matches!(
        worker.read(handle).await,
        Err(StoreError::ProviderReadUnavailable)
    ));
    assert_eq!(worker.shutdown().await.unwrap().failed, 0);
    task.join().await.unwrap();
}
#[tokio::test]
async fn revoked_root_policy_precedes_body_decode_and_selection_and_keeps_the_bytes() {
    let home = tempfile::tempdir().unwrap();
    let path = home.path().join("values");
    let mut backend =
        FileValues::open(&path, codec::Limits::default(), Durability::File, None).unwrap();
    let gate = Gate::new();
    let value = gate.value();
    let handle = backend.store(&value).unwrap();
    let snapshot = backend.describe(&handle, &[]).unwrap().unwrap();
    let raw = std::fs::read(path.join(format!("{handle}.json"))).unwrap();
    let (worker, task) = spawn_store(backend, StoreWorkerLimits::default()).unwrap();
    worker.provider_readers().register(gate.clone()).unwrap();
    let _hold = worker.hold(handle.clone()).await.unwrap().unwrap();
    assert_eq!(
        worker.read(handle.clone()).await.unwrap().unwrap().value,
        value
    );
    gate.set(false);
    // Deliberately damage only the domain payload. A refusal must precede typed decoding.
    let mut damaged: serde_json::Value = serde_json::from_slice(&raw).unwrap();
    damaged["data"] = serde_json::json!({"not":"Text"});
    std::fs::write(
        path.join(format!("{handle}.json")),
        serde_json::to_vec(&damaged).unwrap(),
    )
    .unwrap();
    for result in [
        worker.read(handle.clone()).await.map(|_| ()),
        worker
            .describe(handle.clone(), vec!["missing".into()])
            .await
            .map(|_| ()),
        worker.encoded(handle.clone()).await.map(|_| ()),
        worker.materialize(snapshot.clone()).await.map(|_| ()),
        worker
            .source_range_selected(snapshot.clone(), vec!["missing".into()], 0, 1)
            .await
            .map(|_| ()),
    ] {
        assert!(
            matches!(result, Err(StoreError::ProviderReadUnavailable)),
            "{result:?}"
        );
    }
    assert!(path.join(format!("{handle}.json")).exists());
    std::fs::write(path.join(format!("{handle}.json")), raw).unwrap();
    gate.set(true);
    assert_eq!(worker.materialize(snapshot).await.unwrap().0.value, value);
    assert_eq!(worker.shutdown().await.unwrap().failed, 0);
    task.join().await.unwrap();
    // Retained labels carry no authority after reopening.
    let reopened =
        FileValues::open(&path, codec::Limits::default(), Durability::File, None).unwrap();
    let (worker, task) = spawn_store(reopened, StoreWorkerLimits::default()).unwrap();
    assert!(matches!(
        worker.read(handle).await,
        Err(StoreError::ProviderReadUnavailable)
    ));
    assert_eq!(worker.shutdown().await.unwrap().failed, 0);
    task.join().await.unwrap();
}

struct DelayedRange {
    value: Value,
    fail_after_entry: bool,
    entered: std::sync::Mutex<Option<tokio::sync::oneshot::Sender<()>>>,
    release: std::sync::Mutex<std::sync::mpsc::Receiver<()>>,
}
impl ValueStore for DelayedRange {
    fn read_policy(
        &self,
        _: &wes_engine::storage::ValueHandle,
    ) -> Result<Option<FlowPolicy>, StoreError> {
        Ok(Some(self.value.provenance().policy().clone()))
    }
    fn store(&mut self, _: &Value) -> Result<wes_engine::storage::ValueHandle, StoreError> {
        panic!("read-only fixture")
    }
    fn read(
        &self,
        _: &wes_engine::storage::ValueHandle,
    ) -> Result<Option<wes_engine::storage::LoadedValue>, StoreError> {
        Ok(Some(wes_engine::storage::LoadedValue {
            value: self.value.clone(),
        }))
    }
    fn encoded(&self, _: &wes_engine::storage::ValueHandle) -> Result<Option<Vec<u8>>, StoreError> {
        Ok(Some(b"synthetic immutable value".to_vec()))
    }
    fn size(&self, _: &wes_engine::storage::ValueHandle) -> Result<Option<u64>, StoreError> {
        Ok(Some(128))
    }
    fn release(&mut self, _: &wes_engine::storage::ValueHandle) -> Result<bool, StoreError> {
        Ok(false)
    }
    fn hold(
        &mut self,
        _: &wes_engine::storage::ValueHandle,
    ) -> Result<Option<wes_engine::storage::ValueLease>, StoreError> {
        Ok(Some(wes_engine::storage::ValueLease::retain(())))
    }
    fn source_range(
        &self,
        _: &wes_engine::storage::StoredSnapshot,
        from: u64,
        maximum: usize,
    ) -> Result<wes_engine::storage::SourceRange, StoreError> {
        self.entered
            .lock()
            .unwrap()
            .take()
            .unwrap()
            .send(())
            .unwrap();
        self.release.lock().unwrap().recv().unwrap();
        if self.fail_after_entry {
            return Err(StoreError::Closed);
        }
        let Data::Text(text) = self.value.data() else {
            panic!("Text fixture")
        };
        let from = from as usize;
        Ok(wes_engine::storage::SourceRange {
            bytes: text.as_bytes()[from..text.len().min(from + maximum)].to_vec(),
            extent: text.len() as u64,
            text: true,
        })
    }
}

#[tokio::test]
async fn byte_ranges_discard_late_regranted_or_expired_read_proofs_without_poisoning_shutdown() {
    for regrant in [true, false] {
        let gate = Gate::new();
        let (entered, entry) = tokio::sync::oneshot::channel();
        let (release, wait) = std::sync::mpsc::channel();
        let backend = DelayedRange {
            value: gate.value(),
            fail_after_entry: false,
            entered: std::sync::Mutex::new(Some(entered)),
            release: std::sync::Mutex::new(wait),
        };
        let snapshot = backend
            .describe(&wes_engine::storage::ValueHandle::fresh(), &[])
            .unwrap()
            .unwrap();
        let (worker, task) = spawn_store(backend, StoreWorkerLimits::default()).unwrap();
        worker.provider_readers().register(gate.clone()).unwrap();
        let reader = worker.clone();
        let result = tokio::spawn(async move { reader.source_range(snapshot, 0, 8).await });
        tokio::time::timeout(std::time::Duration::from_secs(2), entry)
            .await
            .unwrap()
            .unwrap();
        if regrant {
            gate.set(false);
            gate.set(true);
        } else {
            // A trusted probe's current grant may expire even without an epoch notification.
            gate.open.store(false, Ordering::SeqCst);
        }
        release.send(()).unwrap();
        let error = result.await.unwrap().unwrap_err();
        if regrant {
            assert!(
                matches!(error, StoreError::ProviderReadChanged),
                "{error:?}"
            );
        } else {
            assert!(
                matches!(error, StoreError::ProviderReadUnavailable),
                "{error:?}"
            );
        }
        assert_eq!(worker.shutdown().await.unwrap().failed, 0);
        task.join().await.unwrap();
    }
}

#[tokio::test]
async fn a_backend_failure_after_successful_scope_admission_still_counts_as_storage_failure() {
    let gate = Gate::new();
    let (entered, entry) = tokio::sync::oneshot::channel();
    let (release, wait) = std::sync::mpsc::channel();
    let backend = DelayedRange {
        value: gate.value(),
        fail_after_entry: true,
        entered: std::sync::Mutex::new(Some(entered)),
        release: std::sync::Mutex::new(wait),
    };
    let snapshot = backend
        .describe(&wes_engine::storage::ValueHandle::fresh(), &[])
        .unwrap()
        .unwrap();
    let (worker, task) = spawn_store(backend, StoreWorkerLimits::default()).unwrap();
    worker.provider_readers().register(gate).unwrap();
    let reader = worker.clone();
    let result = tokio::spawn(async move { reader.source_range(snapshot, 0, 8).await });
    tokio::time::timeout(std::time::Duration::from_secs(2), entry)
        .await
        .unwrap()
        .unwrap();
    release.send(()).unwrap();
    assert!(matches!(result.await.unwrap(), Err(StoreError::Closed)));
    assert_eq!(worker.shutdown().await.unwrap().failed, 1);
    task.join().await.unwrap();
}

#[tokio::test]
async fn selecting_a_public_composite_child_cannot_shed_another_members_read_origin() {
    use wes_engine::storage::{SnapshotMember, ValueHandle};
    let home = tempfile::tempdir().unwrap();
    let mut backend = wes_adapters::storage::TieredValues::open(
        &home.path().join("live"),
        &home.path().join("archive"),
        codec::Limits::default(),
        Durability::File,
        None,
    )
    .unwrap();
    let gate = Gate::new();
    let protected = backend.store(&gate.value()).unwrap();
    assert!(backend.keep(&protected).unwrap());
    let public = backend
        .store(
            &Value::new(
                Shape::Primitive(Primitive::Int),
                Data::Int(7),
                Provenance::default(),
            )
            .unwrap(),
        )
        .unwrap();
    assert!(backend.keep(&public).unwrap());
    let members = [("private", protected), ("public", public)]
        .into_iter()
        .map(|(name, handle)| SnapshotMember {
            name: name.into(),
            digest: backend.digest(&handle).unwrap().unwrap(),
            handle: handle.to_string(),
            selection: vec![],
        })
        .collect::<Vec<_>>();
    let root = ValueHandle::fresh();
    backend.compose(&root, &members).unwrap();
    let selected = backend
        .describe(&root, &["public".into()])
        .unwrap()
        .unwrap();
    assert_eq!(
        selected.provenance.policy().provider_reads(),
        gate.policy().provider_reads()
    );
    let (worker, task) = spawn_store(backend, StoreWorkerLimits::default()).unwrap();
    worker.provider_readers().register(gate.clone()).unwrap();
    assert_eq!(
        worker
            .materialize(selected.clone())
            .await
            .unwrap()
            .0
            .value
            .data(),
        &Data::Int(7)
    );
    gate.set(false);
    assert!(matches!(
        worker.materialize(selected).await,
        Err(StoreError::ProviderReadUnavailable)
    ));
    assert!(matches!(
        worker.describe(root, vec!["missing".into()]).await,
        Err(StoreError::ProviderReadUnavailable)
    ));
    worker.shutdown().await.unwrap();
    task.join().await.unwrap();
}

#[tokio::test]
async fn dataset_manifest_pages_and_heads_keep_the_origin_gate_across_reopen() {
    use wes_adapters::datasets::{DatasetStore, StoreLimits};
    use wes_core::contracts::{ContractRegistry, ResolvedContractBundle, SnapshotLimits};
    use wes_engine::storage::{datasets::*, spawn_storage};
    let home = tempfile::tempdir().unwrap();
    let gate = Gate::new();
    let values = FileValues::open(
        &home.path().join("values"),
        codec::Limits::default(),
        Durability::File,
        None,
    )
    .unwrap();
    let datasets = DatasetStore::open(
        &home.path().join("datasets"),
        Durability::File,
        StoreLimits::default(),
    )
    .unwrap();
    let (worker, task) = spawn_storage(values, datasets, StoreWorkerLimits::default()).unwrap();
    worker.provider_readers().register(gate.clone()).unwrap();
    let source = SourceExtent {
        identity: uuid::Uuid::new_v4().to_string(),
        unit: SourceUnit::Records,
        start: 0,
        end: 0,
    };
    let admission = worker
        .dataset_create(DatasetCreate {
            owner: None,
            dataset: uuid::Uuid::new_v4().to_string(),
            transaction: uuid::Uuid::new_v4().to_string(),
            kind: DatasetKind::Analysis,
            schema: ResolvedContractBundle::capture(
                ContractRegistry::new().resolve("Text").unwrap(),
                SnapshotLimits::default(),
            )
            .unwrap(),
            source: source.clone(),
            policy: gate.policy(),
            checkpoint: None,
            recording: None,
            coverage: None,
        })
        .await
        .unwrap();
    let _writer = admission.lease;
    let reference = worker
        .enqueue_dataset_append(DatasetAppend {
            owner: None,
            previous: admission.reference,
            transaction: uuid::Uuid::new_v4().to_string(),
            rows: vec![DatasetRow {
                ordinal: 0,
                source_start: 0,
                source_end: 1,
                value: gate.value(),
            }],
            source: SourceExtent { end: 1, ..source },
            lifecycle: DatasetLifecycle::Sealed,
            policy: gate.policy(),
            checkpoint: None,
            recording: None,
            coverage: vec![],
        })
        .await
        .unwrap()
        .wait()
        .await
        .unwrap();
    let page = || PageRequest {
        from: 0,
        rows: 1,
        bytes: wes_adapters::datasets::PageLimits::default().bytes,
        segments: wes_adapters::datasets::PageLimits::default().segments,
        charge: None,
        work: None,
    };
    assert_eq!(
        worker
            .dataset_inspect(reference.clone())
            .await
            .unwrap()
            .policy
            .provider_reads(),
        gate.policy().provider_reads()
    );
    assert_eq!(
        worker
            .dataset_page(reference.clone(), page())
            .await
            .unwrap()
            .rows[0]
            .value
            .provenance()
            .policy()
            .provider_reads(),
        gate.policy().provider_reads()
    );
    gate.set(false);
    assert!(matches!(
        worker.dataset_inspect(reference.clone()).await,
        Err(StoreError::ProviderReadUnavailable)
    ));
    assert!(matches!(
        worker.dataset_head(reference.clone()).await,
        Err(StoreError::ProviderReadUnavailable)
    ));
    assert!(matches!(
        worker.dataset_page(reference.clone(), page()).await,
        Err(StoreError::ProviderReadUnavailable)
    ));
    gate.set(true);
    assert_eq!(
        worker
            .dataset_page(reference.clone(), page())
            .await
            .unwrap()
            .rows[0]
            .value
            .data(),
        gate.value().data()
    );
    worker.shutdown().await.unwrap();
    task.join().await.unwrap();
    let values = FileValues::open(
        &home.path().join("values"),
        codec::Limits::default(),
        Durability::File,
        None,
    )
    .unwrap();
    let datasets = DatasetStore::open(
        &home.path().join("datasets"),
        Durability::File,
        StoreLimits::default(),
    )
    .unwrap();
    let (worker, task) = spawn_storage(values, datasets, StoreWorkerLimits::default()).unwrap();
    assert!(matches!(
        worker.dataset_inspect(reference).await,
        Err(StoreError::ProviderReadUnavailable)
    ));
    worker.shutdown().await.unwrap();
    task.join().await.unwrap();
}
