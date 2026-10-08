use super::*;

#[tokio::test]
async fn requested_scan_totals_are_literal_captured_and_keep_incomplete_evidence() {
    let (base, calls) = workspace(None, None);
    let (handle, task) = session::spawn(
        base,
        RecordingMode::Ephemeral,
        no_files(),
        NonZeroUsize::new(1).unwrap(),
    )
    .unwrap();
    let reply = submit(&handle,"bounded",r#"
:package load source:"types: {IntStep: {base: Record, fields: {state: Int, outputs: 'List<Int>'}}}"
:def sum(state:Int, context:Int, item:Int) -> IntStep as :calc pure { return {state:state+item,outputs:[]}; }
:calc { return [1,2,3]; } > raw
:calc { return 10; } > amount
:scan source:$raw transition:sum initial:0 context:0 profile:TypedRecords work:1000000 input:4096 records:2 output:8192 outputs:1 duration:5000 > bounded
"#).await;
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
    let node = &snapshot.names["bounded"].node;
    assert_eq!(
        snapshot.execution.graph.node(node).unwrap().state(),
        NodeState::Failed
    );
    let Data::Record(result) = snapshot.execution.evidence_values[node].value.data() else {
        panic!("incomplete evidence")
    };
    assert_eq!(result["state"], Data::Int(3));
    let Data::Record(receipt) = &result["receipt"] else {
        panic!("receipt")
    };
    assert_eq!(receipt["inputRecords"], Data::Int(2));
    assert_eq!(
        receipt["exhausted"],
        Data::Option(Some(Box::new(Data::Text("input_records".into()))))
    );
    let Data::Record(limits) = &receipt["limits"] else {
        panic!("limits")
    };
    for (name, n) in [
        ("work", 1000000),
        ("inputCharge", 4096),
        ("inputRecords", 2),
        ("outputCharge", 8192),
        ("outputRecords", 1),
        ("durationMs", 5000),
    ] {
        assert_eq!(limits[name], Data::Int(n), "{name}");
    }
    let before = snapshot.execution.graph.len();
    for (index, argument) in [
        "work:$amount",
        "input:$amount",
        "records:$amount",
        "output:$amount",
        "outputs:$amount",
        "duration:$amount",
        "work:64000001",
        "duration:86400001",
        "records:0",
    ]
    .into_iter()
    .enumerate()
    {
        let reply = submit(&handle,&format!("refused-{index}"), &format!(":scan source:$raw transition:sum initial:0 context:0 profile:TypedRecords {argument} > refused")).await;
        assert!(
            reply
                .diagnostics
                .diagnostics
                .iter()
                .any(|d| d.severity == wes_language::Severity::Error),
            "{argument}: {:?}",
            reply.diagnostics
        );
        assert_eq!(
            handle.snapshot().await.unwrap().execution.graph.len(),
            before,
            "refusal must precede run admission: {argument}"
        );
    }
    assert_eq!(calls.load(Ordering::SeqCst), 0);
    stop(handle, task).await;
}
#[tokio::test]
async fn finite_scan_is_one_run_with_captured_source_identity_and_normal_downstream_data() {
    let (base, calls) = workspace(None, None);
    let (handle, task) = session::spawn(
        base,
        RecordingMode::Ephemeral,
        no_files(),
        NonZeroUsize::new(1).unwrap(),
    )
    .unwrap();
    submit(&handle,"types",r#":package load source:"types: {IntStep: {base: Record, fields: {state: Int, outputs: 'List<Int>'}}}""#).await;
    let reply=submit(&handle,"analysis",r#"
:def sum(state:Int, context:Int, item:Int) -> IntStep as :calc pure { return {state:state+item,outputs:[item*2]}; }
:calc { return [1,2,3]; } > raw
:scan source:$raw transition:sum initial:0 context:0 profile:TypedRecords > analysis
:calc { return $analysis.state; } > sum
"#).await;
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
    assert_eq!(snapshot.execution.graph.len(), 3);
    assert_eq!(calls.load(Ordering::SeqCst), 0);
    let node = &snapshot.names["analysis"].node;
    let value = snapshot
        .execution
        .values
        .get(node)
        .unwrap_or_else(|| panic!("{:?}", snapshot.execution.errors));
    let Data::Record(result) = value.data() else {
        panic!()
    };
    assert_eq!(result["state"], Data::Int(6));
    assert_eq!(
        result["outputs"],
        Data::List(vec![Data::Int(2), Data::Int(4), Data::Int(6)])
    );
    let Data::Record(receipt) = &result["receipt"] else {
        panic!()
    };
    assert_eq!(receipt["inputRecords"], Data::Int(3));
    assert_eq!(
        receipt["analysisId"],
        Data::Text(snapshot.execution.runs[node].as_str().into())
    );
    assert_eq!(
        receipt["sourceRun"],
        Data::Option(Some(Box::new(Data::Text(
            snapshot.execution.runs[&snapshot.names["raw"].node]
                .as_str()
                .into()
        ))))
    );
    assert_eq!(
        snapshot.execution.values[&snapshot.names["sum"].node].data(),
        &Data::Int(6)
    );
    assert!(
        !snapshot
            .execution
            .graph
            .node(node)
            .unwrap()
            .payload()
            .traits()
            .repeatable
    );
    stop(handle, task).await;
}
#[tokio::test]
async fn failed_scan_keeps_committed_evidence_without_opening_success_port_or_keeping_it() {
    let (base, calls) = workspace(None, None);
    let (handle, task) = session::spawn(
        base,
        RecordingMode::Ephemeral,
        no_files(),
        NonZeroUsize::new(1).unwrap(),
    )
    .unwrap();
    submit(&handle,"types",r#":package load source:"types: {IntStep: {base: Record, fields: {state: Int, outputs: 'List<Int>'}}}""#).await;
    let reply=submit(&handle,"partial",r#"
:def sum(state:Int, context:Int, item:Int) -> IntStep as :calc pure { if(item==2) { return {state:1/0,outputs:[]}; } return {state:state+item,outputs:[item]}; }
:calc { return [1,2,3]; } > raw
:scan source:$raw transition:sum initial:0 context:0 profile:TypedRecords > partial
:calc { return $partial.state; } > blocked
"#).await;
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
    let node = &snapshot.names["partial"].node;
    assert_eq!(
        snapshot.execution.graph.node(node).unwrap().state(),
        NodeState::Failed
    );
    assert!(!snapshot.execution.values.contains_key(node));
    let evidence = &snapshot.execution.evidence_values[node];
    assert_eq!(evidence.kind, wes_engine::runtime::EvidenceKind::Incomplete);
    let Data::Record(result) = evidence.value.data() else {
        panic!()
    };
    assert_eq!(result["state"], Data::Int(1));
    assert_eq!(result["outputs"], Data::List(vec![Data::Int(1)]));
    assert!(
        !snapshot
            .execution
            .values
            .contains_key(&snapshot.names["blocked"].node)
    );
    assert_eq!(calls.load(Ordering::SeqCst), 0);
    let log = handle.log().await.unwrap();
    let observations=log.entries.iter().filter(|e|matches!(e.entry(),JournalEntry::Observed(r) if r.node()==node && r.state()==NodeState::Failed)).count();
    assert_eq!(
        observations, 1,
        "individual records/progress must not create observations"
    );
    stop(handle, task).await;
}

#[tokio::test]
async fn scan_declaration_types_structured_literals_without_converting_referenced_values() {
    let (base, _) = workspace(None, None);
    let (handle, task) = session::spawn(
        base,
        RecordingMode::Ephemeral,
        no_files(),
        NonZeroUsize::new(1).unwrap(),
    )
    .unwrap();
    let source = r#":package load source:"types: {State: {base: Record, fields: {total: Int}}, Context: {base: Record, fields: {multiplier: Int}}, Step: {base: Record, fields: {state: State, outputs: 'List<Int>'}}}"
:def step(state:State, context:Context, item:Int) -> Step as :calc pure { return {state:{total:state.total+item*context.multiplier},outputs:[]}; }
:calc {return [1,2];} > raw
:scan source:$raw transition:step initial:{total:0} context:{multiplier:2} profile:TypedRecords > total
:calc {return {total:'0'};} > bad
:scan source:$raw transition:step initial:$bad context:{multiplier:2} profile:TypedRecords > refused"#;
    let reply = submit(&handle, "structured", source).await;
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
    let Data::Record(result) = snapshot.execution.values[&snapshot.names["total"].node].data()
    else {
        panic!()
    };
    let Data::Record(state) = &result["state"] else {
        panic!()
    };
    assert_eq!(state["total"], Data::Int(6));
    assert_eq!(
        snapshot
            .execution
            .graph
            .node(&snapshot.names["refused"].node)
            .unwrap()
            .state(),
        NodeState::Failed
    );
    assert!(
        !snapshot
            .execution
            .evidence_values
            .contains_key(&snapshot.names["refused"].node)
    );
    stop(handle, task).await;
}
