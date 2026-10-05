use wes_core::{
    Data, Primitive, Provenance, Shape, Value,
    capability::{Capability, Parameter, ProviderDescription, Safety},
};
use wes_engine::{
    driver::{CancellationToken, Executor},
    graph::{NodeId, NodeState, OutputPort},
    providers::{Call, InvocationFuture, Invoker},
    runtime::{Effect, Outcome, RunTicket},
    tasks::TaskExecutor,
    workspace::{Preparation, PreparedChange, Workspace, WorkspaceError},
};
use wes_language::{SourceText, Span, Statement, parse};
fn local_output(value: Value) -> Value {
    let provenance = value
        .provenance()
        .clone()
        .with_policy(&wes_core::flow::FlowPolicy::default().from_origin("local:fixture"));
    value.with_provenance(provenance)
}
use std::{
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};
#[path = "workspace/controls.rs"]
mod controls;
#[path = "workspace/defined_views.rs"]
mod defined_views;
#[path = "workspace/drafts.rs"]
mod drafts;
#[path = "workspace/help.rs"]
mod help;
#[path = "workspace/queries.rs"]
mod queries;
#[path = "workspace/type_checks.rs"]
mod type_checks;
#[path = "workspace/views.rs"]
mod views;

struct Echo(Arc<AtomicUsize>);
impl Invoker for Echo {
    fn invoke(&self, call: Call, _: CancellationToken) -> InvocationFuture {
        self.0.fetch_add(1, Ordering::SeqCst);
        let value = call
            .arguments
            .get("value")
            .or_else(|| call.arguments.get("amount"))
            .unwrap()
            .clone();
        Box::pin(async move { Ok(value) })
    }
}
fn metadata() -> ProviderDescription {
    let mut echo = Capability::new(["echo"], Shape::Unknown, Safety::Safe);
    echo.parameters = vec![
        Parameter::new("value", Shape::Unknown, true),
        Parameter::new("locale", Shape::Primitive(Primitive::Text), false),
    ];
    echo.provenance_arguments.insert("locale".into());
    let mut count = Capability::new(["count"], Shape::Primitive(Primitive::Int), Safety::Safe);
    count.parameters = vec![Parameter::new(
        "amount",
        Shape::Primitive(Primitive::Int),
        true,
    )];
    ProviderDescription::new("catalog", [echo, count], vec![]).unwrap()
}
fn workspace() -> (Workspace, Arc<AtomicUsize>) {
    let mut workspace =
        Workspace::local(wes_engine::providers::LocalScope::new("fixture").unwrap());
    let calls = Arc::new(AtomicUsize::new(0));
    workspace
        .register_provider(metadata(), Arc::new(Echo(calls.clone())))
        .unwrap();
    (workspace, calls)
}
fn statement(text: &str) -> Statement {
    let parsed = parse(&SourceText::new("test", text));
    assert!(parsed.diagnostics.is_empty(), "{:?}", parsed.diagnostics);
    assert_eq!(parsed.script.statements.len(), 1);
    parsed.script.statements.into_iter().next().unwrap()
}
fn prepare(workspace: &Workspace, text: &str) -> PreparedChange {
    let Preparation::Change(change) = workspace.prepare(&statement(text)).unwrap() else {
        panic!("declaration/call expected")
    };
    change
}
fn commit(workspace: &mut Workspace, text: &str) -> Option<NodeId> {
    workspace.commit(prepare(workspace, text)).unwrap().node
}
fn code(error: WorkspaceError) -> String {
    let WorkspaceError::Rejected { diagnostics, .. } = error else {
        panic!("diagnostic expected")
    };
    diagnostics[0].code.to_string()
}
fn ticket(
    effects: Vec<Effect<wes_engine::tasks::BoundTask>>,
) -> RunTicket<wes_engine::tasks::BoundTask> {
    effects
        .into_iter()
        .find_map(|effect| {
            if let Effect::Spawn(ticket) = effect {
                Some(ticket)
            } else {
                None
            }
        })
        .expect("spawn effect")
}
fn int(n: i64) -> Value {
    Value::new(
        Shape::Primitive(Primitive::Int),
        Data::Int(n),
        Provenance::default(),
    )
    .unwrap()
}

