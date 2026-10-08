use super::*;
use wes_engine::tasks::BoundTask;

fn load(workspace: &mut Workspace) {
    let package = workspace.prepare_type_package(
        "types:\n  Positive: {base: Int, min: 1}\n  Order:\n    base: Record\n    fields:\n      quantity: Positive\n", Span::at(0),
    ).unwrap();
    workspace.commit(package).unwrap();
}

async fn execute(workspace: &mut Workspace, work: RunTicket<BoundTask>) -> Vec<Effect<BoundTask>> {
    assert!(workspace.enter(&work.run));
    let run = work.run.clone();
    let report = TaskExecutor::ephemeral()
        .execute(work, CancellationToken::new())
        .await;
    assert!(report.notices.is_empty());
    workspace.complete(&run, report.outcome, Duration::from_secs(1))
}

#[tokio::test]
async fn checked_literal_is_a_normal_node_with_predicted_and_actual_typing() {
    let (mut workspace, calls) = workspace();
    load(&mut workspace);
    let checked = commit(
        &mut workspace,
        ":type check \"4\" as:Positive > checked *> failure",
    )
    .unwrap();
    assert_eq!(
        workspace.data_typing(&checked).unwrap().shape,
        Shape::Primitive(Primitive::Int)
    );
    assert!(workspace.runtime().value_of(&checked).is_none());
    let work = ticket(workspace.start(Duration::ZERO));
    assert!(matches!(work.payload, BoundTask::TypeCheck(_)));
    assert!(work.payload.traits().repeatable);
    execute(&mut workspace, work).await;
    let value = workspace.runtime().value_of(&checked).unwrap();
    assert_eq!(value.with_metadata(None), int(4));
    assert_eq!(
        serde_json::to_value(value.metadata().unwrap()).unwrap()["contract"]["name"],
        "Positive"
    );
    assert_eq!(
        workspace.runtime().graph().node(&checked).unwrap().state(),
        NodeState::Ready
    );
    assert_eq!(calls.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn invalid_literal_routes_to_the_error_output_instead_of_being_rejected_at_declaration() {
    let (mut workspace, calls) = workspace();
    load(&mut workspace);
    let checked = commit(
        &mut workspace,
        ":type check \"0\" as:Positive > checked *> failure",
    )
    .unwrap();
    let success = commit(&mut workspace, "catalog count amount:$checked > success").unwrap();
    let handler = commit(&mut workspace, "catalog echo value:$failure > details").unwrap();
    let work = ticket(workspace.start(Duration::ZERO));
    let effects = execute(&mut workspace, work).await;
    let error = workspace.runtime().error_of(&checked).unwrap().clone();
    assert_eq!(error.code(), "TYP005");
    assert_eq!(error.issues().len(), 1);
    assert_eq!(error.issues()[0].path, "");
    assert!(workspace.runtime().value_of(&checked).is_none());
    assert_eq!(
        workspace.runtime().graph().node(&success).unwrap().state(),
        NodeState::Failed
    );
    assert_eq!(
        workspace.runtime().error_of(&success).unwrap().code(),
        "RUN004"
    );
    let work = ticket(effects);
    assert_eq!(work.run.node(), &handler);
    execute(&mut workspace, work).await;
    assert_eq!(
        workspace.runtime().value_of(&handler),
        Some(&local_output(error.to_value()))
    );
    assert_eq!(calls.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn a_text_reference_is_never_converted_to_an_integer() {
    let (mut workspace, calls) = workspace();
    let source = commit(&mut workspace, "catalog echo value:4 > source").unwrap();
    let checked = commit(&mut workspace, ":type check $source as:Int > checked").unwrap();
    let work = ticket(workspace.start(Duration::ZERO));
    let effects = execute(&mut workspace, work).await;
    let work = ticket(effects);
    execute(&mut workspace, work).await;
    assert!(
        matches!(workspace.runtime().value_of(&source).unwrap().data(), Data::Text(text) if text.as_ref() == "4")
    );
    assert_eq!(
        workspace.runtime().error_of(&checked).unwrap().code(),
        "TYP005"
    );
    assert_eq!(calls.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn checked_reference_preserves_data_provenance_and_the_original_producer() {
    let (mut workspace, _) = workspace();
    let source = commit(&mut workspace, "catalog echo value:unused > source").unwrap();
    let run = ticket(workspace.start(Duration::ZERO)).run;
    assert!(workspace.enter(&run));
    let original = int(9).with_provenance(
        Provenance::default()
            .with_fact("origin", "example")
            .cautioned(["manual".into()]),
    );
    workspace.complete(
        &run,
        Outcome::Produced(original.clone()),
        Duration::from_secs(1),
    );
    let checked = commit(&mut workspace, ":type check $source as:Unknown > checked").unwrap();
    let work = ticket(workspace.start(Duration::from_secs(2)));
    execute(&mut workspace, work).await;
    let value = workspace.runtime().value_of(&checked).unwrap();
    assert_eq!(value.shape(), &Shape::Unknown);
    assert_eq!(value.data(), original.data());
    assert!(std::ptr::eq(value.data(), original.data()));
    assert_eq!(value.provenance(), original.provenance());
    assert_eq!(workspace.runtime().value_of(&source), Some(&original));
    assert_eq!(
        workspace.data_typing(&source).unwrap().shape,
        Shape::Primitive(Primitive::Int)
    );
}

#[tokio::test]
async fn nested_failures_keep_json_pointer_paths_and_do_not_echo_rejected_values() {
    let (mut workspace, _) = workspace();
    load(&mut workspace);
    let source = commit(&mut workspace, "catalog echo value:unused > source").unwrap();
    let run = ticket(workspace.start(Duration::ZERO)).run;
    assert_eq!(run.node(), &source);
    assert!(workspace.enter(&run));
    let data = Data::List(vec![Data::Record(indexmap::IndexMap::from([(
        "quantity".into(),
        Data::Text("private-payload".into()),
    )]))]);
    let value = Value::new(Shape::Unknown, data, Provenance::default()).unwrap();
    workspace.complete(&run, Outcome::Produced(value), Duration::from_secs(1));
    let checked = commit(&mut workspace, ":type check $source as:\"List<Order>\"").unwrap();
    let work = ticket(workspace.start(Duration::from_secs(2)));
    execute(&mut workspace, work).await;
    let error = workspace.runtime().error_of(&checked).unwrap();
    assert_eq!(error.code(), "TYP005");
    assert_eq!(error.issues()[0].path, "/0/quantity");
    assert!(!format!("{error:?}").contains("private-payload"));
}

#[test]
fn malformed_type_selection_fails_preparation_without_allocating_a_node() {
    let (mut workspace, _) = workspace();
    for text in [
        ":type check value:1",
        ":type check Int as:Text value:1",
        ":type check \"1\" as:Missing",
    ] {
        assert!(workspace.prepare(&statement(text)).is_err(), "{text}");
        assert!(workspace.runtime().graph().is_empty());
    }
    let checked = commit(
        &mut workspace,
        ":type check \"sensitive-input\" as:Text > checked",
    )
    .unwrap();
    assert!(
        !format!(
            "{:?}",
            workspace
                .runtime()
                .graph()
                .node(&checked)
                .unwrap()
                .payload()
        )
        .contains("sensitive-input")
    );
    match workspace.prepare(&statement(":change $checked value:other")) {
        Err(WorkspaceError::Rejected { .. }) => {}
        Ok(Preparation::Meta(change)) => assert!(workspace.prepare_control(change).is_err()),
        other => panic!("provider-only change must be rejected: {other:?}"),
    }
}

#[tokio::test]
async fn cancellation_is_distinct_from_validation_failure_and_late_success_cannot_publish() {
    let (mut workspace, _) = workspace();
    let checked = commit(&mut workspace, ":type check \"4\" as:Int").unwrap();
    let work = ticket(workspace.start(Duration::ZERO));
    assert!(workspace.enter(&work.run));
    let token = CancellationToken::new();
    token.cancel();
    let report = TaskExecutor::ephemeral().execute(work.clone(), token).await;
    assert!(matches!(report.outcome, Outcome::Cancelled(_)));
    workspace.complete(&work.run, report.outcome, Duration::from_secs(1));
    assert_eq!(
        workspace.runtime().graph().node(&checked).unwrap().state(),
        NodeState::Cancelled
    );
    assert_eq!(
        workspace.runtime().error_of(&checked).unwrap().code(),
        "RUN003"
    );
    workspace.complete(
        &work.run,
        Outcome::Produced(int(88)),
        Duration::from_secs(2),
    );
    assert!(workspace.runtime().value_of(&checked).is_none());
    assert!(workspace.runtime().actual_typing(&checked).is_none());
}

#[tokio::test]
async fn type_checks_can_be_staged_with_the_contract_and_provider_consumer_in_one_draft() {
    let (mut workspace, calls) = workspace();
    let mut draft = workspace.draft().unwrap();
    draft
        .stage(
            draft
                .prepare_type_package("types: {Positive: {base: Int, min: 1}}", Span::at(0))
                .unwrap(),
        )
        .unwrap();
    for text in [
        ":type check \"7\" as:Positive > checked",
        "catalog count amount:$checked > consumed",
    ] {
        let Preparation::Change(change) = draft.prepare(&statement(text)).unwrap() else {
            panic!()
        };
        draft.stage(change).unwrap();
    }
    workspace
        .commit_batch(draft.finish(), std::time::Duration::ZERO)
        .unwrap();
    let work = ticket(workspace.start(Duration::ZERO));
    let effects = execute(&mut workspace, work).await;
    execute(&mut workspace, ticket(effects)).await;
    let consumed = workspace.resolve("consumed").unwrap().node;
    let value = workspace.runtime().value_of(&consumed).unwrap();
    assert_eq!(value.with_metadata(None), local_output(int(7)));
    let checked = workspace.resolve("checked").unwrap().node;
    assert_eq!(
        value.metadata(),
        workspace.runtime().value_of(&checked).unwrap().metadata()
    );
    assert_eq!(calls.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn first_class_error_can_be_checked_by_another_local_node() {
    let (mut workspace, calls) = workspace();
    let failed = commit(&mut workspace, ":type check \"bad\" as:Int *> problem").unwrap();
    let observed = commit(&mut workspace, ":type check $problem as:Error > observed").unwrap();
    let work = ticket(workspace.start(Duration::ZERO));
    let effects = execute(&mut workspace, work).await;
    let expected = workspace.runtime().error_of(&failed).unwrap().to_value();
    execute(&mut workspace, ticket(effects)).await;
    let value = workspace.runtime().value_of(&observed).unwrap();
    assert_eq!(value.with_metadata(None), expected);
    assert_eq!(
        serde_json::to_value(value.metadata().unwrap()).unwrap()["contract"]["name"],
        "Error"
    );
    assert_eq!(calls.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn structured_success_shares_data_and_missing_ticket_input_fails_closed() {
    let (mut workspace, _) = workspace();
    let source = commit(&mut workspace, "catalog echo value:unused > source").unwrap();
    let work = ticket(workspace.start(Duration::ZERO));
    workspace.enter(&work.run);
    let original = Value::new(
        Shape::Unknown,
        Data::List(vec![Data::Text("books".into())]),
        Provenance::default().with_fact("source", "fixture"),
    )
    .unwrap();
    workspace.complete(
        &work.run,
        Outcome::Produced(original.clone()),
        Duration::from_secs(1),
    );
    let checked = commit(&mut workspace, ":type check $source as:\"List<Text>\"").unwrap();
    let work = ticket(workspace.start(Duration::from_secs(2)));
    execute(&mut workspace, work).await;
    let value = workspace.runtime().value_of(&checked).unwrap();
    assert!(std::ptr::eq(value.data(), original.data()));
    assert_eq!(value.provenance(), original.provenance());
    assert_eq!(
        value.shape(),
        &Shape::List(Box::new(Shape::Primitive(Primitive::Text)))
    );
    assert_eq!(
        workspace.runtime().value_of(&source).unwrap().shape(),
        &Shape::Unknown
    );
    let missing = commit(&mut workspace, ":type check $source as:Unknown").unwrap();
    let mut work = ticket(workspace.start(Duration::from_secs(3)));
    work.inputs.clear();
    execute(&mut workspace, work).await;
    assert_eq!(
        workspace.runtime().error_of(&missing).unwrap().code(),
        "RUN004"
    );
}

#[tokio::test]
async fn declared_option_rejects_an_unwrapped_text_value() {
    let (mut workspace, _) = workspace();
    let checked = commit(&mut workspace, ":type check \"x\" as:\"Option<Text>\"").unwrap();
    let work = ticket(workspace.start(Duration::ZERO));
    execute(&mut workspace, work).await;
    let error = workspace.runtime().error_of(&checked).unwrap();
    assert_eq!(error.code(), "TYP005");
    assert!(error.issues().iter().any(|issue| issue.code == "TYP005"));
}
