use super::*;
use wes_engine::{
    driver::CancellationToken,
    scan::{Identity, Input, Runner, Settings, Transition, ledger::MemoryPool},
    storage::{
        StoreError,
        datasets::{DatasetStorage, ReadWorkDimension},
    },
};
use wes_language::{Expression, SourceText, Span, templates::Templates};

const STARTUP_WORK: u64 = 32_000_000;

fn reader(reference: DatasetRef) -> Runner {
    let mut types = ContractRegistry::new();
    types
        .load("types: {NumberStep: {base: Record, fields: {state: Int, outputs: 'List<Int>'}}}")
        .unwrap();
    let mut templates = Templates::new();
    let parsed = wes_language::parse(&SourceText::new(
        "synthetic-owned-read",
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
    settings.startup_work = STARTUP_WORK;
    let pool = MemoryPool::new(settings.limits.memory_bytes).unwrap();
    let scalar = || {
        Value::new(
            Shape::Primitive(Primitive::Int),
            Data::Int(0),
            Provenance::default(),
        )
        .unwrap()
    };
    Runner::new(
        Input {
            live: false,
            source: Value::new(
                Shape::Dataset(Box::new(Shape::Primitive(Primitive::Int))),
                Data::Dataset(reference.into()),
                Provenance::default(),
            )
            .unwrap(),
            initial: scalar(),
            context: scalar(),
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
    .unwrap()
}

#[test]
fn physical_reference_bounds_precede_owned_work_for_every_object_kind() {
    let tmp = home();
    let mut store =
        DatasetStore::open(tmp.path(), Durability::File, StoreLimits::default()).unwrap();
    let manifest = create(&mut store);
    store.commit(&manifest, &FlowPolicy::default()).unwrap();
    let runner = reader(store.descriptor(&manifest.dataset).unwrap());
    let (_, request) = runner.source_page_request().unwrap();
    let work = request.work.as_ref().unwrap();
    let before = runner.progress().usage.work;
    let mut oversized = manifest.schema.clone();
    oversized.bytes = u64::MAX;
    let mut undersized = manifest.schema.clone();
    undersized.bytes = 1;
    let mut invalid_id = manifest.schema.clone();
    invalid_id.id = "../outside".into();
    let mut invalid_digest = manifest.schema.clone();
    invalid_digest.digest = "invalid".into();
    for (reference, limited) in [
        (&oversized, true),
        (&undersized, false),
        (&invalid_id, false),
        (&invalid_digest, false),
    ] {
        let results = [
            store.files.read_manifest(reference, Some(work)).map(|_| ()),
            store.files.read_schema(reference, Some(work)).map(|_| ()),
            store
                .files
                .read_checkpoint(reference, &manifest.dataset, Some(work))
                .map(|_| ()),
            store
                .files
                .read_index(
                    reference,
                    &manifest.dataset,
                    super::super::super::Stream::Outputs,
                    Some(work),
                )
                .map(|_| ()),
            store
                .files
                .read_segment(
                    reference,
                    &manifest.dataset,
                    super::super::super::Stream::Outputs,
                    &schema(),
                    Some(work),
                )
                .map(|_| ()),
        ];
        for result in results {
            if limited {
                assert!(matches!(result, Err(ObjectError::Limit("object"))));
            } else {
                assert!(matches!(result, Err(ObjectError::Corrupt)));
            }
        }
        assert_eq!(runner.progress().usage.work, before);
    }
    store
        .files
        .read_schema(&manifest.schema, Some(work))
        .unwrap();
    assert_eq!(
        runner.progress().usage.work - before,
        manifest.schema.bytes * 64 + 4096
    );
    // Entering an admissible physical read costs work even if the file is absent.
    let mut missing = manifest.schema.clone();
    missing.id = uuid::Uuid::new_v4().to_string();
    assert!(matches!(
        store.files.read_schema(&missing, Some(work)),
        Err(ObjectError::Corrupt)
    ));
    assert_eq!(
        runner.progress().usage.work - before,
        2 * (manifest.schema.bytes * 64 + 4096)
    );
}

#[test]
fn checkpoint_verification_charges_nested_schema_reads_and_preserves_exact_refusal() {
    let tmp = home();
    let mut store =
        DatasetStore::open(tmp.path(), Durability::File, StoreLimits::default()).unwrap();
    let manifest = create(&mut store);
    store.commit(&manifest, &FlowPolicy::default()).unwrap();
    let cp = checkpoint(&store, &manifest);
    let object = store
        .files
        .publish_checkpoint(&cp, &FlowPolicy::default())
        .unwrap();
    let runner = reader(store.descriptor(&manifest.dataset).unwrap());
    let (_, request) = runner.source_page_request().unwrap();
    let before = runner.progress().usage.work;
    store
        .files
        .read_checkpoint(&object, &manifest.dataset, request.work.as_ref())
        .unwrap();
    let checkpoint_cost = object.bytes * 64 + 4096;
    let schema_cost = manifest.schema.bytes * 64 + 4096;
    assert_eq!(
        runner.progress().usage.work - before,
        checkpoint_cost + 4 * schema_cost
    );

    let allowance = STARTUP_WORK;
    let used = runner.progress().usage.work;
    request
        .work
        .as_ref()
        .unwrap()
        .charge(allowance - used - checkpoint_cost - schema_cost)
        .unwrap();
    let (_, request) = runner.source_page_request().unwrap();
    let before = runner.progress().usage.work;
    let error = store
        .files
        .read_checkpoint(&object, &manifest.dataset, request.work.as_ref())
        .unwrap_err();
    let ObjectError::ReadWork(refused) = error else {
        panic!("nested work refusal: {error}")
    };
    assert_eq!(refused.dimension(), ReadWorkDimension::Allowance);
    assert_eq!(refused.needed(), checkpoint_cost + 2 * schema_cost);
    assert_eq!(
        runner.progress().usage.work - before,
        checkpoint_cost + schema_cost
    );
}

#[test]
fn owned_page_manifest_refusal_survives_the_common_storage_port() {
    let tmp = home();
    let mut store =
        DatasetStore::open(tmp.path(), Durability::File, StoreLimits::default()).unwrap();
    let manifest = create(&mut store);
    store.commit(&manifest, &FlowPolicy::default()).unwrap();
    let reference = store.descriptor(&manifest.dataset).unwrap();
    let runner = reader(reference.clone());
    let (_, request) = runner.source_page_request().unwrap();
    request
        .work
        .as_ref()
        .unwrap()
        .charge(STARTUP_WORK - runner.progress().usage.work)
        .unwrap();
    let (_, request) = runner.source_page_request().unwrap();
    let error = DatasetStorage::page(&store, &reference, request).unwrap_err();
    let StoreError::ReadWork(refused) = error else {
        panic!("manifest work refusal: {error}")
    };
    assert_eq!(refused.dimension(), ReadWorkDimension::Allowance);
    assert_eq!(runner.progress().usage.input_records, 0);
}

#[test]
fn owned_page_pays_the_exact_physical_sum_without_caller_charges() {
    let tmp = home();
    let mut store =
        DatasetStore::open(tmp.path(), Durability::File, StoreLimits::default()).unwrap();
    let mut manifest = create(&mut store);
    let segment = append(&mut store, &mut manifest, 3);
    store.commit(&manifest, &FlowPolicy::default()).unwrap();
    let reference = store.descriptor(&manifest.dataset).unwrap();
    let runner = reader(reference.clone());
    let (_, request) = runner.source_page_request().unwrap();
    let before = runner.progress().usage.work;
    let page = DatasetStorage::page(&store, &reference, request).unwrap();
    assert_eq!(page.rows.len(), 3);
    let cost = |bytes| bytes * 64 + 4096;
    let expected = cost(reference.manifest_bytes())
        + cost(manifest.schema.bytes)
        + cost(manifest.index.as_ref().unwrap().bytes)
        + cost(segment.bytes);
    assert_eq!(runner.progress().usage.work - before, expected);
}

#[test]
fn ordinal_page_reads_each_branch_once_with_exact_owned_accounting() {
    let tmp = home();
    let mut limits = StoreLimits::default();
    limits.objects.index.fanout = 2;
    let mut store = DatasetStore::open(tmp.path(), Durability::File, limits).unwrap();
    let mut manifest = create(&mut store);
    for _ in 0..6 {
        append(&mut store, &mut manifest, 2);
    }
    store.commit(&manifest, &FlowPolicy::default()).unwrap();
    let reference = store.descriptor(&manifest.dataset).unwrap();
    let runner = reader(reference.clone());
    let cost = |bytes| bytes * 64 + 4096;
    let mut expected = cost(reference.manifest_bytes()) + cost(manifest.schema.bytes);
    let mut left = limits.objects.index.traversal_nodes;
    walk_index(
        &store.files,
        &manifest.dataset,
        super::super::super::Stream::Outputs,
        manifest.index.as_ref(),
        &manifest.summary,
        &manifest.source,
        limits.objects.index.traversal_nodes,
        &mut left,
        |visit| {
            expected += cost(match visit {
                IndexVisit::Node(node) => node.bytes,
                IndexVisit::Segment(entry) => entry.segment.bytes,
            });
            Ok(Visit::Descend)
        },
    )
    .unwrap();
    let (_, request) = runner.source_page_request().unwrap();
    let before = runner.progress().usage.work;
    let page = DatasetStorage::page(&store, &reference, request).unwrap();
    assert_eq!(
        page.rows.iter().map(|r| r.ordinal).collect::<Vec<_>>(),
        (0..12).collect::<Vec<_>>()
    );
    assert_eq!(runner.progress().usage.work - before, expected);
}

#[test]
fn lazy_range_stops_before_unrequested_corrupt_segments_and_rejects_root_summary() {
    let tmp = home();
    let mut store =
        DatasetStore::open(tmp.path(), Durability::File, StoreLimits::default()).unwrap();
    let mut manifest = create(&mut store);
    let first = append(&mut store, &mut manifest, 2);
    let second = append(&mut store, &mut manifest, 2);
    store.commit(&manifest, &FlowPolicy::default()).unwrap();
    let reference = store.descriptor(&manifest.dataset).unwrap();
    let runner = reader(reference.clone());
    let path = tmp
        .path()
        .join("objects")
        .join(format!("{}.segment", second.id));
    let mut bytes = std::fs::read(&path).unwrap();
    *bytes.last_mut().unwrap() ^= 1;
    std::fs::write(path, bytes).unwrap();
    let (_, mut request) = runner.source_page_request().unwrap();
    request.from = 1;
    request.rows = 1;
    let before = runner.progress().usage.work;
    let page = DatasetStorage::page(&store, &reference, request).unwrap();
    assert_eq!((page.first, page.next, page.rows[0].ordinal), (1, 2, 1));
    let cost = |bytes| bytes * 64 + 4096;
    assert_eq!(
        runner.progress().usage.work - before,
        cost(reference.manifest_bytes())
            + cost(manifest.schema.bytes)
            + cost(manifest.index.as_ref().unwrap().bytes)
            + cost(first.bytes)
    );
    let (_, mut request) = runner.source_page_request().unwrap();
    request.from = 2;
    assert!(matches!(
        DatasetStorage::page(&store, &reference, request),
        Err(StoreError::DatasetCorrupt)
    ));
    let root = manifest.index.as_ref().unwrap();
    let mut summary = manifest.summary.clone();
    summary.segment_bytes += 1;
    let mut range = store
        .files
        .range(
            root,
            &manifest.dataset,
            super::super::super::Stream::Outputs,
            &summary,
            0,
            None,
        )
        .unwrap();
    assert!(matches!(range.next_entry(), Err(ObjectError::Corrupt)));
    assert!(
        matches!(range.next_entry(), Err(ObjectError::Corrupt)),
        "a failed walk cannot skip its refused edge"
    );
}

#[test]
fn child_refusals_cannot_skip_to_pending_siblings_or_debit_more_work() {
    use crate::datasets::{Stream, tree::Branch};
    for cause in ["missing", "height", "summary", "stream"] {
        let tmp = home();
        let mut limits = StoreLimits::default();
        limits.objects.index.fanout = 2;
        let mut store = DatasetStore::open(tmp.path(), Durability::File, limits).unwrap();
        let mut manifest = create(&mut store);
        for _ in 0..3 {
            append(&mut store, &mut manifest, 2);
        }
        store.commit(&manifest, &FlowPolicy::default()).unwrap();
        let reference = store.descriptor(&manifest.dataset).unwrap();
        let runner = reader(reference);
        let mut root = store
            .files
            .read_index(
                manifest.index.as_ref().unwrap(),
                &manifest.dataset,
                Stream::Outputs,
                None,
            )
            .unwrap();
        let Entries::Branch { children } = &mut root.entries else {
            panic!("branch")
        };
        assert_eq!(root.height, 1);
        let child_ref = children[0].node.clone();
        match cause {
            "missing" => std::fs::remove_file(
                tmp.path()
                    .join("objects")
                    .join(format!("{}.index", child_ref.id)),
            )
            .unwrap(),
            "summary" => {
                children[0].summary.segment_bytes += 1;
            }
            "height" | "stream" => {
                let mut child = store
                    .files
                    .read_index(&child_ref, &manifest.dataset, Stream::Outputs, None)
                    .unwrap();
                if cause == "height" {
                    child.height = 1;
                    child.entries = Entries::Branch {
                        children: vec![Branch {
                            node: child_ref,
                            summary: child.summary.clone(),
                        }],
                    };
                } else {
                    child.stream = Stream::Coverage;
                    let Entries::Leaf { entries } = &mut child.entries else {
                        panic!("leaf")
                    };
                    for entry in entries {
                        entry.summary.coverage =
                            Some(wes_engine::storage::datasets::CoverageSpan {
                                first_ordinal: entry.summary.first,
                                last_ordinal: entry.summary.end - 1,
                                from: entry.source.start,
                                through: entry.source.end,
                                input_bytes: entry.source.end - entry.source.start,
                            });
                    }
                    child.refresh_summary().unwrap();
                }
                children[0].node = store
                    .files
                    .publish_index(&child, &FlowPolicy::default())
                    .unwrap();
            }
            _ => unreachable!(),
        }
        root.refresh_summary().unwrap();
        let root_ref = store
            .files
            .publish_index(&root, &FlowPolicy::default())
            .unwrap();
        let (_, request) = runner.source_page_request().unwrap();
        let mut range = store
            .files
            .range(
                &root_ref,
                &manifest.dataset,
                Stream::Outputs,
                &root.summary,
                0,
                request.work.as_ref(),
            )
            .unwrap();
        assert!(
            matches!(range.next_entry(), Err(ObjectError::Corrupt)),
            "{cause}"
        );
        let used = runner.progress().usage.work;
        assert!(
            matches!(range.next_entry(), Err(ObjectError::Corrupt)),
            "{cause}"
        );
        assert_eq!(
            runner.progress().usage.work,
            used,
            "poisoned {cause} must not enter another physical read"
        );
    }
}

#[test]
fn branch_pages_preserve_middle_row_byte_segment_and_end_boundaries() {
    use crate::datasets::Stream;
    let tmp = home();
    let mut limits = StoreLimits::default();
    limits.objects.index.fanout = 2;
    let mut store = DatasetStore::open(tmp.path(), Durability::File, limits).unwrap();
    let mut manifest = create(&mut store);
    let segments = (0..3)
        .map(|_| append(&mut store, &mut manifest, 2))
        .collect::<Vec<_>>();
    store.commit(&manifest, &FlowPolicy::default()).unwrap();
    let reference = store.descriptor(&manifest.dataset).unwrap();
    let root = store
        .files
        .read_index(
            manifest.index.as_ref().unwrap(),
            &manifest.dataset,
            Stream::Outputs,
            None,
        )
        .unwrap();
    let Entries::Branch { children } = &root.entries else {
        panic!("branch")
    };
    let runner = reader(reference.clone());
    let cost = |bytes| bytes * 64 + 4096;
    let common = cost(reference.manifest_bytes()) + cost(manifest.schema.bytes);
    for (from, rows, count, end, physical) in [
        (
            0,
            32,
            2,
            4,
            cost(manifest.index.as_ref().unwrap().bytes)
                + cost(children[0].node.bytes)
                + cost(segments[0].bytes)
                + cost(segments[1].bytes),
        ),
        (
            5,
            1,
            8,
            6,
            cost(manifest.index.as_ref().unwrap().bytes)
                + cost(children[1].node.bytes)
                + cost(segments[2].bytes),
        ),
        (6, 32, 8, 6, 0),
    ] {
        let (_, mut request) = runner.source_page_request().unwrap();
        request.from = from;
        request.rows = rows;
        request.segments = count;
        let before = runner.progress().usage.work;
        let page = DatasetStorage::page(&store, &reference, request).unwrap();
        assert_eq!((page.first, page.next), (from, end));
        assert_eq!(page.extent_exhausted, end == 6);
        assert_eq!(runner.progress().usage.work - before, common + physical);
    }
    let first = store.page(&reference, 0, 1, PageLimits::default()).unwrap();
    let bytes = first.encoded_row_bytes;
    let page = store
        .page(
            &reference,
            0,
            32,
            PageLimits {
                bytes: bytes + 1,
                ..PageLimits::default()
            },
        )
        .unwrap();
    assert_eq!((page.next, page.limited_by), (1, Some("bytes")));
}

#[test]
fn logical_page_charge_truncates_owned_rows_and_resumes_without_a_byte_limit() {
    use wes_engine::storage::datasets::PageCharge;
    let tmp = home();
    let mut store =
        DatasetStore::open(tmp.path(), Durability::File, StoreLimits::default()).unwrap();
    let mut manifest = create(&mut store);
    append(&mut store, &mut manifest, 5);
    store.commit(&manifest, &FlowPolicy::default()).unwrap();
    let reference = store.descriptor(&manifest.dataset).unwrap();
    let all = store.page(&reference, 0, 5, PageLimits::default()).unwrap();
    let row_charge = PageCharge::new(u64::MAX, u64::MAX)
        .unwrap()
        .admit(&all.rows[0].value, 0)
        .unwrap();
    for row in &all.rows {
        assert_eq!(
            PageCharge::new(u64::MAX, u64::MAX)
                .unwrap()
                .admit(&row.value, 0),
            Ok(row_charge),
            "this fixture's expected page ends require equal row charges"
        );
    }
    let runner = reader(reference.clone());
    let mut from = 0;
    let mut ordinals = vec![];
    for expected_end in [2, 4, 5] {
        let (_, mut request) = runner.source_page_request().unwrap();
        request.from = from;
        request.charge = PageCharge::new(row_charge, 2 * row_charge);
        let page = DatasetStorage::page(&store, &reference, request).unwrap();
        assert_eq!(page.next, expected_end);
        assert_eq!(
            page.limited_by,
            if expected_end == 5 {
                None
            } else {
                Some("logical charge")
            }
        );
        assert!(
            page.encoded_row_bytes < 65536,
            "encoded and logical caps are independent"
        );
        ordinals.extend(page.rows.iter().map(|row| row.ordinal));
        from = page.next;
    }
    assert_eq!(ordinals, vec![0, 1, 2, 3, 4]);
    assert!(PageCharge::new(0, 100).is_none());
    assert!(PageCharge::new(101, 100).is_none());
}

#[test]
fn a_single_oversized_row_has_a_typed_record_memory_stop_without_renewal() {
    use wes_engine::{
        scan::{Phase, ledger::Dimension},
        storage::datasets::PageCharge,
    };
    let tmp = home();
    let mut store =
        DatasetStore::open(tmp.path(), Durability::File, StoreLimits::default()).unwrap();
    let mut manifest = create(&mut store);
    append(&mut store, &mut manifest, 1);
    store.commit(&manifest, &FlowPolicy::default()).unwrap();
    let reference = store.descriptor(&manifest.dataset).unwrap();
    let row = store.page(&reference, 0, 1, PageLimits::default()).unwrap();
    let charge = PageCharge::new(u64::MAX, u64::MAX)
        .unwrap()
        .admit(&row.rows[0].value, 0)
        .unwrap();
    let mut runner = reader(reference.clone());
    let (_, mut request) = runner.source_page_request().unwrap();
    request.charge = PageCharge::new(charge - 1, charge * 2);
    let error = DatasetStorage::page(&store, &reference, request).unwrap_err();
    assert!(matches!(error,StoreError::DatasetRowCharge {limit} if limit==charge-1));
    runner.refuse_source_read(error);
    assert_eq!(runner.progress().phase, Phase::Stopped);
    assert_eq!(runner.progress().usage.input_records, 0);
    let completion = runner.into_completion().unwrap();
    assert_eq!(
        completion.stop.unwrap().dimension,
        Some(Dimension::RecordMemory)
    );
}
