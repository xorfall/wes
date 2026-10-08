//! Synthetic framing through the real shared publisher, catalog, readers and cleanup.
use super::*;
use wes_core::framing::{Decoding, Delimiter, Frame, Framer, Malformed, Profile, Rejection};
use wes_engine::storage::datasets::{
    AnalysisCheckpoint, CapturedValue, CoveragePolicy, CoverageProgress, DatasetAppend,
    DatasetCreate, DatasetKind as Kind, DatasetLifecycle, DatasetRow, DatasetStorage,
    DatasetWriterAdmission, PageRequest, SourceExtent, SourceUnit,
};

fn request() -> PageRequest {
    PageRequest {
        charge: None,
        from: 0,
        rows: 10,
        bytes: 65536,
        segments: 8,
        work: None,
    }
}
fn covered(store: &mut DatasetStore) -> (DatasetWriterAdmission, AnalysisCheckpoint) {
    let mut manifest = create(store);
    manifest.source.unit = PositionUnit::Bytes;
    manifest.source.end = 16;
    let saved = checkpoint(store, &manifest);
    let scalar = saved
        .state
        .value(&schema(), store.limits.objects.checkpoint)
        .unwrap();
    let policy = CoveragePolicy { excerpt_bytes: 3 };
    let checkpoint = AnalysisCheckpoint {
        coverage: Some(CoverageProgress::empty(policy)),
        stop: None,
        budget: saved.budget,
        analysis: saved.analysis,
        attempt: saved.attempt,
        previous_attempt: None,
        run: saved.run,
        task_revision: saved.bindings.task_revision,
        source: CapturedValue {
            handle: saved.bindings.source_handle,
            digest: saved.bindings.source_digest,
            bytes: saved.bindings.source_bytes,
        },
        followed_source: None,
        item_schema: schema(),
        state_schema: schema(),
        context_schema: schema(),
        state: scalar.clone(),
        context: scalar,
        step_revision: saved.bindings.step_revision,
        finish_revision: None,
        profile_digest: saved.bindings.profile_digest,
        initial_digest: saved.bindings.initial_digest,
        captured_program: saved.bindings.captured_program,
        next_position: 0,
        next_ordinal: 0,
        decoder_carry: vec![],
        work: saved.work,
        usage: saved.usage,
        duration: saved.duration,
        finish_applied: false,
    };
    let admission = store
        .create_owned(DatasetCreate {
            owner: None,
            dataset: manifest.dataset,
            transaction: manifest.transaction,
            kind: Kind::Analysis,
            schema: schema(),
            source: SourceExtent {
                identity: manifest.source.identity,
                unit: SourceUnit::Bytes,
                start: 0,
                end: 16,
            },
            policy: FlowPolicy::default(),
            checkpoint: Some(checkpoint.clone()),
            recording: None,
            coverage: Some(policy),
        })
        .unwrap();
    (admission, checkpoint)
}
fn rejected() -> Vec<Rejection> {
    let mut framer = Framer::new(Profile {
        delimiter: Delimiter::Lines,
        decoding: Decoding::StrictUtf8,
        malformed: Malformed::Forensic { excerpt_bytes: 3 },
        raw_bytes: 3,
        decoded_bytes: 64,
        spans: 16,
    })
    .unwrap();
    let mut rejected = vec![];
    framer
        .push_frames(b"abcdef\nok\nbadxx\n", |frame| {
            if let Frame::Rejected(row) = frame {
                rejected.push(row);
            }
            Ok::<_, ()>(())
        })
        .unwrap();
    framer.finish_frames(|_| Ok::<_, ()>(())).unwrap();
    assert_eq!(rejected.len(), 2);
    rejected
}
fn batch(previous: &DatasetRef, mut checkpoint: AnalysisCheckpoint) -> DatasetAppend {
    let coverage = rejected();
    checkpoint.next_position = 16;
    checkpoint.next_ordinal = 3;
    checkpoint.usage.input_bytes = 16;
    checkpoint.usage.output_bytes = 4096;
    checkpoint.duration.outstanding_ms = 0;
    for row in &coverage {
        checkpoint
            .coverage
            .as_mut()
            .unwrap()
            .observe(row, 3, 16)
            .unwrap();
    }
    DatasetAppend {
        owner: None,
        previous: previous.clone(),
        transaction: uuid::Uuid::new_v4().to_string(),
        rows: vec![DatasetRow {
            ordinal: 0,
            source_start: 7,
            source_end: 10,
            value: Value::new(
                Shape::Primitive(Primitive::Int),
                Data::Int(1),
                Provenance::default(),
            )
            .unwrap(),
        }],
        source: SourceExtent {
            identity: "synthetic-captured-input-r1".into(),
            unit: SourceUnit::Bytes,
            start: 0,
            end: 16,
        },
        lifecycle: DatasetLifecycle::Sealed,
        policy: FlowPolicy::default(),
        checkpoint: Some(checkpoint),
        recording: None,
        coverage,
    }
}

