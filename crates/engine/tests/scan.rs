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
