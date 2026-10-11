use super::*;
use wes_engine::views::commands::CommandRequest;
fn request(
    workspace: &Workspace,
    node: &NodeId,
    template: &str,
    arguments: serde_json::Value,
) -> CommandRequest {
    let frame = workspace.view_frame(node).unwrap();
    let entry = &frame.instances[0];
    CommandRequest {
        root: node.as_str().into(),
        instance: entry.identity.to_string(),
        member: node.as_str().into(),
        revision: entry.revision,
        input_revision: entry.input_revision,
        template: template.into(),
        arguments: serde_json::from_value(arguments).unwrap(),
        environments: None,
    }
}
#[tokio::test]
async fn commands_are_typed_literals_and_prepare_never_dispatches_or_reserves_names() {
    let (mut workspace, calls) = workspace();
    commit(
        &mut workspace,
        ":def send(value:Text) as catalog echo value:?value",
    );
    let node = commit(&mut workspace, ":view create Metric > card").unwrap();
    super::queries::run_all(&mut workspace).await;
    let payload = "quoted\"\n:drop $card\n$secret";
    let draft = workspace
        .prepare_view_command(request(
            &workspace,
            &node,
            "send",
            serde_json::json!({"value":payload}),
        ))
        .unwrap();
    let parsed = statement(&draft.source);
    assert_eq!(parsed.annotations[0].name.text, "view");
    assert!(workspace.prepare(&parsed).is_ok());
    assert_eq!(calls.load(Ordering::SeqCst), 0);
    assert!(workspace.resolve("card").is_some());
    assert!(
        workspace
            .prepare_view_command(request(&workspace, &node, "catalog", serde_json::json!({})))
            .is_err()
    );
    assert!(
        workspace
            .prepare_view_command(request(
                &workspace,
                &node,
                "send",
                serde_json::json!({"value":null})
            ))
            .is_err()
    );
    assert!(
        workspace
            .prepare_view_command(request(
                &workspace,
                &node,
                "send",
                serde_json::json!({"other":"x"})
            ))
            .is_err()
    );
    assert_eq!(calls.load(Ordering::SeqCst), 0);
}
#[tokio::test]
async fn guard_rechecks_interaction_between_planning_and_atomic_commit() {
    let (mut workspace, calls) = workspace();
    commit(
        &mut workspace,
        ":def send(value:Text) as catalog echo value:?value",
    );
    let node = commit(&mut workspace, ":view create Timeline > chart").unwrap();
    super::queries::run_all(&mut workspace).await;
    let source = workspace
        .prepare_view_command(request(
            &workspace,
            &node,
            "send",
            serde_json::json!({"value":"reviewed"}),
        ))
        .unwrap()
        .source;
    let mut draft = workspace.draft().unwrap();
    let Preparation::Change(change) = draft.prepare(&statement(&source)).unwrap() else {
        panic!()
    };
    draft.stage(change).unwrap();
    let batch = draft.finish();
    let frame = workspace.view_frame(&node).unwrap();
    let entry = &frame.instances[0];
    let (owner, state) = workspace.view_interaction(&node, &entry.identity).unwrap();
    workspace
        .commit_view_interaction(
            &node,
            &entry.identity,
            wes_engine::views::InteractionEdit {
                owner: owner.id,
                identity: owner.identity.to_string(),
                definition_revision: owner.revision,
                revision: state.revision,
                fields: [
                    (
                        "viewport".into(),
                        Data::Record(
                            [
                                (
                                    "start".into(),
                                    Data::Instant("2030-01-01T00:00:00Z".parse().unwrap()),
                                ),
                                (
                                    "end".into(),
                                    Data::Instant("2030-01-01T01:00:00Z".parse().unwrap()),
                                ),
                            ]
                            .into(),
                        ),
                    ),
                    ("selection".into(), Data::Option(None)),
                    ("selectedItem".into(), Data::Option(None)),
                ]
                .into(),
                outputs: [
                    ("selection".into(), Data::Option(None)),
                    ("selectedItem".into(), Data::Option(None)),
                ]
                .into(),
                events: vec![],
            },
        )
        .unwrap();
    let before = workspace.runtime().graph().nodes().count();
    assert!(workspace.commit_batch(batch, Duration::ZERO).is_err());
    assert_eq!(workspace.runtime().graph().nodes().count(), before);
    assert!(workspace.prepare(&statement(&source)).is_err());
    assert_eq!(calls.load(Ordering::SeqCst), 0);
}
#[tokio::test]
async fn wrong_root_member_revision_and_untyped_templates_refuse() {
    let (mut workspace, _) = workspace();
    commit(&mut workspace, ":def untyped as catalog echo value:?value");
    commit(
        &mut workspace,
        ":def send(value:Text) as catalog echo value:?value",
    );
    let node = commit(&mut workspace, ":view create Metric > card").unwrap();
    super::queries::run_all(&mut workspace).await;
    let base = request(&workspace, &node, "send", serde_json::json!({"value":"x"}));
    for request in [
        CommandRequest {
            instance: "foreign".into(),
            ..base.clone()
        },
        CommandRequest {
            member: "foreign".into(),
            ..base.clone()
        },
        CommandRequest {
            revision: base.revision + 1,
            ..base.clone()
        },
        CommandRequest {
            input_revision: base.input_revision + 1,
            ..base.clone()
        },
        CommandRequest {
            template: "untyped".into(),
            ..base
        },
    ] {
        assert!(workspace.prepare_view_command(request).is_err());
    }
}
