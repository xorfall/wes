use wes_core::{Data, ErrorId, ErrorValue, Primitive, Provenance, Shape, Timestamp, Value};
use wes_engine::{
    graph::{NodeId, NodeState},
    history::{
        CallRecord, CommandRecord, DiagnosticRecord, ExecutionRecord, JournalEntry, RecoveryEntry,
        RestoreIndex, RetainedResult, unresolved_calls,
    },
    runtime::{Observation, RestoredState, RunId, StaleReason},
    storage::ValueHandle,
};
use wes_language::{Diagnostic, Span};

fn node() -> NodeId {
    NodeId::new("id1000").unwrap()
}
fn run(id: &str) -> RunId {
    RunId::new(id).unwrap()
}
fn at() -> Timestamp {
    "2026-09-10T12:00:00Z".parse().unwrap()
}
fn error() -> ErrorValue {
    ErrorValue::new(
        ErrorId::new("error-1").unwrap(),
        "RUN003",
        "cancelled",
        vec![],
        None,
    )
    .unwrap()
}
fn value() -> Value {
    Value::new(
        Shape::Primitive(Primitive::Int),
        Data::Int(7),
        Provenance::default(),
    )
    .unwrap()
}
fn observed(id: &str, state: NodeState) -> JournalEntry {
    JournalEntry::Observed(
        ExecutionRecord::new(
            format!("observation-{id}"),
            node(),
            Some(run(id)),
            at(),
            state,
            matches!(state, NodeState::Failed | NodeState::Cancelled).then(error),
        )
        .unwrap(),
    )
}
fn retained(id: &str) -> JournalEntry {
    JournalEntry::Result(RetainedResult {
        retention: wes_engine::storage::Retention::Unknown,
        node: node(),
        run: run(id),
        handle: ValueHandle::fresh(),
    })
}
fn call(id: &str) -> RecoveryEntry {
    RecoveryEntry::Calling(CallRecord {
        node: node(),
        run: run(id),
        cell: "cell-a".into(),
        capability: "catalog create".into(),
        safe: false,
        at: at(),
    })
}
fn called(id: &str) -> RecoveryEntry {
    RecoveryEntry::Called {
        node: node(),
        run: run(id),
        produced: true,
    }
}

#[test]
fn only_the_latest_ready_run_can_restore_a_retained_result() {
    let mut entries = vec![
        observed("old", NodeState::Ready),
        retained("old"),
        observed("new", NodeState::Ready),
    ];
    assert!(RestoreIndex::build(&entries).retained.is_empty());
    entries.push(retained("new"));
    // Keeping an old historical handle later must not replace the latest run's result.
    entries.push(retained("old"));
    let index = RestoreIndex::build(&entries);
    assert_eq!(index.retained[&node()].run, run("new"));
    assert!(matches!(
        index.state(&node(), Some(value())),
        RestoredState::Ready(_)
    ));
    assert!(matches!(
        index.state(&node(), None),
        RestoredState::StaleBecause(_)
    ));
}

#[test]
fn failed_cancelled_skipped_and_interrupted_nodes_never_restore_old_success() {
    for state in [
        NodeState::Pending,
        NodeState::Running,
        NodeState::Stale,
        NodeState::Failed,
        NodeState::Cancelled,
        NodeState::Skipped,
    ] {
        let entries = vec![retained("old"), observed("new", state)];
        let index = RestoreIndex::build(&entries);
        assert!(index.retained.is_empty());
        match (state, index.state(&node(), Some(value()))) {
            (NodeState::Failed, RestoredState::Failed(e))
            | (NodeState::Cancelled, RestoredState::Cancelled(e)) => assert_eq!(e, error()),
            (NodeState::Skipped, RestoredState::Skipped) => {}
            (
                NodeState::Pending | NodeState::Running | NodeState::Stale,
                RestoredState::StaleBecause(_),
            ) => {}
            other => panic!("unexpected restored state: {other:?}"),
        }
    }
}

#[test]
fn absent_observations_do_not_invent_execution_but_results_keep_their_run() {
    let entries = vec![retained("completed")];
    let index = RestoreIndex::build(&entries);
    assert_eq!(index.retained[&node()].run, run("completed"));
    assert!(matches!(
        index.state(&node(), Some(value())),
        RestoredState::Ready(_)
    ));
    let empty = RestoreIndex::build(&[]);
    assert!(matches!(
        empty.state(&node(), Some(value())),
        RestoredState::StaleBecause(StaleReason::RestoreUnfinished)
    ));
    let mixed = vec![retained("completed"), observed("new", NodeState::Ready)];
    assert!(RestoreIndex::build(&mixed).retained.is_empty());
}

