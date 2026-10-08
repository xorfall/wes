//! Synthetic finite analysis acceptance. No providers, files or mutable workspace
//! registry are reachable from the record owner.
use wes_core::{
    Data, Provenance, Shape, Value,
    contracts::ContractRegistry,
    framing::{Decoding, Delimiter, Profile},
};
use wes_engine::{
    driver::CancellationToken,
    scan::{
        Identity, Input, Phase, Poll, Runner, Settings, Transition,
        ledger::{Dimension, MemoryPool},
    },
};
use wes_language::{Expression, SourceText, Span, parse, templates::Templates};
fn span() -> Span {
    Span::new(0, 0).unwrap()
}
fn transition(source: &str, finish: bool, types: &ContractRegistry) -> Transition {
    let mut types = types.clone();
    types
        .load("types: {TextStep: {base: Record, fields: {state: Int, outputs: 'List<Text>'}}}")
        .unwrap();
    let parsed = parse(&SourceText::new("synthetic", source));
    assert!(parsed.diagnostics.is_empty(), "{:?}", parsed.diagnostics);
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
    let mut templates = Templates::new();
    templates
        .define_calculation(
            syntax,
            &types,
            &Default::default(),
            wes_language::calc::Package::standard(),
        )
        .unwrap();
    Transition::capture(
        templates.snapshot().values().next().unwrap(),
        finish,
        4 * 1024 * 1024,
        span(),
    )
    .unwrap()
}
fn value(data: Data) -> Value {
    Value::new(Shape::Unknown, data, Provenance::default()).unwrap()
}
fn input(source: Value, body: &str, framing: Option<Profile>) -> Input {
    let types = ContractRegistry::new();
    let step = transition(
        &format!(
            ":def step(state:Int, context:Int, item:Unknown) -> TextStep as :calc pure {{ {body} }}"
        ),
        false,
        &types,
    );
    Input {
        live: false,
        source,
        initial: value(Data::Int(0)),
        context: value(Data::Int(0)),
        step,
        finish: None,
        framing,
        identity: Identity {
            analysis: "synthetic-analysis".into(),
            source: None,
            profile: "synthetic-profile".into(),
            profile_revision: "synthetic-profile-revision".into(),
        },
        control: None,
    }
}
fn lines() -> Profile {
    Profile {
        delimiter: Delimiter::Lines,
        decoding: Decoding::StrictUtf8,
        malformed: wes_core::framing::Malformed::Strict {},
        raw_bytes: 65536,
        decoded_bytes: 262144,
        spans: 2048,
    }
}
fn settings() -> Settings {
    Settings::capture()
}
fn run(input: Input, settings: Settings) -> wes_engine::scan::Completion {
    let pool = MemoryPool::new(settings.limits.memory_bytes).unwrap();
    let mut runner = Runner::new(
        input,
        settings,
        &pool,
        None,
        CancellationToken::new(),
        span(),
    )
    .unwrap();
    for _ in 0..100_000 {
        if matches!(runner.poll(), Poll::Terminal) {
            let completion = runner.into_completion().unwrap();
            assert_eq!(pool.reserved(), Some(settings.limits.memory_bytes));
            return completion;
        }
    }
    panic!("bounded scan never terminated")
}
fn field<'a>(data: &'a Data, name: &str) -> &'a Data {
    let Data::Record(fields) = data else {
        panic!("record")
    };
    &fields[name]
}
#[test]
fn output_totals_do_not_rewrite_the_frozen_invocation_caps() {
    for (dimension, records, bytes, body) in [
        (
            Dimension::OutputRecords,
            1,
            65536,
            "return {state:state+1,outputs:['a','b']};",
        ),
        (
            Dimension::OutputBytes,
            100,
            1,
            "return {state:state+1,outputs:['a']};",
        ),
    ] {
        let mut bounds = settings();
        bounds.limits.output_records = records;
        bounds.limits.output_bytes = bytes;
        assert!(bounds.valid(), "per-record caps remain independent");
        let done = run(
            input(value(Data::List(vec![Data::Int(1)])), body, None),
            bounds,
        );
        assert_eq!(done.progress.committed_position, 0);
        assert_eq!(done.progress.usage.output_records, 0);
        assert_eq!(done.stop.unwrap().dimension, Some(dimension));
    }
}
#[test]
fn receipt_separates_measured_work_and_absent_durable_attempts() {
    let done = run(
        input(
            value(Data::List(vec![Data::Int(1)])),
            "return {state:state+item,outputs:[]};",
            None,
        ),
        settings(),
    );
    let receipt = field(done.value.data(), "receipt");
    assert_eq!(field(receipt, "measuredWork"), field(receipt, "work"));
    for name in [
        "attempt",
        "previousAttempt",
        "outstandingWork",
        "durationOutstandingMs",
    ] {
        assert_eq!(
            field(receipt, name),
            &Data::Option(None),
            "no invented durable {name}"
        );
    }
    assert!(matches!(field(receipt, "durationChargedMs"), Data::Int(n) if *n >= 0));
}
#[test]
fn duration_and_earned_allowance_have_distinct_native_stop_dimensions() {
    let mut bounds = settings();
    bounds.duration = std::time::Duration::from_millis(1);
    let pool = MemoryPool::new(bounds.limits.memory_bytes).unwrap();
    let mut runner = Runner::new(
        input(
            value(Data::List(vec![Data::Int(1)])),
            "return {state:state+item,outputs:[]};",
            None,
        ),
        bounds,
        &pool,
        None,
        CancellationToken::new(),
        span(),
    )
    .unwrap();
    // Readiness precedes this lower-bound wait; there is no upper timing assertion.
    std::thread::sleep(std::time::Duration::from_millis(2));
    assert!(matches!(runner.poll(), Poll::Terminal));
    let done = runner.into_completion().unwrap();
    assert_eq!(done.stop.unwrap().dimension, Some(Dimension::Duration));
    assert_eq!(
        field(field(done.value.data(), "receipt"), "exhausted"),
        &Data::Option(Some(Box::new(Data::Text("duration".into()))))
    );

    let mut bounds = settings();
    bounds.startup_work = 150_000;
    bounds.work_per_input_unit = 1;
    let done = run(
        input(
            value(Data::List((0..100).map(Data::Int).collect())),
            "return {state:state+1,outputs:[]};",
            None,
        ),
        bounds,
    );
    assert!(done.progress.usage.input_records < 100);
    assert!(done.progress.usage.work < bounds.limits.work);
    assert_eq!(done.stop.unwrap().dimension, Some(Dimension::WorkAllowance));
}
#[test]
fn portable_transition_reopens_without_a_live_registry_or_provider_capability() {
    let mut original = input(
        value(Data::List(vec![Data::Int(2), Data::Int(5)])),
        "return {state:state+item,outputs:[text(item)]};",
        None,
    );
    let captured = original
        .step
        .captured_program(4 * 1024 * 1024, span())
        .unwrap();
    let restored = Transition::restore_program(&captured, 4 * 1024 * 1024, span()).unwrap();
    assert_eq!(restored.revision(), original.step.revision());
    assert_eq!(restored.output_shape(), original.step.output_shape());
    original.step = restored;
    let completion = run(original, settings());
    assert_eq!(field(completion.value.data(), "state"), &Data::Int(7));
    assert_eq!(
        field(completion.value.data(), "outputs"),
        &Data::List(vec![Data::Text("2".into()), Data::Text("5".into())])
    );
    let mut unsupported: serde_json::Value = serde_json::from_str(&captured).unwrap();
    unsupported["native"] = serde_json::json!("unsupported-semantics");
    assert!(
        Transition::restore_program(&unsupported.to_string(), 4 * 1024 * 1024, span()).is_err()
    );
    unsupported = serde_json::from_str(&captured).unwrap();
    unsupported["body"] = serde_json::json!("return $ambient;");
    assert!(
        Transition::restore_program(&unsupported.to_string(), 4 * 1024 * 1024, span()).is_err()
    );
}
#[test]
fn framing_block_size_does_not_change_receipt_work_positions_or_outputs() {
    let source = "a😀\r\n\nlast";
    let mut expected = None;
    for block in [1, 2, 3, 7, 4096] {
        let mut settings = settings();
        settings.block = block;
        let result = run(
            input(
                value(Data::Text(source.into())),
                "return {state:state+1,outputs:[item.text]};",
                Some(lines()),
            ),
            settings,
        );
        assert_eq!(result.progress.phase, Phase::Complete);
        assert_eq!(result.progress.usage.input_records, 3);
        assert_eq!(result.progress.committed_position, source.len() as u64);
        assert!(result.stop.is_none());
        let signature = (
            result.progress.usage.work,
            field(result.value.data(), "outputs").clone(),
        );
        if let Some(expected) = &expected {
            assert_eq!(&signature, expected);
        } else {
            expected = Some(signature);
        }
    }
}
#[test]
fn refused_output_candidate_commits_neither_state_position_nor_input_credit() {
    let mut settings = settings();
    settings.limits.output_records = 1;
    settings.outputs_per_record = 1;
    let result = run(
        input(
            value(Data::List(vec![Data::Int(1), Data::Int(2)])),
            "return {state:state+1,outputs:[text(item)]};",
            None,
        ),
        settings,
    );
    assert_eq!(result.progress.phase, Phase::Stopped);
    assert_eq!(
        result.stop.as_ref().unwrap().dimension,
        Some(Dimension::OutputRecords)
    );
    assert_eq!(result.progress.committed_position, 1);
    assert_eq!(result.progress.read_position, 2);
    assert_eq!(result.progress.usage.input_records, 1);
    assert_eq!(result.progress.usage.output_records, 1);
    assert_eq!(field(result.value.data(), "state"), &Data::Int(1));
    assert_eq!(
        field(result.value.data(), "outputs"),
        &Data::List(vec![Data::Text("1".into())])
    );
    let receipt = field(result.value.data(), "receipt");
    assert_eq!(field(receipt, "status"), &Data::Text("stopped".into()));
    assert_eq!(field(receipt, "sourceComplete"), &Data::Option(None));
    assert_eq!(field(receipt, "durableResume"), &Data::Bool(false));
}
#[test]
fn finish_applies_once_for_clean_empty_extent_and_never_after_framing_failure() {
    let finish = transition(
        ":def finish(state:Int, context:Int, end:Unknown) -> TextStep as :calc pure { return {state:state+100,outputs:['finish']}; }",
        true,
        &ContractRegistry::new(),
    );
    for (source, clean) in [(vec![], true), (b"ok\n\xff\n".to_vec(), false)] {
        let mut input = input(
            value(Data::Bytes(source.into())),
            "return {state:state+1,outputs:[item.text]};",
            Some(lines()),
        );
        input.finish = Some(finish.clone());
        let result = run(input, settings());
        assert_eq!(result.progress.finish_applied, clean);
        if clean {
            assert_eq!(field(result.value.data(), "state"), &Data::Int(100));
            assert_eq!(result.progress.usage.input_records, 0);
        } else {
            assert_eq!(field(result.value.data(), "state"), &Data::Int(1));
            assert_eq!(
                result.stop.unwrap().source_span,
                Some(wes_core::framing::ByteSpan { start: 3, end: 4 })
            );
        }
    }
}
#[test]
fn cancellation_and_fresh_vm_work_limits_preserve_only_committed_records_and_release_owner() {
    let settings = settings();
    let pool = MemoryPool::new(settings.limits.memory_bytes).unwrap();
    let token = CancellationToken::new();
    let mut runner = Runner::new(
        input(
            value(Data::List(vec![Data::Int(1), Data::Int(2)])),
            "let f=()=>item; return {state:state+1,outputs:[text(f())]};",
            None,
        ),
        settings,
        &pool,
        None,
        token.clone(),
        span(),
    )
    .unwrap();
    while runner.progress().usage.input_records == 0 {
        assert!(matches!(runner.poll(), Poll::Yield));
    }
    token.cancel();
    assert!(matches!(runner.poll(), Poll::Terminal));
    let done = runner.into_completion().unwrap();
    assert_eq!(done.progress.phase, Phase::Cancelled);
    assert_eq!(done.progress.usage.input_records, 1);
    assert_eq!(pool.reserved(), Some(settings.limits.memory_bytes));
    drop(done);
    assert_eq!(pool.reserved(), Some(0));
    let mut settings = settings;
    settings.scratch.work = 100;
    let done = run(
        input(
            value(Data::List(vec![Data::Int(1)])),
            "while(true) {} return {state:state,outputs:[]};",
            None,
        ),
        settings,
    );
    assert_eq!(done.stop.unwrap().dimension, Some(Dimension::RecordWork));
    assert_eq!(done.progress.usage.input_records, 0);
}
#[test]
fn cumulative_work_refusal_and_private_zero_output_remain_attempt_wide() {
    let mut settings = settings();
    settings.startup_work = 150_000;
    settings.limits.work = 150_000;
    settings.work_per_input_unit = 1;
    let mut input = input(
        value(Data::List((0..100).map(Data::Int).collect())),
        "return {state:state+1,outputs:[]};",
        None,
    );
    input.source = input.source.with_provenance(
        Provenance::default().with_policy(&wes_core::flow::FlowPolicy::default().private()),
    );
    let pool = MemoryPool::new(settings.limits.memory_bytes).unwrap();
    let mut runner = Runner::new(
        input,
        settings,
        &pool,
        None,
        CancellationToken::new(),
        span(),
    )
    .unwrap();
    assert!(runner.execution_progress().counters.is_none());
    while !matches!(runner.poll(), Poll::Terminal) {}
    let result = runner.into_completion().unwrap();
    assert_eq!(result.stop.unwrap().dimension, Some(Dimension::Work));
    assert!(result.value.provenance().policy().is_private());
    assert_eq!(result.progress.usage.output_records, 0);
    assert!(result.progress.usage.input_records < 100);
}
#[test]
fn repeated_contract_fields_share_code_charge_but_distinct_regex_owners_are_bounded() {
    let mut types = ContractRegistry::new();
    types.load("types: {Digest: {base: Text, pattern: '^[a-f0-9]{64}$'}, Pair: {base: Record, fields: {left: Digest, right: Digest}}, PairStep: {base: Record, fields: {state: Pair, outputs: 'List<Pair>'}}}").unwrap();
    let step = transition(
        ":def pair(state:Pair, context:Pair, item:Pair) -> PairStep as :calc pure { return {state:item,outputs:[item]}; }",
        false,
        &types,
    );
    assert!(step.code_charge() < 2 * wes_core::IterRegexCache::COMPILED_CHARGE);
    let saved = step.captured_program(4 * 1024 * 1024, span()).unwrap();
    let restored = Transition::restore_program(&saved, 4 * 1024 * 1024, span()).unwrap();
    assert!(restored.code_charge() <= 4 * 1024 * 1024);
    assert_eq!(restored.revision(), step.revision());
}