#[test]
fn preparation_is_effect_free_and_uncommitted_ids_and_names_remain_available() {
    let (mut workspace, calls) = workspace();
    let prepared = prepare(&workspace, "catalog count amount:4 > total");
    assert_eq!(prepared.node().unwrap().as_str(), "id1000");
    assert!(workspace.runtime().graph().is_empty());
    assert!(workspace.resolve("total").is_none());
    assert_eq!(calls.load(Ordering::SeqCst), 0);
    drop(prepared);
    let node = commit(&mut workspace, "catalog count amount:5 > total").unwrap();
    assert_eq!(node.as_str(), "id1000");
    assert_eq!(workspace.resolve("total").unwrap().node, node);
    assert_eq!(
        workspace.runtime().graph().node(&node).unwrap().state(),
        NodeState::Pending
    );
    assert_eq!(calls.load(Ordering::SeqCst), 0);
}

#[test]
fn successful_definition_is_announced_only_after_commit() {
    let (mut workspace, _) = workspace();
    let definition = prepare(&workspace, ":def named as catalog echo value:?value");
    assert!(definition.diagnostics().is_empty());
    assert!(!workspace.templates().contains("named"));
    let applied = workspace.commit(definition).unwrap();
    assert!(applied.node.is_none());
    assert_eq!(applied.diagnostics[0].code, "TMP000");
    assert!(workspace.templates().contains("named"));
}

#[tokio::test]
async fn provider_replacement_invalidates_preparation_but_does_not_retarget_existing_nodes() {
    let (mut workspace, original_calls) = workspace();
    commit(&mut workspace, "catalog count amount:1 > original");
    let obsolete = prepare(&workspace, "catalog count amount:2 > obsolete");
    let replacement_calls = Arc::new(AtomicUsize::new(0));
    workspace
        .register_provider(metadata(), Arc::new(Echo(replacement_calls.clone())))
        .unwrap();
    assert!(matches!(
        workspace.commit(obsolete),
        Err(WorkspaceError::Obsolete)
    ));
    let first = ticket(workspace.start(Duration::ZERO));
    workspace.enter(&first.run);
    let outcome = TaskExecutor::ephemeral()
        .execute(first.clone(), CancellationToken::new())
        .await
        .outcome;
    workspace.complete(&first.run, outcome, Duration::from_secs(1));
    assert_eq!(original_calls.load(Ordering::SeqCst), 1);
    assert_eq!(replacement_calls.load(Ordering::SeqCst), 0);
    commit(&mut workspace, "catalog count amount:3 > replacement");
    let second = ticket(workspace.start(Duration::from_secs(2)));
    workspace.enter(&second.run);
    let outcome = TaskExecutor::ephemeral()
        .execute(second.clone(), CancellationToken::new())
        .await
        .outcome;
    workspace.complete(&second.run, outcome, Duration::from_secs(3));
    assert_eq!(original_calls.load(Ordering::SeqCst), 1);
    assert_eq!(replacement_calls.load(Ordering::SeqCst), 1);
}

#[test]
fn prepared_changes_cannot_cross_workspaces_or_overwrite_later_declarations() {
    let (mut first, _) = workspace();
    let (mut second, _) = workspace();
    let a = prepare(&first, "catalog count amount:1 > first");
    assert!(matches!(second.commit(a), Err(WorkspaceError::Obsolete)));
    let a = prepare(&first, "catalog count amount:1 > first");
    let b = prepare(&first, "catalog count amount:2 > second");
    first.commit(a).unwrap();
    assert!(matches!(first.commit(b), Err(WorkspaceError::Obsolete)));
    assert!(first.resolve("second").is_none());
}

#[test]
fn both_new_binding_names_and_existing_future_names_are_excluded_from_node_identity() {
    let (mut workspace, _) = workspace();
    let first = commit(&mut workspace, "catalog count amount:1 > id1000 *> id1001").unwrap();
    assert_eq!(first.as_str(), "id1002");
    assert_eq!(workspace.resolve("id1000").unwrap().port, OutputPort::Data);
    assert_eq!(workspace.resolve("id1001").unwrap().port, OutputPort::Error);
    assert_eq!(workspace.resolve("id1002").unwrap().port, OutputPort::Data);
    commit(&mut workspace, "$id1000 > id1003");
    assert_eq!(
        commit(&mut workspace, "catalog count amount:2")
            .unwrap()
            .as_str(),
        "id1004"
    );
    assert_eq!(workspace.resolve("id1003").unwrap().node, first);
}

