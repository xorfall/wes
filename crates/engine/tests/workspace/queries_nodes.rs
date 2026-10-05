use super::*;

/// The structured type an inspection carries for a primitive: `{kind: primitive, name}`.
fn primitive_type(name: &str) -> Data {
    Data::Record(indexmap::IndexMap::from([
        ("kind".to_string(), Data::Text("primitive".into())),
        ("name".to_string(), Data::Text(name.into())),
    ]))
}

fn unknown_type() -> Data {
    Data::Record(indexmap::IndexMap::from([(
        "kind".to_string(),
        Data::Text("unknown".into()),
    )]))
}

/// The name a structured record type carries.
fn type_name(data: &Data) -> String {
    let Data::Record(fields) = data else {
        panic!("a structured type")
    };
    assert_eq!(fields["kind"], Data::Text("record".into()));
    let Data::Text(name) = &fields["name"] else {
        panic!("a record name")
    };
    name.to_string()
}

#[tokio::test]
async fn node_inspection_uses_actual_typing_and_retains_the_entry_snapshot_after_alias_changes() {
    let (mut workspace, calls) = workspace();
    let source = commit(
        &mut workspace,
        "catalog echo value:private-argument > source",
    )
    .unwrap();
    let run = ticket(workspace.start(Duration::ZERO)).run;
    workspace.enter(&run);
    let actual = int(7).with_provenance(Provenance::default().with_fact("origin", "example"));
    workspace.complete(&run, Outcome::Produced(actual), Duration::ZERO);
    commit(&mut workspace, "$source > before");
    let inspected = commit(&mut workspace, ":inspect $source > inspected").unwrap();
    let queued = ticket(workspace.start(Duration::ZERO));
    let entered = workspace.enter_ticket(queued).unwrap().unwrap();
    commit(&mut workspace, "$source > after");
    finish(&mut workspace, entered).await;
    let value = workspace.runtime().value_of(&inspected).unwrap();
    let description = fields(value);
    assert_eq!(description["id"], Data::Text(source.to_string().into()));
    assert_eq!(
        description["names"],
        Data::List(vec![
            Data::Text("source".into()),
            Data::Text("before".into())
        ])
    );
    assert_eq!(description["type"], primitive_type("INT"));
    assert_eq!(description["state"], Data::Text("READY".into()));
    assert_eq!(description["task"], Data::Text("catalog echo".into()));
    assert_eq!(
        description["provenance"],
        Data::Record([("origin".into(), Data::Text("example".into()))].into())
    );
    assert!(!format!("{value:?}").contains("private-argument"));
    let next = ticket(refresh(&mut workspace, &inspected));
    execute(&mut workspace, next).await;
    let Data::List(names) = &fields(workspace.runtime().value_of(&inspected).unwrap())["names"]
    else {
        panic!("names")
    };
    assert_eq!(names.len(), 3);
    assert_eq!(calls.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn names_show_selected_channel_types_and_nodes_preserve_dependency_order_and_failure_text() {
    let (mut workspace, calls) = workspace();
    let root = commit(&mut workspace, "catalog count amount:7 > root *> failure").unwrap();
    commit(&mut workspace, "$root::cancel > cancellation");
    let child = commit(&mut workspace, "catalog echo value:$root > child").unwrap();
    let run = ticket(workspace.start(Duration::ZERO)).run;
    workspace.enter(&run);
    workspace.complete(
        &run,
        Outcome::Failed(RuntimeCode::ExecutionFailed.error("synthetic failure", None)),
        Duration::ZERO,
    );
    let names = commit(&mut workspace, ":list names > names").unwrap();
    let nodes = commit(&mut workspace, ":list nodes > nodes").unwrap();
    run_all(&mut workspace).await;
    let Data::List(rows) = workspace.runtime().value_of(&names).unwrap().data() else {
        panic!("names")
    };
    let row = |name: &str| -> &indexmap::IndexMap<String, Data> {
        rows.iter()
            .find_map(|row| match row {
                Data::Record(fields) if fields["name"] == Data::Text(name.into()) => Some(fields),
                _ => None,
            })
            .unwrap()
    };
    assert_eq!(row("$root")["type"], primitive_type("INT")); // retained prediction, no successful result
    assert_eq!(type_name(&row("$failure")["type"]), "Error");
    assert_eq!(row("$failure")["output"], Data::Text("error".into()));
    assert_eq!(type_name(&row("$cancellation")["type"]), "Cancellation");
    assert_eq!(row("$cancellation")["output"], Data::Text("cancel".into()));
    let Data::List(rows) = workspace.runtime().value_of(&nodes).unwrap().data() else {
        panic!("nodes")
    };
    let row = |node: &NodeId| -> &indexmap::IndexMap<String, Data> {
        rows.iter()
            .find_map(|row| match row {
                Data::Record(fields) if fields["id"] == Data::Text(node.to_string().into()) => {
                    Some(fields)
                }
                _ => None,
            })
            .unwrap()
    };
    assert_eq!(
        row(&root)["failure"],
        Data::Text("synthetic failure".into())
    );
    assert_eq!(
        row(&root)["names"],
        Data::List(
            ["root", "failure", "cancellation"]
                .into_iter()
                .map(|name| Data::Text(name.into()))
                .collect()
        )
    );
    assert_eq!(
        row(&child)["dependsOn"],
        Data::List(vec![Data::Text(root.to_string().into())])
    );
    assert_eq!(row(&nodes)["task"], Data::Text(":list".into()));
    assert_eq!(row(&nodes)["state"], Data::Text("RUNNING".into()));
    assert_eq!(calls.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn node_inspection_does_not_wait_for_its_data_dependency_and_large_failure_text_fails_without_partial_rows()
 {
    let (mut workspace, calls) = workspace();
    let root = commit(&mut workspace, "catalog echo value:hello > root").unwrap();
    let inspected = commit(&mut workspace, ":inspect $root > inspected").unwrap();
    assert_eq!(
        workspace
            .runtime()
            .graph()
            .node(&inspected)
            .unwrap()
            .state(),
        NodeState::Pending
    );
    let mut spawned: Vec<_> = workspace
        .start(Duration::ZERO)
        .into_iter()
        .filter_map(|e| match e {
            Effect::Spawn(t) => Some(t),
            _ => None,
        })
        .collect();
    assert_eq!(spawned.len(), 2);
    let index = spawned
        .iter()
        .position(|t| t.run.node() == &inspected)
        .unwrap();
    execute(&mut workspace, spawned.remove(index)).await;
    assert_eq!(
        fields(workspace.runtime().value_of(&inspected).unwrap())["state"],
        Data::Text("RUNNING".into())
    );
    execute(&mut workspace, spawned.pop().unwrap()).await;
    assert_eq!(
        fields(workspace.runtime().value_of(&inspected).unwrap())["type"],
        unknown_type() // the echo's explicitly untyped value is not inferred from its payload
    );
    let root_work = ticket(refresh(&mut workspace, &root));
    workspace.enter(&root_work.run);
    workspace.complete(
        &root_work.run,
        Outcome::Failed(RuntimeCode::ExecutionFailed.error("x".repeat(4 * 1024 * 1024 + 1), None)),
        Duration::ZERO,
    );
    let listed = commit(&mut workspace, ":list nodes > listed").unwrap();
    run_all(&mut workspace).await;
    assert!(workspace.runtime().value_of(&listed).is_none());
    assert!(
        workspace
            .runtime()
            .error_of(&listed)
            .unwrap()
            .message()
            .contains("budget")
    );
    assert_eq!(calls.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn names_snapshot_retains_entry_time_and_states_until_explicit_refresh() {
    let (mut workspace, _) = workspace();
    let root = commit(&mut workspace, "catalog echo value:hello > root").unwrap();
    let root_ticket = ticket(workspace.start(Duration::ZERO));
    let root_ticket = workspace.enter_ticket(root_ticket).unwrap().unwrap();
    let listed = commit(&mut workspace, ":list names > names").unwrap();
    let query = ticket(workspace.start(Duration::ZERO));
    let query = workspace.enter_ticket(query).unwrap().unwrap();
    let before_execution = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap();
    finish(&mut workspace, root_ticket).await;
    assert_eq!(
        workspace.runtime().graph().node(&root).unwrap().state(),
        NodeState::Ready
    );
    finish(&mut workspace, query).await;
    let saved = workspace.runtime().value_of(&listed).unwrap().clone();
    assert_eq!(saved.provenance().fact("snapshot.kind"), Some("names"));
    let captured: wes_core::Timestamp = saved
        .provenance()
        .fact("snapshot.capturedAt")
        .unwrap()
        .parse()
        .unwrap();
    let parts = captured.parts();
    let before = wes_core::Timestamp::new(
        before_execution.as_secs() as i64,
        before_execution.subsec_nanos(),
    )
    .unwrap();
    assert!((parts.seconds(), parts.nanos()) <= (before.parts().seconds(), before.parts().nanos()));
    let state = |value: &Value| {
        let Data::List(rows) = value.data() else {
            panic!("names list")
        };
        rows.iter()
            .find_map(|row| match row {
                Data::Record(row) if row["name"] == Data::Text("$root".into()) => {
                    Some(row["state"].clone())
                }
                _ => None,
            })
            .unwrap()
    };
    assert_eq!(state(&saved), Data::Text("RUNNING".into()));
    let queued = ticket(refresh(&mut workspace, &listed));
    execute(&mut workspace, queued).await;
    let refreshed = workspace.runtime().value_of(&listed).unwrap();
    assert_eq!(state(refreshed), Data::Text("READY".into()));
    assert_ne!(
        saved.provenance().fact("snapshot.capturedAt"),
        refreshed.provenance().fact("snapshot.capturedAt")
    );
    assert_eq!(state(&saved), Data::Text("RUNNING".into()));
}