#[test]
fn captured_output_declaration_survives_empty_outputs_with_enum_tones() {
    let mut types = ContractRegistry::new();
    types.load("version: 2\ntypes:\n  Status: {base: Text, enum: [ready, failed], display: {enumTones: {ready: ok, failed: bad}}}\n  Item: {base: Record, fields: {status: Status}}\n").unwrap();
    types
        .load("types: {ItemStep: {base: Record, fields: {state: Int, outputs: 'List<Item>'}}}")
        .unwrap();
    let step = transition(
        ":def typed(state:Int, context:Int, item:Int) -> ItemStep as :calc pure { return {state:state+item,outputs:[]}; }",
        false,
        &types,
    );
    let mut input = input(
        value(Data::List(vec![Data::Int(1)])),
        "return {state:state,outputs:[]};",
        None,
    );
    input.step = step;
    let result = run(input, settings());
    assert_eq!(field(result.value.data(), "outputs"), &Data::List(vec![]));
    let metadata = serde_json::to_value(result.value.metadata().unwrap()).unwrap();
    assert_eq!(
        metadata["fields"]["/f:outputs/e/f:status"]["members"],
        serde_json::json!(["ready", "failed"])
    );
    assert_eq!(
        result.value.shape().to_string().starts_with("ScanResult"),
        true
    );
}
#[test]
fn executable_contracts_hidden_under_option_are_refused_before_input() {
    let parsed = parse(&SourceText::new(
        "synthetic",
        ":def bad(state:Option<Iter<Int>>, context:Int, item:Int) -> HiddenStep as :calc pure { return {state:state,outputs:[]}; }",
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
        panic!()
    };
    let mut templates = Templates::new();
    let mut types = ContractRegistry::new();
    types.load("types: {HiddenStep: {base: Record, fields: {state: 'Option<Iter<Int>>', outputs: 'List<Text>'}}}").unwrap();
    templates
        .define_calculation(
            syntax,
            &types,
            &Default::default(),
            wes_language::calc::Package::standard(),
        )
        .unwrap();
    let failure = Transition::capture(
        templates.snapshot().values().next().unwrap(),
        false,
        4 * 1024 * 1024,
        span(),
    )
    .unwrap_err();
    assert_eq!(failure.code, "CAL009");
}

#[test]
fn a_million_empty_records_stop_at_declared_input_cap_with_bounded_held_charge() {
    let mut settings = settings();
    settings.limits.input_records = 5;
    let result = run(
        input(
            value(Data::Text("\n".repeat(1_000_000).into())),
            "return {state:state+1,outputs:[]};",
            Some(lines()),
        ),
        settings,
    );
    assert_eq!(
        result.stop.unwrap().dimension,
        Some(Dimension::InputRecords)
    );
    assert_eq!(result.progress.usage.input_records, 5);
    assert_eq!(result.progress.committed_position, 5);
    assert!(result.progress.usage.high_water_bytes <= settings.limits.memory_bytes);
    assert_eq!(result.progress.usage.output_records, 0);
}

#[test]
fn aggregate_reservation_follows_completion_through_the_execution_report_handoff() {
    let settings = settings();
    let pool = MemoryPool::new(settings.limits.memory_bytes).unwrap();
    let mut runner = Runner::new(
        input(
            value(Data::List(vec![])),
            "return {state:state,outputs:[]};",
            None,
        ),
        settings,
        &pool,
        None,
        CancellationToken::new(),
        span(),
    )
    .unwrap();
    while !matches!(runner.poll(), Poll::Terminal) {}
    let (value, _, _, hold) = runner.into_completion().unwrap().into_parts();
    let mut report: wes_engine::driver::ExecutionReport =
        wes_engine::runtime::Outcome::Produced(value).into();
    report.holds.push(hold);
    assert_eq!(pool.reserved(), Some(settings.limits.memory_bytes));
    drop(report);
    assert_eq!(pool.reserved(), Some(0));
}

#[test]
fn framed_conversion_refusal_reports_its_dimension_and_original_uncommitted_span() {
    let mut settings = settings();
    settings.record_charge = 1024;
    let result = run(
        input(
            value(Data::Text("x\n".into())),
            "return {state:state+1,outputs:[item.text]};",
            Some(lines()),
        ),
        settings,
    );
    let stop = result.stop.as_ref().unwrap();
    assert_eq!(stop.dimension, Some(Dimension::RecordMemory));
    assert_eq!(
        stop.source_span,
        Some(wes_core::framing::ByteSpan { start: 0, end: 2 })
    );
    assert_eq!(result.progress.committed_position, 0);
    assert_eq!(result.progress.read_position, 2);
    assert_eq!(result.progress.usage.input_records, 0);
    assert!(!result.progress.finish_applied);
}

#[test]
fn bounded_dataset_batches_publish_state_and_original_ranges_only_after_acknowledgement() {
    let mut options = settings();
    options.commit_records = 3;
    let original = input(
        value(Data::List((1..=10).map(Data::Int).collect())),
        "return {state:state+item,outputs:[text(item)]};",
        None,
    );
    let pool = MemoryPool::new(options.limits.memory_bytes).unwrap();
    let mut runner = Runner::new(
        original,
        options,
        &pool,
        None,
        CancellationToken::new(),
        span(),
    )
    .unwrap();
    let schema = runner.dataset_admission().unwrap().schema;
    let mut reference = wes_core::DatasetRef::new(
        uuid::Uuid::new_v4().to_string(),
        uuid::Uuid::new_v4().to_string(),
        1,
        uuid::Uuid::new_v4().to_string(),
        format!("sha256:{}", "1".repeat(64)),
        100,
        schema.digest().into(),
        0,
        1,
    )
    .unwrap();
    runner.attach_dataset(reference.clone()).unwrap();
    let mut batches = Vec::new();
    for _ in 0..100_000 {
        match runner.poll() {
            Poll::Yield => {}
            Poll::Commit => {
                let request = runner.dataset_candidate().unwrap();
                assert_eq!(runner.progress().usage.input_records, reference.records());
                assert_eq!(runner.progress().committed_position, reference.records());
                assert_eq!(runner.progress().usage.output_records, reference.records());
                assert_eq!(
                    request
                        .rows
                        .iter()
                        .map(|row| (row.source_start, row.source_end))
                        .collect::<Vec<_>>(),
                    (reference.records()..reference.records() + request.rows.len() as u64)
                        .map(|n| (n, n + 1))
                        .collect::<Vec<_>>()
                );
                batches.push(request.rows.len());
                reference = wes_core::DatasetRef::new(
                    reference.store().into(),
                    reference.dataset().into(),
                    reference.generation() + 1,
                    uuid::Uuid::new_v4().to_string(),
                    format!("sha256:{}", "2".repeat(64)),
                    100,
                    schema.digest().into(),
                    reference.records() + request.rows.len() as u64,
                    1,
                )
                .unwrap();
                runner.acknowledge_dataset(reference.clone()).unwrap();
            }
            Poll::Terminal => break,
            _ => panic!("inline non-durable sink needs no I/O grant or page"),
        }
    }
    assert_eq!(batches, vec![3, 3, 3, 1]);
    let done = runner.into_completion().unwrap();
    assert!(done.stop.is_none());
    assert_eq!(field(done.value.data(), "state"), &Data::Int(55));
    assert_eq!(done.progress.committed_position, 10);
}

#[test]
fn finishing_outputs_follow_the_unpublished_input_batch_at_its_exact_boundary() {
    let mut options = settings();
    options.commit_records = 3;
    let mut original = input(
        value(Data::List(vec![Data::Int(1), Data::Int(2)])),
        "return {state:state+item,outputs:[text(item)]};",
        None,
    );
    original.finish = Some(transition(
        ":def finish(state:Int, context:Int, end:Unknown) -> TextStep as :calc pure { return {state:state+100,outputs:['finish']}; }",
        true,
        &ContractRegistry::new(),
    ));
    let pool = MemoryPool::new(options.limits.memory_bytes).unwrap();
    let mut runner = Runner::new(
        original,
        options,
        &pool,
        None,
        CancellationToken::new(),
        span(),
    )
    .unwrap();
    let schema = runner.dataset_admission().unwrap().schema;
    let reference = wes_core::DatasetRef::new(
        uuid::Uuid::new_v4().to_string(),
        uuid::Uuid::new_v4().to_string(),
        1,
        uuid::Uuid::new_v4().to_string(),
        format!("sha256:{}", "1".repeat(64)),
        100,
        schema.digest().into(),
        0,
        1,
    )
    .unwrap();
    runner.attach_dataset(reference.clone()).unwrap();
    let mut committed = false;
    for _ in 0..100_000 {
        match runner.poll() {
            Poll::Yield => {}
            Poll::Commit => {
                assert!(!committed, "the two inputs and finish must share one batch");
                let request = runner.dataset_candidate().unwrap();
                assert_eq!(runner.progress().committed_position, 0);
                assert_eq!(
                    request
                        .rows
                        .iter()
                        .map(|row| (row.source_start, row.source_end))
                        .collect::<Vec<_>>(),
                    [(0, 1), (1, 2), (2, 2)],
                    "finish output belongs to EOF, not the previous acknowledged boundary"
                );
                assert_eq!(
                    request
                        .rows
                        .iter()
                        .map(|row| row.value.data().clone())
                        .collect::<Vec<_>>(),
                    [
                        Data::Text("1".into()),
                        Data::Text("2".into()),
                        Data::Text("finish".into())
                    ]
                );
                assert_eq!(
                    request.lifecycle,
                    wes_engine::storage::datasets::DatasetLifecycle::Sealed
                );
                let acknowledged = wes_core::DatasetRef::new(
                    reference.store().into(),
                    reference.dataset().into(),
                    2,
                    uuid::Uuid::new_v4().to_string(),
                    format!("sha256:{}", "2".repeat(64)),
                    100,
                    schema.digest().into(),
                    3,
                    1,
                )
                .unwrap();
                runner.acknowledge_dataset(acknowledged).unwrap();
                committed = true;
            }
            Poll::Terminal => break,
            _ => panic!("inline sink needs no source I/O"),
        }
    }
    assert!(committed);
    let done = runner.into_completion().unwrap();
    assert!(done.stop.is_none());
    assert_eq!(done.progress.committed_position, 2);
    assert_eq!(done.progress.usage.input_records, 2);
    assert!(done.progress.finish_applied);
    assert_eq!(field(done.value.data(), "state"), &Data::Int(103));
}

#[test]
fn cancelling_an_unpublished_batch_discards_speculation_without_refunding_work() {
    let mut options = settings();
    options.commit_records = 3;
    let original = input(
        value(Data::List((1..=10).map(Data::Int).collect())),
        "return {state:state+item,outputs:[text(item)]};",
        None,
    );
    let pool = MemoryPool::new(options.limits.memory_bytes).unwrap();
    let token = CancellationToken::new();
    let mut runner = Runner::new(original, options, &pool, None, token.clone(), span()).unwrap();
    let schema = runner.dataset_admission().unwrap().schema;
    runner
        .attach_dataset(
            wes_core::DatasetRef::new(
                uuid::Uuid::new_v4().to_string(),
                uuid::Uuid::new_v4().to_string(),
                1,
                uuid::Uuid::new_v4().to_string(),
                format!("sha256:{}", "1".repeat(64)),
                100,
                schema.digest().into(),
                0,
                1,
            )
            .unwrap(),
        )
        .unwrap();
    for _ in 0..100_000 {
        assert!(matches!(runner.poll(), Poll::Yield));
        if runner.progress().read_position == 2 && runner.progress().phase == Phase::Reading {
            break;
        }
    }
    assert_eq!(runner.progress().read_position, 2);
    let measured = runner.progress().usage.work;
    assert!(measured > 0);
    token.cancel();
    assert!(matches!(runner.poll(), Poll::Terminal));
    let done = runner.into_completion().unwrap();
    assert_eq!(done.progress.committed_position, 0);
    assert_eq!(done.progress.usage.input_records, 0);
    assert_eq!(done.progress.usage.output_records, 0);
    assert_eq!(done.progress.usage.work, measured);
    assert_eq!(field(done.value.data(), "state"), &Data::Int(0));
}

#[test]
fn explicit_durable_resume_keeps_the_original_cursor_state_and_budget() {
    use wes_engine::storage::datasets::CapturedValue;
    let source = value(Data::List(vec![Data::Int(2), Data::Int(5)]));
    let mut original = input(
        source.clone(),
        "return {state:state+item,outputs:[text(item)]};",
        None,
    );
    original.identity.analysis = uuid::Uuid::new_v4().to_string();
    original.identity.profile_revision = format!("sha256:{}", "0".repeat(64));
    let pool = MemoryPool::new(settings().limits.memory_bytes).unwrap();
    let runner = Runner::new(
        original,
        settings(),
        &pool,
        None,
        CancellationToken::new(),
        span(),
    )
    .unwrap();
    let mut checkpoint = runner
        .checkpoint_seed(CapturedValue {
            handle: uuid::Uuid::new_v4().to_string(),
            digest: format!("sha256:{}", "1".repeat(64)),
            bytes: 100,
        })
        .unwrap();
    let output = runner.dataset_admission().unwrap().schema;
    checkpoint.state = Value::new(
        Shape::Primitive(wes_core::Primitive::Int),
        Data::Int(2),
        Provenance::default(),
    )
    .unwrap();
    checkpoint.next_position = 1;
    checkpoint.next_ordinal = 1;
    checkpoint.usage.input_bytes = 8;
    checkpoint.usage.output_bytes = 64;
    checkpoint.initial_digest = format!("sha256:{}", "2".repeat(64));
    let interrupted = checkpoint.clone();
    checkpoint.stop = Some(wes_engine::storage::datasets::AnalysisStop::Cancelled);
    checkpoint.duration.outstanding_ms = 0;
    let used = checkpoint.work.charged;
    let allowance = checkpoint.usage.work_allowance;
    let reference = wes_core::DatasetRef::new(
        uuid::Uuid::new_v4().to_string(),
        uuid::Uuid::new_v4().to_string(),
        3,
        uuid::Uuid::new_v4().to_string(),
        format!("sha256:{}", "3".repeat(64)),
        100,
        output.digest().into(),
        1,
        1,
    )
    .unwrap();
    drop(runner);
    assert!(
        Runner::prepare_resume(
            source.clone(),
            false,
            reference.clone(),
            interrupted,
            uuid::Uuid::new_v4().to_string(),
            &pool,
            None,
            CancellationToken::new(),
            span()
        )
        .is_err(),
        "an unacknowledged active interval consumes the reserved duration"
    );
    let mut exhausted = checkpoint.clone();
    exhausted.work.outstanding = allowance - used;
    exhausted.work.granted = allowance;
    assert!(
        Runner::prepare_resume(
            source.clone(),
            false,
            reference.clone(),
            exhausted,
            uuid::Uuid::new_v4().to_string(),
            &pool,
            None,
            CancellationToken::new(),
            span()
        )
        .is_err(),
        "crashed outstanding work is charged, never refunded"
    );
    let prepared = Runner::prepare_resume(
        source,
        false,
        reference.clone(),
        checkpoint,
        uuid::Uuid::new_v4().to_string(),
        &pool,
        None,
        CancellationToken::new(),
        span(),
    )
    .unwrap();
    let admitted = prepared.request();
    assert_eq!(admitted.checkpoint.usage.work_allowance, allowance);
    assert_eq!(admitted.checkpoint.work.charged, used);
    let successor = |prior: &wes_core::DatasetRef, count: u64| {
        wes_core::DatasetRef::new(
            prior.store().into(),
            prior.dataset().into(),
            prior.generation() + 1,
            uuid::Uuid::new_v4().to_string(),
            format!("sha256:{}", "4".repeat(64)),
            100,
            prior.schema_digest().into(),
            count,
            1,
        )
        .unwrap()
    };
    let mut runner = prepared.acknowledge(successor(&reference, 1)).unwrap();
    assert_eq!(runner.progress().committed_position, 1);
    for _ in 0..100000 {
        match runner.poll() {
            Poll::Yield => {}
            Poll::ReadPage | Poll::ReadHead => {
                panic!("inline captured source cannot request dataset I/O")
            }
            Poll::Commit => {
                let request = runner.dataset_candidate().unwrap();
                assert!(
                    request.rows.iter().all(|row| row.ordinal >= 1),
                    "accepted prefix cannot be replayed"
                );
                let count = request.previous.records() + request.rows.len() as u64;
                runner
                    .acknowledge_dataset(successor(&request.previous, count))
                    .unwrap();
            }
            Poll::Settle => {
                let request = runner.durable_settlement().unwrap();
                runner
                    .acknowledge_settlement(successor(
                        &request.previous,
                        request.previous.records(),
                    ))
                    .unwrap();
            }
            Poll::Grant => {
                let request = runner.durable_grant().unwrap();
                let reference = successor(&request.previous, request.previous.records());
                runner
                    .acknowledge_grant(reference, request.checkpoint.unwrap())
                    .unwrap();
            }
            Poll::Terminal => break,
        }
    }
    assert_eq!(runner.progress().phase, Phase::Complete);
    assert_eq!(runner.progress().usage.input_records, 2);
    assert!(runner.progress().usage.work > used);
    let completion = runner.into_completion().unwrap();
    assert_eq!(field(completion.value.data(), "state"), &Data::Int(7));
    let Data::Dataset(outputs) = field(completion.value.data(), "outputs") else {
        panic!("dataset");
    };
    assert_eq!(outputs.records(), 2);
}

#[test]
fn forensic_frame_input_requires_a_durable_sink_before_source_work() {
    let mut profile = lines();
    profile.malformed = wes_core::framing::Malformed::Forensic { excerpt_bytes: 3 };
    let completion = run(
        input(
            value(Data::Bytes(b"abcdef\nok\n".to_vec().into())),
            "return {state:state+1,outputs:[item.text]};",
            Some(profile),
        ),
        settings(),
    );
    assert_eq!(completion.progress.phase, Phase::Stopped);
    assert_eq!(completion.progress.read_position, 0);
    assert_eq!(completion.progress.usage.input_records, 0);
    assert_eq!(completion.progress.coverage.unwrap().records, 0);
    assert!(
        completion
            .stop
            .unwrap()
            .failure
            .message
            .contains("durable Dataset")
    );
}
