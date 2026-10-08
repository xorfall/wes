use super::*;
use crate::datasets::catalog::RootKind;
use crate::datasets::{
    DatasetKind, IndexEntry, IndexSummary, PositionUnit, Record, SegmentHeader, SourceRange,
};
use wes_core::{
    Data, Primitive, Provenance, Shape, Value,
    contracts::{ContractRegistry, ResolvedContractBundle, SnapshotLimits},
};
#[test]
fn deletion_revalidates_roots_blocks_live_readers_and_preserves_other_shared_schema() {
    let tmp = home();
    let mut store = DatasetStore::open(
        tmp.path(),
        Durability::FileAndDirectory,
        StoreLimits::default(),
    )
    .unwrap();
    let mut first = create(&mut store);
    append(&mut store, &mut first, 3);
    first.lifecycle = Lifecycle::Sealed;
    store.commit(&first, &FlowPolicy::default()).unwrap();
    let first_ref = store.descriptor(&first.dataset).unwrap();
    let mut other = create(&mut store);
    other.schema = first.schema.clone();
    other.schema_digest = first.schema_digest.clone();
    append(&mut store, &mut other, 1);
    other.lifecycle = Lifecycle::Sealed;
    store.commit(&other, &FlowPolicy::default()).unwrap();
    let other_ref = store.descriptor(&other.dataset).unwrap();
    let transaction = uuid::Uuid::new_v4().to_string();
    let root = ReferenceRootChange {
        root: uuid::Uuid::new_v4().to_string(),
        kind: RootKind::Keep,
        owner_dataset: None,
        owner_workspace: None,
        expected_generation: 0,
        generation: 1,
        prefixes: vec![first_ref.clone()],
        captures: vec![],
        retention: RootRetention::Protected,
        transaction: transaction.clone(),
    };
    let stale = store.plan_delete_owned(&first_ref).unwrap();
    store
        .update_references(&transaction, &[root], &FlowPolicy::default())
        .unwrap();
    assert!(matches!(
        store.delete_owned(&stale.token, true, true),
        Err(DatasetError::Conflict)
    ));
    let plan = store.plan_delete_owned(&first_ref).unwrap();
    assert_eq!(plan.references.len(), 1);
    assert!(plan.protected_bytes > 0);
    assert!(store.delete_owned(&plan.token, true, false).is_err());
    let reader = store.acquire_reader(&first_ref).unwrap();
    assert!(store.delete_owned(&plan.token, true, true).is_err());
    let cloned_reader = reader.clone();
    drop(reader);
    assert!(store.delete_owned(&plan.token, true, true).is_err());
    drop(cloned_reader);
    let cleanup = store.delete_owned(&plan.token, true, true).unwrap();
    assert!(cleanup.reclaimed_bytes.unwrap() > 0);
    assert!(cleanup.shared_bytes.unwrap() > 0);
    assert_eq!(cleanup.pending_bytes, Some(0));
    assert!(cleanup.complete);
    assert!(matches!(
        store.read_exact(&first_ref),
        Err(DatasetError::Withdrawn)
    ));
    assert!(store.delete_owned(&plan.token, true, true).is_err());
    assert_eq!(
        store
            .page(&other_ref, 0, 10, PageLimits::default())
            .unwrap()
            .rows
            .len(),
        1
    );
    drop(store);
    let store = DatasetStore::open(
        tmp.path(),
        Durability::FileAndDirectory,
        StoreLimits::default(),
    )
    .unwrap();
    assert!(matches!(
        store.read_exact(&first_ref),
        Err(DatasetError::Withdrawn)
    ));
    assert_eq!(
        store
            .page(&other_ref, 0, 10, PageLimits::default())
            .unwrap()
            .rows
            .len(),
        1
    );
}
#[test]
fn coarse_withdrawal_follows_captured_source_roots_and_survives_reopen_without_payload_control() {
    let tmp = home();
    let mut store =
        DatasetStore::open(tmp.path(), Durability::File, StoreLimits::default()).unwrap();
    let mut a = create(&mut store);
    append(&mut store, &mut a, 1);
    let mut b = create(&mut store);
    append(&mut store, &mut b, 1);
    store.commit(&a, &FlowPolicy::default()).unwrap();
    store.commit(&b, &FlowPolicy::default()).unwrap();
    let a_ref = store.descriptor(&a.dataset).unwrap();
    let b_ref = store.descriptor(&b.dataset).unwrap();
    let capture = uuid::Uuid::new_v4().to_string();
    let tx = uuid::Uuid::new_v4().to_string();
    let roots = vec![
        ReferenceRootChange {
            root: capture.clone(),
            kind: RootKind::Value,
            owner_dataset: None,
            owner_workspace: None,
            expected_generation: 0,
            generation: 1,
            prefixes: vec![a_ref.clone()],
            captures: vec![],
            retention: RootRetention::Protected,
            transaction: tx.clone(),
        },
        ReferenceRootChange {
            root: uuid::Uuid::new_v4().to_string(),
            kind: RootKind::Checkpoint,
            owner_dataset: Some(b.dataset.clone()),
            owner_workspace: None,
            expected_generation: 0,
            generation: 1,
            prefixes: vec![b_ref.clone()],
            captures: vec![wes_engine::storage::datasets::CapturedValue {
                handle: capture,
                digest: format!("sha256:{}", "a".repeat(64)),
                bytes: 100,
            }],
            retention: RootRetention::Protected,
            transaction: tx.clone(),
        },
    ];
    store
        .update_references(&tx, &roots, &FlowPolicy::default())
        .unwrap();
    let before = store.physical_bytes;
    let withdrawn = store.withdraw_owned(&a_ref).unwrap();
    assert_eq!(withdrawn.len(), 2);
    let bytes = read_active(&store.catalog_dir, store.limits)
        .unwrap()
        .unwrap();
    let control = &bytes[before..];
    assert!(!String::from_utf8_lossy(control).contains("schema_digest"));
    assert!(!String::from_utf8_lossy(control).contains("origins"));
    for r in [&a_ref, &b_ref] {
        assert!(matches!(store.read_exact(r), Err(DatasetError::Withdrawn)));
    }
    drop(store);
    let reopened =
        DatasetStore::open(tmp.path(), Durability::File, StoreLimits::default()).unwrap();
    for r in [&a_ref, &b_ref] {
        assert!(matches!(
            reopened.read_exact(r),
            Err(DatasetError::Withdrawn)
        ));
    }
}

#[test]
fn acknowledged_deletion_preserves_pending_cleanup_captures_across_reopen() {
    use wes_engine::storage::{ValueHandle, datasets::DatasetStorage};
    let tmp = home();
    let mut store =
        DatasetStore::open(tmp.path(), Durability::File, StoreLimits::default()).unwrap();
    let mut manifest = create(&mut store);
    let cp = checkpoint(&store, &manifest);
    let capture = CapturedValue {
        handle: cp.bindings.source_handle.clone(),
        digest: cp.bindings.source_digest.clone(),
        bytes: cp.bindings.source_bytes,
    };
    manifest.checkpoint = Some(
        store
            .prepare()
            .unwrap()
            .publish_checkpoint(&cp, &FlowPolicy::default())
            .unwrap(),
    );
    store.commit(&manifest, &FlowPolicy::default()).unwrap();
    let reference = store.descriptor(&manifest.dataset).unwrap();
    let plan = store.plan_delete_owned(&reference).unwrap();
    // Corrupt an unreachable owned object, rather than inventing a removable
    // foreign file. Cleanup must refuse to unlink the inventory as a whole.
    let corrupt = tmp
        .path()
        .join("objects")
        .join(format!("{}.segment", uuid::Uuid::new_v4()));
    std::fs::write(&corrupt, b"synthetic invalid owned object").unwrap();
    let cleanup = store.delete_owned(&plan.token, true, true).unwrap();
    assert!(!cleanup.complete);
    assert_eq!(cleanup.pending_bytes, None);
    assert_eq!(cleanup.released_captures, vec![capture.clone()]);
    assert!(matches!(
        store.read_exact(&reference),
        Err(DatasetError::Withdrawn)
    ));
    assert!(
        !DatasetStorage::protects_value(&store, &ValueHandle::new(&capture.handle).unwrap())
            .unwrap()
    );
    drop(store);
    std::fs::remove_file(corrupt).unwrap();
    let mut store =
        DatasetStore::open(tmp.path(), Durability::File, StoreLimits::default()).unwrap();
    let retried = store.collect_owned().unwrap();
    assert!(retried.complete);
    assert!(retried.released_captures.contains(&capture));
    store.acknowledge_captures(&[capture]).unwrap();
    assert!(store.collect_owned().unwrap().released_captures.is_empty());
}

#[test]
fn retention_counts_captured_dataset_prefixes_once_instead_of_only_their_small_descriptors() {
    let tmp = home();
    let mut store =
        DatasetStore::open(tmp.path(), Durability::File, StoreLimits::default()).unwrap();
    let mut source = create(&mut store);
    append(&mut store, &mut source, 100);
    store.commit(&source, &FlowPolicy::default()).unwrap();
    let source_ref = store.descriptor(&source.dataset).unwrap();
    let source_size = store.retention_bytes(&source_ref).unwrap();
    let mut derived = create(&mut store);
    let cp = checkpoint(&store, &derived);
    let tx = uuid::Uuid::new_v4().to_string();
    let root = ReferenceRootChange {
        root: cp.bindings.source_handle.clone(),
        kind: RootKind::Value,
        owner_dataset: None,
        owner_workspace: None,
        expected_generation: 0,
        generation: 1,
        prefixes: vec![source_ref],
        captures: vec![],
        retention: RootRetention::Protected,
        transaction: tx.clone(),
    };
    store
        .update_references(&tx, &[root], &FlowPolicy::default())
        .unwrap();
    derived.checkpoint = Some(
        store
            .prepare()
            .unwrap()
            .publish_checkpoint(&cp, &FlowPolicy::default())
            .unwrap(),
    );
    store.commit(&derived, &FlowPolicy::default()).unwrap();
    let reference = store.descriptor(&derived.dataset).unwrap();
    assert!(
        store.retention_bytes(&reference).unwrap() > source_size,
        "transitive source records plus checkpoint are charged"
    );
    store.collect_owned().unwrap();
    assert!(store.retention_bytes(&reference).unwrap() > source_size);
}

#[test]
fn read_origins_survive_reopen_and_withdraw_derived_datasets_without_retention_edges() {
    use wes_engine::storage::datasets::DatasetStorage;
    let tmp = home();
    let mut store =
        DatasetStore::open(tmp.path(), Durability::File, StoreLimits::default()).unwrap();
    let mut source = create(&mut store);
    append(&mut store, &mut source, 1);
    store.commit(&source, &FlowPolicy::default()).unwrap();
    let source_ref = store.descriptor(&source.dataset).unwrap();
    let policy = FlowPolicy::default().read_from_dataset(&source_ref);
    let mut derived = create(&mut store);
    append(&mut store, &mut derived, 1);
    derived.dataset_reads = policy.dataset_reads().iter().cloned().collect();
    assert!(matches!(
        store.commit(&derived, &FlowPolicy::default()),
        Err(DatasetError::Restricted)
    ));
    store.commit(&derived, &policy).unwrap();
    let derived_ref = store.descriptor(&derived.dataset).unwrap();
    assert!(
        store.references.is_empty(),
        "read labels do not create retention roots"
    );
    drop(store);
    let mut store =
        DatasetStore::open(tmp.path(), Durability::File, StoreLimits::default()).unwrap();
    assert_eq!(
        DatasetStorage::inspect(&store, &derived_ref)
            .unwrap()
            .policy
            .dataset_reads(),
        policy.dataset_reads()
    );
    DatasetStorage::check_read_origins(&store, policy.dataset_reads()).unwrap();
    let affected = store.withdraw_owned(&source_ref).unwrap();
    assert!(affected.contains(&derived.dataset));
    assert!(DatasetStorage::check_read_origins(&store, policy.dataset_reads()).is_err());
    assert!(matches!(
        store.read_exact(&derived_ref),
        Err(DatasetError::Withdrawn)
    ));
    drop(store);
    let store = DatasetStore::open(tmp.path(), Durability::File, StoreLimits::default()).unwrap();
    assert!(matches!(
        store.read_exact(&derived_ref),
        Err(DatasetError::Withdrawn)
    ));
}