#[test]
fn alias_binding_is_atomic_and_preserves_channel_selection_and_old_dependencies() {
    let (mut workspace, _) = workspace();
    let first = commit(&mut workspace, "catalog echo value:one > first").unwrap();
    commit(&mut workspace, "$first::error > failure");
    let consumer = commit(&mut workspace, "catalog echo value:$failure > handled").unwrap();
    let second = commit(&mut workspace, "catalog echo value:two > second").unwrap();
    commit(&mut workspace, "$second::error > failure");
    assert_eq!(workspace.resolve("failure").unwrap().node, second);
    let dependency = workspace
        .runtime()
        .graph()
        .node(&consumer)
        .unwrap()
        .dependencies();
    assert_eq!(dependency[&first], OutputPort::Error);
    assert!(!dependency.contains_key(&second));
    let error = workspace
        .prepare(&statement("$first > duplicate *> duplicate"))
        .unwrap_err();
    assert_eq!(code(error), "ENG004");
    assert!(workspace.resolve("duplicate").is_none());
    let error = workspace
        .prepare(&statement("$first > id1001"))
        .unwrap_err();
    assert_eq!(code(error), "ENG003");
}

#[test]
fn definitions_reject_namespace_collisions_and_meta_bodies_before_installation() {
    let (mut workspace, _) = workspace();
    for (text, expected) in [
        (":def catalog as other value:?value", "TMP002"),
        (":def help as catalog echo value:hello", "TMP002"),
        (":def unsafe as help", "TMP001"),
        (":def unsafe as :help", "TMP001"),
        (":def unsafe as catalog echo value:hello > result", "TMP001"),
    ] {
        assert_eq!(
            code(workspace.prepare(&statement(text)).unwrap_err()),
            expected,
            "{text}"
        );
    }
    assert!(workspace.templates().snapshot().is_empty());
    commit(&mut workspace, ":def echo as catalog echo value:?value");
    let name = ProviderDescription::new("echo", [], vec![]).unwrap();
    assert_eq!(
        code(
            workspace
                .register_provider(name, Arc::new(Echo(Arc::new(AtomicUsize::new(0)))))
                .unwrap_err()
        ),
        "TMP002"
    );
    assert!(workspace.catalogue().provider("echo").is_none());
}

#[test]
fn type_package_preparation_is_atomic_and_invalidates_obsolete_call_preparations() {
    let (mut workspace, _) = workspace();
    let before = prepare(&workspace, "catalog count amount:3");
    let package = workspace
        .prepare_type_package(
            "types:\n  Positive:\n    base: Int\n    min: 1\n",
            Span::at(0),
        )
        .unwrap();
    assert!(workspace.contracts().resolve("Positive").is_err());
    workspace.commit(package).unwrap();
    assert!(workspace.contracts().resolve("Positive").is_ok());
    assert!(matches!(
        workspace.commit(before),
        Err(WorkspaceError::Obsolete)
    ));
    assert!(
        workspace
            .prepare_type_package(
                "types:\n  Partial:\n    base: Int\n  Broken:\n    base: Missing\n",
                Span::at(0)
            )
            .is_err()
    );
    assert!(workspace.contracts().resolve("Partial").is_err());
}

#[test]
fn invalid_template_literals_preserve_structured_validation_issues() {
    let (mut workspace, _) = workspace();
    let package = workspace
        .prepare_type_package(
            "types:\n  Positive:\n    base: Int\n    min: 1\n",
            Span::at(0),
        )
        .unwrap();
    workspace.commit(package).unwrap();
    commit(
        &mut workspace,
        ":def positive(value: Positive) as catalog count amount:?value",
    );
    let WorkspaceError::Rejected {
        diagnostics,
        issues,
    } = workspace
        .prepare(&statement("positive value:0"))
        .unwrap_err()
    else {
        panic!("validation error")
    };
    assert_eq!(diagnostics[0].code, "TYP005");
    assert!(!issues.is_empty());
    assert!(
        issues
            .iter()
            .all(|issue| issue.path.starts_with("/arguments/amount"))
    );
    assert!(workspace.runtime().graph().is_empty());
}

