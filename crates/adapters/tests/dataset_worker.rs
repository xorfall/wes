use std::sync::Arc;
use wes_adapters::{
    datasets::{
        DatasetKind, DatasetPersistence, DatasetStore, IndexEntry, IndexSummary, Lifecycle,
        Manifest, PageLimits, PositionUnit, Record, SegmentHeader, SourceRange, StoreLimits,
    },
    storage::{Durability, FileValues},
};
use wes_core::{
    Data, Primitive, Provenance, Shape, Value,
    contracts::{ContractRegistry, ResolvedContractBundle, SnapshotLimits},
    flow::FlowPolicy,
};
use wes_engine::storage::{StoreWorkerLimits, datasets::PageRequest, spawn_storage};

fn home() -> tempfile::TempDir {
    let mut builder = tempfile::Builder::new();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        builder.permissions(std::fs::Permissions::from_mode(0o700));
    }
    builder.tempdir().unwrap()
}
fn scalar(value: i64) -> Value {
    Value::new(
        Shape::Primitive(Primitive::Int),
        Data::Int(value),
        Provenance::default(),
    )
    .unwrap()
}

#[tokio::test]
async fn abandoned_writer_admission_commits_locally_but_drops_all_process_writer_authority() {
    use wes_engine::storage::datasets::*;
    let tmp = home();
    let datasets = DatasetStore::open(
        &tmp.path().join("datasets"),
        Durability::FileAndDirectory,
        StoreLimits::default(),
    )
    .unwrap();
    let values = FileValues::open(
        &tmp.path().join("values"),
        wes_adapters::codec::Limits::default(),
        Durability::FileAndDirectory,
        None,
    )
    .unwrap();
    let (worker, task) = spawn_storage(values, datasets, StoreWorkerLimits::default()).unwrap();
    let schema = ResolvedContractBundle::capture(
        ContractRegistry::new().resolve("Int").unwrap(),
        SnapshotLimits::default(),
    )
    .unwrap();
    let request = |run: String| {
        let dataset = uuid::Uuid::new_v4().to_string();
        let source = uuid::Uuid::new_v4().to_string();
        DatasetCreate {
            owner: Some(DatasetWriteOwner {
                role: DatasetWriteRole::Recording,
                lineage: run.clone(),
                run,
            }),
            dataset: dataset.clone(),
            transaction: uuid::Uuid::new_v4().to_string(),
            kind: wes_engine::storage::datasets::DatasetKind::EventLog,
            schema: schema.clone(),
            source: SourceExtent {
                identity: source.clone(),
                unit: SourceUnit::Records,
                start: 0,
                end: 0,
            },
            policy: FlowPolicy::default(),
            checkpoint: None,
            recording: Some(EventLogCoverage {
                run: source,
                epoch: dataset,
                first: 1,
                accepted_through: 0,
                committed_through: 0,
                pending: Some(0),
                rejected: 0,
                termination: None,
            }),
        }
    };
    let run = uuid::Uuid::new_v4().to_string();
    let pending = worker
        .enqueue_dataset_create(request(run.clone()))
        .await
        .unwrap();
    drop(pending);
    // Reconciliation is queued behind admission. It observes the real commit,
    // but cannot recreate the discarded process lifetime or enter a producer.
    let receipt = worker
        .dataset_reconcile(DatasetWriteSelection {
            role: DatasetWriteRole::Recording,
            run,
        })
        .await
        .unwrap();
    assert_eq!(receipt.outcome, DatasetWriteOutcome::Committed);
    let reference = receipt.committed.unwrap();
    let info = worker.dataset_inspect(reference.clone()).await.unwrap();
    assert_eq!(info.lifecycle, DatasetLifecycle::Interrupted);
    assert!(
        !worker
            .dataset_plan_delete(reference)
            .await
            .unwrap()
            .active_writer
    );

    let admission = worker
        .dataset_create(request(uuid::Uuid::new_v4().to_string()))
        .await
        .unwrap();
    let reference = admission.reference.clone();
    assert_eq!(
        worker
            .dataset_inspect(reference.clone())
            .await
            .unwrap()
            .lifecycle,
        DatasetLifecycle::Open
    );
    let shared = admission.lease.clone();
    drop(admission);
    assert!(
        worker
            .dataset_plan_delete(reference.clone())
            .await
            .unwrap()
            .active_writer
    );
    drop(shared);
    assert!(
        !worker
            .dataset_plan_delete(reference.clone())
            .await
            .unwrap()
            .active_writer
    );
    assert_eq!(
        worker.dataset_inspect(reference).await.unwrap().lifecycle,
        DatasetLifecycle::Interrupted
    );
    worker.shutdown().await.unwrap();
    task.join().await.unwrap();
}
#[tokio::test]
async fn scan_candidate_advances_only_after_the_shared_store_confirms_its_outputs() {
    use wes_engine::{
        driver::CancellationToken,
        scan::{Identity, Input, Poll, Runner, Settings, Transition, ledger::MemoryPool},
    };
    use wes_language::{Expression, SourceText, Span, templates::Templates};
    let span = Span::at(0);
    let mut types = ContractRegistry::new();
    types
        .load("types: {NumberStep: {base: Record, fields: {state: Int, outputs: 'List<Int>'}}}")
        .unwrap();
    let parsed = wes_language::parse(&SourceText::new(
        "synthetic-dataset-analysis",
        ":def step(state:Int, context:Int, item:Int) -> NumberStep as :calc pure { return {state:state+1,outputs:[item,item+10]}; }",
    ));
    assert!(parsed.diagnostics.is_empty());
    let Expression::Definition(syntax) = parsed
        .script
        .statements
        .into_iter()
        .next()
        .unwrap()
        .expression
    else {
        panic!("pure definition")
    };
    let mut templates = Templates::new();
    templates
        .define_calculation(
            syntax,
            &types,
            &Default::default(),
            wes_language::calc::Package::standard(),
        )
        .unwrap();
    let transition = Transition::capture(
        templates.snapshot().values().next().unwrap(),
        false,
        4 * 1024 * 1024,
        span,
    )
    .unwrap();
    let settings = Settings::capture();
    let pool = MemoryPool::new(settings.limits.memory_bytes).unwrap();
    let input = Input {
        live: false,
        source: Value::new(
            Shape::List(Box::new(Shape::Primitive(Primitive::Int))),
            Data::List(vec![Data::Int(1), Data::Int(2)]),
            Provenance::default(),
        )
        .unwrap(),
        initial: scalar(0),
        context: scalar(0),
        step: transition,
        finish: None,
        framing: None,
        identity: Identity {
            analysis: uuid::Uuid::new_v4().to_string(),
            source: None,
            profile: "TypedRecords".into(),
            profile_revision: format!("sha256:{}", "1".repeat(64)),
        },
        control: None,
    };
    let mut runner =
        Runner::new(input, settings, &pool, None, CancellationToken::new(), span).unwrap();
    let tmp = home();
    let path = tmp.path().join("datasets");
    let datasets =
        DatasetStore::open(&path, Durability::FileAndDirectory, StoreLimits::default()).unwrap();
    let values = FileValues::open(
        &tmp.path().join("values"),
        wes_adapters::codec::Limits::default(),
        Durability::FileAndDirectory,
        None,
    )
    .unwrap();
    let (worker, task) = spawn_storage(values, datasets, StoreWorkerLimits::default()).unwrap();
    let admission = worker
        .dataset_create(runner.dataset_admission().unwrap())
        .await
        .unwrap();
    let reference = admission.reference;
    let _writer = admission.lease;
    runner.attach_dataset(reference.clone()).unwrap();
    let mut acknowledged = 0;
    loop {
        match runner.poll() {
            Poll::ReadPage | Poll::ReadHead => {
                panic!("inline scan unexpectedly requested dataset I/O")
            }
            Poll::Yield => {}
            Poll::Grant => panic!("non-durable sink does not need a disk grant"),
            Poll::Terminal => break,
            Poll::Commit => {
                let before = runner.progress();
                let request = runner.dataset_candidate().unwrap();
                assert_eq!(before.usage.input_records, acknowledged);
                assert_eq!(before.usage.output_records, acknowledged * 2);
                assert!(matches!(runner.poll(), Poll::Commit));
                assert_eq!(
                    runner.progress().committed_position,
                    before.committed_position
                );
                let accepted_inputs = request.rows.len() as u64 / 2;
                assert_eq!(
                    request
                        .rows
                        .iter()
                        .map(|row| (row.source_start, row.source_end))
                        .collect::<Vec<_>>(),
                    vec![(0, 1), (0, 1), (1, 2), (1, 2)]
                );
                let pending = worker.enqueue_dataset_append(request).await.unwrap();
                let committed = pending.wait().await.unwrap();
                // Physical visibility precedes the runner acknowledgement, never the reverse.
                assert_eq!(
                    worker
                        .dataset_inspect(committed.clone())
                        .await
                        .unwrap()
                        .reference,
                    committed
                );
                assert_eq!(
                    runner.progress().usage.input_records,
                    before.usage.input_records
                );
                runner.acknowledge_dataset(committed).unwrap();
                acknowledged += accepted_inputs;
            }
        }
    }
    let done = runner.into_completion().unwrap();
    assert!(done.stop.is_none());
    let Data::Record(fields) = done.value.data() else {
        panic!("result")
    };
    assert_eq!(fields["state"], Data::Int(2));
    let Data::Dataset(output) = &fields["outputs"] else {
        panic!("paged output")
    };
    assert_eq!(output.records(), 4);
    assert_eq!(done.progress.usage.input_records, 2);
    let cap = PageLimits::default();
    let page = worker
        .dataset_page(
            output.as_ref().clone(),
            PageRequest {
                work: None,
                from: 0,
                rows: 10,
                bytes: cap.bytes,
                segments: cap.segments,
            },
        )
        .await
        .unwrap();
    assert_eq!(
        page.rows
            .iter()
            .map(|r| r.value.data().clone())
            .collect::<Vec<_>>(),
        vec![Data::Int(1), Data::Int(11), Data::Int(2), Data::Int(12)]
    );
    assert_eq!(
        page.rows
            .iter()
            .map(|r| (r.source_start, r.source_end))
            .collect::<Vec<_>>(),
        vec![(0, 1), (0, 1), (1, 2), (1, 2)]
    );
    worker.shutdown().await.unwrap();
    task.join().await.unwrap();
    let restored =
        DatasetStore::open(&path, Durability::FileAndDirectory, StoreLimits::default()).unwrap();
    assert_eq!(
        restored.descriptor(output.dataset()).unwrap(),
        output.as_ref().clone()
    );
}
fn recorded(store: &mut DatasetStore) -> wes_core::DatasetRef {
    let schema = ResolvedContractBundle::capture(
        ContractRegistry::new().resolve("Int").unwrap(),
        SnapshotLimits::default(),
    )
    .unwrap();
    let policy = FlowPolicy::default();
    let dataset = uuid::Uuid::new_v4().to_string();
    let source = SourceRange {
        identity: "synthetic-input-r1".into(),
        unit: PositionUnit::Records,
        start: 0,
        end: 3,
    };
    let header = SegmentHeader {
        store: store.store_id().into(),
        dataset: dataset.clone(),
        schema: schema.digest().into(),
        first: 0,
        count: 3,
        source: source.clone(),
    };
    let records: Vec<_> = (0..3)
        .map(|n| Record {
            source_start: n,
            source_end: n + 1,
            value: scalar(n as i64 + 10),
        })
        .collect();
    let schema_ref = store
        .prepare()
        .unwrap()
        .publish_schema(&schema, &policy)
        .unwrap();
    let segment = store
        .prepare()
        .unwrap()
        .publish_segment(&header, &records, &schema, &policy)
        .unwrap();
    let (index, summary) = store
        .prepare()
        .unwrap()
        .append_index(
            None,
            &dataset,
            IndexEntry {
                summary: IndexSummary {
                    first: 0,
                    end: 3,
                    segment_bytes: segment.bytes,
                },
                segment,
                source: source.clone(),
            },
            &policy,
        )
        .unwrap();
    let manifest = Manifest {
        version: 2,
        store: store.store_id().into(),
        dataset: dataset.clone(),
        kind: DatasetKind::Analysis,
        generation: 1,
        previous: None,
        ancestors: vec![],
        transaction: uuid::Uuid::new_v4().to_string(),
        schema: schema_ref,
        schema_digest: schema.digest().into(),
        index: Some(index),
        summary,
        source,
        lifecycle: Lifecycle::Sealed,
        checkpoint: None,
        recording: None,
        requested: DatasetPersistence::FileAndDirectorySynced,
        established: DatasetPersistence::FileAndDirectorySynced,
        authorization_generation: 1,
        origins: vec![],
        dataset_reads: vec![],
    };
    store.commit(&manifest, &policy).unwrap();
    store.descriptor(&dataset).unwrap()
}
#[tokio::test]
async fn shared_writer_commits_exact_typed_batches_and_terminal_empty_extents() {
    use wes_engine::storage::datasets::{
        DatasetAppend, DatasetCreate, DatasetKind, DatasetLifecycle, DatasetRow, SourceExtent,
        SourceUnit,
    };
    let tmp = home();
    let path = tmp.path().join("datasets");
    let datasets =
        DatasetStore::open(&path, Durability::FileAndDirectory, StoreLimits::default()).unwrap();
    let values = FileValues::open(
        &tmp.path().join("values"),
        wes_adapters::codec::Limits::default(),
        Durability::FileAndDirectory,
        None,
    )
    .unwrap();
    let (worker, task) = spawn_storage(values, datasets, StoreWorkerLimits::default()).unwrap();
    let schema = ResolvedContractBundle::capture(
        ContractRegistry::new().resolve("Int").unwrap(),
        SnapshotLimits::default(),
    )
    .unwrap();
    let extent = |end| SourceExtent {
        identity: "synthetic-epoch-one".into(),
        unit: SourceUnit::Records,
        start: 0,
        end,
    };
    let create = DatasetCreate {
        owner: None,
        checkpoint: None,
        recording: None,
        dataset: uuid::Uuid::new_v4().to_string(),
        transaction: uuid::Uuid::new_v4().to_string(),
        kind: DatasetKind::Analysis,
        schema,
        source: extent(0),
        policy: FlowPolicy::default(),
    };
    let admission = worker.dataset_create(create.clone()).await.unwrap();
    let initial = admission.reference;
    let _writer = admission.lease;
    assert_eq!(initial.records(), 0);
    let row = |ordinal| DatasetRow {
        ordinal,
        source_start: ordinal,
        source_end: ordinal + 1,
        value: scalar(ordinal as i64 + 4),
    };
    let mut append = DatasetAppend {
        owner: None,
        checkpoint: None,
        recording: None,
        previous: initial.clone(),
        transaction: uuid::Uuid::new_v4().to_string(),
        rows: vec![row(1)],
        source: extent(1),
        lifecycle: DatasetLifecycle::Open,
        policy: FlowPolicy::default(),
    };
    assert!(
        worker
            .enqueue_dataset_append(append.clone())
            .await
            .unwrap()
            .wait()
            .await
            .is_err()
    );
    assert_eq!(
        worker
            .dataset_inspect(initial.clone())
            .await
            .unwrap()
            .reference,
        initial
    );
    append.rows = vec![row(0)];
    append.transaction = uuid::Uuid::new_v4().to_string();
    let committed = worker
        .enqueue_dataset_append(append.clone())
        .await
        .unwrap()
        .wait()
        .await
        .unwrap();
    assert_eq!(committed.records(), 1);
    // A stale writer cannot extend or overwrite another confirmed generation.
    append.transaction = uuid::Uuid::new_v4().to_string();
    assert!(
        worker
            .enqueue_dataset_append(append)
            .await
            .unwrap()
            .wait()
            .await
            .is_err()
    );
    let sealed = worker
        .enqueue_dataset_append(DatasetAppend {
            owner: None,
            checkpoint: None,
            recording: None,
            previous: committed,
            transaction: uuid::Uuid::new_v4().to_string(),
            rows: vec![],
            source: extent(1),
            lifecycle: DatasetLifecycle::Sealed,
            policy: FlowPolicy::default(),
        })
        .await
        .unwrap()
        .wait()
        .await
        .unwrap();
    let cap = PageLimits::default();
    let page = worker
        .dataset_page(
            sealed.clone(),
            PageRequest {
                work: None,
                from: 0,
                rows: 10,
                bytes: cap.bytes,
                segments: cap.segments,
            },
        )
        .await
        .unwrap();
    assert_eq!(page.rows.len(), 1);
    assert_eq!(page.rows[0].value.data(), &Data::Int(4));
    assert_eq!(
        worker
            .dataset_inspect(sealed.clone())
            .await
            .unwrap()
            .lifecycle,
        DatasetLifecycle::Sealed
    );
    assert_eq!(
        worker
            .dataset_page(
                initial.clone(),
                PageRequest {
                    work: None,
                    from: 0,
                    rows: 10,
                    bytes: cap.bytes,
                    segments: cap.segments
                }
            )
            .await
            .unwrap()
            .rows
            .len(),
        0
    );
    worker.shutdown().await.unwrap();
    task.join().await.unwrap();
    let restored =
        DatasetStore::open(&path, Durability::FileAndDirectory, StoreLimits::default()).unwrap();
    assert_eq!(restored.descriptor(&create.dataset).unwrap(), sealed);
}
#[tokio::test]
async fn values_and_dataset_pages_share_shutdown_and_owned_directory_lifetimes() {
    let tmp = home();
    let path = tmp.path().join("datasets");
    let mut datasets =
        DatasetStore::open(&path, Durability::FileAndDirectory, StoreLimits::default()).unwrap();
    let reference = recorded(&mut datasets);
    let values = FileValues::open(
        &tmp.path().join("values"),
        wes_adapters::codec::Limits::default(),
        Durability::FileAndDirectory,
        None,
    )
    .unwrap();
    let (worker, task) = spawn_storage(values, datasets, StoreWorkerLimits::default()).unwrap();
    let handle = worker.store(scalar(7)).await.unwrap();
    assert_eq!(
        worker.read(handle).await.unwrap().unwrap().value.data(),
        &Data::Int(7)
    );
    let limits = PageLimits::default();
    let page = worker
        .dataset_page(
            reference.clone(),
            PageRequest {
                work: None,
                from: 1,
                rows: 2,
                bytes: limits.bytes,
                segments: limits.segments,
            },
        )
        .await
        .unwrap();
    assert_eq!((page.first, page.next, page.extent_exhausted), (1, 3, true));
    assert_eq!(page.rows[0].ordinal, 1);
    assert_eq!(page.rows[0].value.data(), &Data::Int(11));
    let info = worker.dataset_inspect(reference.clone()).await.unwrap();
    assert_eq!(info.reference, reference);
    assert_eq!(info.schema.digest(), reference.schema_digest());
    assert!(
        DatasetStore::open(&path, Durability::FileAndDirectory, StoreLimits::default()).is_err()
    );
    let drain = worker.shutdown().await.unwrap();
    assert_eq!((drain.attempted, drain.failed), (4, 0));
    task.join().await.unwrap();
    let restored =
        DatasetStore::open(&path, Durability::FileAndDirectory, StoreLimits::default()).unwrap();
    assert_eq!(restored.descriptor(reference.dataset()).unwrap(), reference);
}
#[tokio::test]
async fn insufficient_shared_credit_refuses_before_entering_the_dataset_reader() {
    use std::{
        num::NonZeroU32,
        sync::atomic::{AtomicUsize, Ordering},
    };
    use wes_engine::storage::{
        StoreError,
        datasets::{DatasetInfo, DatasetPage, DatasetStorage},
    };
    struct Reader(Arc<AtomicUsize>);
    impl DatasetStorage for Reader {
        fn retention_size(&self, _: &wes_core::DatasetRef) -> Result<u64, StoreError> {
            Err(StoreError::DatasetUnavailable)
        }
        fn root_covers(
            &self,
            _: &wes_engine::storage::ValueHandle,
            _: &[wes_core::DatasetRef],
            _: wes_engine::storage::Retention,
        ) -> Result<bool, StoreError> {
            Err(StoreError::DatasetUnavailable)
        }
        fn set_root(
            &mut self,
            _: wes_engine::storage::datasets::DatasetRootRequest,
        ) -> Result<wes_engine::storage::datasets::DatasetRootReceipt, StoreError> {
            self.0.fetch_add(1, Ordering::SeqCst);
            Err(StoreError::DatasetUnavailable)
        }
        fn read_charge(&self) -> u64 {
            8192
        }
        fn inspect(&self, _: &wes_core::DatasetRef) -> Result<DatasetInfo, StoreError> {
            self.0.fetch_add(1, Ordering::SeqCst);
            Err(StoreError::DatasetMissing)
        }
        fn page(
            &self,
            _: &wes_core::DatasetRef,
            _: PageRequest,
        ) -> Result<DatasetPage, StoreError> {
            self.0.fetch_add(1, Ordering::SeqCst);
            Err(StoreError::DatasetMissing)
        }
    }
    let tmp = home();
    let mut datasets = DatasetStore::open(
        &tmp.path().join("datasets"),
        Durability::FileAndDirectory,
        StoreLimits::default(),
    )
    .unwrap();
    let reference = recorded(&mut datasets);
    drop(datasets);
    let calls = Arc::new(AtomicUsize::new(0));
    let values = FileValues::open(
        &tmp.path().join("values"),
        wes_adapters::codec::Limits::default(),
        Durability::FileAndDirectory,
        None,
    )
    .unwrap();
    let (worker, task) = spawn_storage(
        values,
        Reader(calls.clone()),
        StoreWorkerLimits {
            bytes: NonZeroU32::new(4096).unwrap(),
            ..StoreWorkerLimits::default()
        },
    )
    .unwrap();
    assert!(matches!(
        worker.dataset_inspect(reference).await,
        Err(StoreError::Limit("queue payload"))
    ));
    assert_eq!(calls.load(Ordering::SeqCst), 0);
    worker.shutdown().await.unwrap();
    task.join().await.unwrap();
}