fn home() -> tempfile::TempDir {
    let mut builder = tempfile::Builder::new();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        builder.permissions(std::fs::Permissions::from_mode(0o700));
    }
    builder.tempdir().unwrap()
}
fn schema() -> ResolvedContractBundle {
    ResolvedContractBundle::capture(
        ContractRegistry::new().resolve("Int").unwrap(),
        SnapshotLimits::default(),
    )
    .unwrap()
}
fn create(store: &mut DatasetStore) -> Manifest {
    let schema = schema();
    let reference = store
        .prepare()
        .unwrap()
        .publish_schema(&schema, &FlowPolicy::default())
        .unwrap();
    Manifest {
        version: 2,
        store: store.store_id().into(),
        dataset: uuid::Uuid::new_v4().to_string(),
        kind: DatasetKind::Analysis,
        generation: 1,
        previous: None,
        ancestors: vec![],
        transaction: uuid::Uuid::new_v4().to_string(),
        schema: reference,
        schema_digest: schema.digest().into(),
        index: None,
        summary: IndexSummary {
            first: 0,
            end: 0,
            segment_bytes: 0,
        },
        source: SourceRange {
            identity: "synthetic-captured-input-r1".into(),
            unit: PositionUnit::Records,
            start: 0,
            end: 128,
        },
        lifecycle: Lifecycle::Open,
        checkpoint: None,
        recording: None,
        requested: match store.files.durability() {
            Durability::File => Persistence::FileSynced,
            Durability::FileAndDirectory => Persistence::FileAndDirectorySynced,
        },
        established: match store.files.durability() {
            Durability::File => Persistence::FileSynced,
            Durability::FileAndDirectory => Persistence::FileAndDirectorySynced,
        },
        authorization_generation: 1,
        origins: vec![],
        dataset_reads: vec![],
    }
}
fn next(store: &DatasetStore, previous: &Manifest) -> Manifest {
    let mut next = previous.clone();
    next.generation += 1;
    next.previous = Some(store.root(&previous.dataset).unwrap().unwrap().0);
    next.ancestors = store
        .ancestry_links(&next.dataset, next.generation, next.previous.as_ref())
        .unwrap();
    next.transaction = uuid::Uuid::new_v4().to_string();
    next
}
fn append(store: &mut DatasetStore, manifest: &mut Manifest, count: u64) -> ObjectRef {
    let first = manifest.summary.end;
    let source = SourceRange {
        start: first,
        end: first + count,
        ..manifest.source.clone()
    };
    let header = SegmentHeader {
        store: store.store_id().into(),
        dataset: manifest.dataset.clone(),
        schema: manifest.schema_digest.clone(),
        first,
        count,
        source: source.clone(),
    };
    let records: Vec<_> = (first..first + count)
        .map(|n| Record {
            source_start: n,
            source_end: n + 1,
            value: Value::new(
                Shape::Primitive(Primitive::Int),
                Data::Int(n as i64),
                Provenance::default(),
            )
            .unwrap(),
        })
        .collect();
    let segment = store
        .prepare()
        .unwrap()
        .publish_segment(&header, &records, &schema(), &FlowPolicy::default())
        .unwrap();
    let (index, summary) = store
        .prepare()
        .unwrap()
        .append_index(
            manifest.index.as_ref(),
            &manifest.dataset,
            IndexEntry {
                summary: IndexSummary {
                    first,
                    end: first + count,
                    segment_bytes: segment.bytes,
                },
                segment: segment.clone(),
                source,
            },
            &FlowPolicy::default(),
        )
        .unwrap();
    manifest.index = Some(index);
    manifest.summary = summary;
    segment
}
#[test]
fn segment_proofs_survive_new_leaf_revisions_but_do_not_replace_page_integrity_checks() {
    let tmp = home();
    let mut store =
        DatasetStore::open(tmp.path(), Durability::File, StoreLimits::default()).unwrap();
    let mut manifest = create(&mut store);
    let first = append(&mut store, &mut manifest, 1);
    store.commit(&manifest, &FlowPolicy::default()).unwrap();
    for _ in 0..3 {
        manifest = next(&store, &manifest);
        append(&mut store, &mut manifest, 1);
        store.commit(&manifest, &FlowPolicy::default()).unwrap();
    }
    assert_eq!(store.verified_segments.len(), 4);
    let reference = store.descriptor(&manifest.dataset).unwrap();
    let path = tmp
        .path()
        .join("objects")
        .join(format!("{}.segment", first.id));
    let mut bytes = std::fs::read(&path).unwrap();
    bytes[25] ^= 1;
    std::fs::write(&path, bytes).unwrap();
    assert!(store.page(&reference, 0, 1, PageLimits::default()).is_err());
    drop(store);
    assert!(DatasetStore::open(tmp.path(), Durability::File, StoreLimits::default()).is_err());
}
#[test]
fn copy_on_write_tree_preserves_old_generations_and_bounded_catalog_rotates() {
    let tmp = home();
    let mut limits = StoreLimits::default();
    limits.objects.index.fanout = 2;
    limits.catalog.frames = 3;
    let mut store = DatasetStore::open(tmp.path(), Durability::FileAndDirectory, limits).unwrap();
    let mut manifest = create(&mut store);
    append(&mut store, &mut manifest, 1);
    store.commit(&manifest, &FlowPolicy::default()).unwrap();
    let first_index = manifest.index.clone().unwrap();
    for _ in 0..63 {
        manifest = next(&store, &manifest);
        append(&mut store, &mut manifest, 1);
        store.commit(&manifest, &FlowPolicy::default()).unwrap();
    }
    assert_eq!(manifest.summary.end, 64);
    for ordinal in 0..64 {
        let entry = store
            .objects()
            .locate(manifest.index.as_ref().unwrap(), &manifest.dataset, ordinal)
            .unwrap()
            .unwrap();
        assert_eq!(
            (entry.summary.first, entry.summary.end),
            (ordinal, ordinal + 1)
        );
    }
    assert!(
        store
            .objects()
            .locate(&first_index, &manifest.dataset, 1)
            .unwrap()
            .is_none()
    );
    assert!(store.frames < 3);
    assert!(store.physical_bytes <= limits.snapshot_bytes + 2 * limits.catalog.frame_bytes);
    let charged = store.charged_bytes();
    drop(store);
    let restored = DatasetStore::open(tmp.path(), Durability::FileAndDirectory, limits).unwrap();
    assert_eq!(
        restored.root(&manifest.dataset).unwrap().unwrap().1,
        manifest
    );
    assert_eq!(restored.charged_bytes(), charged);
}
#[test]
fn unconfirmed_sync_blocks_mutation_and_same_transaction_reconciles_once() {
    let tmp = home();
    let mut store = DatasetStore::open(
        tmp.path(),
        Durability::FileAndDirectory,
        StoreLimits::default(),
    )
    .unwrap();
    let mut manifest = create(&mut store);
    append(&mut store, &mut manifest, 2);
    assert!(
        matches!(store.commit_with_sync(&manifest, &FlowPolicy::default(), |_| Err(io::Error::other("synthetic catalog sync failure"))), Err(DatasetError::CommitUnconfirmed { transaction }) if transaction == manifest.transaction)
    );
    assert!(store.root(&manifest.dataset).unwrap().is_none());
    assert!(matches!(
        store.prepare(),
        Err(DatasetError::CommitUnconfirmed { .. })
    ));
    let Reconciliation::Committed(receipt) = store.reconcile(&manifest.transaction).unwrap() else {
        panic!("complete catalog frame was not recovered");
    };
    assert_eq!((receipt.records, receipt.generation), (2, 1));
    let bytes = store.charged_bytes();
    assert_eq!(
        store.commit(&manifest, &FlowPolicy::default()).unwrap(),
        receipt
    );
    assert_eq!(store.charged_bytes(), bytes);
    let mut conflict = manifest.clone();
    conflict.authorization_generation += 1;
    assert!(matches!(
        store.commit(&conflict, &FlowPolicy::default()),
        Err(DatasetError::Conflict)
    ));
}
#[test]
fn complete_commit_without_ack_restores_but_corrupt_referenced_segment_never_rolls_back() {
    let tmp = home();
    let limits = StoreLimits::default();
    let mut store = DatasetStore::open(tmp.path(), Durability::FileAndDirectory, limits).unwrap();
    let mut manifest = create(&mut store);
    append(&mut store, &mut manifest, 1);
    store.commit(&manifest, &FlowPolicy::default()).unwrap();
    manifest = next(&store, &manifest);
    let segment = append(&mut store, &mut manifest, 1);
    let _ = store.commit_with_sync(&manifest, &FlowPolicy::default(), |_| {
        Err(io::Error::other(
            "synthetic disconnect before acknowledgement",
        ))
    });
    drop(store);
    let restored = DatasetStore::open(tmp.path(), Durability::FileAndDirectory, limits).unwrap();
    assert_eq!(
        restored
            .root(&manifest.dataset)
            .unwrap()
            .unwrap()
            .1
            .summary
            .end,
        2
    );
    drop(restored);
    let path = tmp
        .path()
        .join("objects")
        .join(format!("{}.segment", segment.id));
    let mut bytes = std::fs::read(&path).unwrap();
    *bytes.last_mut().unwrap() ^= 1;
    std::fs::write(path, bytes).unwrap();
    assert!(matches!(
        DatasetStore::open(tmp.path(), Durability::FileAndDirectory, limits),
        Err(DatasetError::StorageCorrupt)
    ));
}
#[test]
fn torn_tail_is_inert_until_explicit_reconciliation_and_complete_corruption_is_refused() {
    let tmp = home();
    let limits = StoreLimits::default();
    let mut store = DatasetStore::open(tmp.path(), Durability::FileAndDirectory, limits).unwrap();
    let manifest = create(&mut store);
    store.commit(&manifest, &FlowPolicy::default()).unwrap();
    drop(store);
    let path = tmp.path().join("catalog/active");
    let valid = std::fs::read(&path).unwrap();
    let mut torn = valid.clone();
    torn.extend_from_slice(b"WESCAT04");
    std::fs::write(&path, torn).unwrap();
    let mut restored =
        DatasetStore::open(tmp.path(), Durability::FileAndDirectory, limits).unwrap();
    assert_eq!(
        restored.root(&manifest.dataset).unwrap().unwrap().1,
        manifest
    );
    assert!(matches!(
        restored.prepare(),
        Err(DatasetError::NeedsReconciliation)
    ));
    assert_eq!(
        restored
            .reconcile(&uuid::Uuid::new_v4().to_string())
            .unwrap(),
        Reconciliation::Unknown
    );
    assert_eq!(std::fs::read(&path).unwrap(), valid);
    drop(restored);
    let mut corrupt = valid;
    *corrupt.last_mut().unwrap() ^= 1;
    std::fs::write(path, corrupt).unwrap();
    assert!(DatasetStore::open(tmp.path(), Durability::FileAndDirectory, limits).is_err());
}
#[test]
fn unknown_or_private_policy_is_rejected_before_catalog_or_object_mutation() {
    let tmp = home();
    let mut store = DatasetStore::open(
        tmp.path(),
        Durability::FileAndDirectory,
        StoreLimits::default(),
    )
    .unwrap();
    let manifest = create(&mut store);
    let before = store.charged_bytes();
    let bytes = std::fs::read(tmp.path().join("catalog/active")).unwrap();
    for policy in [
        FlowPolicy::default().private(),
        FlowPolicy::default().unknown(),
    ] {
        assert!(matches!(
            store.commit(&manifest, &policy),
            Err(DatasetError::Restricted)
        ));
    }
    assert_eq!(before, store.charged_bytes());
    assert_eq!(
        bytes,
        std::fs::read(tmp.path().join("catalog/active")).unwrap()
    );
}
#[test]
fn catalog_orphans_are_charged_and_cannot_bypass_admission_on_next_commit() {
    let tmp = home();
    let mut limits = StoreLimits::default();
    limits.catalog.frame_bytes = 2048;
    limits.objects.disk_bytes = 16384;
    let mut store = DatasetStore::open(tmp.path(), Durability::FileAndDirectory, limits).unwrap();
    let manifest = create(&mut store);
    std::fs::write(
        tmp.path().join("catalog/synthetic-orphan.pending"),
        vec![0; 16300],
    )
    .unwrap();
    assert!(matches!(
        store.commit(&manifest, &FlowPolicy::default()),
        Err(DatasetError::Limit("catalog disk"))
    ));
    assert!(store.root(&manifest.dataset).unwrap().is_none());
}