#[test]
fn all_checker_diagnostics_are_retained_and_change_checks_the_merged_original_call() {
    let (mut workspace, _) = workspace();
    commit(&mut workspace, "catalog count amount:3 > number");
    let WorkspaceError::Rejected { diagnostics, .. } = workspace
        .prepare(&statement("@unknown catalog count amount:bad extra:oops"))
        .unwrap_err()
    else {
        panic!("checker errors")
    };
    assert!(diagnostics.len() >= 3);
    assert_eq!(
        code(
            workspace
                .prepare(&statement(":change $number amount:bad"))
                .unwrap_err()
        ),
        "CHK004"
    );
    assert!(matches!(
        workspace
            .prepare(&statement(":change $number amount:4"))
            .unwrap(),
        Preparation::Meta(_)
    ));
}

#[test]
fn remaining_meta_plans_are_non_executing_and_revision_checked() {
    let (mut workspace, calls) = workspace();
    commit(&mut workspace, "catalog echo value:one > source");
    let Preparation::Meta(meta) = workspace
        .prepare(&statement(":workspace save \"kept\""))
        .unwrap()
    else {
        panic!("meta plan")
    };
    workspace.validate_meta(&meta).unwrap();
    assert!(workspace.resolve("kept").is_none());
    assert_eq!(workspace.runtime().graph().len(), 1);
    commit(&mut workspace, "$source > alias");
    assert!(matches!(
        workspace.validate_meta(&meta),
        Err(WorkspaceError::Obsolete)
    ));
    assert_eq!(calls.load(Ordering::SeqCst), 0);
}

#[test]
fn drop_removes_dependent_names_predictions_and_prepared_bindings() {
    let (mut workspace, _) = workspace();
    let root = commit(&mut workspace, "catalog echo value:one > root").unwrap();
    let child = commit(&mut workspace, "catalog echo value:$root > child").unwrap();
    let alias = prepare(&workspace, "$child > alias");
    let (removed, _) = workspace.drop_node(&root).unwrap();
    assert_eq!(removed.len(), 2);
    assert!(workspace.resolve("root").is_none());
    assert!(workspace.resolve("child").is_none());
    assert!(workspace.data_typing(&root).is_none());
    assert!(workspace.data_typing(&child).is_none());
    assert!(matches!(
        workspace.commit(alias),
        Err(WorkspaceError::Obsolete)
    ));
}

#[test]
fn accepted_actual_typing_replaces_prediction_but_a_cancelled_worker_cannot_retype() {
    let (mut workspace, _) = workspace();
    let root = commit(&mut workspace, "catalog echo value:one locale:en > root").unwrap();
    assert_eq!(workspace.data_typing(&root).unwrap().shape, Shape::Unknown);
    assert_eq!(
        workspace
            .data_typing(&root)
            .unwrap()
            .provenance
            .fact("locale"),
        Some("en")
    );
    let task = ticket(workspace.start(Duration::ZERO));
    workspace.enter(&task.run);
    workspace.cancel(&root, Duration::from_secs(1));
    workspace.complete(&task.run, Outcome::Produced(int(7)), Duration::from_secs(2));
    assert_eq!(workspace.data_typing(&root).unwrap().shape, Shape::Unknown);
    let second = commit(&mut workspace, "catalog echo value:two > second").unwrap();
    let task = ticket(workspace.start(Duration::from_secs(3)));
    workspace.enter(&task.run);
    workspace.complete(&task.run, Outcome::Produced(int(8)), Duration::from_secs(4));
    assert_eq!(
        workspace.data_typing(&second).unwrap().shape,
        Shape::Primitive(Primitive::Int)
    );
    assert!(
        workspace
            .data_typing(&second)
            .unwrap()
            .provenance
            .is_empty()
    );
}