#[tokio::test]
async fn dataset_retention_accounts_for_payload_and_preserves_the_exact_prefix_after_restart() {
    use wes_adapters::storage::TieredValues;
    use wes_engine::storage::{AutoKeep, PublicationPolicy};
    let tmp = home();
    let dataset_path = tmp.path().join("datasets");
    let mut datasets = DatasetStore::open(
        &dataset_path,
        Durability::FileAndDirectory,
        StoreLimits::default(),
    )
    .unwrap();
    let reference = recorded(&mut datasets);
    let footprint = datasets.retention_bytes(&reference).unwrap();
    let value = Value::new(
        Shape::Dataset(Box::new(Shape::Primitive(Primitive::Int))),
        Data::Dataset(reference.clone().into()),
        Provenance::default(),
    )
    .unwrap();
    let values = TieredValues::open(
        &tmp.path().join("live"),
        &tmp.path().join("archive"),
        wes_adapters::codec::Limits::default(),
        Durability::FileAndDirectory,
        None,
    )
    .unwrap();
    let (worker, task) = spawn_storage(values, datasets, StoreWorkerLimits::default()).unwrap();
    let temp = worker
        .publish(value.clone(), PublicationPolicy::Temporary)
        .await
        .unwrap();
    assert!(!temp.kept);
    assert!(
        !worker
            .dataset_inspect(reference.clone())
            .await
            .unwrap()
            .protected
    );
    let preview = worker
        .dataset_retention_preview(reference.clone())
        .await
        .unwrap();
    assert_eq!(preview.total_bytes, footprint);
    assert_eq!(preview.shared_bytes, 0);
    assert_eq!(preview.exclusive_bytes, footprint);
    assert_eq!(preview.captured_source_bytes, 0);
    // A descriptor-sized automatic budget must not silently retain a much larger data extent.
    assert!(temp.bytes < footprint);
    let automatic = worker
        .publish(value.clone(), AutoKeep::UpToBytes(temp.bytes).into())
        .await
        .unwrap();
    assert!(!automatic.kept);
    let automatic_full = worker
        .publish(
            value.clone(),
            AutoKeep::UpToBytes(temp.bytes + footprint).into(),
        )
        .await
        .unwrap();
    assert!(automatic_full.kept);
    let kept = worker.retain(temp.handle.clone()).await.unwrap();
    assert!(kept.kept);
    assert!(
        worker
            .dataset_inspect(reference.clone())
            .await
            .unwrap()
            .protected
    );
    assert!(worker.is_kept(kept.handle.clone()).await.unwrap());
    let preview = worker
        .dataset_retention_preview(reference.clone())
        .await
        .unwrap();
    assert_eq!(preview.total_bytes, footprint);
    assert_eq!(preview.shared_bytes, footprint);
    assert_eq!(preview.exclusive_bytes, 0);

    worker.release(automatic.handle).await.unwrap();
    worker.release(automatic_full.handle).await.unwrap();
    worker.shutdown().await.unwrap();
    task.join().await.unwrap();
    let values = TieredValues::open(
        &tmp.path().join("live"),
        &tmp.path().join("archive"),
        wes_adapters::codec::Limits::default(),
        Durability::FileAndDirectory,
        None,
    )
    .unwrap();
    let datasets = DatasetStore::open(
        &dataset_path,
        Durability::FileAndDirectory,
        StoreLimits::default(),
    )
    .unwrap();
    assert!(datasets.is_protected(&reference));
    let (worker, task) = spawn_storage(values, datasets, StoreWorkerLimits::default()).unwrap();
    let restored = worker.recover(kept.handle.clone()).await.unwrap().unwrap();
    assert_eq!(restored.loaded.value.data(), value.data());
    assert_eq!(
        worker
            .dataset_page(
                reference.clone(),
                PageRequest {
                    work: None,
                    from: 0,
                    rows: 3,
                    bytes: 1048576,
                    segments: 1
                }
            )
            .await
            .unwrap()
            .rows
            .len(),
        3
    );
    worker.release(kept.handle).await.unwrap();
    assert!(!worker.dataset_inspect(reference).await.unwrap().protected);
    worker.shutdown().await.unwrap();
    task.join().await.unwrap();
}