fn checkpoint(store: &DatasetStore, manifest: &Manifest) -> crate::datasets::Checkpoint {
    use crate::datasets::{
        Checkpoint, CheckpointBindings, CheckpointLimits, InlineSnapshot, WorkLedger,
    };
    let scalar = Value::new(
        Shape::Primitive(Primitive::Int),
        Data::Int(0),
        Provenance::default(),
    )
    .unwrap();
    let inline = InlineSnapshot::capture(
        &scalar,
        manifest.schema.clone(),
        &schema(),
        CheckpointLimits::default(),
    )
    .unwrap();
    let program = ":calc pure { return {state:state,outputs:[]}; }";
    let analysis = uuid::Uuid::new_v4().to_string();
    let attempt = uuid::Uuid::new_v4().to_string();
    let mut totals = wes_engine::scan::Settings::capture().totals();
    totals.work = 1000;
    Checkpoint {
        stop: None,
        budget: wes_engine::storage::datasets::AnalysisBudget::initial(
            analysis.clone(),
            attempt.clone(),
            totals,
            wes_engine::scan::Settings::capture().totals(),
        ),
        duration: wes_engine::storage::datasets::AnalysisDuration {
            spent_ms: 0,
            outstanding_ms: if manifest.lifecycle == Lifecycle::Open {
                totals.duration_ms
            } else {
                0
            },
        },
        version: 3,
        followed_source: None,
        store: store.store_id().into(),
        dataset: manifest.dataset.clone(),
        analysis,
        attempt,
        previous_attempt: None,
        run: uuid::Uuid::new_v4().to_string(),
        transaction: manifest.transaction.clone(),
        bindings: CheckpointBindings {
            task_revision: super::super::checkpoint::digest(b"synthetic-task-r1"),
            source_handle: uuid::Uuid::new_v4().to_string(),
            source_digest: super::super::checkpoint::digest(b"synthetic-input-r1"),
            source_bytes: 18,
            source: manifest.source.clone(),
            item_schema: manifest.schema.clone(),
            item_schema_digest: manifest.schema_digest.clone(),
            output_schema: manifest.schema.clone(),
            output_schema_digest: manifest.schema_digest.clone(),
            step_revision: format!("sha256:{}", "0".repeat(64)),
            finish_revision: None,
            language_version: 1,
            native_version: "wes.calc.native.v1".into(),
            profile_digest: super::super::checkpoint::digest(b"TypedRecords-v1"),
            initial_digest: inline.value_digest.clone(),
            captured_program: program.into(),
            program_digest: super::super::checkpoint::digest(program.as_bytes()),
        },
        state: inline.clone(),
        context: inline,
        next_position: 0,
        next_ordinal: 0,
        output_end: 0,
        decoder_carry: String::new(),
        work: WorkLedger {
            charged: 0,
            outstanding: 0,
            granted: 0,
            completed: 0,
            grants: 0,
        },
        usage: wes_engine::storage::datasets::AnalysisUsage {
            work_allowance: 1000,
            ..Default::default()
        },
        finish_applied: false,
        lifecycle: manifest.lifecycle,
        origins: vec![],
        dataset_reads: vec![],
    }
}
#[test]
fn checkpoint_codec_requires_one_versioned_budget_and_rejects_old_limit_fields() {
    let tmp = home();
    let mut store = DatasetStore::open(
        tmp.path(),
        Durability::FileAndDirectory,
        StoreLimits::default(),
    )
    .unwrap();
    let manifest = create(&mut store);
    let cp = checkpoint(&store, &manifest);
    let limits = crate::datasets::CheckpointLimits::default();
    let encoded = cp.encode(limits).unwrap();
    assert_eq!(
        crate::datasets::Checkpoint::decode(&encoded, limits).unwrap(),
        cp
    );
    let raw: serde_json::Value = serde_json::from_slice(&encoded).unwrap();
    for mutation in 0..4 {
        let mut old = raw.clone();
        match mutation {
            0 => old["version"] = serde_json::json!(2),
            1 => {
                old.as_object_mut().unwrap().remove("budget");
            }
            2 => old["work"]["limit"] = serde_json::json!(1000),
            3 => old["duration"]["limit_ms"] = serde_json::json!(60000),
            _ => unreachable!(),
        }
        assert!(
            crate::datasets::Checkpoint::decode(&serde_json::to_vec(&old).unwrap(), limits)
                .is_err(),
            "no competing or legacy receipt: {mutation}"
        );
    }
}
#[test]
fn zero_output_progress_and_work_grants_share_the_catalog_root_and_survive_reopen() {
    let tmp = home();
    let limits = StoreLimits::default();
    let mut store = DatasetStore::open(tmp.path(), Durability::FileAndDirectory, limits).unwrap();
    let mut manifest = create(&mut store);
    let mut cp = checkpoint(&store, &manifest);
    manifest.checkpoint = Some(
        store
            .prepare()
            .unwrap()
            .publish_checkpoint(&cp, &FlowPolicy::default())
            .unwrap(),
    );
    store.commit(&manifest, &FlowPolicy::default()).unwrap();
    // Admission of a grant precedes execution and cannot advance state or its cursor.
    manifest = next(&store, &manifest);
    cp.transaction = manifest.transaction.clone();
    cp.work.granted = 300;
    cp.work.outstanding = 300;
    cp.work.grants = 1;
    manifest.checkpoint = Some(
        store
            .prepare()
            .unwrap()
            .publish_checkpoint(&cp, &FlowPolicy::default())
            .unwrap(),
    );
    store.commit(&manifest, &FlowPolicy::default()).unwrap();
    // A zero-output step still commits its state, position and completed work.
    manifest = next(&store, &manifest);
    cp.transaction = manifest.transaction.clone();
    cp.next_position = 1;
    cp.next_ordinal = 1;
    cp.work.completed = 75;
    cp.work.charged = 75;
    cp.work.outstanding = 0;
    let scalar = Value::new(
        Shape::Primitive(Primitive::Int),
        Data::Int(9),
        Provenance::default(),
    )
    .unwrap();
    cp.state = crate::datasets::InlineSnapshot::capture(
        &scalar,
        manifest.schema.clone(),
        &schema(),
        crate::datasets::CheckpointLimits::default(),
    )
    .unwrap();
    manifest.checkpoint = Some(
        store
            .prepare()
            .unwrap()
            .publish_checkpoint(&cp, &FlowPolicy::default())
            .unwrap(),
    );
    assert!(matches!(
        store.commit_with_sync(&manifest, &FlowPolicy::default(), |_| Err(
            io::Error::other("synthetic lost ack")
        )),
        Err(DatasetError::CommitUnconfirmed { .. })
    ));
    drop(store);
    let mut store = DatasetStore::open(tmp.path(), Durability::FileAndDirectory, limits).unwrap();
    let (_, restored) = store.root(&manifest.dataset).unwrap().unwrap();
    let recovered = store
        .objects()
        .read_checkpoint(restored.checkpoint.as_ref().unwrap(), &manifest.dataset)
        .unwrap();
    assert_eq!(
        (
            recovered.next_position,
            recovered.next_ordinal,
            recovered.output_end,
            recovered.work.granted
        ),
        (1, 1, 0, 300)
    );
    assert_eq!(
        recovered
            .state
            .value(&schema(), limits.objects.checkpoint)
            .unwrap()
            .data(),
        &Data::Int(9)
    );
    assert_eq!(
        store
            .commit(&manifest, &FlowPolicy::default())
            .unwrap()
            .generation,
        3
    );
    let mut candidate = next(&store, &manifest);
    let mut refund = recovered.clone();
    refund.transaction = candidate.transaction.clone();
    refund.work.granted = 75;
    candidate.checkpoint = Some(
        store
            .prepare()
            .unwrap()
            .publish_checkpoint(&refund, &FlowPolicy::default())
            .unwrap(),
    );
    assert!(matches!(
        store.commit(&candidate, &FlowPolicy::default()),
        Err(DatasetError::Conflict)
    ));
    assert_eq!(store.root(&manifest.dataset).unwrap().unwrap().1, manifest);
}
#[test]
fn grants_cannot_hide_outputs_and_schema_or_context_changes_cannot_resume_old_state() {
    let tmp = home();
    let mut store = DatasetStore::open(
        tmp.path(),
        Durability::FileAndDirectory,
        StoreLimits::default(),
    )
    .unwrap();
    let mut manifest = create(&mut store);
    let cp = checkpoint(&store, &manifest);
    manifest.checkpoint = Some(
        store
            .prepare()
            .unwrap()
            .publish_checkpoint(&cp, &FlowPolicy::default())
            .unwrap(),
    );
    store.commit(&manifest, &FlowPolicy::default()).unwrap();
    for changed in 0..3 {
        let mut candidate = next(&store, &manifest);
        let mut forged = cp.clone();
        forged.transaction = candidate.transaction.clone();
        match changed {
            0 => {
                forged.work.granted = 100;
                forged.work.grants = 1;
                forged.next_position = 1;
                forged.next_ordinal = 1;
            }
            1 => {
                forged.bindings.profile_digest =
                    super::super::checkpoint::digest(b"different-profile");
            }
            _ => {
                let scalar = Value::new(
                    Shape::Primitive(Primitive::Int),
                    Data::Int(1),
                    Provenance::default(),
                )
                .unwrap();
                forged.context = crate::datasets::InlineSnapshot::capture(
                    &scalar,
                    manifest.schema.clone(),
                    &schema(),
                    crate::datasets::CheckpointLimits::default(),
                )
                .unwrap();
            }
        }
        candidate.checkpoint = Some(
            store
                .prepare()
                .unwrap()
                .publish_checkpoint(&forged, &FlowPolicy::default())
                .unwrap(),
        );
        assert!(matches!(
            store.commit(&candidate, &FlowPolicy::default()),
            Err(DatasetError::Conflict)
        ));
    }
    let candidate = next(&store, &manifest);
    let mut forged = cp.clone();
    forged.transaction = candidate.transaction.clone();
    forged.state.encoded = "MQ==".into();
    assert!(
        store
            .prepare()
            .unwrap()
            .publish_checkpoint(&forged, &FlowPolicy::default())
            .is_err()
    );
}
#[test]
fn exact_empty_snapshot_keep_changes_retention_without_advancing_checkpoint() {
    let tmp = home();
    let mut store = DatasetStore::open(
        tmp.path(),
        Durability::FileAndDirectory,
        StoreLimits::default(),
    )
    .unwrap();
    let mut manifest = create(&mut store);
    let cp = checkpoint(&store, &manifest);
    manifest.checkpoint = Some(
        store
            .prepare()
            .unwrap()
            .publish_checkpoint(&cp, &FlowPolicy::default())
            .unwrap(),
    );
    store.commit(&manifest, &FlowPolicy::default()).unwrap();
    let prefix = store.descriptor(&manifest.dataset).unwrap();
    let tx = uuid::Uuid::new_v4().to_string();
    let root = ReferenceRootChange {
        root: uuid::Uuid::new_v4().to_string(),
        kind: RootKind::Keep,
        owner_dataset: None,
        owner_workspace: None,
        expected_generation: 0,
        generation: 1,
        prefixes: vec![prefix.clone()],
        captures: vec![],
        retention: RootRetention::Protected,
        transaction: tx.clone(),
    };
    store
        .update_references(&tx, &[root], &FlowPolicy::default())
        .unwrap();
    assert!(store.is_protected(&prefix));
    assert_eq!(store.descriptor(&manifest.dataset).unwrap(), prefix);
    assert_eq!(
        store.root(&manifest.dataset).unwrap().unwrap().1.checkpoint,
        manifest.checkpoint
    );
}

#[test]
fn pages_seek_original_ordinals_hold_exact_generations_and_report_each_budget_boundary() {
    let tmp = home();
    let mut store = DatasetStore::open(
        tmp.path(),
        Durability::FileAndDirectory,
        StoreLimits::default(),
    )
    .unwrap();
    let mut manifest = create(&mut store);
    append(&mut store, &mut manifest, 4);
    store.commit(&manifest, &FlowPolicy::default()).unwrap();
    let old = store.descriptor(&manifest.dataset).unwrap();
    manifest = next(&store, &manifest);
    append(&mut store, &mut manifest, 4);
    store.commit(&manifest, &FlowPolicy::default()).unwrap();
    let current = store.descriptor(&manifest.dataset).unwrap();
    let page = store.page(&current, 2, 5, PageLimits::default()).unwrap();
    assert_eq!(
        (
            page.first,
            page.next,
            page.rows.len(),
            page.extent_exhausted,
            page.limited_by
        ),
        (2, 7, 5, false, Some("rows"))
    );
    for (ordinal, row) in (2..7).zip(&page.rows) {
        assert_eq!(row.value.data(), &Data::Int(ordinal));
        assert_eq!(
            (row.source_start, row.source_end),
            (ordinal as u64, ordinal as u64 + 1)
        );
    }
    let old_page = store.page(&old, 2, 5, PageLimits::default()).unwrap();
    assert_eq!(
        (
            old_page.next,
            old_page.rows.len(),
            old_page.extent_exhausted
        ),
        (4, 2, true)
    );
    let one_segment = store
        .page(
            &current,
            2,
            5,
            PageLimits {
                segments: 1,
                ..PageLimits::default()
            },
        )
        .unwrap();
    assert_eq!(
        (one_segment.next, one_segment.limited_by),
        (4, Some("segments"))
    );
    let encoded = crate::codec::encode_value(&page.rows[0].value, crate::codec::Limits::default())
        .unwrap()
        .len();
    let small = store
        .page(
            &current,
            2,
            5,
            PageLimits {
                bytes: encoded + 1,
                ..PageLimits::default()
            },
        )
        .unwrap();
    assert_eq!((small.next, small.limited_by), (3, Some("bytes")));
    assert!(matches!(
        store.page(
            &current,
            2,
            5,
            PageLimits {
                bytes: 1,
                ..PageLimits::default()
            }
        ),
        Err(DatasetError::Limit("single page row"))
    ));
    let empty = store.page(&current, 8, 1, PageLimits::default()).unwrap();
    assert!(empty.extent_exhausted);
    assert!(empty.rows.is_empty());
    assert!(matches!(
        store.page(&current, 9, 1, PageLimits::default()),
        Err(DatasetError::Range)
    ));
}
#[test]
fn physical_uncommitted_manifests_foreign_refs_and_withdrawn_old_generations_cannot_be_read() {
    let tmp = home();
    let mut store = DatasetStore::open(
        tmp.path(),
        Durability::FileAndDirectory,
        StoreLimits::default(),
    )
    .unwrap();
    let mut manifest = create(&mut store);
    append(&mut store, &mut manifest, 1);
    store.commit(&manifest, &FlowPolicy::default()).unwrap();
    let old = store.descriptor(&manifest.dataset).unwrap();
    let candidate = next(&store, &manifest);
    let physical = store
        .prepare()
        .unwrap()
        .publish_manifest(&candidate, &FlowPolicy::default())
        .unwrap();
    let uncommitted = super::descriptor(&physical, &candidate).unwrap();
    assert!(matches!(
        store.read_exact(&uncommitted),
        Err(DatasetError::Unavailable)
    ));
    let other = home();
    let foreign = DatasetStore::open(
        other.path(),
        Durability::FileAndDirectory,
        StoreLimits::default(),
    )
    .unwrap();
    assert!(matches!(
        foreign.read_exact(&old),
        Err(DatasetError::Unavailable)
    ));
    let mut restricted = next(&store, &manifest);
    restricted.lifecycle = Lifecycle::Restricted;
    restricted.authorization_generation += 1;
    store.commit(&restricted, &FlowPolicy::default()).unwrap();
    assert!(matches!(
        store.page(&old, 0, 1, PageLimits::default()),
        Err(DatasetError::Withdrawn)
    ));
}
#[test]
fn missing_catalog_never_reinitializes_owned_objects_and_failed_reconciliation_blocks_old_roots() {
    let tmp = home();
    let limits = StoreLimits::default();
    let mut store = DatasetStore::open(tmp.path(), Durability::FileAndDirectory, limits).unwrap();
    let manifest = create(&mut store);
    store.commit(&manifest, &FlowPolicy::default()).unwrap();
    let path = tmp.path().join("catalog/active");
    let valid = std::fs::read(&path).unwrap();
    let mut corrupt = valid.clone();
    *corrupt.last_mut().unwrap() ^= 1;
    std::fs::write(&path, corrupt).unwrap();
    assert!(store.reconcile(&manifest.transaction).is_err());
    assert!(matches!(
        store.root(&manifest.dataset),
        Err(DatasetError::StorageCorrupt)
    ));
    assert!(matches!(store.prepare(), Err(DatasetError::StorageCorrupt)));
    std::fs::write(&path, valid).unwrap();
    assert!(matches!(
        store.reconcile(&manifest.transaction).unwrap(),
        Reconciliation::Committed(_)
    ));
    drop(store);
    // A path absent after loss is not evidence that this was a new empty store.
    std::fs::rename(&path, tmp.path().join("synthetic-lost-catalog")).unwrap();
    assert!(matches!(
        DatasetStore::open(tmp.path(), Durability::FileAndDirectory, limits),
        Err(DatasetError::StorageCorrupt)
    ));
    assert!(!path.exists());
}