#[test]
fn late_and_unrelated_completions_cannot_close_a_different_run() {
    let entries = vec![call("old"), call("new"), called("old"), called("unrelated")];
    let pending = unresolved_calls(&entries);
    assert_eq!(pending.len(), 1);
    assert_eq!(pending[0].run, run("new"));
    assert!(!pending[0].safe);
    assert!(unresolved_calls(&[call("same"), call("same"), called("same")]).is_empty());
}

#[test]
fn observation_validation_preserves_error_identity_and_excludes_payloads() {
    for state in [NodeState::Ready, NodeState::Failed, NodeState::Cancelled] {
        let must_error = state != NodeState::Ready;
        assert!(
            ExecutionRecord::new(
                "obs".into(),
                node(),
                None,
                at(),
                state,
                (!must_error).then(error)
            )
            .is_err()
        );
        let observation = Observation {
            revision: 0,
            stale_reason: None,
            delivery: None,
            evidence: None,
            node: node(),
            run: Some(run("run")),
            state,
            error: must_error.then(error),
            value: Some(value()),
        };
        let record = ExecutionRecord::capture("obs".into(), at(), &observation).unwrap();
        assert_eq!(record.error(), observation.error.as_ref());
        assert_eq!(record.run(), observation.run.as_ref());
    }
    assert!(ExecutionRecord::new(" ".into(), node(), None, at(), NodeState::Ready, None).is_err());
}

#[test]
fn future_diagnostic_codes_and_source_spans_are_owned_not_leaked_static_strings() {
    let source = "say 😀";
    let diagnostic = Diagnostic::error(
        String::from("EXTENSION123"),
        Span::new(4, 8).unwrap(),
        "message",
    );
    let record = DiagnosticRecord::new(
        "diag".into(),
        at(),
        "cell".into(),
        source.into(),
        diagnostic.clone(),
    )
    .unwrap();
    assert_eq!(record.diagnostic().code, "EXTENSION123");
    let invalid = Diagnostic {
        span: Span::new(4, 5).unwrap(),
        ..diagnostic
    };
    assert!(DiagnosticRecord::new("diag".into(), at(), "".into(), source.into(), invalid).is_err());
    let entry = JournalEntry::Diagnosed(record);
    let command = JournalEntry::Command(CommandRecord {
        source_name: "fixture.wes".into(),
        source_start: wes_language::Position { line: 1, column: 1 },
        changed_nodes: vec![],
        document: None,
        revision_of: None,
        environments: None,
        cell: "cell".into(),
        text: "original".into(),
        replay: "captured".into(),
        nodes: vec![node()],
        type_sources: [("types.yaml".into(), "verbatim\r\n".into())].into(),
        calculation_package: None,
        imports: vec![],
    });
    let entries = [entry, command];
    let index = RestoreIndex::build(&entries);
    assert_eq!(index.commands.len(), 1);
    assert_eq!(index.diagnostics.len(), 1);
    assert!(index.accepted_cells.contains("cell"));
    assert_eq!(index.commands[0].type_sources["types.yaml"], "verbatim\r\n");
    assert_eq!(index.commands[0].replay, "captured");
}

#[test]
fn restore_explanations_use_recorded_evidence_without_guessing_private_status() {
    let ready = [observed("r", NodeState::Ready)];
    assert!(matches!(
        RestoreIndex::build(&ready).state(&node(), None),
        RestoredState::StaleBecause(StaleReason::RestoreNotRetained)
    ));
    let saved = [observed("r", NodeState::Ready), retained("r")];
    assert!(matches!(
        RestoreIndex::build(&saved).state(&node(), None),
        RestoredState::StaleBecause(StaleReason::RestoreUnavailable)
    ));
    for cause in [
        None,
        Some(StaleReason::DependencyChanged),
        Some(StaleReason::DependencyRefreshed),
        Some(StaleReason::ResultEvicted),
    ] {
        let record = ExecutionRecord::new(
            "e".into(),
            node(),
            Some(run("r")),
            at(),
            NodeState::Stale,
            None,
        )
        .unwrap()
        .with_stale_reason(cause)
        .unwrap();
        let entries = [JournalEntry::Observed(record)];
        let RestoredState::StaleBecause(actual) =
            RestoreIndex::build(&entries).state(&node(), None)
        else {
            panic!()
        };
        assert_eq!(actual, cause.unwrap_or(StaleReason::Unknown));
    }
    assert!(
        ExecutionRecord::new("e".into(), node(), None, at(), NodeState::Ready, None)
            .unwrap()
            .with_stale_reason(Some(StaleReason::DependencyChanged))
            .is_err()
    );
}