#[tokio::test]
async fn nested_template_calls_reach_runtime_validation_and_route_the_error_without_calling_the_rejected_provider()
 {
    let (mut workspace, calls) = workspace();
    let package = workspace
        .prepare_type_package(
            "types:\n  Positive:\n    base: Int\n    min: 1\n",
            Span::at(0),
        )
        .unwrap();
    workspace.commit(package).unwrap();
    commit(
        &mut workspace,
        ":def inner(value: Positive) as catalog count amount:?value",
    );
    commit(
        &mut workspace,
        ":def outer(value: Int) as inner value:?value",
    );
    let source = commit(&mut workspace, "catalog echo value:bad > source").unwrap();
    let checked = commit(&mut workspace, "outer value:$source *> failure").unwrap();
    let handled = commit(&mut workspace, "catalog echo value:$failure > handled").unwrap();
    let first = ticket(workspace.start(Duration::ZERO));
    assert_eq!(first.run.node(), &source);
    workspace.enter(&first.run);
    let outcome = TaskExecutor::ephemeral()
        .execute(first.clone(), CancellationToken::new())
        .await
        .outcome;
    let second = ticket(workspace.complete(&first.run, outcome, Duration::from_secs(1)));
    assert_eq!(second.run.node(), &checked);
    workspace.enter(&second.run);
    let outcome = TaskExecutor::ephemeral()
        .execute(second.clone(), CancellationToken::new())
        .await
        .outcome;
    assert!(matches!(&outcome, Outcome::Failed(error) if error.code() == "TYP005"));
    let third = ticket(workspace.complete(&second.run, outcome, Duration::from_secs(2)));
    assert_eq!(third.run.node(), &handled);
    workspace.enter(&third.run);
    let outcome = TaskExecutor::ephemeral()
        .execute(third.clone(), CancellationToken::new())
        .await
        .outcome;
    workspace.complete(&third.run, outcome, Duration::from_secs(3));
    assert_eq!(calls.load(Ordering::SeqCst), 2); // source and handler, never the rejected count call.
    assert_eq!(
        workspace.runtime().graph().node(&handled).unwrap().state(),
        NodeState::Ready
    );
    assert_eq!(
        workspace.runtime().value_of(&handled).unwrap().shape(),
        &wes_core::ErrorValue::shape()
    );
}

#[test]
fn record_argument_types_and_change_dependencies_are_checked_before_execution() {
    use wes_core::RecordShape;
    let (mut workspace, calls) = workspace();
    let shape = Shape::Record(
        RecordShape::new(
            "Item",
            [
                ("id".into(), Shape::Primitive(Primitive::Int)),
                ("other".into(), Shape::Primitive(Primitive::Int)),
                ("name".into(), Shape::Primitive(Primitive::Text)),
                (
                    "maybe".into(),
                    Shape::Option(Box::new(Shape::Primitive(Primitive::Int))),
                ),
            ],
        )
        .unwrap(),
    );
    workspace
        .register_provider(
            ProviderDescription::new(
                "records",
                [Capability::new(["get"], shape, Safety::Safe)],
                vec![],
            )
            .unwrap(),
            Arc::new(Echo(calls.clone())),
        )
        .unwrap();
    let first = commit(&mut workspace, "records get > item").unwrap();
    let second = commit(&mut workspace, "records get > another").unwrap();
    let selected = commit(&mut workspace, "catalog count amount:$item.id > selected").unwrap();
    assert_eq!(
        workspace
            .runtime()
            .graph()
            .node(&selected)
            .unwrap()
            .dependencies()[&first],
        OutputPort::Data
    );
    for text in [
        "catalog count amount:$item.name",
        "catalog count amount:$item.absent",
        "catalog count amount:$item.id.child",
        "catalog count amount:$item.maybe",
        "catalog count amount:$missing.id",
        ":refresh $item.id",
        "$item.id > alias",
    ] {
        assert!(workspace.prepare(&statement(text)).is_err(), "{text}");
    }
    assert!(
        workspace
            .prepare(&statement(":change $selected amount:$item.other"))
            .is_ok()
    );
    assert!(
        workspace
            .prepare(&statement(":change $selected amount:$item.name"))
            .is_err()
    );
    let Preparation::Meta(change) = workspace
        .prepare(&statement(":change $selected amount:$another.id"))
        .unwrap()
    else {
        panic!("meta");
    };
    assert_eq!(
        code(workspace.prepare_control(change).unwrap_err()),
        "MET014"
    );
    let Preparation::Meta(change) = workspace
        .prepare(&statement(":change $selected amount:$item.other"))
        .unwrap()
    else {
        panic!("meta");
    };
    let change = workspace.prepare_control(change).unwrap();
    workspace.apply_control(change, Duration::ZERO).unwrap();
    assert_eq!(
        workspace.runtime().graph().node(&selected).unwrap().state(),
        NodeState::Stale
    );
    commit(&mut workspace, "$another > item");
    let dependencies = workspace
        .runtime()
        .graph()
        .node(&selected)
        .unwrap()
        .dependencies();
    assert!(dependencies.contains_key(&first));
    assert!(!dependencies.contains_key(&second));
    assert_eq!(calls.load(Ordering::SeqCst), 0);
}