#[test]
fn independent_keep_root_holds_only_the_shown_prefix_across_rotation_and_release() {
    let tmp = home();
    let mut limits = StoreLimits::default();
    limits.catalog.frames = 3;
    let mut store = DatasetStore::open(tmp.path(), Durability::FileAndDirectory, limits).unwrap();
    let mut manifest = create(&mut store);
    append(&mut store, &mut manifest, 3);
    store.commit(&manifest, &FlowPolicy::default()).unwrap();
    let shown = store.descriptor(&manifest.dataset).unwrap();
    let tx = uuid::Uuid::new_v4().to_string();
    let mut keep = ReferenceRootChange {
        root: uuid::Uuid::new_v4().to_string(),
        kind: RootKind::Pin,
        owner_dataset: None,
        owner_workspace: None,
        expected_generation: 0,
        generation: 1,
        prefixes: vec![shown.clone()],
        captures: vec![],
        retention: RootRetention::Protected,
        transaction: tx.clone(),
    };
    let receipt = store
        .update_references(&tx, &[keep.clone()], &FlowPolicy::default())
        .unwrap();
    let before = store.charged_bytes();
    assert_eq!(
        store
            .update_references(&tx, &[keep.clone()], &FlowPolicy::default())
            .unwrap(),
        receipt
    );
    assert_eq!(store.charged_bytes(), before);
    for _ in 0..5 {
        manifest = next(&store, &manifest);
        append(&mut store, &mut manifest, 1);
        store.commit(&manifest, &FlowPolicy::default()).unwrap();
    }
    let later = store.descriptor(&manifest.dataset).unwrap();
    assert!(store.is_protected(&shown));
    assert!(!store.is_protected(&later));
    drop(store);
    let mut store = DatasetStore::open(tmp.path(), Durability::FileAndDirectory, limits).unwrap();
    assert_eq!(
        store.reference_root(&keep.root).unwrap(),
        Some(keep.clone())
    );
    assert_eq!(
        store
            .page(&shown, 0, 3, PageLimits::default())
            .unwrap()
            .rows
            .len(),
        3
    );
    let stale = keep.clone();
    keep.expected_generation = 1;
    keep.generation = 2;
    keep.transaction = uuid::Uuid::new_v4().to_string();
    keep.prefixes.clear();
    store
        .update_references(&keep.transaction, &[keep.clone()], &FlowPolicy::default())
        .unwrap();
    assert!(!store.is_protected(&shown));
    assert!(matches!(
        store.update_references(
            &stale.transaction,
            std::slice::from_ref(&stale),
            &FlowPolicy::default()
        ),
        Err(DatasetError::Conflict)
    ));
    assert_eq!(store.descriptor(&manifest.dataset).unwrap(), later);
}

#[test]
fn reference_catalog_lost_ack_is_reconciled_without_replaying_or_new_generation() {
    let tmp = home();
    let mut store =
        DatasetStore::open(tmp.path(), Durability::File, StoreLimits::default()).unwrap();
    let manifest = create(&mut store);
    store.commit(&manifest, &FlowPolicy::default()).unwrap();
    let shown = store.descriptor(&manifest.dataset).unwrap();
    let transaction = uuid::Uuid::new_v4().to_string();
    let change = ReferenceRootChange {
        root: uuid::Uuid::new_v4().to_string(),
        kind: RootKind::Workspace,
        owner_dataset: None,
        owner_workspace: Some(uuid::Uuid::new_v4().to_string()),
        expected_generation: 0,
        generation: 1,
        prefixes: vec![shown.clone()],
        captures: vec![],
        retention: RootRetention::Protected,
        transaction: transaction.clone(),
    };
    assert!(matches!(
        store.update_references_with_sync(
            &transaction,
            &[change.clone()],
            &FlowPolicy::default(),
            |_| Err(io::Error::other("synthetic sync refusal"))
        ),
        Err(DatasetError::CommitUnconfirmed { .. })
    ));
    assert!(matches!(
        store.reference_root(&change.root),
        Err(DatasetError::CommitUnconfirmed { .. })
    ));
    let Reconciliation::ReferencesCommitted(receipt) = store.reconcile(&transaction).unwrap()
    else {
        panic!("complete reference frame must reconcile");
    };
    assert_eq!(receipt.roots, vec![(change.root.clone(), 1)]);
    assert!(store.is_protected(&shown));
    assert_eq!(
        store.reference_root(&change.root).unwrap(),
        Some(change.clone())
    );
    assert_eq!(
        store
            .update_references(&transaction, &[change], &FlowPolicy::default())
            .unwrap(),
        receipt
    );
}

#[test]
fn revocation_withdraws_reads_but_does_not_discard_an_existing_protection_root() {
    let tmp = home();
    let mut store =
        DatasetStore::open(tmp.path(), Durability::File, StoreLimits::default()).unwrap();
    let manifest = create(&mut store);
    store.commit(&manifest, &FlowPolicy::default()).unwrap();
    let shown = store.descriptor(&manifest.dataset).unwrap();
    let transaction = uuid::Uuid::new_v4().to_string();
    let change = ReferenceRootChange {
        root: uuid::Uuid::new_v4().to_string(),
        kind: RootKind::Keep,
        owner_dataset: None,
        owner_workspace: None,
        expected_generation: 0,
        generation: 1,
        prefixes: vec![shown.clone()],
        captures: vec![],
        retention: RootRetention::Protected,
        transaction: transaction.clone(),
    };
    store
        .update_references(&transaction, &[change.clone()], &FlowPolicy::default())
        .unwrap();
    let mut revoked = next(&store, &manifest);
    revoked.authorization_generation += 1;
    revoked.lifecycle = Lifecycle::Restricted;
    store.commit(&revoked, &FlowPolicy::default()).unwrap();
    assert!(matches!(
        store.read_exact(&shown),
        Err(DatasetError::Withdrawn)
    ));
    drop(store);
    let store = DatasetStore::open(tmp.path(), Durability::File, StoreLimits::default()).unwrap();
    assert!(store.is_protected(&shown));
    assert_eq!(store.reference_root(&change.root).unwrap(), Some(change));
    assert!(matches!(
        store.read_exact(&shown),
        Err(DatasetError::Withdrawn)
    ));
}

#[test]
fn reopening_is_inert_and_continue_charges_an_interrupted_grant_exactly_once() {
    use wes_engine::storage::datasets::{DatasetLifecycle, DatasetResume, DatasetStorage};
    let tmp = home();
    let limits = StoreLimits::default();
    let mut store = DatasetStore::open(tmp.path(), Durability::FileAndDirectory, limits).unwrap();
    let mut manifest = create(&mut store);
    let mut cp = checkpoint(&store, &manifest);
    cp.work.granted = 100;
    cp.work.outstanding = 100;
    cp.work.grants = 1;
    cp.budget.totals.duration_ms = 750;
    cp.duration.spent_ms = 250;
    cp.duration.outstanding_ms = 500;
    manifest.checkpoint = Some(
        store
            .prepare()
            .unwrap()
            .publish_checkpoint(&cp, &FlowPolicy::default())
            .unwrap(),
    );
    store.commit(&manifest, &FlowPolicy::default()).unwrap();
    let reference = store.descriptor(&manifest.dataset).unwrap();
    drop(store);
    let mut store = DatasetStore::open(tmp.path(), Durability::FileAndDirectory, limits).unwrap();
    assert_eq!(
        DatasetStorage::inspect(&store, &reference)
            .unwrap()
            .lifecycle,
        DatasetLifecycle::Interrupted
    );
    let old = DatasetStorage::checkpoint(&store, &reference)
        .unwrap()
        .unwrap();
    let mut new = old.clone();
    new.previous_attempt = Some(old.attempt.clone());
    new.attempt = uuid::Uuid::new_v4().to_string();
    new.run = uuid::Uuid::new_v4().to_string();
    new.work.charged = 100;
    new.work.outstanding = 200;
    new.work.granted = 300;
    new.work.grants = 2;
    new.budget.previous = Some(old.budget.digest());
    new.budget.issued_attempt = new.attempt.clone();
    new.budget.totals.duration_ms = 60000;
    new.duration.spent_ms = 750;
    new.duration.outstanding_ms = new.budget.totals.duration_ms - new.duration.spent_ms;
    let request = DatasetResume {
        kind: wes_engine::storage::datasets::AnalysisAttemptKind::Continue,
        previous: reference.clone(),
        transaction: uuid::Uuid::new_v4().to_string(),
        checkpoint: new.clone(),
        policy: FlowPolicy::default(),
    };
    for corrupt in 0..7 {
        let mut forged = request.clone();
        forged.transaction = uuid::Uuid::new_v4().to_string();
        match corrupt {
            0 => forged.checkpoint.work.charged = 0,
            1 => forged.checkpoint.previous_attempt = None,
            2 => forged.checkpoint.next_position = 1,
            3 => forged.checkpoint.usage.work_allowance += 1,
            4 => forged.checkpoint.duration.spent_ms = 250,
            5 => forged.checkpoint.budget.totals.duration_ms += 1,
            _ => forged.checkpoint.duration.outstanding_ms -= 1,
        };
        assert!(DatasetStorage::resume(&mut store, forged).is_err());
        assert_eq!(store.descriptor(&manifest.dataset).unwrap(), reference);
    }
    let admission = DatasetStorage::resume(&mut store, request.clone()).unwrap();
    let resumed = admission.reference;
    let writer = admission.lease;
    assert!(
        matches!(
            DatasetStorage::head(&store, &reference),
            Err(wes_engine::storage::StoreError::Conflict)
        ),
        "following a previous attempt must never silently join its explicit resume"
    );
    assert_eq!(
        DatasetStorage::head(&store, &resumed).unwrap().reference,
        resumed
    );
    let snapshot = |target: &DatasetRef| wes_engine::storage::datasets::DatasetSnapshot {
        basis: reference.manifest_digest().into(),
        generation: target.generation(),
        digest: target.manifest_digest().into(),
    };
    assert_eq!(
        DatasetStorage::snapshot(&store, &reference, snapshot(&reference))
            .unwrap()
            .reference,
        reference,
        "old committed same-attempt evidence is capturable after an explicit Resume"
    );
    assert!(
        matches!(
            DatasetStorage::snapshot(&store, &reference, snapshot(&resumed)),
            Err(wes_engine::storage::StoreError::Conflict)
        ),
        "a captured digest cannot grant cross-attempt authority"
    );
    assert_eq!(
        DatasetStorage::inspect(&store, &resumed).unwrap().lifecycle,
        DatasetLifecycle::Open
    );
    assert!(
        DatasetStorage::resume(&mut store, request).is_err(),
        "a second writer cannot enter or double-charge the old grant"
    );
    assert_eq!(
        DatasetStorage::checkpoint(&store, &resumed)
            .unwrap()
            .unwrap()
            .work
            .charged,
        100
    );
    drop(store);
    let store = DatasetStore::open(tmp.path(), Durability::FileAndDirectory, limits).unwrap();
    assert_eq!(
        DatasetStorage::inspect(&store, &resumed).unwrap().lifecycle,
        DatasetLifecycle::Interrupted
    );
    let recovered = DatasetStorage::checkpoint(&store, &resumed)
        .unwrap()
        .unwrap();
    assert_eq!(recovered.work.charged, 100);
    assert_eq!(recovered.work.outstanding, 200);
    assert_eq!(recovered.duration.spent_ms, 750);
    assert_eq!(recovered.duration.outstanding_ms, 59250);
    drop(writer);
    assert_eq!(recovered.previous_attempt, Some(old.attempt));
}

#[test]
fn checkpoint_output_deletion_releases_the_whole_root_without_stranding_source_protection() {
    let tmp = home();
    let mut store =
        DatasetStore::open(tmp.path(), Durability::File, StoreLimits::default()).unwrap();
    let mut recording = create(&mut store);
    append(&mut store, &mut recording, 1);
    recording.lifecycle = Lifecycle::Sealed;
    store.commit(&recording, &FlowPolicy::default()).unwrap();
    let source = store.descriptor(&recording.dataset).unwrap();
    let mut analysis = create(&mut store);
    append(&mut store, &mut analysis, 1);
    analysis.lifecycle = Lifecycle::Sealed;
    store.commit(&analysis, &FlowPolicy::default()).unwrap();
    let output = store.descriptor(&analysis.dataset).unwrap();
    let identity = uuid::Uuid::new_v4().to_string();
    let capture = CapturedValue {
        handle: uuid::Uuid::new_v4().to_string(),
        digest: format!("sha256:{}", "a".repeat(64)),
        bytes: 100,
    };
    let transaction = uuid::Uuid::new_v4().to_string();
    let root = ReferenceRootChange {
        root: identity.clone(),
        kind: RootKind::Checkpoint,
        owner_dataset: Some(analysis.dataset.clone()),
        owner_workspace: None,
        expected_generation: 0,
        generation: 1,
        // Deliberately source first: ownership is semantic, never positional.
        prefixes: vec![source.clone(), output.clone()],
        captures: vec![capture.clone()],
        retention: RootRetention::Protected,
        transaction: transaction.clone(),
    };
    store
        .update_references(&transaction, &[root], &FlowPolicy::default())
        .unwrap();
    assert!(store.is_protected(&source));
    let plan = store.plan_delete_owned(&output).unwrap();
    let cleanup = store.delete_owned(&plan.token, true, true).unwrap();
    assert_eq!(cleanup.released_captures, vec![capture]);
    let released = store.reference_root(&identity).unwrap().unwrap();
    assert!(released.prefixes.is_empty());
    assert_eq!(released.retention, RootRetention::Temporary);
    assert!(!store.is_protected(&source));
    assert!(store.read_exact(&source).is_ok());
    drop(store);
    let reopened =
        DatasetStore::open(tmp.path(), Durability::File, StoreLimits::default()).unwrap();
    assert!(
        reopened
            .reference_root(&identity)
            .unwrap()
            .unwrap()
            .prefixes
            .is_empty()
    );
    assert!(reopened.read_exact(&source).is_ok());
}

#[test]
fn tainted_checkpoint_withdraws_its_output_without_revoking_an_independent_retained_source() {
    let tmp = home();
    let mut store =
        DatasetStore::open(tmp.path(), Durability::File, StoreLimits::default()).unwrap();
    let mut input = create(&mut store);
    append(&mut store, &mut input, 1);
    store.commit(&input, &FlowPolicy::default()).unwrap();
    let mut output = create(&mut store);
    append(&mut store, &mut output, 1);
    store.commit(&output, &FlowPolicy::default()).unwrap();
    let mut independent = create(&mut store);
    append(&mut store, &mut independent, 1);
    store.commit(&independent, &FlowPolicy::default()).unwrap();
    let a = store.descriptor(&input.dataset).unwrap();
    let b = store.descriptor(&output.dataset).unwrap();
    let c = store.descriptor(&independent.dataset).unwrap();
    let capture = uuid::Uuid::new_v4().to_string();
    let transaction = uuid::Uuid::new_v4().to_string();
    let roots = vec![
        ReferenceRootChange {
            root: capture.clone(),
            kind: RootKind::Value,
            owner_dataset: None,
            owner_workspace: None,
            expected_generation: 0,
            generation: 1,
            prefixes: vec![a.clone()],
            captures: vec![],
            retention: RootRetention::Protected,
            transaction: transaction.clone(),
        },
        ReferenceRootChange {
            root: uuid::Uuid::new_v4().to_string(),
            kind: RootKind::Checkpoint,
            owner_dataset: Some(output.dataset.clone()),
            owner_workspace: None,
            expected_generation: 0,
            generation: 1,
            prefixes: vec![c.clone(), b.clone()],
            captures: vec![CapturedValue {
                handle: capture,
                digest: format!("sha256:{}", "a".repeat(64)),
                bytes: 100,
            }],
            retention: RootRetention::Protected,
            transaction: transaction.clone(),
        },
    ];
    store
        .update_references(&transaction, &roots, &FlowPolicy::default())
        .unwrap();
    let withdrawn = store.withdraw_owned(&a).unwrap();
    assert!(withdrawn.contains(&output.dataset));
    assert!(!withdrawn.contains(&independent.dataset));
    assert!(matches!(store.read_exact(&b), Err(DatasetError::Withdrawn)));
    assert!(store.read_exact(&c).is_ok());
}

