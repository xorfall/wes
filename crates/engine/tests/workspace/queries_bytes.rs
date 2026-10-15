use super::*;
use wes_core::flow::FlowPolicy;

fn text(s: &str) -> Value {
    Value::new(
        Shape::Primitive(Primitive::Text),
        Data::Text(s.into()),
        Provenance::default(),
    )
    .unwrap()
}

fn seed(workspace: &mut Workspace, value: Value) -> NodeId {
    let source = commit(workspace, "catalog echo value:unused > byte_source").unwrap();
    let run = ticket(workspace.start(Duration::ZERO)).run;
    workspace.enter(&run);
    workspace.complete(&run, Outcome::Produced(value), Duration::ZERO);
    source
}

#[tokio::test]
async fn text_byte_coordinates_include_partial_utf8_and_capture_exact_source_identity() {
    let (mut workspace, calls) = workspace();
    let source = seed(&mut workspace, text("Aé😀Z"));
    let source_run = workspace.runtime().value_run(&source).unwrap().to_string();
    let revision = workspace.runtime().display_revision(&source);
    let read = commit(
        &mut workspace,
        ":read bytes:$byte_source from:\"2\" limit:3 > slice",
    )
    .unwrap();
    let typing = workspace.data_typing(&read).unwrap();
    assert_eq!(
        typing.shape.field("data"),
        Some(&Shape::Primitive(Primitive::Bytes))
    );
    assert_eq!(
        typing.shape.field("from"),
        Some(&Shape::Primitive(Primitive::Text))
    );
    assert!(
        workspace
            .runtime()
            .graph()
            .node(&read)
            .unwrap()
            .dependencies()
            .is_empty()
    );
    run_all(&mut workspace).await;
    let value = workspace.runtime().value_of(&read).unwrap();
    let f = fields(value);
    assert_eq!(f["source"], Data::Text(source.to_string().into()));
    assert_eq!(
        f["run"],
        Data::Option(Some(Box::new(Data::Text(source_run.into()))))
    );
    assert_eq!(f["revision"], Data::Text(revision.to_string().into()));
    assert_eq!(
        f["data"],
        Data::Bytes("Aé😀Z".as_bytes()[2..5].to_vec().into())
    );
    assert_eq!(f["from"], Data::Text("2".into()));
    assert_eq!(f["next"], Data::Text("5".into()));
    assert_eq!(f["extent"], Data::Text("8".into()));
    assert_eq!(f["text"], Data::Bool(true));
    assert_eq!(f["extentExhausted"], Data::Bool(false));
    assert!(value.metadata().is_some());
    assert_eq!(calls.load(Ordering::SeqCst), 0);

    let eof = commit(
        &mut workspace,
        ":read bytes:$byte_source from:\"8\" limit:65536",
    )
    .unwrap();
    let past = commit(
        &mut workspace,
        ":read bytes:$byte_source from:\"18446744073709551615\" limit:1",
    )
    .unwrap();
    run_all(&mut workspace).await;
    let eof = fields(workspace.runtime().value_of(&eof).unwrap());
    assert_eq!(eof["data"], Data::Bytes(vec![].into()));
    assert_eq!(eof["extentExhausted"], Data::Bool(true));
    assert!(
        workspace
            .runtime()
            .error_of(&past)
            .unwrap()
            .message()
            .contains("extent")
    );
    assert_eq!(calls.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn selected_private_bytes_and_read_failures_keep_root_policy_without_descriptors() {
    let (mut workspace, calls) = workspace();
    let policy = FlowPolicy::default()
        .private()
        .from_origin("synthetic-private-source");
    seed(
        &mut workspace,
        Value::new(
            Shape::Unknown,
            Data::Record(IndexMap::from([
                ("raw".into(), Data::Bytes(vec![0, 255, 1, 2].into())),
                ("other".into(), Data::Text("synthetic-private-field".into())),
            ])),
            Provenance::default().with_policy(&policy),
        )
        .unwrap(),
    );
    let read = commit(
        &mut workspace,
        ":read bytes:$byte_source from:\"1\" limit:2 select:raw",
    )
    .unwrap();
    let wrong = commit(
        &mut workspace,
        ":read bytes:$byte_source from:\"0\" limit:2 select:absent",
    )
    .unwrap();
    run_all(&mut workspace).await;
    let value = workspace.runtime().value_of(&read).unwrap();
    assert!(value.provenance().policy().is_private());
    assert!(
        value
            .provenance()
            .policy()
            .origins()
            .contains("synthetic-private-source")
    );
    assert_eq!(fields(value)["data"], Data::Bytes(vec![255, 1].into()));
    assert_eq!(fields(value)["handle"], Data::Option(None));
    assert_eq!(fields(value)["digest"], Data::Option(None));
    assert_eq!(
        fields(value)["selection"],
        Data::List(vec![Data::Text("raw".into())])
    );
    assert!(
        workspace
            .runtime()
            .error_of(&wrong)
            .unwrap()
            .policy()
            .is_private()
    );
    assert_eq!(calls.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn byte_read_validation_refuses_noncanonical_or_unbounded_inputs_before_admission() {
    let (mut workspace, calls) = workspace();
    seed(&mut workspace, text("synthetic"));
    for options in [
        "limit:1",
        "from:\"0\"",
        "from:\"01\" limit:1",
        "from:\"+1\" limit:1",
        "from:\"-1\" limit:1",
        "from:\"18446744073709551616\" limit:1",
        "from:\"0\" limit:0",
        "from:\"0\" limit:65537",
        "from:\"0\" limit:1 select:raw..data",
        "from:\"0\" limit:1 offset:0",
        "from:\"0\" limit:1 run:old",
    ] {
        assert!(
            workspace
                .prepare(&statement(&format!(":read bytes:$byte_source {options}")))
                .is_err(),
            "{options}"
        );
    }
    assert_eq!(workspace.runtime().graph().len(), 1);
    assert_eq!(calls.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn cancelled_byte_read_publishes_no_slice() {
    let (mut workspace, calls) = workspace();
    seed(&mut workspace, text("old"));
    let read = commit(
        &mut workspace,
        ":read bytes:$byte_source from:\"0\" limit:3",
    )
    .unwrap();
    let captured = ticket(workspace.start(Duration::ZERO));
    let captured = workspace.enter_ticket(captured).unwrap().unwrap();
    let token = CancellationToken::new();
    token.cancel();
    let report = TaskExecutor::ephemeral().execute(captured, token).await;
    assert!(matches!(report.outcome, Outcome::Cancelled(_)));
    assert!(workspace.runtime().value_of(&read).is_none());
    assert_eq!(calls.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn entered_byte_read_keeps_the_original_run_when_source_is_explicitly_refreshed() {
    let (mut workspace, calls) = workspace();
    let source = seed(&mut workspace, text("old"));
    let original_run = workspace.runtime().value_run(&source).unwrap().to_string();
    let read = commit(
        &mut workspace,
        ":read bytes:$byte_source from:\"0\" limit:3",
    )
    .unwrap();
    let captured = ticket(workspace.start(Duration::ZERO));
    let captured = workspace.enter_ticket(captured).unwrap().unwrap();
    let new = ticket(refresh(&mut workspace, &source));
    workspace.enter(&new.run);
    workspace.complete(&new.run, Outcome::Produced(text("new")), Duration::ZERO);
    finish(&mut workspace, captured).await;
    let old = fields(workspace.runtime().value_of(&read).unwrap());
    assert_eq!(old["data"], Data::Bytes(b"old".to_vec().into()));
    assert_eq!(
        old["run"],
        Data::Option(Some(Box::new(Data::Text(original_run.into()))))
    );
    let fresh = commit(
        &mut workspace,
        ":read bytes:$byte_source from:\"0\" limit:3",
    )
    .unwrap();
    run_all(&mut workspace).await;
    let fresh = fields(workspace.runtime().value_of(&fresh).unwrap());
    assert_eq!(fresh["data"], Data::Bytes(b"new".to_vec().into()));
    assert_eq!(
        fresh["run"],
        Data::Option(Some(Box::new(Data::Text(new.run.id().to_string().into()))))
    );
    assert_eq!(calls.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn byte_query_on_pending_source_has_no_dependency_or_implicit_producer_run() {
    let (mut workspace, calls) = workspace();
    let source = commit(&mut workspace, "catalog echo value:pending > byte_source").unwrap();
    let read = commit(
        &mut workspace,
        ":read bytes:$byte_source from:\"0\" limit:1",
    )
    .unwrap();
    let work = workspace
        .start(Duration::ZERO)
        .into_iter()
        .find_map(|effect| match effect {
            Effect::Spawn(ticket) if ticket.run.node() == &read => Some(ticket),
            _ => None,
        })
        .unwrap();
    assert!(
        workspace
            .runtime()
            .graph()
            .node(&read)
            .unwrap()
            .dependencies()
            .is_empty()
    );
    execute(&mut workspace, work).await;
    assert!(
        workspace
            .runtime()
            .error_of(&read)
            .unwrap()
            .message()
            .contains("did not run its producer")
    );
    assert!(workspace.runtime().value_of(&source).is_none());
    assert_eq!(calls.load(Ordering::SeqCst), 0);
}