fn structured_workspace() -> (Workspace, Arc<AtomicUsize>) {
    let (mut workspace, calls) = workspace();
    let body = Shape::Record(
        wes_core::RecordShape::new(
            "Body",
            [
                ("mode".into(), Shape::Primitive(Primitive::Text)),
                ("count".into(), Shape::Primitive(Primitive::Int)),
                (
                    "nested".into(),
                    Shape::List(Box::new(Shape::Primitive(Primitive::Int))),
                ),
            ],
        )
        .unwrap(),
    );
    let mut echo = Capability::new(["echo"], Shape::Unknown, Safety::Safe);
    echo.parameters = vec![Parameter::new("value", body, true)];
    workspace
        .register_provider(
            ProviderDescription::new("catalog", [echo], vec![]).unwrap(),
            Arc::new(Echo(calls.clone())),
        )
        .unwrap();
    (workspace, calls)
}

#[tokio::test]
async fn nested_arguments_use_captured_projections_and_contextual_literal_types() {
    let (mut workspace, calls) = structured_workspace();
    let level = commit(&mut workspace, ":calc { return \"degraded\"; } > level").unwrap();
    let cfg = commit(&mut workspace, ":calc { return {count:7}; } > cfg").unwrap();
    queries::run_all(&mut workspace).await;
    let result = commit(
        &mut workspace,
        "catalog echo value:{mode:$level, count:$cfg.count, nested:[\"8\", $cfg.count]} > result",
    )
    .unwrap();
    let dependencies = workspace
        .runtime()
        .graph()
        .node(&result)
        .unwrap()
        .dependencies();
    assert_eq!(dependencies.len(), 2);
    assert_eq!(dependencies[&level], OutputPort::Data);
    assert_eq!(dependencies[&cfg], OutputPort::Data);
    assert_eq!(calls.load(Ordering::SeqCst), 0);
    queries::run_all(&mut workspace).await;
    let Data::Record(fields) = workspace.runtime().value_of(&result).unwrap().data() else {
        panic!("body")
    };
    assert_eq!(fields["mode"], Data::Text("degraded".into()));
    assert_eq!(fields["count"], Data::Int(7));
    assert_eq!(
        fields["nested"],
        Data::List(vec![Data::Int(8), Data::Int(7)])
    );
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    let literal = commit(
        &mut workspace,
        "catalog echo value:{mode:ready, count:\"9\", nested:[]} > literal",
    )
    .unwrap();
    queries::run_all(&mut workspace).await;
    let Data::Record(fields) = workspace.runtime().value_of(&literal).unwrap().data() else {
        panic!("body")
    };
    assert_eq!(fields["count"], Data::Int(9));
    assert_eq!(fields["nested"], Data::List(vec![]));
}

#[tokio::test]
async fn nested_argument_refusal_and_change_keep_shared_dependency_checks() {
    let (mut workspace, calls) = structured_workspace();
    commit(&mut workspace, ":calc { return \"degraded\"; } > level");
    commit(&mut workspace, ":calc { return \"7\"; } > textual_count");
    queries::run_all(&mut workspace).await;
    for text in [
        "catalog echo value:{mode:$level, count:$textual_count, nested:[]}",
        "catalog echo value:{mode:$absent, count:7, nested:[]}",
        "catalog echo value:{mode:$level, count:7, nested:[true]}",
    ] {
        assert!(workspace.prepare(&statement(text)).is_err(), "{text}");
    }
    assert_eq!(calls.load(Ordering::SeqCst), 0);
    commit(
        &mut workspace,
        "catalog echo value:{mode:$level, count:7, nested:[]} > response",
    );
    queries::run_all(&mut workspace).await;
    controls::apply(
        &mut workspace,
        ":change $response value:{mode:$level, count:8, nested:[9]}",
    );
    commit(&mut workspace, ":calc { return \"new\"; } > other");
    queries::run_all(&mut workspace).await;
    assert_eq!(
        code(controls::invalid(
            &workspace,
            ":change $response value:{mode:$other, count:8, nested:[]}"
        )),
        "MET014"
    );
    assert_eq!(calls.load(Ordering::SeqCst), 1);
}