#[test]
fn committed_ancestry_reads_old_prefixes_logarithmically_and_refuses_foreign_jumps() {
    use std::sync::atomic::Ordering;
    let tmp = home();
    let limits = StoreLimits::default();
    let mut store = DatasetStore::open(tmp.path(), Durability::FileAndDirectory, limits).unwrap();
    let mut manifest = create(&mut store);
    append(&mut store, &mut manifest, 1);
    store.commit(&manifest, &FlowPolicy::default()).unwrap();
    let first = store.descriptor(&manifest.dataset).unwrap();
    for _ in 0..256 {
        manifest = next(&store, &manifest);
        store.commit(&manifest, &FlowPolicy::default()).unwrap();
    }
    assert_eq!(manifest.generation, 257);
    for reopened in [false, true] {
        if reopened {
            drop(store);
            store = DatasetStore::open(tmp.path(), Durability::FileAndDirectory, limits).unwrap();
        }
        store.files.manifest_reads.store(0, Ordering::Relaxed);
        let page = store.page(&first, 0, 1, PageLimits::default()).unwrap();
        assert_eq!(page.rows.len(), 1);
        assert_eq!(page.rows[0].source_start, 0);
        let reads = store.files.manifest_reads.load(Ordering::Relaxed);
        assert!(
            reads <= 4,
            "old prefix read walked {reads} manifests instead of the committed binary link"
        );
    }
    let foreign = create(&mut store);
    let foreign_receipt = store.commit(&foreign, &FlowPolicy::default()).unwrap();
    let mut forged = next(&store, &manifest);
    forged.ancestors[1] = foreign_receipt.manifest;
    assert!(matches!(
        store.commit(&forged, &FlowPolicy::default()),
        Err(DatasetError::Conflict)
    ));
    assert_eq!(
        store.descriptor(&manifest.dataset).unwrap().generation(),
        257
    );
    drop(store);
    let store = DatasetStore::open(tmp.path(), Durability::FileAndDirectory, limits).unwrap();
    assert_eq!(
        store
            .page(&first, 0, 1, PageLimits::default())
            .unwrap()
            .rows
            .len(),
        1
    );
}

fn write_owner() -> wes_engine::storage::datasets::DatasetWriteOwner {
    let run = uuid::Uuid::new_v4().to_string();
    wes_engine::storage::datasets::DatasetWriteOwner {
        role: wes_engine::storage::datasets::DatasetWriteRole::Analysis,
        lineage: run.clone(),
        run,
    }
}
#[test]
fn owned_write_admission_survives_rotation_and_reopen_without_inventing_a_commit() {
    use wes_engine::storage::datasets::DatasetWriteOutcome;
    let tmp = home();
    let owner = write_owner();
    let transaction = uuid::Uuid::new_v4().to_string();
    let dataset = uuid::Uuid::new_v4().to_string();
    let mut store = DatasetStore::open(
        tmp.path(),
        Durability::FileAndDirectory,
        StoreLimits::default(),
    )
    .unwrap();
    store
        .admit_write(
            Some(&owner),
            &transaction,
            &dataset,
            None,
            DatasetKind::Analysis,
            catalog::WriteOperation::Create,
        )
        .unwrap();
    assert!(
        store.roots.is_empty(),
        "durable intent is not data publication"
    );
    store.rotate().unwrap();
    drop(store);
    let mut store = DatasetStore::open(
        tmp.path(),
        Durability::FileAndDirectory,
        StoreLimits::default(),
    )
    .unwrap();
    let receipt = store.reconcile_owner(&owner.selection()).unwrap();
    assert_eq!(receipt.outcome, DatasetWriteOutcome::Absent);
    assert_eq!(receipt.transaction.as_deref(), Some(transaction.as_str()));
    assert!(receipt.committed.is_none());
    assert!(receipt.execution_unknown);
    assert_eq!(
        store.reconcile_owner(&owner.selection()).unwrap().outcome,
        DatasetWriteOutcome::Absent
    );
    let unknown = store.reconcile_owner(&write_owner().selection()).unwrap();
    assert_eq!(unknown.outcome, DatasetWriteOutcome::Unknown);
    assert!(unknown.transaction.is_none());
    assert!(store.roots.is_empty());
}
#[test]
fn owned_write_lost_acknowledgement_confirms_only_the_exact_data_transaction() {
    use wes_engine::storage::datasets::DatasetWriteOutcome;
    let tmp = home();
    let mut store =
        DatasetStore::open(tmp.path(), Durability::File, StoreLimits::default()).unwrap();
    let owner = write_owner();
    let manifest = create(&mut store);
    store
        .admit_write(
            Some(&owner),
            &manifest.transaction,
            &manifest.dataset,
            None,
            manifest.kind,
            catalog::WriteOperation::Create,
        )
        .unwrap();
    assert!(matches!(
        store.commit_with_sync(&manifest, &FlowPolicy::default(), |_| Err(
            io::Error::other("synthetic lost commit acknowledgement")
        )),
        Err(DatasetError::CommitUnconfirmed { .. })
    ));
    drop(store);
    let mut store =
        DatasetStore::open(tmp.path(), Durability::File, StoreLimits::default()).unwrap();
    let receipt = store.reconcile_owner(&owner.selection()).unwrap();
    assert_eq!(receipt.outcome, DatasetWriteOutcome::Committed);
    assert_eq!(
        receipt.transaction.as_deref(),
        Some(manifest.transaction.as_str())
    );
    assert_eq!(
        receipt.committed,
        Some(store.descriptor(&manifest.dataset).unwrap())
    );
    assert!(receipt.execution_unknown);
    let next = next(&store, &manifest);
    let prefix = store.descriptor(&manifest.dataset).unwrap();
    store
        .admit_write(
            Some(&owner),
            &next.transaction,
            &manifest.dataset,
            Some(&prefix),
            manifest.kind,
            catalog::WriteOperation::Append,
        )
        .unwrap();
    let receipt = store.reconcile_owner(&owner.selection()).unwrap();
    assert_eq!(receipt.outcome, DatasetWriteOutcome::Absent);
    assert_eq!(
        receipt.transaction.as_deref(),
        Some(next.transaction.as_str())
    );
    assert_eq!(receipt.predecessor, Some(prefix));
    assert!(
        receipt.committed.is_none(),
        "an old checkpoint cannot confirm a newer absent write"
    );
}
#[test]
fn owned_write_torn_data_frame_leaves_orphaned_objects_uncommitted_and_unpinned() {
    use wes_engine::storage::datasets::DatasetWriteOutcome;
    let tmp = home();
    let mut store =
        DatasetStore::open(tmp.path(), Durability::File, StoreLimits::default()).unwrap();
    let manifest = create(&mut store);
    let owner = write_owner();
    store
        .admit_write(
            Some(&owner),
            &manifest.transaction,
            &manifest.dataset,
            None,
            manifest.kind,
            catalog::WriteOperation::Create,
        )
        .unwrap();
    let path = tmp.path().join("catalog").join(ACTIVE);
    let boundary = store.valid_bytes;
    store.commit(&manifest, &FlowPolicy::default()).unwrap();
    let mut bytes = std::fs::read(&path).unwrap();
    bytes.truncate(boundary + 20);
    std::fs::write(&path, &bytes).unwrap();
    drop(store);
    let mut store =
        DatasetStore::open(tmp.path(), Durability::File, StoreLimits::default()).unwrap();
    let receipt = store.reconcile_owner(&owner.selection()).unwrap();
    assert_eq!(receipt.outcome, DatasetWriteOutcome::Absent);
    assert!(store.descriptor(&manifest.dataset).is_err());
    let cleanup = store.collect_owned().unwrap();
    assert!(
        cleanup.reclaimed_bytes.unwrap() > 0,
        "witnesses are not object retention roots"
    );
    assert_eq!(
        store.reconcile_owner(&owner.selection()).unwrap().outcome,
        DatasetWriteOutcome::Absent
    );
}
#[test]
fn owned_write_capacity_and_unresolved_identity_refuse_before_new_object_mutation() {
    let tmp = home();
    let limits = StoreLimits {
        datasets: 1,
        ..Default::default()
    };
    let mut store = DatasetStore::open(tmp.path(), Durability::File, limits).unwrap();
    let owner = write_owner();
    let dataset = uuid::Uuid::new_v4().to_string();
    store
        .admit_write(
            Some(&owner),
            &uuid::Uuid::new_v4().to_string(),
            &dataset,
            None,
            DatasetKind::Analysis,
            catalog::WriteOperation::Create,
        )
        .unwrap();
    let bytes = store.charged_bytes();
    assert!(
        store
            .admit_write(
                Some(&owner),
                &uuid::Uuid::new_v4().to_string(),
                &dataset,
                None,
                DatasetKind::Analysis,
                catalog::WriteOperation::Create
            )
            .is_err()
    );
    assert!(
        store
            .admit_write(
                Some(&write_owner()),
                &uuid::Uuid::new_v4().to_string(),
                &uuid::Uuid::new_v4().to_string(),
                None,
                DatasetKind::Analysis,
                catalog::WriteOperation::Create
            )
            .is_err()
    );
    assert_eq!(store.charged_bytes(), bytes);
    assert_eq!(store.writes.len(), 1);
}
#[test]
fn owned_write_withdrawal_releases_witnesses_and_never_restores_payload_authority() {
    use wes_engine::storage::datasets::DatasetWriteOutcome;
    let tmp = home();
    let mut store =
        DatasetStore::open(tmp.path(), Durability::File, StoreLimits::default()).unwrap();
    let owner = write_owner();
    let manifest = create(&mut store);
    store
        .admit_write(
            Some(&owner),
            &manifest.transaction,
            &manifest.dataset,
            None,
            manifest.kind,
            catalog::WriteOperation::Create,
        )
        .unwrap();
    store.commit(&manifest, &FlowPolicy::default()).unwrap();
    let prefix = store.descriptor(&manifest.dataset).unwrap();
    store.withdraw_owned(&prefix).unwrap();
    assert!(!store.writes.contains_key(&manifest.dataset));
    drop(store);
    let mut store =
        DatasetStore::open(tmp.path(), Durability::File, StoreLimits::default()).unwrap();
    let receipt = store.reconcile_owner(&owner.selection()).unwrap();
    assert_eq!(receipt.outcome, DatasetWriteOutcome::Unknown);
    assert!(receipt.transaction.is_none());
    assert!(store.read_exact(&prefix).is_err());
}

#[test]
fn resolved_failed_creates_release_bounded_identity_capacity_without_becoming_data() {
    use catalog::WriteOperation;
    use wes_engine::storage::datasets::DatasetWriteOutcome;
    let tmp = home();
    let limits = StoreLimits {
        datasets: 1,
        ..Default::default()
    };
    let mut store = DatasetStore::open(tmp.path(), Durability::FileAndDirectory, limits).unwrap();
    let first = write_owner();
    store
        .admit_write(
            Some(&first),
            &uuid::Uuid::new_v4().to_string(),
            &uuid::Uuid::new_v4().to_string(),
            None,
            DatasetKind::Analysis,
            WriteOperation::Create,
        )
        .unwrap();
    assert_eq!(
        store.reconcile_owner(&first.selection()).unwrap().outcome,
        DatasetWriteOutcome::Absent
    );
    let second = write_owner();
    let transaction = uuid::Uuid::new_v4().to_string();
    store
        .admit_write(
            Some(&second),
            &transaction,
            &uuid::Uuid::new_v4().to_string(),
            None,
            DatasetKind::Analysis,
            WriteOperation::Create,
        )
        .unwrap();
    assert_eq!(store.writes.len(), 1);
    assert_eq!(
        store.reconcile_owner(&first.selection()).unwrap().outcome,
        DatasetWriteOutcome::Unknown
    );
    store.rotate().unwrap();
    drop(store);
    let mut store = DatasetStore::open(tmp.path(), Durability::FileAndDirectory, limits).unwrap();
    assert!(store.roots.is_empty());
    assert_eq!(
        store.reconcile_owner(&first.selection()).unwrap().outcome,
        DatasetWriteOutcome::Unknown
    );
    let receipt = store.reconcile_owner(&second.selection()).unwrap();
    assert_eq!(receipt.outcome, DatasetWriteOutcome::Absent);
    assert_eq!(receipt.transaction.as_deref(), Some(transaction.as_str()));
}