#[tokio::test]
async fn dataset_descriptor_cannot_forge_its_element_type_or_reference_a_foreign_home() {
    use wes_engine::storage::StoreError;
    let tmp = home();
    let mut datasets = DatasetStore::open(
        &tmp.path().join("datasets"),
        Durability::FileAndDirectory,
        StoreLimits::default(),
    )
    .unwrap();
    let reference = recorded(&mut datasets);
    let values = FileValues::open(
        &tmp.path().join("values"),
        wes_adapters::codec::Limits::default(),
        Durability::FileAndDirectory,
        None,
    )
    .unwrap();
    let (worker, task) = spawn_storage(values, datasets, StoreWorkerLimits::default()).unwrap();
    let forged = Value::new(
        Shape::Dataset(Box::new(Shape::Primitive(Primitive::Text))),
        Data::Dataset(reference.clone().into()),
        Provenance::default(),
    )
    .unwrap();
    assert!(matches!(
        worker.store(forged).await,
        Err(StoreError::Conflict)
    ));
    let mut wire = serde_json::to_value(&reference).unwrap();
    wire["store"] = uuid::Uuid::new_v4().to_string().into();
    let foreign: wes_core::DatasetRef = serde_json::from_value(wire).unwrap();
    let foreign = Value::new(
        Shape::Dataset(Box::new(Shape::Primitive(Primitive::Int))),
        Data::Dataset(foreign.into()),
        Provenance::default(),
    )
    .unwrap();
    assert!(matches!(
        worker.store(foreign).await,
        Err(StoreError::DatasetMissing)
    ));
    // No descriptor value was published for either rejected claim.
    let drain = worker.shutdown().await.unwrap();
    assert_eq!((drain.attempted, drain.failed), (2, 2));
    task.join().await.unwrap();
}

