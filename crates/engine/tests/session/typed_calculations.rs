use super::*;

async fn start() -> (
    SessionHandle,
    wes_engine::session::SessionTask,
    Arc<AtomicUsize>,
) {
    let (base, calls) = workspace(None, None);
    let (handle, task) = session::spawn(
        base,
        RecordingMode::Ephemeral,
        no_files(),
        NonZeroUsize::new(1).unwrap(),
    )
    .unwrap();
    (handle, task, calls)
}

#[tokio::test]
async fn typed_calculations_execute_actual_conversion_example_and_inspect_capture() {
    let (handle, task, calls) = start().await;
    let reply = submit(
        &handle,
        "example",
        include_str!("../../../../examples/typed-calculations/conversion.wes"),
    )
    .await;
    assert!(
        !reply
            .diagnostics
            .diagnostics
            .iter()
            .any(|d| d.severity == wes_language::Severity::Error),
        "{:?}",
        reply.diagnostics
    );
    handle.wait_idle().await.unwrap();
    let snapshot = handle.snapshot().await.unwrap();
    for name in ["direct", "piped", "explicitPipe"] {
        assert_eq!(
            snapshot.execution.values[&snapshot.names[name].node].data(),
            &Data::List(vec![Data::Int(20), Data::Int(30)])
        );
    }
    let Data::Record(definition) =
        snapshot.execution.values[&snapshot.names["definition"].node].data()
    else {
        panic!()
    };
    let Data::Record(calculation) =
        snapshot.execution.values[&snapshot.names["calculation"].node].data()
    else {
        panic!()
    };
    assert_eq!(definition["purity"], Data::Text("pure".into()));
    assert_eq!(definition["conversionEligible"], Data::Bool(true));
    assert_eq!(definition["requiresPure"], Data::Bool(true));
    assert_eq!(definition["revision"], calculation["definitionRevision"]);
    assert_eq!(calls.load(Ordering::SeqCst), 0);
    stop(handle, task).await;
}

#[tokio::test]
async fn typed_calculations_reject_bad_inputs_before_effects_and_bad_outputs_before_publication() {
    let (handle, task, calls) = start().await;
    submit(&handle, "definitions", ":def effect(input: Int) -> Int as :calc { call('catalog', ['echo'], {value: 'entered'}); return input; }\n:def bad(input: Int) -> Text as :calc pure { return input; }").await;
    let reply = submit(&handle, "input", ":calc { return 'not an integer'; } > wrong\n $wrong | effect > invalid\n:calc { return 4; } | bad > invalidOutput").await;
    assert_eq!(reply.nodes.len(), 4, "{:?}", reply.diagnostics);
    handle.wait_idle().await.unwrap();
    let snapshot = handle.snapshot().await.unwrap();
    for name in ["invalid", "invalidOutput"] {
        let node = &snapshot.names[name].node;
        assert_eq!(
            snapshot.execution.graph.node(node).unwrap().state(),
            NodeState::Failed
        );
        assert_eq!(snapshot.execution.errors[node].code(), "TYP005");
        assert!(!snapshot.execution.values.contains_key(node));
    }
    assert_eq!(calls.load(Ordering::SeqCst), 0);
    for (index, source) in [
        "effect",
        "effect input:1 input:2",
        "effect input:1 extra:2",
        "effect input:wrong",
    ]
    .iter()
    .enumerate()
    {
        let reply = submit(&handle, &format!("rejected{index}"), source).await;
        assert!(reply.nodes.is_empty(), "{source}");
        assert!(
            reply
                .diagnostics
                .diagnostics
                .iter()
                .any(|d| d.severity == wes_language::Severity::Error)
        );
    }
    stop(handle, task).await;
}

#[tokio::test]
async fn typed_calculations_assertions_and_syntax_errors_never_enter_providers() {
    let (handle, task, calls) = start().await;
    for (index, source) in [
        ":calc pure { if (false) { call('catalog', ['echo'], {value:'secret'}); } return 1; }",
        ":def bad(input: Int) -> Int as :calc pure { return call('catalog', ['echo'], {value:input}); }",
        "catalog echo value:must-not-run\n:def malformed(input: Int) -> Int as :calc { return ; }",
        ":def captures(input: Int) -> Int as :calc { return $private; }",
    ].iter().enumerate() {
        let reply = submit(&handle, &format!("reject{index}"), source).await;
        assert!(reply.nodes.is_empty(), "{source}");
        assert!(reply.diagnostics.diagnostics.iter().any(|d| d.severity == wes_language::Severity::Error));
    }
    assert_eq!(calls.load(Ordering::SeqCst), 0);
    stop(handle, task).await;
}