#[test]
fn analysis_reconciliation_preserves_lineage_after_an_admitted_resume_has_no_checkpoint() {
    use catalog::WriteOperation;
    use wes_engine::storage::datasets::{
        DatasetResume, DatasetStorage, DatasetWriteOutcome, DatasetWriteOwner, DatasetWriteRole,
        DatasetWriteSelection,
    };
    let tmp = home();
    let mut store = DatasetStore::open(
        tmp.path(),
        Durability::FileAndDirectory,
        StoreLimits::default(),
    )
    .unwrap();
    let mut manifest = create(&mut store);
    manifest.lifecycle = Lifecycle::Cancelled;
    let mut cp = checkpoint(&store, &manifest);
    cp.run = cp.analysis.clone();
    let first = DatasetWriteOwner {
        role: DatasetWriteRole::Analysis,
        run: cp.run.clone(),
        lineage: cp.analysis.clone(),
    };
    store
        .admit_write(
            Some(&first),
            &manifest.transaction,
            &manifest.dataset,
            None,
            DatasetKind::Analysis,
            WriteOperation::Create,
        )
        .unwrap();
    manifest.checkpoint = Some(
        store
            .files
            .publish_checkpoint(&cp, &FlowPolicy::default())
            .unwrap(),
    );
    store.commit(&manifest, &FlowPolicy::default()).unwrap();
    let start = store.descriptor(&manifest.dataset).unwrap();
    let mut resume = store.read_analysis_checkpoint(&start).unwrap().unwrap();
    resume.previous_attempt = Some(resume.attempt.clone());
    resume.attempt = uuid::Uuid::new_v4().to_string();
    resume.run = uuid::Uuid::new_v4().to_string();
    resume.work.outstanding = 200;
    resume.work.granted = 200;
    resume.work.grants = 1;
    resume.duration.outstanding_ms = resume.budget.totals.duration_ms;
    let admission = store
        .resume_owned(DatasetResume {
            kind: wes_engine::storage::datasets::AnalysisAttemptKind::Resume,
            previous: start.clone(),
            transaction: uuid::Uuid::new_v4().to_string(),
            checkpoint: resume.clone(),
            policy: FlowPolicy::default(),
        })
        .unwrap();
    let middle = admission.reference;
    let lease = admission.lease;
    assert!(
        matches!(
            DatasetStorage::resume(
                &mut store,
                DatasetResume {
                    kind: wes_engine::storage::datasets::AnalysisAttemptKind::Resume,
                    previous: start,
                    transaction: uuid::Uuid::new_v4().to_string(),
                    checkpoint: resume.clone(),
                    policy: FlowPolicy::default(),
                }
            ),
            Err(wes_engine::storage::StoreError::DatasetNewerAttempt)
        ),
        "a consumed older attempt is a typed refusal, not an unexplained conflict"
    );
    // Commit real progress and unused credit before the next attempt. An
    // interrupted all-remaining time reservation must never mint fresh time.
    resume.work.charged = 100;
    resume.work.completed = 100;
    resume.work.outstanding = 0;
    resume.duration.spent_ms = 100;
    resume.duration.outstanding_ms = 0;
    let middle = store
        .append_owned(wes_engine::storage::datasets::DatasetAppend {
            owner: Some(DatasetWriteOwner {
                role: DatasetWriteRole::Analysis,
                run: resume.run.clone(),
                lineage: resume.analysis.clone(),
            }),
            previous: middle,
            transaction: uuid::Uuid::new_v4().to_string(),
            rows: vec![],
            source: wes_engine::storage::datasets::SourceExtent {
                identity: manifest.source.identity.clone(),
                unit: match manifest.source.unit {
                    PositionUnit::Bytes => wes_engine::storage::datasets::SourceUnit::Bytes,
                    PositionUnit::Records => wes_engine::storage::datasets::SourceUnit::Records,
                },
                start: manifest.source.start,
                end: manifest.source.end,
            },
            lifecycle: wes_engine::storage::datasets::DatasetLifecycle::Incomplete,
            checkpoint: Some(resume.clone()),
            recording: None,
            policy: FlowPolicy::default(),
        })
        .unwrap();
    drop(lease);
    let failed = DatasetWriteOwner {
        role: DatasetWriteRole::Analysis,
        run: uuid::Uuid::new_v4().to_string(),
        lineage: first.lineage.clone(),
    };
    let failed_tx = uuid::Uuid::new_v4().to_string();
    store
        .admit_write(
            Some(&failed),
            &failed_tx,
            &manifest.dataset,
            Some(&middle),
            DatasetKind::Analysis,
            WriteOperation::Resume,
        )
        .unwrap();
    store.rotate().unwrap();
    drop(store);
    let mut store = DatasetStore::open(
        tmp.path(),
        Durability::FileAndDirectory,
        StoreLimits::default(),
    )
    .unwrap();
    for run in [&first.run, &resume.run, &failed.run] {
        let selected = DatasetWriteSelection {
            role: DatasetWriteRole::Analysis,
            run: run.clone(),
        };
        let receipt = store.reconcile_owner(&selected).unwrap();
        assert_eq!(receipt.outcome, DatasetWriteOutcome::Absent);
        assert_eq!(receipt.owner, Some(failed.clone()));
        assert_eq!(receipt.predecessor, Some(middle.clone()));
        assert_eq!(receipt.transaction.as_deref(), Some(failed_tx.as_str()));
        assert_eq!(DatasetStorage::continuation(&store, run).unwrap(), middle);
    }
    let mut next = resume.clone();
    next.previous_attempt = Some(resume.attempt.clone());
    next.attempt = uuid::Uuid::new_v4().to_string();
    next.run = uuid::Uuid::new_v4().to_string();
    next.work.charged = 100;
    next.work.outstanding = 200;
    next.work.granted = 400;
    next.work.grants = 2;
    next.duration.spent_ms = 100;
    next.duration.outstanding_ms = next.budget.totals.duration_ms - 100;
    let admission = store
        .resume_owned(DatasetResume {
            kind: wes_engine::storage::datasets::AnalysisAttemptKind::Resume,
            previous: middle,
            transaction: uuid::Uuid::new_v4().to_string(),
            checkpoint: next,
            policy: FlowPolicy::default(),
        })
        .unwrap();
    let committed = admission.reference;
    let lease = admission.lease;
    drop(lease);
    let current = DatasetStorage::continuation(&store, &first.run).unwrap();
    assert_eq!(current, committed);
    assert_eq!(
        store
            .read_analysis_checkpoint(&current)
            .unwrap()
            .unwrap()
            .work
            .charged,
        100
    );
}

#[test]
fn known_absent_append_can_close_but_a_different_owner_cannot_take_over() {
    use catalog::WriteOperation;
    let tmp = home();
    let mut store =
        DatasetStore::open(tmp.path(), Durability::File, StoreLimits::default()).unwrap();
    let owner = write_owner();
    let manifest = create(&mut store);
    store
        .admit_write(
            Some(&owner),
            &manifest.transaction,
            &manifest.dataset,
            None,
            DatasetKind::Analysis,
            WriteOperation::Create,
        )
        .unwrap();
    store.commit(&manifest, &FlowPolicy::default()).unwrap();
    let prefix = store.descriptor(&manifest.dataset).unwrap();
    let failed = uuid::Uuid::new_v4().to_string();
    store
        .admit_write(
            Some(&owner),
            &failed,
            &manifest.dataset,
            Some(&prefix),
            DatasetKind::Analysis,
            WriteOperation::Append,
        )
        .unwrap();
    // Segment encoding failed after admission, before a data frame. The common
    // ready boundary proves no late publication remains queued in this owner.
    let mut closing = next(&store, &manifest);
    closing.lifecycle = Lifecycle::Incomplete;
    assert!(
        store
            .admit_write(
                Some(&write_owner()),
                &closing.transaction,
                &manifest.dataset,
                Some(&prefix),
                DatasetKind::Analysis,
                WriteOperation::Append
            )
            .is_err()
    );
    store
        .admit_write(
            Some(&owner),
            &closing.transaction,
            &manifest.dataset,
            Some(&prefix),
            DatasetKind::Analysis,
            WriteOperation::Append,
        )
        .unwrap();
    store.commit(&closing, &FlowPolicy::default()).unwrap();
    drop(store);
    let mut store =
        DatasetStore::open(tmp.path(), Durability::File, StoreLimits::default()).unwrap();
    let receipt = store.reconcile_owner(&owner.selection()).unwrap();
    assert_eq!(
        store
            .read_exact(receipt.committed.as_ref().unwrap())
            .unwrap()
            .lifecycle,
        Lifecycle::Incomplete
    );
    assert_eq!(
        receipt.transaction.as_deref(),
        Some(closing.transaction.as_str())
    );
}

#[test]
fn receipt_joins_fresh_prefix_origins_and_read_withdrawal_dependencies() {
    use catalog::WriteOperation;
    let tmp = home();
    let mut store =
        DatasetStore::open(tmp.path(), Durability::File, StoreLimits::default()).unwrap();
    let source = create(&mut store);
    store.commit(&source, &FlowPolicy::default()).unwrap();
    let source_ref = store.descriptor(&source.dataset).unwrap();
    let owner = write_owner();
    let manifest = create(&mut store);
    store
        .admit_write(
            Some(&owner),
            &manifest.transaction,
            &manifest.dataset,
            None,
            DatasetKind::Analysis,
            WriteOperation::Create,
        )
        .unwrap();
    store.commit(&manifest, &FlowPolicy::default()).unwrap();
    let prefix = store.descriptor(&manifest.dataset).unwrap();
    let mut changed = next(&store, &manifest);
    let policy = FlowPolicy::default()
        .from_origin("synthetic-added-origin")
        .read_from_dataset(&source_ref);
    changed.origins = policy.origins().iter().cloned().collect();
    changed.dataset_reads = policy.dataset_reads().iter().cloned().collect();
    store
        .admit_write(
            Some(&owner),
            &changed.transaction,
            &manifest.dataset,
            Some(&prefix),
            DatasetKind::Analysis,
            WriteOperation::Append,
        )
        .unwrap();
    store.commit(&changed, &policy).unwrap();
    let receipt = store.reconcile_owner(&owner.selection()).unwrap();
    assert!(receipt.policy.origins().contains("synthetic-added-origin"));
    assert!(
        receipt
            .policy
            .dataset_reads()
            .iter()
            .any(|r| r.dataset() == source_ref.dataset())
    );
    assert!(
        receipt
            .policy
            .dataset_reads()
            .iter()
            .any(|r| r.dataset() == manifest.dataset)
    );
    store.withdraw_owned(&source_ref).unwrap();
    let removed = store.reconcile_owner(&owner.selection()).unwrap();
    assert_eq!(
        removed.outcome,
        wes_engine::storage::datasets::DatasetWriteOutcome::Unknown
    );
    assert!(removed.committed.is_none());
    assert!(removed.predecessor.is_none());
    assert!(removed.transaction.is_none());
}

#[test]
fn active_writer_and_unknown_selection_never_recover_or_trim_an_unrelated_pending_frame() {
    use catalog::WriteOperation;
    let tmp = home();
    let mut store =
        DatasetStore::open(tmp.path(), Durability::File, StoreLimits::default()).unwrap();
    let owner = write_owner();
    let manifest = create(&mut store);
    store
        .admit_write(
            Some(&owner),
            &manifest.transaction,
            &manifest.dataset,
            None,
            DatasetKind::Analysis,
            WriteOperation::Create,
        )
        .unwrap();
    store.commit(&manifest, &FlowPolicy::default()).unwrap();
    store
        .active_writers
        .lock()
        .unwrap()
        .insert(manifest.dataset.clone(), uuid::Uuid::new_v4().to_string());
    let unrelated = uuid::Uuid::new_v4().to_string();
    store.uncertain = Some(unrelated.clone());
    let path = tmp.path().join("catalog").join(ACTIVE);
    let mut bytes = std::fs::read(&path).unwrap();
    bytes.extend_from_slice(b"torn");
    std::fs::write(&path, &bytes).unwrap();
    assert!(matches!(
        store.reconcile_owner(&owner.selection()),
        Err(DatasetError::Conflict)
    ));
    assert_eq!(
        store
            .reconcile_owner(&write_owner().selection())
            .unwrap()
            .outcome,
        wes_engine::storage::datasets::DatasetWriteOutcome::Unknown
    );
    assert_eq!(store.uncertain, Some(unrelated));
    assert_eq!(std::fs::read(path).unwrap(), bytes);
}

#[test]
fn unconfirmed_admission_recovers_its_owner_without_claiming_a_data_commit() {
    use catalog::WriteOperation;
    use wes_engine::storage::datasets::DatasetWriteOutcome;
    for torn in [false, true] {
        let tmp = home();
        let mut store = DatasetStore::open(
            tmp.path(),
            Durability::FileAndDirectory,
            StoreLimits::default(),
        )
        .unwrap();
        let owner = write_owner();
        let dataset = uuid::Uuid::new_v4().to_string();
        let transaction = uuid::Uuid::new_v4().to_string();
        let boundary = store.valid_bytes;
        let error = store
            .admit_write_with_sync(
                Some(&owner),
                &transaction,
                &dataset,
                None,
                DatasetKind::Analysis,
                WriteOperation::Create,
                |_| Err(io::Error::other("synthetic lost admission acknowledgement")),
            )
            .unwrap_err();
        let DatasetError::AdmissionUnconfirmed {
            transaction: reported,
            admission,
        } = error
        else {
            panic!("typed admission error");
        };
        assert_eq!(reported, transaction);
        assert_ne!(admission, transaction);
        if torn {
            let path = tmp.path().join("catalog").join(ACTIVE);
            let mut bytes = std::fs::read(&path).unwrap();
            bytes.truncate(boundary + 16);
            std::fs::write(path, bytes).unwrap();
        }
        // Bare recovery can settle the process-owned admission even after the
        // original graph node was removed or its run replaced.
        store.reconcile_store().unwrap();
        let receipt = store.reconcile_owner(&owner.selection()).unwrap();
        assert_eq!(receipt.outcome, DatasetWriteOutcome::Absent);
        assert_eq!(receipt.transaction.as_deref(), Some(transaction.as_str()));
        assert!(receipt.committed.is_none());
        assert!(store.roots.is_empty());
        drop(store);
        let mut store = DatasetStore::open(
            tmp.path(),
            Durability::FileAndDirectory,
            StoreLimits::default(),
        )
        .unwrap();
        assert_eq!(
            store.reconcile_owner(&owner.selection()).unwrap().outcome,
            DatasetWriteOutcome::Absent
        );
    }
}

