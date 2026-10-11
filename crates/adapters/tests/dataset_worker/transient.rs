use super::*;
use wes_adapters::storage::TieredValues;
use wes_engine::{
    driver::CancellationToken,
    scan::{Identity, Input, Poll, Runner, Settings, Transition, ledger::MemoryPool},
    storage::{AutoKeep, PublicationPolicy, StoreWorker, datasets::*},
};
use wes_language::{Expression, SourceText, Span, templates::Templates};

fn values(root: &std::path::Path) -> TieredValues {
    TieredValues::open(
        &root.join("live"),
        &root.join("archive"),
        wes_adapters::codec::Limits::default(),
        Durability::FileAndDirectory,
        None,
    )
    .unwrap()
}
fn datasets(root: &std::path::Path) -> DatasetStore {
    DatasetStore::open(
        &root.join("datasets"),
        Durability::FileAndDirectory,
        StoreLimits::default(),
    )
    .unwrap()
}
fn page() -> PageRequest {
    PageRequest {
        charge: None,
        work: None,
        from: 0,
        rows: 10,
        bytes: 1048576,
        segments: 8,
    }
}
fn descriptor(reference: &wes_core::DatasetRef) -> Value {
    Value::new(
        Shape::Dataset(Box::new(Shape::Primitive(Primitive::Int))),
        Data::Dataset(reference.clone().into()),
        Provenance::default(),
    )
    .unwrap()
}
fn create() -> DatasetCreate {
    DatasetCreate {
        owner: None,
        checkpoint: None,
        recording: None,
        coverage: None,
        dataset: uuid::Uuid::new_v4().to_string(),
        transaction: uuid::Uuid::new_v4().to_string(),
        kind: wes_engine::storage::datasets::DatasetKind::Analysis,
        schema: ResolvedContractBundle::capture(
            ContractRegistry::new().resolve("Int").unwrap(),
            SnapshotLimits::default(),
        )
        .unwrap(),
        source: SourceExtent {
            identity: uuid::Uuid::new_v4().to_string(),
            unit: SourceUnit::Records,
            start: 0,
            end: 0,
        },
        policy: FlowPolicy::default(),
    }
}
async fn append(
    worker: &StoreWorker,
    previous: wes_core::DatasetRef,
    source: SourceExtent,
    count: u64,
    sealed: bool,
) -> wes_core::DatasetRef {
    let first = previous.records();
    worker
        .enqueue_dataset_append(DatasetAppend {
            coverage: vec![],
            owner: None,
            checkpoint: None,
            recording: None,
            previous,
            transaction: uuid::Uuid::new_v4().to_string(),
            rows: (first..count)
                .map(|n| DatasetRow {
                    ordinal: n,
                    source_start: n,
                    source_end: n + 1,
                    value: scalar(n as i64),
                })
                .collect(),
            source: SourceExtent {
                end: count,
                ..source
            },
            lifecycle: if sealed {
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
        .unwrap()
}

#[tokio::test]
async fn saved_prefix_remains_exact_after_later_unsaved_suffix_close_and_reopen() {
    let root = home();
    let (worker, task) = spawn_storage(
        values(root.path()),
        datasets(root.path()),
        StoreWorkerLimits::default(),
    )
    .unwrap();
    let scope = worker.transient_scope().unwrap();
    let request = create();
    let source = request.source.clone();
    let admitted = scope.dataset_create(request).await.unwrap();
    let a = append(&scope, admitted.reference, source.clone(), 2, false).await;
    let output = scope
        .publish(descriptor(&a), PublicationPolicy::Temporary)
        .await
        .unwrap();
    assert!(!output.kept);
    assert!(worker.read(output.handle.clone()).await.is_err());
    assert!(worker.dataset_page(a.clone(), page()).await.is_err());
    assert!(
        scope
            .publish(scalar(3), AutoKeep::default().into())
            .await
            .is_err()
    );
    assert!(scope.retain(output.handle.clone()).await.unwrap().kept);
    let b = append(&scope, a.clone(), source, 4, true).await;
    let unsaved = scope
        .publish(descriptor(&b), PublicationPolicy::Temporary)
        .await
        .unwrap();
    let ordinary = worker
        .publish(scalar(11), AutoKeep::default().into())
        .await
        .unwrap();
    assert!(ordinary.kept);
    drop(admitted.lease);
    scope.close_transient_scope().await.unwrap();
    scope.close_transient_scope().await.unwrap();
    assert!(scope.read(unsaved.handle.clone()).await.is_err());
    assert!(worker.dataset_page(b.clone(), page()).await.is_err());
    assert_eq!(
        worker
            .dataset_page(a.clone(), page())
            .await
            .unwrap()
            .rows
            .len(),
        2
    );
    worker.shutdown().await.unwrap();
    task.join().await.unwrap();
    let (worker, task) = spawn_storage(
        values(root.path()),
        datasets(root.path()),
        StoreWorkerLimits::default(),
    )
    .unwrap();
    assert_eq!(
        worker
            .recover(output.handle)
            .await
            .unwrap()
            .unwrap()
            .loaded
            .value
            .data(),
        descriptor(&a).data()
    );
    assert!(worker.recover(unsaved.handle).await.unwrap().is_none());
    assert!(worker.dataset_page(b, page()).await.is_err());
    assert_eq!(worker.dataset_page(a, page()).await.unwrap().rows.len(), 2);
    assert!(worker.recover(ordinary.handle).await.unwrap().is_some());
    worker.shutdown().await.unwrap();
    task.join().await.unwrap();
}

async fn scan(worker: &StoreWorker) -> (Value, AnalysisCheckpoint) {
    let span = Span::at(0);
    let mut types = ContractRegistry::new();
    types
        .load("types: {IntegerStep: {base: Record, fields: {state: Int, outputs: 'List<Int>'}}}")
        .unwrap();
    let parsed = wes_language::parse(&SourceText::new(
        "scope-scan",
        ":def step(state:Int, context:Int, item:Int) -> IntegerStep as :calc pure { return {state:state+1,outputs:[item]}; }",
    ));
    assert!(parsed.diagnostics.is_empty());
    let Expression::Definition(definition) = parsed
        .script
        .statements
        .into_iter()
        .next()
        .unwrap()
        .expression
    else {
        panic!("calculation")
    };
    let mut templates = Templates::new();
    templates
        .define_calculation(
            definition,
            &types,
            &Default::default(),
            wes_language::calc::Package::standard(),
        )
        .unwrap();
    let settings = Settings::capture();
    let pool = MemoryPool::new(settings.limits.memory_bytes).unwrap();
    let mut runner = Runner::new(
        Input {
            live: false,
            source: Value::new(
                Shape::List(Box::new(Shape::Primitive(Primitive::Int))),
                Data::List(vec![Data::Int(6), Data::Int(7)]),
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
        CancellationToken::new(),
        span,
    )
    .unwrap();
    let capture = worker
        .capture_scan_source(runner.captured_source())
        .await
        .unwrap();
    assert!(
        !worker
            .is_kept(wes_engine::storage::ValueHandle::new(&capture.handle).unwrap())
            .await
            .unwrap()
    );
    let mut request = runner.dataset_admission().unwrap();
    request.checkpoint = Some(runner.checkpoint_seed(capture).unwrap());
    let admitted = worker.dataset_create(request).await.unwrap();
    let mut reference = admitted.reference;
    let checkpoint = worker
        .dataset_checkpoint(reference.clone())
        .await
        .unwrap()
        .unwrap();
    runner
        .attach_durable(reference.clone(), checkpoint)
        .unwrap();
    loop {
        let poll = runner.poll();
        match poll {
            Poll::Yield => {}
            Poll::Terminal => break,
            Poll::ReadPage | Poll::ReadHead => panic!("inline input"),
            Poll::Grant | Poll::Settle | Poll::Commit => {
                let request = match poll {
                    Poll::Grant => runner.durable_grant().unwrap(),
                    Poll::Settle => runner.durable_settlement().unwrap(),
                    _ => runner.dataset_candidate().unwrap(),
                };
                let checkpoint = request.checkpoint.clone().unwrap();
                reference = worker
                    .enqueue_dataset_append(request)
                    .await
                    .unwrap()
                    .wait()
                    .await
                    .unwrap();
                match poll {
                    Poll::Grant => runner
                        .acknowledge_grant(reference.clone(), checkpoint)
                        .unwrap(),
                    Poll::Settle => runner.acknowledge_settlement(reference.clone()).unwrap(),
                    _ => runner.acknowledge_dataset(reference.clone()).unwrap(),
                }
            }
        }
    }
    let checkpoint = worker.dataset_checkpoint(reference).await.unwrap().unwrap();
    let completion = runner.into_completion().unwrap();
    drop(admitted.lease);
    (completion.value, checkpoint)
}

#[tokio::test]
async fn reopening_never_renews_an_unsaved_scope_even_when_its_disk_bytes_remain() {
    let root = home();
    let (worker, task) = spawn_storage(
        values(root.path()),
        datasets(root.path()),
        StoreWorkerLimits::default(),
    )
    .unwrap();
    let scope = worker.transient_scope().unwrap();
    let admitted = scope.dataset_create(create()).await.unwrap();
    let reference = admitted.reference.clone();
    let output = scope
        .publish(descriptor(&reference), PublicationPolicy::Temporary)
        .await
        .unwrap();
    // Stop only the process owner, leaving the temporary catalog intact as a crash would.
    worker.shutdown().await.unwrap();
    task.join().await.unwrap();
    drop(admitted.lease);
    let (worker, task) = spawn_storage(
        values(root.path()),
        datasets(root.path()),
        StoreWorkerLimits::default(),
    )
    .unwrap();
    assert!(worker.recover(output.handle).await.unwrap().is_none());
    assert!(
        worker
            .dataset_page(reference.clone(), page())
            .await
            .is_err()
    );
    assert!(
        worker
            .transient_scope()
            .unwrap()
            .dataset_page(reference, page())
            .await
            .is_err()
    );
    assert!(worker.dataset_collect().await.unwrap().complete);
    worker.shutdown().await.unwrap();
    task.join().await.unwrap();
}

#[tokio::test]
async fn an_independently_kept_capture_survives_retiring_its_temporary_checkpoint() {
    let root = home();
    let (worker, task) = spawn_storage(
        values(root.path()),
        datasets(root.path()),
        StoreWorkerLimits::default(),
    )
    .unwrap();
    let scope = worker.transient_scope().unwrap();
    let (_, checkpoint) = scan(&scope).await;
    let capture = wes_engine::storage::ValueHandle::new(&checkpoint.source.handle).unwrap();
    let exact = scope.read(capture.clone()).await.unwrap().unwrap().value;
    assert!(scope.retain(capture.clone()).await.unwrap().kept);
    scope.close_transient_scope().await.unwrap();
    worker.shutdown().await.unwrap();
    task.join().await.unwrap();
    let (worker, task) = spawn_storage(
        values(root.path()),
        datasets(root.path()),
        StoreWorkerLimits::default(),
    )
    .unwrap();
    assert_eq!(
        worker.recover(capture).await.unwrap().unwrap().loaded.value,
        exact
    );
    assert!(worker.dataset_analysis_read(checkpoint.run).await.is_err());
    worker.shutdown().await.unwrap();
    task.join().await.unwrap();
}

#[tokio::test]
async fn checkpoint_and_capture_are_temporary_until_exact_result_keep() {
    for keep in [false, true] {
        let root = home();
        let (worker, task) = spawn_storage(
            values(root.path()),
            datasets(root.path()),
            StoreWorkerLimits::default(),
        )
        .unwrap();
        let scope = worker.transient_scope().unwrap();
        let (value, checkpoint) = scan(&scope).await;
        let capture = wes_engine::storage::ValueHandle::new(&checkpoint.source.handle).unwrap();
        let output = scope
            .publish(value, PublicationPolicy::Temporary)
            .await
            .unwrap();
        assert_eq!(
            std::fs::read_dir(root.path().join("archive"))
                .unwrap()
                .filter_map(Result::ok)
                .filter(|e| e.path().extension().is_some_and(|e| e == "json"))
                .count(),
            0
        );
        assert!(
            scope
                .dataset_analysis_read(checkpoint.run.clone())
                .await
                .is_ok()
        );
        assert!(
            worker
                .dataset_analysis_read(checkpoint.run.clone())
                .await
                .is_err()
        );
        if keep {
            assert!(scope.retain(output.handle.clone()).await.unwrap().kept);
        }
        scope.close_transient_scope().await.unwrap();
        worker.shutdown().await.unwrap();
        task.join().await.unwrap();
        let (worker, task) = spawn_storage(
            values(root.path()),
            datasets(root.path()),
            StoreWorkerLimits::default(),
        )
        .unwrap();
        assert_eq!(worker.recover(output.handle).await.unwrap().is_some(), keep);
        assert_eq!(worker.recover(capture).await.unwrap().is_some(), keep);
        assert_eq!(
            worker.dataset_analysis_read(checkpoint.run).await.is_ok(),
            keep
        );
        worker.shutdown().await.unwrap();
        task.join().await.unwrap();
    }
}
