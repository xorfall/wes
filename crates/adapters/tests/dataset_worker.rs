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
            coverage: None,
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
            Poll::Grant | Poll::Settle => panic!("non-durable sink does not need a disk grant"),
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
                charge: None,
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
        coverage: None,
        store: store.store_id().into(),
        dataset: dataset.clone(),
        stream: wes_adapters::datasets::Stream::Outputs,
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
            wes_adapters::datasets::Stream::Outputs,
            IndexEntry {
                summary: IndexSummary {
                    coverage: None,
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
        coverage: None,
        version: 3,
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
        coverage: None,
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
        coverage: Vec::new(),
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
            coverage: Vec::new(),
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
                charge: None,
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
                    charge: None,
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
                charge: None,
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
                    charge: None,
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

#[tokio::test]
async fn forensic_runner_skips_callbacks_and_acknowledges_output_coverage_state_and_cursor_together()
 {
    forensic_scan(false, 2, None, 3).await;
}
#[tokio::test]
async fn long_skipped_frames_renew_prepaid_work_without_advancing_the_committed_boundary() {
    forensic_scan(true, 2, None, 3).await;
}
#[tokio::test]
async fn finish_outputs_use_the_extent_end_even_inside_an_unpublished_forensic_batch() {
    forensic_scan(false, 128, None, 3).await;
}
#[tokio::test]
async fn long_skip_cannot_renew_real_absolute_or_earned_budget_exhaustion() {
    use wes_engine::scan::ledger::Dimension;
    forensic_scan(true, 2, Some(Dimension::Work), 3).await;
    forensic_scan(true, 2, Some(Dimension::WorkAllowance), 3).await;
}
#[tokio::test]
async fn large_rejection_excerpts_fit_one_renewed_handoff_with_small_invocation_bounds() {
    forensic_scan(true, 2, None, 4096).await;
}
async fn forensic_scan(
    long: bool,
    batch: usize,
    exhausted: Option<wes_engine::scan::ledger::Dimension>,
    excerpt: usize,
) {
    use wes_core::framing::{Decoding, Delimiter, Malformed, Profile};
    use wes_engine::{
        driver::CancellationToken,
        scan::{Identity, Input, Poll, Runner, Settings, Transition, ledger::MemoryPool},
    };
    use wes_language::{Expression, SourceText, Span, templates::Templates};
    let span = Span::at(0);
    let mut types = ContractRegistry::new();
    types
        .load("types: {TextStep: {base: Record, fields: {state: Int, outputs: 'List<Text>'}}}")
        .unwrap();
    let mut templates = Templates::new();
    for text in [
        ":def step(state:Int, context:Int, item:Unknown) -> TextStep as :calc pure { return {state:state+1,outputs:[item.text]}; }",
        ":def finish(state:Int, context:Int, end:Unknown) -> TextStep as :calc pure { return {state:state+100,outputs:['end']}; }",
    ] {
        let parsed = wes_language::parse(&SourceText::new("synthetic-forensic-analysis", text));
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
        templates
            .define_calculation(
                syntax,
                &types,
                &Default::default(),
                wes_language::calc::Package::standard(),
            )
            .unwrap();
    }
    let step =
        Transition::capture(&templates.snapshot()["step"], false, 4 * 1024 * 1024, span).unwrap();
    let finish =
        Transition::capture(&templates.snapshot()["finish"], true, 4 * 1024 * 1024, span).unwrap();
    let mut settings = Settings::capture();
    settings.commit_records = batch;
    settings.block = if long { 128 } else { 2 };
    if long {
        settings.record_charge = 16384;
        settings.state_charge = 4096;
        settings.context_charge = 4096;
        settings.scratch.bytes = 32768;
        settings.scratch.work = if excerpt > 3 { 1024 } else { 16384 };
        if excerpt > 3 {
            settings.block = 16;
        }
        settings.patterns = 1;
    }
    if let Some(dimension) = exhausted {
        settings.startup_work = 2_000_000;
        if dimension == wes_engine::scan::ledger::Dimension::Work {
            settings.limits.work = settings.startup_work;
        }
    }
    let rejected_raw = if long {
        vec![b'x'; 200000]
    } else {
        b"abcdef".to_vec()
    };
    let mut raw = b"ok\n".to_vec();
    raw.extend_from_slice(&rejected_raw);
    raw.extend_from_slice(b"\n\xff\nzz");
    let raw_length = raw.len() as u64;
    let rejected_bytes = rejected_raw.len() as u64 + 3;
    let pool = MemoryPool::new(settings.limits.memory_bytes).unwrap();
    let mut runner = Runner::new(
        Input {
            live: false,
            source: Value::new(
                Shape::Primitive(Primitive::Bytes),
                Data::Bytes(raw.into()),
                Provenance::default(),
            )
            .unwrap(),
            initial: scalar(0),
            context: scalar(0),
            step,
            finish: Some(finish),
            framing: Some(Profile {
                delimiter: Delimiter::Lines,
                decoding: Decoding::StrictUtf8,
                malformed: Malformed::Forensic {
                    excerpt_bytes: excerpt,
                },
                raw_bytes: 3,
                decoded_bytes: 64,
                spans: 16,
            }),
            identity: Identity {
                analysis: uuid::Uuid::new_v4().to_string(),
                source: None,
                profile: "LinesUtf8".into(),
                profile_revision: format!("sha256:{}", "1".repeat(64)),
            },
            control: None,
        },
        settings,
        &pool,
        None,
        CancellationToken::new(),
        span,
    )
    .unwrap();
    let tmp = home();
    let path = tmp.path().join("datasets");
    let datasets =
        DatasetStore::open(&path, Durability::FileAndDirectory, StoreLimits::default()).unwrap();
    let values = wes_adapters::storage::TieredValues::open(
        &tmp.path().join("live"),
        &tmp.path().join("archive"),
        wes_adapters::codec::Limits::default(),
        Durability::FileAndDirectory,
        None,
    )
    .unwrap();
    let (worker, task) = spawn_storage(values, datasets, StoreWorkerLimits::default()).unwrap();
    let source = worker
        .capture_scan_source(runner.captured_source())
        .await
        .unwrap();
    let mut create = runner.dataset_admission().unwrap();
    create.checkpoint = Some(runner.checkpoint_seed(source).unwrap());
    let admission = worker.dataset_create(create).await.unwrap();
    let initial = admission.reference.clone();
    let _writer = admission.lease;
    let cp = worker
        .dataset_checkpoint(initial.clone())
        .await
        .unwrap()
        .unwrap();
    runner.attach_durable(initial, cp).unwrap();
    let mut final_ref = None;
    let mut settlements = 0;
    for _ in 0..100000 {
        match runner.poll() {
            Poll::Yield => {}
            Poll::ReadPage | Poll::ReadHead => panic!("inline bytes require no dataset source I/O"),
            Poll::Settle => {
                settlements += 1;
                let before = runner.progress();
                let request = runner.durable_settlement().unwrap();
                let previous = worker
                    .dataset_checkpoint(request.previous.clone())
                    .await
                    .unwrap()
                    .unwrap();
                let pending = request.checkpoint.as_ref().unwrap();
                assert_eq!(pending.next_position, previous.next_position);
                assert_eq!(pending.next_ordinal, previous.next_ordinal);
                assert_eq!(pending.coverage, previous.coverage);
                assert_eq!(pending.state.data(), previous.state.data());
                assert_eq!(pending.usage.input_bytes, previous.usage.input_bytes);
                assert_eq!(pending.usage.output_bytes, previous.usage.output_bytes);
                assert_eq!(pending.usage.work_allowance, previous.usage.work_allowance);
                assert_eq!(pending.work.outstanding, 0);
                assert!(pending.work.charged > previous.work.charged);
                assert!(before.read_position >= pending.next_position);
                assert!(matches!(runner.poll(), Poll::Settle));
                let reference = worker
                    .enqueue_dataset_append(request)
                    .await
                    .unwrap()
                    .wait()
                    .await
                    .unwrap();
                runner.acknowledge_settlement(reference.clone()).unwrap();
                assert_eq!(
                    runner.progress().committed_position,
                    before.committed_position
                );
                assert_eq!(runner.progress().read_position, before.read_position);
                final_ref = Some(reference);
            }
            Poll::Grant => {
                let request = runner.durable_grant().unwrap();
                let cp = request.checkpoint.clone().unwrap();
                let reference = worker
                    .enqueue_dataset_append(request)
                    .await
                    .unwrap()
                    .wait()
                    .await
                    .unwrap();
                runner.acknowledge_grant(reference.clone(), cp).unwrap();
                final_ref = Some(reference);
            }
            Poll::Commit => {
                let before = runner.progress();
                let request = runner.dataset_candidate().unwrap();
                let cp = request.checkpoint.clone().unwrap();
                assert_eq!(
                    worker
                        .dataset_checkpoint(request.previous.clone())
                        .await
                        .unwrap()
                        .unwrap()
                        .coverage,
                    before.coverage
                );
                assert!(matches!(runner.poll(), Poll::Commit));
                assert_eq!(
                    runner.progress().usage.input_records,
                    before.usage.input_records
                );
                assert_eq!(runner.progress().coverage, before.coverage);
                let reference = worker
                    .enqueue_dataset_append(request)
                    .await
                    .unwrap()
                    .wait()
                    .await
                    .unwrap();
                assert_eq!(
                    worker
                        .dataset_checkpoint(reference.clone())
                        .await
                        .unwrap()
                        .unwrap()
                        .coverage,
                    cp.coverage
                );
                runner.acknowledge_dataset(reference.clone()).unwrap();
                assert_eq!(runner.progress().coverage, cp.coverage);
                assert_eq!(runner.progress().committed_position, cp.next_position);
                final_ref = Some(reference);
            }
            Poll::Terminal => break,
        }
    }
    if let Some(dimension) = exhausted {
        assert_eq!(runner.progress().phase, wes_engine::scan::Phase::Stopped);
        let request = runner.durable_terminal_update().unwrap().unwrap();
        let checkpoint = request.checkpoint.clone().unwrap();
        let reference = worker
            .enqueue_dataset_append(request)
            .await
            .unwrap()
            .wait()
            .await
            .unwrap();
        runner
            .acknowledge_terminal(reference.clone(), checkpoint)
            .unwrap();
        let done = runner.into_completion().unwrap();
        assert_eq!(done.stop.as_ref().unwrap().dimension, Some(dimension));
        assert_eq!(done.progress.committed_position, 3);
        assert!(done.progress.read_position > 3 && done.progress.read_position < raw_length);
        assert_eq!(done.progress.usage.input_records, 1);
        assert_eq!(done.progress.usage.output_records, 1);
        assert_eq!(done.progress.coverage.as_ref().unwrap().records, 0);
        assert!(!done.progress.finish_applied);
        let Data::Record(result) = done.value.data() else {
            panic!("scan result")
        };
        assert_eq!(result["state"], Data::Int(1));
        let saved = worker
            .dataset_checkpoint(reference.clone())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            saved.stop,
            Some(wes_engine::storage::datasets::AnalysisStop::Cumulative(
                dimension
            ))
        );
        assert_eq!(saved.next_position, 3);
        assert_eq!(saved.work.outstanding, 0);
        assert!(settlements > 2);
        assert_eq!(
            worker
                .dataset_coverage_page(
                    reference,
                    PageRequest {
                        charge: None,
                        work: None,
                        from: 0,
                        rows: 10,
                        bytes: 65536,
                        segments: 8
                    }
                )
                .await
                .unwrap()
                .rows
                .len(),
            0
        );
        drop((_writer, done.value));
        worker.shutdown().await.unwrap();
        task.join().await.unwrap();
        return;
    }
    let completion = runner.into_completion().unwrap();
    assert_eq!(
        completion.progress.phase,
        wes_engine::scan::Phase::Complete,
        "{:?}",
        completion.stop
    );
    assert_eq!(completion.progress.usage.input_records, 4);
    assert_eq!(completion.progress.usage.input_bytes, raw_length);
    if long {
        assert!(
            settlements > 2,
            "a long skip must cross several actual prepaid leases"
        );
    }
    assert_eq!(completion.progress.usage.output_records, 3);
    assert!(completion.progress.finish_applied);
    let coverage = completion.progress.coverage.unwrap();
    assert_eq!(
        (coverage.records, coverage.input_bytes),
        (2, rejected_bytes)
    );
    let Data::Record(result) = completion.value.data() else {
        panic!("scan result")
    };
    assert_eq!(
        result["state"],
        Data::Int(102),
        "only two valid callbacks and one finish may change state"
    );
    let Data::Record(receipt) = &result["receipt"] else {
        panic!("native receipt")
    };
    assert_eq!(receipt["malformed"], Data::Text("forensic".into()));
    assert_eq!(receipt["rejectedRecords"], Data::Int(2));
    assert_eq!(
        receipt["rejectedInputBytes"],
        Data::Int(rejected_bytes as i64)
    );
    assert_eq!(
        receipt["sourceComplete"],
        Data::Option(None),
        "an inline extent is not an upstream producer completion claim"
    );
    let reference = final_ref.unwrap();
    let request = PageRequest {
        charge: None,
        work: None,
        from: 0,
        rows: 10,
        bytes: 65536,
        segments: 8,
    };
    let page = worker
        .dataset_page(reference.clone(), request.clone())
        .await
        .unwrap();
    assert_eq!(
        page.rows
            .iter()
            .map(|row| row.value.data())
            .collect::<Vec<_>>(),
        vec![
            &Data::Text("ok".into()),
            &Data::Text("zz".into()),
            &Data::Text("end".into())
        ]
    );
    let last = page.rows.last().unwrap();
    assert_eq!(
        (last.source_start, last.source_end),
        (raw_length, raw_length),
        "finish outputs identify EOF, not the earlier committed boundary"
    );
    drop(page);
    let page = worker
        .dataset_coverage_page(reference.clone(), request.clone())
        .await
        .unwrap();
    let rejected = page
        .rows
        .iter()
        .map(|row| {
            wes_engine::storage::datasets::read_rejection(&row.value, coverage.policy).unwrap()
        })
        .collect::<Vec<_>>();
    assert_eq!(
        rejected.iter().map(|r| r.ordinal).collect::<Vec<_>>(),
        vec![1, 2]
    );
    assert_eq!(
        rejected[0].excerpt,
        &rejected_raw[..excerpt.min(rejected_raw.len())]
    );
    assert_eq!(rejected[1].excerpt, b"\xff");
    drop((page, _writer, completion.value));
    worker.shutdown().await.unwrap();
    task.join().await.unwrap();
    let store =
        DatasetStore::open(&path, Durability::FileAndDirectory, StoreLimits::default()).unwrap();
    assert_eq!(
        wes_engine::storage::datasets::DatasetStorage::coverage_page(&store, &reference, request)
            .unwrap()
            .rows
            .len(),
        2
    );
}

#[tokio::test]
async fn immutable_dataset_pages_renew_exact_read_demand_without_repeating_callbacks() {
    paged_read_renewal(None, false, false, SourceRowBound::Fits).await;
}
#[tokio::test]
async fn an_oversized_source_row_preserves_the_valid_prefix_at_the_exact_ordinal() {
    paged_read_renewal(None, false, false, SourceRowBound::Logical).await;
}
#[tokio::test]
async fn a_source_encoded_row_limit_preserves_prefix_and_reports_its_frozen_byte_cap() {
    paged_read_renewal(None, false, false, SourceRowBound::Encoded).await;
}
#[tokio::test]
async fn immutable_dataset_reads_stop_on_real_absolute_or_earned_work_limits() {
    use wes_engine::scan::ledger::Dimension;
    paged_read_renewal(Some(Dimension::Work), false, false, SourceRowBound::Fits).await;
    paged_read_renewal(
        Some(Dimension::WorkAllowance),
        false,
        false,
        SourceRowBound::Fits,
    )
    .await;
}
#[tokio::test]
async fn cancellation_after_a_joined_read_refusal_does_not_enter_another_read() {
    paged_read_renewal(None, true, false, SourceRowBound::Fits).await;
}
#[derive(Clone, Copy, PartialEq, Eq)]
enum SourceRowBound {
    Fits,
    Logical,
    Encoded,
}
async fn paged_read_renewal(
    exhausted: Option<wes_engine::scan::ledger::Dimension>,
    cancel: bool,
    withdraw: bool,
    row_bound: SourceRowBound,
) {
    use wes_engine::{
        driver::CancellationToken,
        scan::{Identity, Input, Phase, Poll, Runner, Settings, Transition, ledger::MemoryPool},
        storage::{StoreError, datasets::*},
    };
    use wes_language::{Expression, SourceText, Span, templates::Templates};
    let tmp = home();
    let datasets = DatasetStore::open(
        &tmp.path().join("datasets"),
        Durability::FileAndDirectory,
        StoreLimits::default(),
    )
    .unwrap();
    let values = wes_adapters::storage::TieredValues::open(
        &tmp.path().join("live"),
        &tmp.path().join("archive"),
        wes_adapters::codec::Limits::default(),
        Durability::FileAndDirectory,
        None,
    )
    .unwrap();
    let (worker, task) = spawn_storage(values, datasets, StoreWorkerLimits::default()).unwrap();
    let schema = ResolvedContractBundle::capture(
        ContractRegistry::new().resolve("Text").unwrap(),
        SnapshotLimits::default(),
    )
    .unwrap();
    let extent = |end| SourceExtent {
        identity: "synthetic-paged-source".into(),
        unit: SourceUnit::Records,
        start: 0,
        end,
    };
    let admitted = worker
        .dataset_create(DatasetCreate {
            coverage: None,
            owner: None,
            checkpoint: None,
            recording: None,
            dataset: uuid::Uuid::new_v4().to_string(),
            transaction: uuid::Uuid::new_v4().to_string(),
            kind: wes_engine::storage::datasets::DatasetKind::Analysis,
            schema,
            source: extent(0),
            policy: FlowPolicy::default(),
        })
        .await
        .unwrap();
    let mut source = admitted.reference;
    for n in 0..3 {
        source = worker
            .enqueue_dataset_append(DatasetAppend {
                coverage: vec![],
                owner: None,
                checkpoint: None,
                recording: None,
                previous: source,
                transaction: uuid::Uuid::new_v4().to_string(),
                rows: vec![DatasetRow {
                    ordinal: n,
                    source_start: n,
                    source_end: n + 1,
                    value: Value::new(
                        Shape::Primitive(Primitive::Text),
                        Data::Text(
                            format!(
                                "{n}:{}",
                                "x".repeat(if row_bound != SourceRowBound::Fits && n == 2 {
                                    14000
                                } else {
                                    4000
                                })
                            )
                            .into(),
                        ),
                        Provenance::default(),
                    )
                    .unwrap(),
                }],
                source: extent(n + 1),
                lifecycle: if n == 2 {
                    DatasetLifecycle::Sealed
                } else {
                    DatasetLifecycle::Open
                },
                policy: FlowPolicy::default(),
            })
            .await
            .unwrap()
            .wait()
            .await
            .unwrap();
    }
    let span = Span::at(0);
    let mut types = ContractRegistry::new();
    types
        .load("types: {NumberStep: {base: Record, fields: {state: Int, outputs: 'List<Int>'}}}")
        .unwrap();
    let mut templates = Templates::new();
    let parsed = wes_language::parse(&SourceText::new(
        "synthetic-paged-read",
        ":def step(state:Int, context:Int, item:Text) -> NumberStep as :calc pure { return {state:state+1,outputs:[state+1]}; }",
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
        panic!("definition")
    };
    templates
        .define_calculation(
            syntax,
            &types,
            &Default::default(),
            wes_language::calc::Package::standard(),
        )
        .unwrap();
    let step =
        Transition::capture(&templates.snapshot()["step"], false, 4 * 1024 * 1024, span).unwrap();
    let mut settings = Settings::capture();
    settings.startup_work = 8_000_000;
    settings.record_charge = 65536;
    settings.state_charge = 4096;
    settings.context_charge = 4096;
    settings.scratch.bytes = 65536;
    settings.scratch.work = 16384;
    settings.block = 64;
    settings.page_rows = 32;
    if row_bound == SourceRowBound::Encoded {
        // All three logical rows must fit this window so only encoded bytes truncate it.
        settings.record_charge = 262144;
        settings.page_bytes = 10000;
    }
    if let Some(dim) = exhausted {
        settings.startup_work = 200000;
        if dim == wes_engine::scan::ledger::Dimension::Work {
            settings.limits.work = settings.startup_work;
        }
    }
    let pool = MemoryPool::new(settings.limits.memory_bytes).unwrap();
    let token = CancellationToken::new();
    let mut runner = Runner::new(
        Input {
            live: false,
            source: Value::new(
                Shape::Dataset(Box::new(Shape::Primitive(Primitive::Text))),
                Data::Dataset(source.clone().into()),
                Provenance::default(),
            )
            .unwrap(),
            initial: scalar(0),
            context: scalar(0),
            step,
            finish: None,
            framing: None,
            identity: Identity {
                analysis: uuid::Uuid::new_v4().to_string(),
                source: None,
                profile: "TypedRecords".into(),
                profile_revision: format!("sha256:{}", "1".repeat(64)),
            },
            control: None,
        },
        settings,
        &pool,
        None,
        token.clone(),
        span,
    )
    .unwrap();
    let captured = worker
        .capture_scan_source(runner.captured_source())
        .await
        .unwrap();
    let mut create = runner.dataset_admission().unwrap();
    create.checkpoint = Some(runner.checkpoint_seed(captured).unwrap());
    let admission = worker.dataset_create(create).await.unwrap();
    let _writer = admission.lease;
    let mut reference = admission.reference;
    let cp = worker
        .dataset_checkpoint(reference.clone())
        .await
        .unwrap()
        .unwrap();
    runner.attach_durable(reference.clone(), cp).unwrap();
    let mut read_refusals = 0;
    let mut settlements = 0;
    let mut logical_pages = 0;
    let mut byte_pages = 0;
    let mut row_refusals = 0;
    let mut read_minimum = None;
    let mut preferred_grants = 0;
    for _ in 0..1000 {
        match runner.poll() {
            Poll::Yield => {}
            Poll::ReadHead => panic!("finite source never discovers a moving head"),
            Poll::ReadPage => {
                let before = runner.progress();
                let (selected, request) = runner.source_page_request().unwrap();
                assert_eq!(selected, source);
                match worker.dataset_page(selected, request).await {
                    Ok(page) => {
                        if page.limited_by == Some("logical charge") {
                            logical_pages += 1;
                            assert_eq!(page.first, 0);
                            assert_eq!(page.rows.len(), 2);
                            assert_eq!(page.next, 2);
                        }
                        if page.limited_by == Some("bytes") {
                            byte_pages += 1;
                            assert_eq!(page.next, 2);
                        }
                        runner.acknowledge_source_page(page).unwrap();
                        read_minimum = None;
                    }
                    Err(error) => {
                        if let StoreError::ReadWork(refused) = &error {
                            read_refusals += 1;
                            if exhausted.is_none() {
                                assert_eq!(refused.dimension(), ReadWorkDimension::Prepaid);
                            }
                            if refused.dimension() == ReadWorkDimension::Prepaid {
                                read_minimum = Some(refused.needed() + 4096);
                            }
                        } else if withdraw {
                            assert!(matches!(error, StoreError::DatasetWithdrawn));
                        } else if row_bound != SourceRowBound::Fits {
                            match row_bound {
                                SourceRowBound::Logical => assert!(matches!(
                                    error,
                                    StoreError::DatasetRowCharge { limit: 65536 }
                                )),
                                SourceRowBound::Encoded => assert!(matches!(
                                    error,
                                    StoreError::DatasetRowBytes { limit: 10000 }
                                )),
                                SourceRowBound::Fits => unreachable!(),
                            }
                            row_refusals += 1;
                            assert_eq!(before.committed_position, 2);
                            assert_eq!(before.usage.input_records, 2);
                        } else {
                            panic!("unexpected read failure: {error:?}")
                        }
                        assert_eq!(
                            runner.progress().committed_position,
                            before.committed_position
                        );
                        assert_eq!(
                            runner.progress().usage.input_records,
                            before.usage.input_records
                        );
                        runner.refuse_source_read(error);
                        if cancel {
                            token.cancel();
                        }
                    }
                }
            }
            Poll::Settle => {
                settlements += 1;
                let before = worker
                    .dataset_checkpoint(reference.clone())
                    .await
                    .unwrap()
                    .unwrap();
                let request = runner.durable_settlement().unwrap();
                let after = request.checkpoint.as_ref().unwrap();
                assert_eq!(after.next_position, before.next_position);
                assert_eq!(after.state.data(), before.state.data());
                assert_eq!(after.usage.input_bytes, before.usage.input_bytes);
                assert_eq!(after.usage.work_allowance, before.usage.work_allowance);
                assert!(after.work.charged >= before.work.charged);
                reference = worker
                    .enqueue_dataset_append(request)
                    .await
                    .unwrap()
                    .wait()
                    .await
                    .unwrap();
                runner.acknowledge_settlement(reference.clone()).unwrap();
            }
            Poll::Grant => {
                let request = runner.durable_grant().unwrap();
                let cp = request.checkpoint.clone().unwrap();
                if let Some(minimum) = read_minimum {
                    let available = (cp.budget.totals.work - cp.work.charged)
                        .min(cp.usage.work_allowance - cp.work.charged);
                    assert!(cp.work.outstanding >= minimum);
                    assert!(cp.work.outstanding <= available);
                    if available >= minimum * 2 {
                        assert!(cp.work.outstanding >= minimum * 2);
                        preferred_grants += 1;
                    } else {
                        assert_eq!(cp.work.outstanding, available);
                    }
                }
                reference = worker
                    .enqueue_dataset_append(request)
                    .await
                    .unwrap()
                    .wait()
                    .await
                    .unwrap();
                runner.acknowledge_grant(reference.clone(), cp).unwrap();
                if withdraw && read_refusals == 1 {
                    worker.dataset_withdraw(source.clone()).await.unwrap();
                }
            }
            Poll::Commit => {
                reference = worker
                    .enqueue_dataset_append(runner.dataset_candidate().unwrap())
                    .await
                    .unwrap()
                    .wait()
                    .await
                    .unwrap();
                runner.acknowledge_dataset(reference.clone()).unwrap();
            }
            Poll::Terminal => break,
        }
    }
    let request = runner.durable_terminal_update().unwrap();
    if let Some(request) = request {
        let cp = request.checkpoint.clone().unwrap();
        let written = worker
            .enqueue_dataset_append(request)
            .await
            .unwrap()
            .wait()
            .await;
        if withdraw {
            let error = written.unwrap_err();
            assert!(matches!(error, StoreError::DatasetWithdrawn));
            runner.refuse_dataset(error.to_string());
        } else {
            reference = written.unwrap();
            runner.acknowledge_terminal(reference.clone(), cp).unwrap();
        }
    }
    let done = runner.into_completion().unwrap();
    if let Some(dimension) = exhausted {
        assert_eq!(done.progress.phase, Phase::Stopped);
        assert_eq!(done.stop.as_ref().unwrap().dimension, Some(dimension));
        assert_eq!(done.progress.usage.input_records, 0);
        assert_eq!(settlements, 0);
    } else if withdraw {
        assert_eq!(done.progress.phase, Phase::Stopped);
        assert_eq!(done.stop.as_ref().unwrap().dimension, None);
        assert_eq!(read_refusals, 1);
        assert_eq!(done.progress.usage.input_records, 0);
    } else if cancel {
        assert_eq!(done.progress.phase, Phase::Cancelled);
        assert_eq!(done.progress.usage.input_records, 0);
        assert_eq!(settlements, 0);
    } else if row_bound != SourceRowBound::Fits {
        assert_eq!(done.progress.phase, Phase::Stopped);
        assert_eq!(
            done.stop.as_ref().unwrap().dimension,
            Some(if row_bound == SourceRowBound::Logical {
                wes_engine::scan::ledger::Dimension::RecordMemory
            } else {
                wes_engine::scan::ledger::Dimension::SourcePageBytes
            })
        );
        assert_eq!(row_refusals, 1);
        assert_eq!(
            logical_pages,
            usize::from(row_bound == SourceRowBound::Logical)
        );
        assert_eq!(
            byte_pages,
            usize::from(row_bound == SourceRowBound::Encoded)
        );
        assert_eq!(done.progress.committed_position, 2);
        assert_eq!(done.progress.usage.input_records, 2);
        assert_eq!(done.progress.usage.output_records, 2);
        let Data::Record(result) = done.value.data() else {
            panic!("result")
        };
        assert_eq!(result["state"], Data::Int(2));
        let Data::Record(receipt) = &result["receipt"] else {
            panic!("receipt")
        };
        let Data::Record(limits) = &receipt["limits"] else {
            panic!("limits")
        };
        assert_eq!(limits["pageBytes"], Data::Int(settings.page_bytes as i64));
        let page = worker
            .dataset_page(
                reference.clone(),
                PageRequest {
                    charge: None,
                    from: 0,
                    rows: 10,
                    bytes: 65536,
                    segments: 8,
                    work: None,
                },
            )
            .await
            .unwrap();
        assert_eq!(
            page.rows
                .iter()
                .map(|r| r.value.data().clone())
                .collect::<Vec<_>>(),
            vec![Data::Int(1), Data::Int(2)]
        );
        let checkpoint = worker
            .dataset_checkpoint(reference.clone())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(checkpoint.next_ordinal, 2);
        assert_eq!(checkpoint.state.data(), &Data::Int(2));
        assert_eq!(checkpoint.work.outstanding, 0);
    } else {
        assert_eq!(done.progress.phase, Phase::Complete, "{:?}", done.stop);
        assert!(
            read_refusals >= 2,
            "must cross both manifest/index and segment work leases"
        );
        assert!(settlements >= 2);
        assert_eq!(done.progress.usage.input_records, 3);
        assert_eq!(done.progress.usage.output_records, 3);
        assert_eq!(logical_pages, 1);
        assert!(
            preferred_grants > 0,
            "actual read renewal must use the measured preference"
        );
        let Data::Record(result) = done.value.data() else {
            panic!("result")
        };
        assert_eq!(result["state"], Data::Int(3));
        let page = worker
            .dataset_page(
                reference.clone(),
                PageRequest {
                    charge: None,
                    from: 0,
                    rows: 10,
                    bytes: 65536,
                    segments: 8,
                    work: None,
                },
            )
            .await
            .unwrap();
        assert_eq!(
            page.rows
                .iter()
                .map(|r| r.value.data().clone())
                .collect::<Vec<_>>(),
            vec![Data::Int(1), Data::Int(2), Data::Int(3)]
        );
        assert_eq!(
            worker
                .dataset_checkpoint(reference)
                .await
                .unwrap()
                .unwrap()
                .work
                .outstanding,
            0
        );
    }
    drop((done, _writer, admitted.lease));
    worker.shutdown().await.unwrap();
    task.join().await.unwrap();
}

#[tokio::test]
async fn a_read_retry_rechecks_withdrawal_and_does_not_renew_an_access_failure() {
    paged_read_renewal(None, false, true, SourceRowBound::Fits).await;
}

#[tokio::test]
async fn eventlog_head_schema_refusals_preserve_typed_work_and_renew_at_the_same_boundary() {
    use wes_engine::{
        driver::CancellationToken,
        scan::{Identity, Input, Poll, Runner, Settings, Transition, ledger::MemoryPool},
        storage::{StoreError, datasets::*},
    };
    use wes_language::{Expression, SourceText, Span, templates::Templates};
    let tmp = home();
    let datasets = DatasetStore::open(
        &tmp.path().join("datasets"),
        Durability::FileAndDirectory,
        StoreLimits::default(),
    )
    .unwrap();
    let values = wes_adapters::storage::TieredValues::open(
        &tmp.path().join("live"),
        &tmp.path().join("archive"),
        wes_adapters::codec::Limits::default(),
        Durability::FileAndDirectory,
        None,
    )
    .unwrap();
    let (worker, task) = spawn_storage(values, datasets, StoreWorkerLimits::default()).unwrap();
    let dataset = uuid::Uuid::new_v4().to_string();
    let run = uuid::Uuid::new_v4().to_string();
    let admitted = worker
        .dataset_create(DatasetCreate {
            dataset: dataset.clone(),
            transaction: uuid::Uuid::new_v4().to_string(),
            kind: wes_engine::storage::datasets::DatasetKind::EventLog,
            coverage: None,
            checkpoint: None,
            owner: Some(DatasetWriteOwner {
                role: DatasetWriteRole::Recording,
                lineage: run.clone(),
                run: run.clone(),
            }),
            schema: ResolvedContractBundle::capture(
                ContractRegistry::new().resolve("Int").unwrap(),
                SnapshotLimits::default(),
            )
            .unwrap(),
            source: SourceExtent {
                identity: dataset.clone(),
                unit: SourceUnit::Records,
                start: 0,
                end: 0,
            },
            policy: FlowPolicy::default(),
            recording: Some(EventLogCoverage {
                run,
                epoch: dataset,
                first: 1,
                accepted_through: 0,
                committed_through: 0,
                pending: Some(0),
                rejected: 0,
                termination: None,
            }),
        })
        .await
        .unwrap();
    let source = admitted.reference;
    let mut types = ContractRegistry::new();
    types
        .load("types: {NumberStep: {base: Record, fields: {state: Int, outputs: 'List<Int>'}}}")
        .unwrap();
    let mut templates = Templates::new();
    let parsed = wes_language::parse(&SourceText::new(
        "synthetic-head-read",
        ":def step(state:Int, context:Int, item:Int) -> NumberStep as :calc pure { return {state:state+1,outputs:[item]}; }",
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
        panic!("definition")
    };
    templates
        .define_calculation(
            syntax,
            &types,
            &Default::default(),
            wes_language::calc::Package::standard(),
        )
        .unwrap();
    let span = Span::at(0);
    let mut settings = Settings::capture();
    settings.startup_work = 8_000_000;
    settings.record_charge = 4096;
    settings.state_charge = 4096;
    settings.scratch.bytes = 1;
    settings.scratch.work = 1;
    settings.block = 1;
    let token = CancellationToken::new();
    let pool = MemoryPool::new(settings.limits.memory_bytes).unwrap();
    let mut runner = Runner::new(
        Input {
            live: false,
            source: Value::new(
                Shape::Dataset(Box::new(Shape::Primitive(Primitive::Int))),
                Data::Dataset(source.clone().into()),
                Provenance::default(),
            )
            .unwrap(),
            initial: scalar(0),
            context: scalar(0),
            step: Transition::capture(&templates.snapshot()["step"], false, 4 * 1024 * 1024, span)
                .unwrap(),
            finish: None,
            framing: None,
            identity: Identity {
                analysis: uuid::Uuid::new_v4().to_string(),
                source: None,
                profile: "TypedRecords".into(),
                profile_revision: format!("sha256:{}", "1".repeat(64)),
            },
            control: None,
        },
        settings,
        &pool,
        None,
        token.clone(),
        span,
    )
    .unwrap();
    let capture = worker
        .capture_scan_source(runner.captured_source())
        .await
        .unwrap();
    let mut create = runner.dataset_admission().unwrap();
    create.checkpoint = Some(runner.checkpoint_seed(capture).unwrap());
    let analysis = worker.dataset_create(create).await.unwrap();
    let mut reference = analysis.reference;
    let checkpoint = worker
        .dataset_checkpoint(reference.clone())
        .await
        .unwrap()
        .unwrap();
    runner
        .attach_durable(reference.clone(), checkpoint)
        .unwrap();
    let mut needed = 0;
    let mut refusals = 0;
    for _ in 0..16 {
        if runner.needs_durable_grant() {
            let grant = runner.durable_grant().unwrap();
            let cp = grant.checkpoint.clone().unwrap();
            reference = worker
                .enqueue_dataset_append(grant)
                .await
                .unwrap()
                .wait()
                .await
                .unwrap();
            runner.acknowledge_grant(reference.clone(), cp).unwrap();
        }
        let (_, request) = runner.source_page_request().unwrap();
        match worker.eventlog_head(source.clone(), request.work).await {
            Ok(head) => {
                assert_eq!(head.reference, source);
                assert_eq!(head.recording.unwrap().committed_through, 0);
                assert!(refusals >= 1, "small lease must refuse a real head granule");
                break;
            }
            Err(StoreError::ReadWork(refused)) => {
                assert_eq!(refused.dimension(), ReadWorkDimension::Prepaid);
                assert!(refused.needed() > needed);
                needed = refused.needed();
                refusals += 1;
                runner.refuse_source_read(StoreError::ReadWork(refused));
                assert!(matches!(runner.poll(), Poll::Settle));
                let settlement = runner.durable_settlement().unwrap();
                let cp = settlement.checkpoint.as_ref().unwrap();
                assert_eq!(cp.next_position, 0);
                assert_eq!(cp.next_ordinal, 0);
                assert_eq!(cp.state.data(), &Data::Int(0));
                reference = worker
                    .enqueue_dataset_append(settlement)
                    .await
                    .unwrap()
                    .wait()
                    .await
                    .unwrap();
                runner.acknowledge_settlement(reference.clone()).unwrap();
            }
            Err(error) => panic!("head work must not become corruption: {error:?}"),
        }
    }
    assert!(refusals < 16);
    assert_eq!(runner.progress().usage.input_records, 0);
    assert_eq!(runner.progress().usage.output_records, 0);
    assert!(
        worker
            .dataset_plan_delete(source)
            .await
            .unwrap()
            .active_writer,
        "reading never stops a producer's recording"
    );
    token.cancel();
    assert!(matches!(runner.poll(), Poll::Terminal));
    drop((runner, analysis.lease, admitted.lease));
    worker.shutdown().await.unwrap();
    task.join().await.unwrap();
}