#[test]
fn admitted_recording_segment_failure_still_persists_its_incomplete_terminal_reason() {
    use wes_engine::storage::datasets::{
        DatasetAppend, DatasetCreate, DatasetKind as Kind, DatasetLifecycle, DatasetRow,
        DatasetWriteOwner, DatasetWriteRole, EventLogCoverage, RecordingEnd, SourceExtent,
        SourceUnit,
    };
    let tmp = home();
    let mut limits = StoreLimits::default();
    limits.objects.segment.segment_bytes = 128;
    let mut store = DatasetStore::open(tmp.path(), Durability::FileAndDirectory, limits).unwrap();
    let dataset = uuid::Uuid::new_v4().to_string();
    let run = uuid::Uuid::new_v4().to_string();
    let owner = DatasetWriteOwner {
        role: DatasetWriteRole::Recording,
        lineage: run.clone(),
        run,
    };
    let mut coverage = EventLogCoverage {
        run: uuid::Uuid::new_v4().to_string(),
        epoch: dataset.clone(),
        first: 1,
        accepted_through: 0,
        committed_through: 0,
        pending: Some(0),
        rejected: 0,
        termination: None,
    };
    let extent = |end| SourceExtent {
        identity: dataset.clone(),
        unit: SourceUnit::Records,
        start: 0,
        end,
    };
    let admission = store
        .create_owned(DatasetCreate {
            owner: Some(owner.clone()),
            dataset: dataset.clone(),
            transaction: uuid::Uuid::new_v4().to_string(),
            kind: Kind::EventLog,
            schema: schema(),
            source: extent(0),
            policy: FlowPolicy::default(),
            checkpoint: None,
            recording: Some(coverage.clone()),
        })
        .unwrap();
    let prefix = admission.reference;
    let lease = admission.lease;
    coverage.accepted_through = 1;
    coverage.committed_through = 1;
    let transaction = uuid::Uuid::new_v4().to_string();
    let error = store
        .append_owned(DatasetAppend {
            owner: Some(owner.clone()),
            previous: prefix.clone(),
            transaction: transaction.clone(),
            rows: vec![DatasetRow {
                ordinal: 0,
                source_start: 0,
                source_end: 1,
                value: Value::new(
                    Shape::Primitive(Primitive::Int),
                    Data::Int(9),
                    Provenance::default(),
                )
                .unwrap(),
            }],
            source: extent(1),
            lifecycle: DatasetLifecycle::Open,
            policy: FlowPolicy::default(),
            checkpoint: None,
            recording: Some(coverage.clone()),
        })
        .unwrap_err();
    assert!(matches!(
        error,
        DatasetError::Objects(ObjectError::Format(FormatError::Limit(_)))
    ));
    assert_eq!(store.writes[&dataset].transaction, transaction);
    assert_eq!(store.writes[&dataset].state, catalog::WriteState::Requested);
    assert_eq!(store.descriptor(&dataset).unwrap(), prefix);
    coverage.committed_through = 0;
    coverage.pending = Some(1);
    coverage.termination = Some(RecordingEnd::WriteFailed);
    let closed = store
        .append_owned(DatasetAppend {
            owner: Some(owner.clone()),
            previous: prefix,
            transaction: uuid::Uuid::new_v4().to_string(),
            rows: vec![],
            source: extent(0),
            lifecycle: DatasetLifecycle::Incomplete,
            policy: FlowPolicy::default(),
            checkpoint: None,
            recording: Some(coverage.clone()),
        })
        .unwrap();
    drop(lease);
    drop(store);
    let mut store = DatasetStore::open(tmp.path(), Durability::FileAndDirectory, limits).unwrap();
    let restored = store.read_exact(&closed).unwrap();
    assert_eq!(restored.lifecycle, Lifecycle::Incomplete);
    assert_eq!(restored.recording, Some(coverage));
    let receipt = store.reconcile_owner(&owner.selection()).unwrap();
    assert_eq!(receipt.committed, Some(closed));
}

#[test]
fn explicit_store_recovery_repairs_unowned_reference_uncertainty_without_a_write_owner() {
    let tmp = home();
    let mut store = DatasetStore::open(
        tmp.path(),
        Durability::FileAndDirectory,
        StoreLimits::default(),
    )
    .unwrap();
    let manifest = create(&mut store);
    store.commit(&manifest, &FlowPolicy::default()).unwrap();
    let prefix = store.descriptor(&manifest.dataset).unwrap();
    let transaction = uuid::Uuid::new_v4().to_string();
    let change = ReferenceRootChange {
        root: uuid::Uuid::new_v4().to_string(),
        kind: RootKind::Keep,
        owner_dataset: None,
        owner_workspace: None,
        expected_generation: 0,
        generation: 1,
        prefixes: vec![prefix],
        captures: vec![],
        retention: RootRetention::Protected,
        transaction: transaction.clone(),
    };
    assert!(matches!(
        store.update_references_with_sync(
            &transaction,
            &[change.clone()],
            &FlowPolicy::default(),
            |_| Err(io::Error::other("synthetic lost reference acknowledgement"))
        ),
        Err(DatasetError::CommitUnconfirmed { .. })
    ));
    assert!(store.writes.is_empty());
    assert_eq!(
        store.reconcile_store().unwrap(),
        wes_engine::history::Persistence::FileAndDirectorySynced
    );
    assert_eq!(store.reference_root(&change.root).unwrap(), Some(change));
    assert!(store.writes.is_empty());
    // A checksum-valid empty home with a torn tail is repairable without fabricating an owner.
    let empty = home();
    let store = DatasetStore::open(empty.path(), Durability::File, StoreLimits::default()).unwrap();
    let path = empty.path().join("catalog").join(ACTIVE);
    let mut bytes = std::fs::read(&path).unwrap();
    bytes.extend_from_slice(b"WESC");
    std::fs::write(path, bytes).unwrap();
    drop(store);
    let mut store =
        DatasetStore::open(empty.path(), Durability::File, StoreLimits::default()).unwrap();
    assert!(matches!(
        store.ready(),
        Err(DatasetError::NeedsReconciliation)
    ));
    store.reconcile_store().unwrap();
    assert!(store.ready().is_ok());
    assert!(store.roots.is_empty());
}

#[test]
fn reference_root_capacity_refuses_recording_before_creating_admission_or_payload() {
    use wes_engine::storage::datasets::{
        DatasetCreate, DatasetKind as Kind, DatasetWriteOwner, DatasetWriteRole, EventLogCoverage,
        SourceExtent, SourceUnit,
    };
    let tmp = home();
    let limits = StoreLimits {
        reference_roots: 1,
        ..Default::default()
    };
    let mut store = DatasetStore::open(tmp.path(), Durability::File, limits).unwrap();
    let existing = create(&mut store);
    store.commit(&existing, &FlowPolicy::default()).unwrap();
    let prefix = store.descriptor(&existing.dataset).unwrap();
    let transaction = uuid::Uuid::new_v4().to_string();
    store
        .update_references(
            &transaction,
            &[ReferenceRootChange {
                root: uuid::Uuid::new_v4().to_string(),
                kind: RootKind::Keep,
                owner_dataset: None,
                owner_workspace: None,
                expected_generation: 0,
                generation: 1,
                prefixes: vec![prefix],
                captures: vec![],
                retention: RootRetention::Protected,
                transaction: transaction.clone(),
            }],
            &FlowPolicy::default(),
        )
        .unwrap();
    let before = store.charged_bytes();
    for _ in 0..3 {
        let dataset = uuid::Uuid::new_v4().to_string();
        let run = uuid::Uuid::new_v4().to_string();
        let request = DatasetCreate {
            owner: Some(DatasetWriteOwner {
                role: DatasetWriteRole::Recording,
                run: run.clone(),
                lineage: run,
            }),
            dataset: dataset.clone(),
            transaction: uuid::Uuid::new_v4().to_string(),
            kind: Kind::EventLog,
            schema: schema(),
            source: SourceExtent {
                identity: dataset.clone(),
                unit: SourceUnit::Records,
                start: 0,
                end: 0,
            },
            policy: FlowPolicy::default(),
            checkpoint: None,
            recording: Some(EventLogCoverage {
                run: uuid::Uuid::new_v4().to_string(),
                epoch: dataset,
                first: 1,
                accepted_through: 0,
                committed_through: 0,
                pending: Some(0),
                rejected: 0,
                termination: None,
            }),
        };
        assert!(matches!(
            store.create_owned(request),
            Err(DatasetError::Limit("reference roots"))
        ));
        assert!(store.writes.is_empty());
        assert_eq!(store.roots.len(), 1);
        assert_eq!(store.charged_bytes(), before);
    }
}

#[test]
fn workspace_roots_hold_exact_shared_prefixes_until_each_generation_is_physically_retired() {
    use wes_engine::storage::ValueHandle;
    let tmp = home();
    let limits = StoreLimits::default();
    let mut store = DatasetStore::open(tmp.path(), Durability::FileAndDirectory, limits).unwrap();
    let mut manifest = create(&mut store);
    append(&mut store, &mut manifest, 2);
    store.commit(&manifest, &FlowPolicy::default()).unwrap();
    let prefix = store.descriptor(&manifest.dataset).unwrap();
    let handle = ValueHandle::new(&uuid::Uuid::new_v4().to_string()).unwrap();
    let tx = uuid::Uuid::new_v4().to_string();
    let root = ReferenceRootChange {
        root: handle.to_string(),
        kind: RootKind::Value,
        expected_generation: 0,
        generation: 1,
        prefixes: vec![prefix.clone()],
        owner_dataset: None,
        owner_workspace: None,
        captures: vec![],
        retention: RootRetention::Automatic,
        transaction: tx.clone(),
    };
    store
        .update_references(&tx, &[root.clone()], &FlowPolicy::default())
        .unwrap();
    let a = uuid::Uuid::new_v4().to_string();
    let b = uuid::Uuid::new_v4().to_string();
    store
        .protect_workspace_owned(&a, std::slice::from_ref(&handle))
        .unwrap();
    store
        .protect_workspace_owned(&b, std::slice::from_ref(&handle))
        .unwrap();
    store
        .protect_workspace_owned(&a, &[handle.clone(), handle.clone()])
        .unwrap();
    assert_eq!(store.references.len(), 3);
    let mut release = root;
    release.expected_generation = 1;
    release.generation = 2;
    release.prefixes.clear();
    release.retention = RootRetention::Temporary;
    release.transaction = uuid::Uuid::new_v4().to_string();
    store
        .update_references(
            &release.transaction.clone(),
            &[release],
            &FlowPolicy::default(),
        )
        .unwrap();
    let plan = store.plan_delete_owned(&prefix).unwrap();
    assert_eq!(plan.references.len(), 2);
    assert!(matches!(
        store.delete_owned(&plan.token, false, false),
        Err(DatasetError::Limit(_))
    ));
    store.retire_workspace_owned(&a).unwrap();
    store.rotate().unwrap();
    drop(store);
    let mut store = DatasetStore::open(tmp.path(), Durability::FileAndDirectory, limits).unwrap();
    assert!(store.is_protected(&prefix));
    assert_eq!(
        store.plan_delete_owned(&prefix).unwrap().references.len(),
        1
    );
    store.retire_workspace_owned(&a).unwrap(); // Physically completed cleanup is idempotent.
    store.retire_workspace_owned(&b).unwrap();
    assert!(!store.is_protected(&prefix));
    assert_eq!(store.references.len(), 1); // Released workspace roots consume no permanent slots.
    let plan = store.plan_delete_owned(&prefix).unwrap();
    assert!(
        store
            .delete_owned(&plan.token, true, true)
            .unwrap()
            .complete
    );
}

#[test]
fn workspace_protection_never_upgrades_a_temporary_value_or_publishes_beyond_capacity() {
    use wes_engine::storage::ValueHandle;
    let tmp = home();
    let mut store = DatasetStore::open(
        tmp.path(),
        Durability::File,
        StoreLimits {
            reference_roots: 1,
            ..StoreLimits::default()
        },
    )
    .unwrap();
    let manifest = create(&mut store);
    store.commit(&manifest, &FlowPolicy::default()).unwrap();
    let prefix = store.descriptor(&manifest.dataset).unwrap();
    let handle = ValueHandle::new(&uuid::Uuid::new_v4().to_string()).unwrap();
    let tx = uuid::Uuid::new_v4().to_string();
    let root = ReferenceRootChange {
        root: handle.to_string(),
        kind: RootKind::Value,
        expected_generation: 0,
        generation: 1,
        prefixes: vec![prefix],
        owner_dataset: None,
        owner_workspace: None,
        captures: vec![],
        retention: RootRetention::Temporary,
        transaction: tx.clone(),
    };
    store
        .update_references(&tx, &[root.clone()], &FlowPolicy::default())
        .unwrap();
    let saved_generation = uuid::Uuid::new_v4().to_string();
    let sequence = store.next_sequence;
    assert!(matches!(
        store.protect_workspace_owned(&saved_generation, std::slice::from_ref(&handle)),
        Err(DatasetError::Conflict)
    ));
    assert_eq!(store.next_sequence, sequence);
    let mut root = root;
    root.expected_generation = 1;
    root.generation = 2;
    root.retention = RootRetention::Automatic;
    root.transaction = uuid::Uuid::new_v4().to_string();
    store
        .update_references(&root.transaction.clone(), &[root], &FlowPolicy::default())
        .unwrap();
    let sequence = store.next_sequence;
    assert!(matches!(
        store.protect_workspace_owned(&saved_generation, &[handle]),
        Err(DatasetError::Limit("workspace roots"))
    ));
    assert_eq!(store.next_sequence, sequence);
    assert_eq!(store.references.len(), 1);
}

#[test]
fn workspace_protection_survives_read_withdrawal_and_unrelated_catalog_uncertainty_without_granting_access()
 {
    use wes_engine::storage::ValueHandle;
    let tmp = home();
    let limits = StoreLimits::default();
    let mut store = DatasetStore::open(tmp.path(), Durability::FileAndDirectory, limits).unwrap();
    let manifest = create(&mut store);
    store.commit(&manifest, &FlowPolicy::default()).unwrap();
    let prefix = store.descriptor(&manifest.dataset).unwrap();
    let handle = ValueHandle::new(&uuid::Uuid::new_v4().to_string()).unwrap();
    let tx = uuid::Uuid::new_v4().to_string();
    let root = ReferenceRootChange {
        root: handle.to_string(),
        kind: RootKind::Value,
        expected_generation: 0,
        generation: 1,
        prefixes: vec![prefix.clone()],
        owner_dataset: None,
        owner_workspace: None,
        captures: vec![],
        retention: RootRetention::Protected,
        transaction: tx.clone(),
    };
    store
        .update_references(&tx, &[root], &FlowPolicy::default())
        .unwrap();
    let a = uuid::Uuid::new_v4().to_string();
    let b = uuid::Uuid::new_v4().to_string();
    store
        .protect_workspace_owned(&a, std::slice::from_ref(&handle))
        .unwrap();
    store.withdraw_owned(&prefix).unwrap();
    assert!(matches!(
        store.read_exact(&prefix),
        Err(DatasetError::Withdrawn)
    ));
    store
        .protect_workspace_owned(&a, std::slice::from_ref(&handle))
        .unwrap();
    store
        .protect_workspace_owned(&b, std::slice::from_ref(&handle))
        .unwrap();
    store.rotate().unwrap();
    drop(store);
    let mut store = DatasetStore::open(tmp.path(), Durability::FileAndDirectory, limits).unwrap();
    assert!(matches!(
        store.read_exact(&prefix),
        Err(DatasetError::Withdrawn)
    ));
    assert_eq!(
        store
            .references
            .values()
            .filter(|r| r.owner_workspace.is_some())
            .count(),
        2
    );
    // A complete unrelated root write loses its ack. Stable retention metadata remains usable,
    // but new writes and the root IDs touched by that uncertain mutation remain blocked.
    let tx = uuid::Uuid::new_v4().to_string();
    let unrelated = ReferenceRootChange {
        root: uuid::Uuid::new_v4().to_string(),
        kind: RootKind::Keep,
        expected_generation: 0,
        generation: 1,
        prefixes: vec![],
        owner_dataset: None,
        owner_workspace: None,
        captures: vec![],
        retention: RootRetention::Temporary,
        transaction: tx.clone(),
    };
    assert!(matches!(
        store.update_references_with_sync(&tx, &[unrelated], &FlowPolicy::default(), |_| Err(
            io::Error::other("synthetic lost sync")
        )),
        Err(DatasetError::CommitUnconfirmed { .. })
    ));
    let sequence = store.next_sequence;
    store
        .protect_workspace_owned(&a, std::slice::from_ref(&handle))
        .unwrap();
    let ordinary = ValueHandle::new(&uuid::Uuid::new_v4().to_string()).unwrap();
    store.protect_workspace_owned(&a, &[ordinary]).unwrap();
    assert_eq!(store.next_sequence, sequence);
    assert!(
        store
            .protect_workspace_owned(&uuid::Uuid::new_v4().to_string(), &[handle])
            .is_err()
    );
    store.reconcile(&tx).unwrap();
    assert!(matches!(
        store.read_exact(&prefix),
        Err(DatasetError::Withdrawn)
    ));
}