#[test]
fn two_typed_trees_and_checkpoint_publish_as_one_exact_parent_prefix() {
    let tmp = home();
    let mut store =
        DatasetStore::open(tmp.path(), Durability::File, StoreLimits::default()).unwrap();
    let (writer, saved) = covered(&mut store);
    let old = writer.reference.clone();
    let current = store.append_owned(batch(&old, saved)).unwrap();
    assert_eq!(
        current.records(),
        1,
        "the output descriptor does not invent coverage outputs"
    );
    let info = DatasetStorage::inspect(&store, &current).unwrap();
    let coverage = info.coverage.unwrap();
    assert_eq!(
        (coverage.progress.records, coverage.progress.input_bytes),
        (2, 13)
    );
    assert_eq!(
        (coverage.progress.last_ordinal, coverage.progress.through),
        (Some(2), Some(16))
    );
    let cp = store.read_analysis_checkpoint(&current).unwrap().unwrap();
    assert_eq!((cp.next_position, cp.next_ordinal), (16, 3));
    assert_eq!(cp.coverage.as_ref(), Some(&coverage.progress));
    let outputs = DatasetStorage::page(&store, &current, request()).unwrap();
    assert_eq!(outputs.rows.len(), 1);
    assert_eq!(outputs.rows[0].value.data(), &Data::Int(1));
    assert_eq!(outputs.schema.digest(), schema().digest());
    let page = DatasetStorage::coverage_page(&store, &current, request()).unwrap();
    assert_eq!(
        page.reference, current,
        "lease and authority bind the actual parent manifest"
    );
    assert_eq!((page.first, page.next, page.extent_exhausted), (0, 2, true));
    assert_ne!(page.schema.digest(), outputs.schema.digest());
    for (row, expected) in page.rows.iter().zip(rejected()) {
        assert_eq!(
            wes_engine::storage::datasets::read_rejection(&row.value, coverage.progress.policy)
                .unwrap(),
            expected
        );
    }
    assert!(
        DatasetStorage::coverage_page(&store, &old, request())
            .unwrap()
            .rows
            .is_empty()
    );
    let second = DatasetStorage::coverage_page(
        &store,
        &current,
        PageRequest {
            charge: None,
            from: 1,
            rows: 1,
            ..request()
        },
    )
    .unwrap();
    assert_eq!(
        (second.first, second.next),
        (1, 2),
        "coverage ordinals are independent of one output row"
    );
    assert!(
        DatasetStorage::coverage_page(
            &store,
            &current,
            PageRequest {
                charge: None,
                from: 3,
                ..request()
            }
        )
        .is_err()
    );
    let held = store.retention_bytes(&current).unwrap();
    assert!(held > store.retention_bytes(&old).unwrap());
    let manifest = store.read_exact(&current).unwrap();
    assert_eq!(
        info.segment_bytes,
        manifest.summary.segment_bytes + manifest.coverage.as_ref().unwrap().summary.segment_bytes
    );
    drop((page, outputs, second, writer));
    drop(store);
    let mut store =
        DatasetStore::open(tmp.path(), Durability::File, StoreLimits::default()).unwrap();
    assert_eq!(store.retention_bytes(&current).unwrap(), held);
    assert_eq!(
        DatasetStorage::coverage_page(&store, &current, request())
            .unwrap()
            .rows
            .len(),
        2
    );
    assert_eq!(
        store
            .read_analysis_checkpoint(&current)
            .unwrap()
            .unwrap()
            .coverage,
        Some(coverage.progress)
    );
    let reader = DatasetStorage::coverage_page(&store, &current, request()).unwrap();
    let plan = store.plan_delete_owned(&current).unwrap();
    assert!(store.delete_owned(&plan.token, true, true).is_err());
    drop(reader);
    let cleanup = store.delete_owned(&plan.token, true, true).unwrap();
    assert!(cleanup.reclaimed_bytes.unwrap() > 0);
    assert!(matches!(
        DatasetStorage::coverage_page(&store, &current, request()),
        Err(wes_engine::storage::StoreError::DatasetWithdrawn)
    ));
}