#[tokio::test]
async fn typed_pipe_input_binds_each_predecessor_and_preserves_explicit_mappings_and_error_port() {
    let (handle, task, calls) = start().await;
    let source = r#"
:def twice(input: Int) -> Int as :calc pure { return input * 2; }
:def add(input: Int, delta: Int) -> Int as :calc pure { return input + delta; }
:def code(input: Unknown) -> Text as :calc pure { return input.code; }
:def fixed() -> Int as :calc pure { return 9; }
:calc { return 3; } > seed | twice | add delta:1 | twice > chained
:calc { return 11; } > other
$seed | twice input:$other > explicitReference
$seed | twice input:5 > explicitLiteral
$seed | twice input:input > explicitLegacy
:calc { return {number: 7}; } | twice input:input.number > selectedField
$seed | fixed > constant
:calc { return 1 / 0; } > failed
$failed::error | code > handled
"#;
    let reply = submit(&handle, "pipe-input", source).await;
    assert!(
        !reply
            .diagnostics
            .diagnostics
            .iter()
            .any(|d| d.severity == wes_language::Severity::Error),
        "{:?}",
        reply.diagnostics
    );
    handle.wait_idle().await.unwrap();
    let snapshot = handle.snapshot().await.unwrap();
    for (name, expected) in [
        ("chained", 14),
        ("explicitReference", 22),
        ("explicitLiteral", 10),
        ("explicitLegacy", 6),
        ("selectedField", 14),
        ("constant", 9),
    ] {
        assert_eq!(
            snapshot.execution.values[&snapshot.names[name].node].data(),
            &Data::Int(expected),
            "{name}"
        );
    }
    let failed = &snapshot.names["failed"].node;
    let handled = &snapshot.names["handled"].node;
    assert_eq!(
        snapshot.execution.values[handled].data(),
        &Data::Text(snapshot.execution.errors[failed].code().into())
    );
    assert_eq!(
        snapshot
            .execution
            .graph
            .node(handled)
            .unwrap()
            .dependencies()
            .get(failed),
        Some(&wes_engine::graph::OutputPort::Error)
    );
    assert_eq!(calls.load(Ordering::SeqCst), 0);
    stop(handle, task).await;
}

#[tokio::test]
async fn typed_pipe_input_does_not_guess_other_parameters_or_bind_ordinary_calls() {
    let (handle, task, calls) = start().await;
    submit(
        &handle,
        "definitions",
        r#"
:def twice(input: Int) -> Int as :calc pure { return input * 2; }
:def add(input: Int, delta: Int) -> Int as :calc pure { return input + delta; }
:def different(value: Int) -> Int as :calc pure { return value; }
:def echo(input: Text) as catalog echo value:?input
:calc { return 3; } > seed
"#,
    )
    .await;
    handle.wait_idle().await.unwrap();
    for (index, source) in [
        "twice",                         // No pipe means no implicit value.
        "$seed | add",                   // Other parameters stay required.
        "$seed | twice input:1 input:2", // No silent duplicate overwrite.
        "$seed | different",             // No type/position-based inference.
        "$seed | echo",                  // An ordinary template's input is not injected.
        "$seed | catalog echo",          // Provider parameters remain explicit.
        ":def conflict(input: Int, input: Int) -> Int as :calc pure { return input; }",
        "$seed | :calc pure { const input = 9; return input; }",
    ]
    .iter()
    .enumerate()
    {
        let reply = submit(&handle, &format!("reject-input-{index}"), source).await;
        assert!(reply.nodes.is_empty(), "{source}");
        assert!(
            reply
                .diagnostics
                .diagnostics
                .iter()
                .any(|d| d.severity == wes_language::Severity::Error),
            "{source}: {:?}",
            reply.diagnostics
        );
    }
    assert_eq!(calls.load(Ordering::SeqCst), 0);
    stop(handle, task).await;
}

#[tokio::test]
async fn temporal_example_uses_workspace_field_references_without_provider_calls() {
    let (handle, task, calls) = start().await;
    let reply = submit(
        &handle,
        "temporal",
        include_str!("../../../../examples/temporal/interval.wes"),
    )
    .await;
    assert!(
        !reply
            .diagnostics
            .diagnostics
            .iter()
            .any(|d| d.severity == wes_language::Severity::Error),
        "{:?}",
        reply.diagnostics
    );
    handle.wait_idle().await.unwrap();
    let snapshot = handle.snapshot().await.unwrap();
    let data = |name: &str| snapshot.execution.values[&snapshot.names[name].node].data();
    assert_eq!(data("elapsed"), &Data::Duration("PT10M".parse().unwrap()));
    assert_eq!(data("roundtrip"), &Data::Bool(true));
    assert_eq!(
        data("exactEpochDisplay"),
        &Data::Text("1735732800000000001".into())
    );
    assert_eq!(
        data("startSeconds"),
        &Data::Decimal("1735732800.000000000".parse().unwrap())
    );
    let Data::List(selected) = data("selected") else {
        panic!("selected rows")
    };
    assert_eq!(selected.len(), 2);
    assert_eq!(calls.load(Ordering::SeqCst), 0);
    stop(handle, task).await;
}

#[tokio::test]
async fn sort_selectors_reject_provider_calls_before_any_effect_is_dispatched() {
    let (handle, task, calls) = start().await;
    for (index, source) in [
        ":calc { return [1].sortBy(x=>call('catalog',['echo'],{value:'synthetic'})); }",
        ":calc { function helper(x){ return call('catalog',['echo'],{value:'synthetic'}); } return [].sortBy(x=>helper(x)); }",
    ].iter().enumerate() {
        let reply = submit(&handle, &format!("sort-{index}"), source).await;
        assert!(reply.diagnostics.diagnostics.iter().any(|d| d.code == "CAL009"), "{:?}", reply.diagnostics);
    }
    handle.wait_idle().await.unwrap();
    assert_eq!(calls.load(Ordering::SeqCst), 0);
    stop(handle, task).await;
}