#[test]
fn workspace_metadata_budget_preflights_the_whole_request_without_partial_protection() {
    use wes_engine::storage::ValueHandle;
    let tmp = home();
    let mut store = DatasetStore::open(
        tmp.path(),
        Durability::File,
        StoreLimits {
            workspace_root_bytes: 1024,
            ..StoreLimits::default()
        },
    )
    .unwrap();
    let manifest = create(&mut store);
    store.commit(&manifest, &FlowPolicy::default()).unwrap();
    let prefix = store.descriptor(&manifest.dataset).unwrap();
    let mut handles = Vec::new();
    for _ in 0..12 {
        let handle = ValueHandle::new(&uuid::Uuid::new_v4().to_string()).unwrap();
        let tx = uuid::Uuid::new_v4().to_string();
        let root = ReferenceRootChange {
            root: handle.to_string(),
            kind: RootKind::Value,
            expected_generation: 0,
            generation: 1,
            prefixes: vec![prefix.clone()],
            owner_dataset: None,
            owner_workspace: None,
            captures: vec![],
            retention: RootRetention::Protected,
            transaction: tx.clone(),
        };
        store
            .update_references(&tx, &[root], &FlowPolicy::default())
            .unwrap();
        handles.push(handle);
    }
    let sequence = store.next_sequence;
    assert!(matches!(
        store.protect_workspace_owned(&uuid::Uuid::new_v4().to_string(), &handles),
        Err(DatasetError::Format(FormatError::Limit(_)))
    ));
    assert_eq!(store.next_sequence, sequence);
    assert!(
        store
            .references
            .values()
            .all(|root| root.owner_workspace.is_none())
    );
}

#[test]
fn workspace_retention_does_not_merge_independent_values_into_one_bounded_data_flow() {
    use wes_engine::storage::ValueHandle;
    let tmp = home();
    let limits = StoreLimits::default();
    let mut store = DatasetStore::open(tmp.path(), Durability::File, limits).unwrap();
    let mut handles = Vec::new();
    for _ in 0..129 {
        let manifest = create(&mut store);
        store.commit(&manifest, &FlowPolicy::default()).unwrap();
        let prefix = store.descriptor(&manifest.dataset).unwrap();
        let handle = ValueHandle::new(&uuid::Uuid::new_v4().to_string()).unwrap();
        let tx = uuid::Uuid::new_v4().to_string();
        store
            .update_references(
                &tx,
                &[ReferenceRootChange {
                    root: handle.to_string(),
                    kind: RootKind::Value,
                    expected_generation: 0,
                    generation: 1,
                    prefixes: vec![prefix],
                    owner_dataset: None,
                    owner_workspace: None,
                    captures: vec![],
                    retention: RootRetention::Protected,
                    transaction: tx.clone(),
                }],
                &FlowPolicy::default(),
            )
            .unwrap();
        handles.push(handle);
    }
    store
        .protect_workspace_owned(&uuid::Uuid::new_v4().to_string(), &handles)
        .unwrap();
    assert_eq!(
        store
            .references
            .values()
            .filter(|r| r.owner_workspace.is_some())
            .count(),
        129
    );
    drop(store);
    let store = DatasetStore::open(tmp.path(), Durability::File, limits).unwrap();
    assert_eq!(
        store
            .references
            .values()
            .filter(|r| r.owner_workspace.is_some())
            .count(),
        129
    );
}

#[test]
fn reviewed_delete_preserves_other_withdrawn_prefixes_consistently_in_replay_and_rotation() {
    let tmp = home();
    let limits = StoreLimits::default();
    let mut store = DatasetStore::open(tmp.path(), Durability::FileAndDirectory, limits).unwrap();
    let d = create(&mut store);
    store.commit(&d, &FlowPolicy::default()).unwrap();
    let d = store.descriptor(&d.dataset).unwrap();
    let e = create(&mut store);
    store.commit(&e, &FlowPolicy::default()).unwrap();
    let e = store.descriptor(&e.dataset).unwrap();
    let identity = uuid::Uuid::new_v4().to_string();
    let tx = uuid::Uuid::new_v4().to_string();
    store
        .update_references(
            &tx,
            &[ReferenceRootChange {
                root: identity.clone(),
                kind: RootKind::Value,
                expected_generation: 0,
                generation: 1,
                prefixes: vec![d.clone(), e.clone()],
                owner_dataset: None,
                owner_workspace: None,
                captures: vec![],
                retention: RootRetention::Protected,
                transaction: tx.clone(),
            }],
            &FlowPolicy::default(),
        )
        .unwrap();
    store.withdraw_owned(&e).unwrap();
    let plan = store.plan_delete_owned(&d).unwrap();
    store.delete_owned(&plan.token, true, true).unwrap();
    drop(store);
    let mut store = DatasetStore::open(tmp.path(), Durability::FileAndDirectory, limits).unwrap();
    assert_eq!(store.references[&identity].prefixes, vec![e.clone()]);
    assert!(matches!(store.read_exact(&e), Err(DatasetError::Withdrawn)));
    assert!(matches!(store.read_exact(&d), Err(DatasetError::Withdrawn)));
    store.rotate().unwrap();
    drop(store);
    let store = DatasetStore::open(tmp.path(), Durability::FileAndDirectory, limits).unwrap();
    assert_eq!(store.references[&identity].prefixes, vec![e.clone()]);
    assert!(matches!(store.read_exact(&e), Err(DatasetError::Withdrawn)));
}

#[test]
fn snapshot_captures_exact_committed_extension_and_preserves_read_only_prefix_lifecycle() {
    use wes_engine::storage::datasets::{DatasetLifecycle as L, DatasetSnapshot, DatasetStorage};
    let tmp = home();
    let mut store = DatasetStore::open(
        tmp.path(),
        Durability::FileAndDirectory,
        StoreLimits::default(),
    )
    .unwrap();
    let mut first = create(&mut store);
    append(&mut store, &mut first, 1);
    store.commit(&first, &FlowPolicy::default()).unwrap();
    let anchor = store.descriptor(&first.dataset).unwrap();
    let mut second = next(&store, &first);
    append(&mut store, &mut second, 2);
    store.commit(&second, &FlowPolicy::default()).unwrap();
    let shown = store.descriptor(&first.dataset).unwrap();
    let mut sealed = next(&store, &second);
    sealed.lifecycle = Lifecycle::Sealed;
    store.commit(&sealed, &FlowPolicy::default()).unwrap();
    let terminal = store.descriptor(&first.dataset).unwrap();
    let selection = |r: &DatasetRef| DatasetSnapshot {
        basis: anchor.manifest_digest().into(),
        generation: r.generation(),
        digest: r.manifest_digest().into(),
    };
    let captured = DatasetStorage::snapshot(&store, &anchor, selection(&shown)).unwrap();
    assert_eq!(captured.reference, shown);
    assert_eq!(captured.lifecycle, L::Prefix);
    assert_eq!(
        DatasetStorage::inspect(&store, &anchor).unwrap().lifecycle,
        L::Prefix
    );
    assert_eq!(
        DatasetStorage::snapshot(&store, &anchor, selection(&terminal))
            .unwrap()
            .lifecycle,
        L::Sealed
    );
    assert_eq!(
        terminal.records(),
        shown.records(),
        "same-count terminal metadata is capturable"
    );
    assert!(!store.is_protected(&shown), "capture reads do not Keep");
    assert_eq!(
        store.descriptor(&first.dataset).unwrap(),
        terminal,
        "capture does not mutate the head"
    );
    let mut bad = selection(&shown);
    bad.basis = format!("sha256:{}", "f".repeat(64));
    assert!(DatasetStorage::snapshot(&store, &anchor, bad).is_err());
    let mut bad = selection(&shown);
    bad.digest = terminal.manifest_digest().into();
    assert!(DatasetStorage::snapshot(&store, &anchor, bad).is_err());
    let mut bad = selection(&shown);
    bad.generation = terminal.generation() + 1;
    assert!(DatasetStorage::snapshot(&store, &anchor, bad).is_err());
    let orphan = next(&store, &sealed);
    let physical = store
        .prepare()
        .unwrap()
        .publish_manifest(&orphan, &FlowPolicy::default())
        .unwrap();
    let bad = DatasetSnapshot {
        basis: anchor.manifest_digest().into(),
        generation: orphan.generation,
        digest: physical.digest,
    };
    assert!(
        DatasetStorage::snapshot(&store, &anchor, bad).is_err(),
        "uncommitted physical objects grant no authority"
    );
    drop(store);
    let mut store = DatasetStore::open(
        tmp.path(),
        Durability::FileAndDirectory,
        StoreLimits::default(),
    )
    .unwrap();
    assert_eq!(
        DatasetStorage::snapshot(&store, &anchor, selection(&shown))
            .unwrap()
            .lifecycle,
        L::Prefix
    );
    store.withdraw_owned(&terminal).unwrap();
    assert!(matches!(
        DatasetStorage::snapshot(&store, &anchor, selection(&shown)),
        Err(wes_engine::storage::StoreError::DatasetWithdrawn)
    ));
}

#[test]
fn retention_preview_counts_a_shared_schema_once_and_keeps_withdrawal_separate_from_overlap_accounting()
 {
    use wes_engine::storage::datasets::DatasetStorage;
    let tmp = home();
    let mut store = DatasetStore::open(
        tmp.path(),
        Durability::FileAndDirectory,
        StoreLimits::default(),
    )
    .unwrap();
    let mut first = create(&mut store);
    append(&mut store, &mut first, 2);
    first.lifecycle = Lifecycle::Sealed;
    store.commit(&first, &FlowPolicy::default()).unwrap();
    let a = store.descriptor(&first.dataset).unwrap();
    let mut second = create(&mut store);
    second.schema = first.schema.clone();
    second.schema_digest = first.schema_digest.clone();
    append(&mut store, &mut second, 1);
    second.lifecycle = Lifecycle::Sealed;
    store.commit(&second, &FlowPolicy::default()).unwrap();
    let b = store.descriptor(&second.dataset).unwrap();
    let total = store.retention_bytes(&b).unwrap();
    assert_eq!(
        store.retention_bytes_many(&[a.clone(), b.clone()]).unwrap(),
        store.retention_bytes(&a).unwrap() + total - first.schema.bytes,
        "a larger value pays shared schema storage once"
    );
    assert_eq!(
        store.retention_bytes_many(&[b.clone(), b.clone()]).unwrap(),
        total,
        "aliases are not extra physical storage"
    );
    let mut sequence = store.next_sequence;
    let preview = DatasetStorage::retention_preview(&store, &b).unwrap();
    assert_eq!(preview.object_bytes, total);
    assert_eq!(preview.shared_object_bytes, 0);
    assert!(preview.captures.is_empty());
    assert_eq!(
        store.next_sequence, sequence,
        "preview never creates a root or reservation"
    );
    let keep = |reference: DatasetRef| {
        let transaction = uuid::Uuid::new_v4().to_string();
        ReferenceRootChange {
            root: uuid::Uuid::new_v4().to_string(),
            kind: RootKind::Keep,
            owner_dataset: None,
            owner_workspace: None,
            expected_generation: 0,
            generation: 1,
            prefixes: vec![reference],
            captures: vec![],
            retention: RootRetention::Protected,
            transaction,
        }
    };
    let root = keep(a.clone());
    store
        .update_references(&root.transaction, &[root.clone()], &FlowPolicy::default())
        .unwrap();
    let preview = DatasetStorage::retention_preview(&store, &b).unwrap();
    assert_eq!(preview.shared_object_bytes, first.schema.bytes);
    assert_eq!(preview.object_bytes, total);
    assert!(!store.is_protected(&b));
    store.withdraw_owned(&a).unwrap();
    assert_eq!(
        DatasetStorage::retention_preview(&store, &b)
            .unwrap()
            .shared_object_bytes,
        first.schema.bytes,
        "withdrawn independent bytes stay protected; overlap accounting grants no content access"
    );
    assert!(
        DatasetStorage::retention_preview(&store, &a).is_err(),
        "selected withdrawal still refuses"
    );
    let root = keep(b.clone());
    store
        .update_references(&root.transaction, &[root.clone()], &FlowPolicy::default())
        .unwrap();
    sequence = store.next_sequence;
    let preview = DatasetStorage::retention_preview(&store, &b).unwrap();
    assert_eq!(preview.shared_object_bytes, total);
    assert_eq!(preview.catalog_revision, sequence - 1);
    assert_eq!(store.next_sequence, sequence);
    store.limits.objects.index.traversal_nodes = 1;
    assert!(
        matches!(
            DatasetStorage::retention_preview(&store, &b),
            Err(wes_engine::storage::StoreError::Limit(_))
        ),
        "never return a partial footprint after a bounded walk refuses"
    );
}