#[test]
fn torn_publication_exposes_neither_tree_nor_advanced_checkpoint_and_cleanup_reclaims_both() {
    let tmp = home();
    let mut store =
        DatasetStore::open(tmp.path(), Durability::File, StoreLimits::default()).unwrap();
    let (writer, saved) = covered(&mut store);
    let old = writer.reference.clone();
    let boundary = store.valid_bytes;
    let advanced = store.append_owned(batch(&old, saved)).unwrap();
    let path = tmp.path().join("catalog").join(ACTIVE);
    let mut bytes = std::fs::read(&path).unwrap();
    bytes.truncate(boundary + 20);
    drop(writer);
    drop(store);
    std::fs::write(&path, bytes).unwrap();
    let mut store =
        DatasetStore::open(tmp.path(), Durability::File, StoreLimits::default()).unwrap();
    assert_eq!(
        store.descriptor(old.dataset()).unwrap(),
        old,
        "the complete predecessor remains readable without publishing either candidate tree"
    );
    assert!(matches!(
        store.prepare(),
        Err(DatasetError::NeedsReconciliation)
    ));
    assert_eq!(std::fs::read(&path).unwrap().len(), boundary + 20);
    store.reconcile_store().unwrap();
    assert_eq!(store.descriptor(old.dataset()).unwrap(), old);
    assert!(
        DatasetStorage::page(&store, &old, request())
            .unwrap()
            .rows
            .is_empty()
    );
    assert!(
        DatasetStorage::coverage_page(&store, &old, request())
            .unwrap()
            .rows
            .is_empty()
    );
    let cp = store.read_analysis_checkpoint(&old).unwrap().unwrap();
    assert_eq!(
        (
            cp.next_position,
            cp.next_ordinal,
            cp.coverage.unwrap().records
        ),
        (0, 0, 0)
    );
    assert!(DatasetStorage::coverage_page(&store, &advanced, request()).is_err());
    assert!(store.collect_owned().unwrap().reclaimed_bytes.unwrap() > 0);
}

#[test]
fn forged_or_reordered_coverage_refuses_before_admission_and_cannot_advance_either_root() {
    let tmp = home();
    let mut store =
        DatasetStore::open(tmp.path(), Durability::File, StoreLimits::default()).unwrap();
    let (writer, saved) = covered(&mut store);
    let old = writer.reference.clone();
    let mut bad = batch(&old, saved.clone());
    bad.coverage.reverse();
    let boundary = store.valid_bytes;
    assert!(store.append_owned(bad).is_err());
    assert_eq!(store.valid_bytes, boundary);
    let mut bad = batch(&old, saved.clone());
    bad.coverage[0].excerpt.truncate(1);
    assert!(store.append_owned(bad).is_err());
    let mut bad = batch(&old, saved);
    bad.checkpoint
        .as_mut()
        .unwrap()
        .coverage
        .as_mut()
        .unwrap()
        .input_bytes += 1;
    assert!(store.append_owned(bad).is_err());
    assert_eq!(store.descriptor(old.dataset()).unwrap(), old);
    assert_eq!(store.valid_bytes, boundary);
    assert!(
        DatasetStorage::coverage_page(&store, &old, request())
            .unwrap()
            .rows
            .is_empty()
    );
}