#[tokio::test]
async fn workspace_history_protects_only_retained_dataset_values_and_releases_after_physical_cleanup()
 {
    use wes_adapters::{journal::ReadLimits, storage::TieredValues, workspaces::FileWorkspaces};
    use wes_engine::{
        graph::NodeId,
        history::{
            HistoryCapture, HistoryCaptureLimits, HistoryCheckpoint, JournalEntry, JournalSink,
            Record as HistoryRecord, RetainedResult,
        },
        runtime::RunId,
        storage::{PublicationPolicy, datasets::DatasetRootKind},
        workspace::WorkspaceName,
    };
    let tmp = home();
    let path = tmp.path().join("datasets");
    let mut datasets =
        DatasetStore::open(&path, Durability::FileAndDirectory, StoreLimits::default()).unwrap();
    let reference = recorded(&mut datasets);
    let values = TieredValues::open(
        &tmp.path().join("live"),
        &tmp.path().join("archive"),
        wes_adapters::codec::Limits::default(),
        Durability::FileAndDirectory,
        None,
    )
    .unwrap();
    let (worker, task) = spawn_storage(values, datasets, StoreWorkerLimits::default()).unwrap();
    let value = Value::new(
        Shape::Dataset(Box::new(Shape::Primitive(Primitive::Int))),
        Data::Dataset(reference.clone().into()),
        Provenance::default(),
    )
    .unwrap();
    let transient = worker
        .publish(value.clone(), PublicationPolicy::Temporary)
        .await
        .unwrap();
    let kept = worker
        .publish(value, PublicationPolicy::Protected)
        .await
        .unwrap();
    let publication = worker.retained_dataset_publication().unwrap();
    let ws_path = tmp.path().join("workspaces");
    let name = WorkspaceName::new("synthetic".into()).unwrap();
    let transient_handle = transient.handle.clone();
    let kept_handle = kept.handle.clone();
    let (mut workspaces, mut history) = tokio::task::spawn_blocking(move || {
        let mut workspaces = FileWorkspaces::open(
            &ws_path,
            ReadLimits::default(),
            Durability::FileAndDirectory,
        )
        .unwrap();
        workspaces.set_retained_dataset_publication(publication);
        let mut capture = HistoryCapture::new(HistoryCaptureLimits::default());
        capture
            .push(HistoryRecord::Journal(JournalEntry::Payload {
                node: NodeId::new("id0").unwrap(),
                run: RunId::new(uuid::Uuid::new_v4().to_string()).unwrap(),
                handle: transient_handle,
            }))
            .unwrap();
        // An old declared retained result whose actual bytes are temporary is not resurrected.
        capture
            .push(HistoryRecord::Journal(JournalEntry::Result(
                RetainedResult {
                    node: NodeId::new("id1").unwrap(),
                    run: RunId::new(uuid::Uuid::new_v4().to_string()).unwrap(),
                    handle: transient.handle,
                    retention: wes_engine::storage::Retention::Automatic,
                },
            )))
            .unwrap();
        let image = capture.finish(HistoryCheckpoint {
            journal: wes_engine::history::AppendReceipt {
                persistence: wes_engine::history::Persistence::Volatile,
                end_offset: 0,
            },
            recovery: wes_engine::history::AppendReceipt {
                persistence: wes_engine::history::Persistence::Volatile,
                end_offset: 0,
            },
        });
        workspaces.save(&name, &image).unwrap();
        let (mut history, _) = workspaces.load(&name).unwrap();
        history
            .append(&HistoryRecord::Journal(JournalEntry::Result(
                RetainedResult {
                    node: NodeId::new("id2").unwrap(),
                    run: RunId::new(uuid::Uuid::new_v4().to_string()).unwrap(),
                    handle: kept_handle,
                    retention: wes_engine::storage::Retention::Protected,
                },
            )))
            .unwrap();
        (workspaces, history)
    })
    .await
    .unwrap();
    let plan = worker.dataset_plan_delete(reference.clone()).await.unwrap();
    assert_eq!(
        plan.references
            .iter()
            .filter(|r| r.kind == DatasetRootKind::Workspace)
            .count(),
        1
    );
    worker.release(kept.handle).await.unwrap();
    assert!(
        worker
            .dataset_inspect(reference.clone())
            .await
            .unwrap()
            .protected
    );
    let name = WorkspaceName::new("synthetic".into()).unwrap();
    tokio::task::spawn_blocking(move || {
        let checkpoint = history
            .capture(HistoryCaptureLimits::default())
            .unwrap()
            .checkpoint();
        let empty = HistoryCapture::new(HistoryCaptureLimits::default()).finish(checkpoint);
        workspaces.save(&name, &empty).unwrap();
        assert_eq!(workspaces.collect_unused().unwrap().active, 1);
        drop(history);
        assert_eq!(workspaces.collect_unused().unwrap().removed, 1);
    })
    .await
    .unwrap();
    assert!(!worker.dataset_inspect(reference).await.unwrap().protected);
    assert!(worker.shutdown().await.unwrap().failed == 0);
    task.join().await.unwrap();
}
