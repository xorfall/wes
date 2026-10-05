use super::*;
use indexmap::IndexMap;
use wes_core::capability::{DeclaredRule, Rule, RuleBasis};
use wes_engine::{
    graph::OutputRef,
    runtime::{OutputState, RuntimeCode},
    tasks::BoundTask,
};
#[path = "queries_nodes.rs"]
mod nodes;
#[path = "queries_types.rs"]
mod types;
#[path = "queries_views.rs"]
mod view_packages;

#[tokio::test]
async fn name_registries_preserve_text_typing_before_execution_and_for_empty_results() {
    let (mut workspace, calls) = workspace();
    let text_list = Shape::List(Box::new(Shape::Primitive(Primitive::Text)));
    for registry in [
        "homes",
        "environments",
        "templates",
        "adapters",
        "views",
        "importers",
        "workspaces",
        "commands",
    ] {
        let node = commit(&mut workspace, &format!(":list {registry}")).unwrap();
        assert_eq!(
            workspace.data_typing(&node).unwrap().shape,
            text_list,
            "{registry}"
        );
        run_all(&mut workspace).await;
        let value = workspace.runtime().value_of(&node).unwrap();
        assert_eq!(value.shape(), &text_list, "{registry}");
        let Data::List(names) = value.data() else {
            panic!("name list")
        };
        assert!(
            names.iter().all(|name| matches!(name, Data::Text(_))),
            "{registry}"
        );
        if matches!(registry, "templates" | "adapters" | "workspaces") {
            assert!(names.is_empty(), "{registry}");
        }
    }
    for registry in ["providers", "names"] {
        let node = commit(&mut workspace, &format!(":list {registry}")).unwrap();
        run_all(&mut workspace).await;
        let value = workspace.runtime().value_of(&node).unwrap();
        assert_eq!(value.shape(), &Shape::List(Box::new(Shape::Unknown)));
        let Data::List(rows) = value.data() else {
            panic!("record list")
        };
        assert!(rows.iter().all(|row| matches!(row, Data::Record(_))));
    }
    assert_eq!(calls.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn missing_inspection_targets_identify_provider_and_capability_without_provider_execution() {
    let (mut workspace, calls) = workspace();
    replace(&mut workspace, "metadata");
    for (source, expected) in [
        (":inspect absent operation", "RES004"),
        (":inspect inventory wrong path", "RES005"),
    ] {
        let error = workspace.prepare(&statement(source)).unwrap_err();
        assert_eq!(code(error), expected);
        assert!(workspace.runtime().graph().is_empty());
    }
    assert_eq!(calls.load(Ordering::SeqCst), 0);
}

fn replace(workspace: &mut Workspace, summary: &str) {
    let mut capability = Capability::new(
        ["items", "get"],
        Shape::Primitive(Primitive::Text),
        Safety::Safe,
    );
    capability.summary = summary.into();
    capability.parameters = vec![Parameter::new("id", Shape::Primitive(Primitive::Int), true)];
    capability.rules = vec![DeclaredRule {
        rule: Rule::Requires {
            key: "locale".into(),
            needs: "id".into(),
        },
        basis: RuleBasis::Documented { note: None },
    }];
    workspace
        .register_provider(
            ProviderDescription::new(
                "inventory",
                [capability],
                vec!["PRIVATE_CREDENTIAL_NAME".into()],
            )
            .unwrap(),
            Arc::new(Echo(Arc::new(AtomicUsize::new(0)))),
        )
        .unwrap();
}
async fn execute(
    workspace: &mut Workspace,
    ticket: RunTicket<BoundTask>,
) -> Vec<Effect<BoundTask>> {
    let ticket = workspace
        .enter_ticket(ticket)
        .expect("entry")
        .expect("ticket");
    finish(workspace, ticket).await
}
async fn finish(workspace: &mut Workspace, ticket: RunTicket<BoundTask>) -> Vec<Effect<BoundTask>> {
    let run = ticket.run.clone();
    let report = TaskExecutor::ephemeral()
        .execute(ticket, CancellationToken::new())
        .await;
    assert!(report.notices.is_empty());
    workspace.complete(&run, report.outcome, Duration::ZERO)
}
pub(super) async fn run_all(workspace: &mut Workspace) {
    let mut queue = std::collections::VecDeque::from(workspace.start(Duration::ZERO));
    while let Some(effect) = queue.pop_front() {
        if let Effect::Spawn(ticket) = effect {
            queue.extend(execute(workspace, ticket).await);
        }
    }
}
fn fields(value: &Value) -> &indexmap::IndexMap<String, Data> {
    let Data::Record(fields) = value.data() else {
        panic!("record")
    };
    fields
}
fn invocation(value: &Value) -> &indexmap::IndexMap<String, Data> {
    let Data::Record(invocation) = &fields(value)["invocation"] else {
        panic!("invocation")
    };
    invocation
}
fn refresh(workspace: &mut Workspace, node: &NodeId) -> Vec<Effect<BoundTask>> {
    let Preparation::Meta(meta) = workspace
        .prepare(&statement(&format!(":refresh ${node}")))
        .unwrap()
    else {
        panic!("refresh")
    };
    let control = workspace.prepare_control(meta).unwrap();
    workspace
        .apply_control(control, Duration::ZERO)
        .unwrap()
        .effects
}

#[tokio::test]
async fn capability_inspection_captures_at_entry_not_at_preparation_and_refresh_recaptures() {
    let (mut workspace, calls) = workspace();
    replace(&mut workspace, "initial");
    let inspected = commit(&mut workspace, ":inspect inventory items get > inspected").unwrap();
    let queued = ticket(workspace.start(Duration::ZERO));
    replace(&mut workspace, "at entry");
    let entered = workspace.enter_ticket(queued).unwrap().unwrap();
    replace(&mut workspace, "after entry");
    finish(&mut workspace, entered).await;
    let value = workspace.runtime().value_of(&inspected).unwrap();
    assert_eq!(invocation(value)["summary"], Data::Text("at entry".into()));
    assert_eq!(
        invocation(value)["capability"],
        Data::Text("items get".into())
    );
    assert_eq!(
        invocation(value)["rules"],
        Data::List(vec![Data::Text("'locale' needs 'id'".into())])
    );
    assert!(!format!("{value:?}").contains("PRIVATE_CREDENTIAL_NAME"));
    let queued = ticket(refresh(&mut workspace, &inspected));
    execute(&mut workspace, queued).await;
    assert_eq!(
        invocation(workspace.runtime().value_of(&inspected).unwrap())["summary"],
        Data::Text("after entry".into())
    );
    assert_eq!(calls.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn registry_lists_are_values_with_reference_compatible_order_filtering_and_reserved_names() {
    let (mut workspace, calls) = workspace();
    replace(&mut workspace, "Items");
    let providers = commit(&mut workspace, ":list providers > providers").unwrap();
    let capabilities = commit(
        &mut workspace,
        ":list capabilities provider:inventory > capabilities",
    )
    .unwrap();
    let missing = commit(
        &mut workspace,
        ":list capabilities provider:absent > missing",
    )
    .unwrap();
    let templates = commit(&mut workspace, ":list templates > templates").unwrap();
    let commands = commit(&mut workspace, ":list commands > commands").unwrap();
    let copied = commit(&mut workspace, "catalog echo value:$providers > copied").unwrap();
    run_all(&mut workspace).await;
    assert_eq!(
        workspace.runtime().value_of(&providers).unwrap().data(),
        &Data::List(vec![
            Data::Record(IndexMap::from([
                ("provider".into(), Data::Text("catalog".into())),
                ("environment".into(), Data::Option(None))
            ])),
            Data::Record(IndexMap::from([
                ("provider".into(), Data::Text("inventory".into())),
                ("environment".into(), Data::Option(None))
            ]))
        ])
    );
    assert_eq!(
        workspace.runtime().value_of(&copied),
        workspace
            .runtime()
            .value_of(&providers)
            .cloned()
            .map(local_output)
            .as_ref()
    );
    let Data::List(rows) = workspace.runtime().value_of(&capabilities).unwrap().data() else {
        panic!("list")
    };
    assert_eq!(rows.len(), 1);
    for node in [missing, templates] {
        assert_eq!(
            workspace.runtime().value_of(&node).unwrap().data(),
            &Data::List(vec![])
        );
    }
    let Data::List(commands) = workspace.runtime().value_of(&commands).unwrap().data() else {
        panic!("list")
    };
    assert_eq!(
        commands.len(),
        wes_language::vocabulary::commands::roots().len()
    );
    assert!(commands.contains(&Data::Text("node".into())));
    assert!(!commands.contains(&Data::Text("for".into())));
    assert_eq!(calls.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn selected_error_and_cancellation_inspection_preserve_identity_issues_and_data() {
    for port in [OutputPort::Error, OutputPort::Cancel] {
        let (mut workspace, calls) = workspace();
        let source = commit(&mut workspace, "catalog echo value:unused > source").unwrap();
        let error = wes_core::ErrorValue::new(
            wes_core::ErrorId::new("original").unwrap(),
            "EXAMPLE",
            "problem",
            vec![wes_core::ValidationIssue {
                path: "/id".into(),
                code: "invalid".into(),
                message: "not valid".into(),
            }],
            Some(wes_core::ErrorId::new("earlier").unwrap()),
        )
        .unwrap();
        let run = ticket(workspace.start(Duration::ZERO)).run;
        workspace.enter(&run);
        let outcome = if port == OutputPort::Error {
            Outcome::Failed(error.clone())
        } else {
            Outcome::Cancelled(error.clone())
        };
        workspace.complete(&run, outcome, Duration::ZERO);
        let inspected = commit(
            &mut workspace,
            &format!(":read $source::{} > inspected", port.selector()),
        )
        .unwrap();
        let next = ticket(workspace.start(Duration::ZERO));
        execute(&mut workspace, next).await;
        let OutputState::Available(original) = workspace
            .runtime()
            .output(&OutputRef { node: source, port })
        else {
            panic!("selected value")
        };
        assert_eq!(workspace.runtime().value_of(&inspected), Some(&original));
        assert_eq!(calls.load(Ordering::SeqCst), 0);
    }
}

#[tokio::test]
async fn missing_capability_is_a_catchable_failure_but_missing_capture_never_reads_live_state() {
    let (mut workspace, calls) = workspace();
    let missing = commit(&mut workspace, ":inspect env:absent *> problem").unwrap();
    let handled = commit(&mut workspace, "catalog echo value:$problem > handled").unwrap();
    run_all(&mut workspace).await;
    assert_eq!(
        workspace.runtime().value_of(&handled),
        Some(&local_output(
            workspace.runtime().error_of(&missing).unwrap().to_value()
        ))
    );
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    let uncaptured = commit(&mut workspace, ":list providers > uncaptured").unwrap();
    let work = ticket(workspace.start(Duration::ZERO));
    workspace.enter(&work.run); // lower-level entry alone deliberately cannot capture query state
    finish(&mut workspace, work).await;
    assert!(
        workspace
            .runtime()
            .error_of(&uncaptured)
            .unwrap()
            .message()
            .contains("captured")
    );
    assert!(workspace.runtime().value_of(&uncaptured).is_none());
}

#[tokio::test]
async fn inspection_limits_are_explicit_failures_not_truncated_descriptions() {
    let (mut workspace, calls) = workspace();
    replace(&mut workspace, &"x".repeat(4 * 1024 * 1024 + 1));
    let large = commit(&mut workspace, ":inspect inventory items get > large").unwrap();
    run_all(&mut workspace).await;
    assert!(workspace.runtime().value_of(&large).is_none());
    assert!(
        workspace
            .runtime()
            .error_of(&large)
            .unwrap()
            .message()
            .contains("budget")
    );
    let mut deep = Shape::Unknown;
    for _ in 0..66 {
        deep = Shape::List(Box::new(deep));
    }
    workspace
        .register_provider(
            ProviderDescription::new(
                "deep",
                [Capability::new(["get"], deep, Safety::Safe)],
                vec![],
            )
            .unwrap(),
            Arc::new(Echo(calls.clone())),
        )
        .unwrap();
    let deep = commit(&mut workspace, ":inspect deep get > deep").unwrap();
    run_all(&mut workspace).await;
    assert!(
        workspace
            .runtime()
            .error_of(&deep)
            .unwrap()
            .message()
            .contains("budget")
    );
    assert_eq!(calls.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn obsolete_query_tickets_cannot_capture_and_cancelled_captured_work_has_no_value() {
    let (mut workspace, calls) = workspace();
    let node = commit(&mut workspace, ":list providers > providers").unwrap();
    let old = ticket(workspace.start(Duration::ZERO));
    let old_run = old.run.clone();
    workspace.cancel(&node, Duration::ZERO);
    let new = ticket(refresh(&mut workspace, &node));
    assert!(workspace.enter_ticket(old).unwrap().is_none());
    // The obsolete queued worker's eventual completion cannot revoke its replacement.
    workspace.complete(
        &old_run,
        Outcome::Failed(RuntimeCode::ExecutionFailed.error("did not enter", None)),
        Duration::ZERO,
    );
    let new = workspace.enter_ticket(new).unwrap().unwrap();
    let run = new.run.clone();
    let token = CancellationToken::new();
    token.cancel();
    let report = TaskExecutor::ephemeral().execute(new, token).await;
    let Outcome::Cancelled(error) = &report.outcome else {
        panic!("cancelled")
    };
    assert_eq!(error.code(), RuntimeCode::Cancelled.as_str());
    workspace.complete(&run, report.outcome, Duration::ZERO);
    assert!(workspace.runtime().value_of(&node).is_none());
    assert_eq!(calls.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn capability_filter_is_an_order_preserving_selection_of_unfiltered_rows() {
    let (mut workspace, calls) = workspace();
    replace(&mut workspace, "Items");
    let all = commit(&mut workspace, ":list capabilities > all_caps").unwrap();
    let selected: Vec<_> = ["catalog", "inventory", "absent"]
        .into_iter()
        .map(|name| {
            (
                name,
                commit(
                    &mut workspace,
                    &format!(":list capabilities provider:{name}"),
                )
                .unwrap(),
            )
        })
        .collect();
    run_all(&mut workspace).await;
    let Data::List(rows) = workspace.runtime().value_of(&all).unwrap().data() else {
        panic!();
    };
    for (name, node) in selected {
        let expected: Vec<_> = rows
            .iter()
            .filter(|row| {
                let Data::Record(fields) = row else {
                    panic!();
                };
                fields["provider"] == Data::Text(name.into())
            })
            .cloned()
            .collect();
        assert_eq!(
            workspace.runtime().value_of(&node).unwrap().data(),
            &Data::List(expected)
        );
    }
    assert_eq!(calls.load(Ordering::SeqCst), 0);
}

#[test]
fn unsupported_registries_never_invoke_providers() {
    let (workspace, calls) = workspace();
    for source in [":list widgets", ":list renderers"] {
        assert!(workspace.prepare(&statement(source)).is_err());
    }
    assert_eq!(calls.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn bounded_read_projects_content_without_copying_unselected_fields_and_inspect_is_metadata() {
    let (mut workspace, calls) = workspace();
    let root = commit(&mut workspace, "catalog echo value:unused > content").unwrap();
    let run = ticket(workspace.start(Duration::ZERO)).run;
    workspace.enter(&run);
    let original = Value::new(
        Shape::Unknown,
        Data::Record(
            [
                (
                    "rows".into(),
                    Data::List((0..1000).map(Data::Int).collect()),
                ),
                ("large".into(), Data::Text("x".repeat(100_000).into())),
            ]
            .into(),
        ),
        Provenance::default().with_fact("origin", "synthetic"),
    )
    .unwrap();
    workspace.complete(&run, Outcome::Produced(original), Duration::ZERO);
    let page = commit(
        &mut workspace,
        ":read $content select:rows offset:10 limit:2 > page",
    )
    .unwrap();
    let full = commit(&mut workspace, ":read $content > too_large").unwrap();
    let invalid = commit(&mut workspace, ":read $content select:rows..id > invalid").unwrap();
    let metadata = commit(&mut workspace, ":inspect value:$content > metadata").unwrap();
    run_all(&mut workspace).await;
    let page_value = workspace.runtime().value_of(&page).unwrap();
    assert_eq!(
        fields(page_value)["data"],
        Data::List(vec![Data::Int(10), Data::Int(11)])
    );
    assert_eq!(fields(page_value)["total"], Data::Int(1000));
    assert_eq!(fields(page_value)["hasMore"], Data::Bool(true));
    assert_eq!(
        page_value.provenance().facts().get("origin"),
        Some(&"synthetic".to_string())
    );
    assert!(
        workspace
            .runtime()
            .error_of(&full)
            .unwrap()
            .message()
            .contains("budget")
    );
    assert!(
        workspace
            .runtime()
            .error_of(&invalid)
            .unwrap()
            .message()
            .contains("nonempty")
    );
    assert_eq!(
        fields(workspace.runtime().value_of(&metadata).unwrap())["node"],
        Data::Text(root.to_string().into())
    );
    assert!(
        workspace
            .prepare(&statement(":read $content run:wrong"))
            .is_err()
    );
    assert_eq!(calls.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn stale_reads_use_names_and_refresh_hints_without_reexecuting_any_provider() {
    let (mut workspace, calls) = workspace();
    commit(&mut workspace, "catalog echo value:one > producer").unwrap();
    commit(&mut workspace, "catalog echo value:$producer > consumer").unwrap();
    run_all(&mut workspace).await;
    let before = calls.load(Ordering::SeqCst);
    let Preparation::Meta(meta) = workspace
        .prepare(&statement(":change $producer value:two"))
        .unwrap()
    else {
        panic!("control")
    };
    let prepared = workspace.prepare_control(meta).unwrap();
    workspace.apply_control(prepared, Duration::ZERO).unwrap();
    let read = commit(&mut workspace, ":read $consumer").unwrap();
    run_all(&mut workspace).await;
    let error = workspace.runtime().error_of(&read).unwrap();
    assert!(error.message().contains("$consumer is stale"), "{error:?}");
    assert!(
        error
            .message()
            .contains(":refresh $producer scope:downstream"),
        "{error:?}"
    );
    assert!(!error.message().contains("Missing available output"));
    assert_eq!(calls.load(Ordering::SeqCst), before);
}

#[tokio::test]
async fn adapter_discovery_uses_existing_pure_typed_definitions_without_invoking_them() {
    let (mut workspace, calls) = workspace();
    for source in [
        ":def map(input:Int) -> Int as :calc { return input * 2; }",
        ":def constant(seed:Int) -> Int as :calc { return seed; }",
        ":def effect(input:Int) -> Int as :calc { call('catalog', ['echo'], {value:'synthetic'}); return input; }",
        ":def object as catalog echo value:?value",
    ] {
        let prepared = workspace.prepare(&statement(source)).unwrap();
        let Preparation::Change(change) = prepared else {
            panic!("definition")
        };
        workspace.commit(change).unwrap();
    }
    let listed = commit(&mut workspace, ":list adapters").unwrap();
    let inspected = commit(&mut workspace, ":inspect template:map").unwrap();
    run_all(&mut workspace).await;
    assert_eq!(
        workspace.runtime().value_of(&listed).unwrap().data(),
        &Data::List(vec![Data::Text("map".into())])
    );
    let description = fields(workspace.runtime().value_of(&inspected).unwrap());
    assert_eq!(description["conversionEligible"], Data::Bool(true));
    assert_eq!(description["outputType"], Data::Text("Int".into()));
    assert_eq!(
        description["revision"],
        Data::Text(workspace.templates().snapshot()["map"].revision().into())
    );
    assert_eq!(calls.load(Ordering::SeqCst), 0);
}