#[test]
fn checksummed_index_and_checkpoint_cannot_invent_segment_coverage_or_its_policy() {
    for change_policy in [false, true] {
        let tmp = home();
        let mut store =
            DatasetStore::open(tmp.path(), Durability::File, StoreLimits::default()).unwrap();
        let (writer, saved) = covered(&mut store);
        let current = store.append_owned(batch(&writer.reference, saved)).unwrap();
        let mut manifest = store.read_exact(&current).unwrap();
        let root = manifest.coverage.as_mut().unwrap();
        if change_policy {
            root.progress.policy.excerpt_bytes = 2;
        } else {
            let mut index = store
                .files
                .read_index(
                    root.index.as_ref().unwrap(),
                    &manifest.dataset,
                    crate::datasets::Stream::Coverage,
                    None,
                )
                .unwrap();
            let Entries::Leaf { entries } = &mut index.entries else {
                panic!("synthetic leaf")
            };
            entries[0].summary.coverage.as_mut().unwrap().input_bytes -= 1;
            index.refresh_summary().unwrap();
            root.summary = index.summary.clone();
            root.index = Some(
                store
                    .files
                    .publish_index(&index, &FlowPolicy::default())
                    .unwrap(),
            );
            root.progress.input_bytes -= 1;
        }
        let mut cp = store
            .files
            .read_checkpoint(
                manifest.checkpoint.as_ref().unwrap(),
                &manifest.dataset,
                None,
            )
            .unwrap();
        cp.coverage = Some(root.progress.clone());
        manifest.checkpoint = Some(
            store
                .files
                .publish_checkpoint(&cp, &FlowPolicy::default())
                .unwrap(),
        );
        // Each envelope and the aggregate agree, but neither attests the actual physical rows.
        manifest.encode(store.limits.objects.manifest).unwrap();
        store.verified_nodes.clear();
        store.verified_segments.clear();
        assert!(
            matches!(
                store.verify_manifest(&manifest),
                Err(DatasetError::StorageCorrupt)
            ),
            "change_policy={change_policy}"
        );
        assert_eq!(store.descriptor(current.dataset()).unwrap(), current);
    }
}

#[test]
fn unterminated_rejection_requires_the_frozen_extent_end() {
    let tmp = home();
    let mut store =
        DatasetStore::open(tmp.path(), Durability::File, StoreLimits::default()).unwrap();
    let (writer, saved) = covered(&mut store);
    let old = writer.reference.clone();
    let mut request = batch(&old, saved);
    request.coverage[0].delimiter.end = request.coverage[0].delimiter.start;
    request.coverage[0].unterminated = true;
    let cp = request.checkpoint.as_mut().unwrap();
    let mut progress = CoverageProgress::empty(cp.coverage.as_ref().unwrap().policy);
    for row in &request.coverage {
        progress
            .observe(row, cp.next_ordinal, cp.next_position)
            .unwrap();
    }
    cp.coverage = Some(progress);
    let boundary = store.valid_bytes;
    assert!(matches!(
        store.append_owned(request),
        Err(DatasetError::Conflict)
    ));
    assert_eq!(store.valid_bytes, boundary);
    assert_eq!(store.descriptor(old.dataset()).unwrap(), old);
}

#[test]
fn physical_inventory_admission_cannot_exceed_uncached_recovery_capacity() {
    let tmp = home();
    let path = tmp.path().join("invalid-limits");
    let mut limits = StoreLimits::default();
    limits.objects.index.traversal_nodes = limits.objects.inventory_entries - 1;
    assert!(matches!(
        DatasetStore::open(&path, Durability::File, limits),
        Err(DatasetError::Limit("configuration"))
    ));
    assert!(
        !path.exists(),
        "invalid admission capacity must refuse before creating the store"
    );
    // A small coherent inventory is deliberately small by real two-tree publications.
    limits.objects.inventory_entries = 32;
    limits.objects.index.traversal_nodes = 32;
    let mut store = DatasetStore::open(&path, Durability::File, limits).unwrap();
    let (writer, saved) = covered(&mut store);
    let old = writer.reference.clone();
    let current = store.append_owned(batch(&old, saved)).unwrap();
    store.collect_owned().unwrap();
    drop(writer);
    drop(store);
    let store = DatasetStore::open(&path, Durability::File, limits).unwrap();
    assert_eq!(store.descriptor(current.dataset()).unwrap(), current);
    assert_eq!(
        DatasetStorage::coverage_page(&store, &current, request())
            .unwrap()
            .rows
            .len(),
        2
    );
}
