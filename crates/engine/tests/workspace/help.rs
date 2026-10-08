use super::*;
use wes_engine::{graph::OutputRef, runtime::OutputState};

async fn run_all(workspace: &mut Workspace) {
    let mut effects = std::collections::VecDeque::from(workspace.start(Duration::ZERO));
    while let Some(effect) = effects.pop_front() {
        if let Effect::Spawn(ticket) = effect {
            let run = ticket.run.clone();
            assert!(workspace.enter(&run));
            let report = TaskExecutor::ephemeral()
                .execute(ticket, CancellationToken::new())
                .await;
            assert!(report.notices.is_empty());
            effects.extend(workspace.complete(&run, report.outcome, Duration::ZERO));
        }
    }
}

#[tokio::test]
async fn environment_help_explains_confidential_residence_without_execution() {
    let (mut workspace, calls) = workspace();
    let node = commit(&mut workspace, ":help env plan > guidance").unwrap();
    run_all(&mut workspace).await;
    let Data::Record(fields) = workspace.runtime().value_of(&node).unwrap().data() else {
        panic!("help record")
    };
    let Data::Text(syntax) = &fields["syntax"] else {
        panic!("syntax guidance")
    };
    for phrase in [
        "private is memory-only",
        "confidential-temporary",
        "explicit Keep only",
        "strictest residence wins",
        "--storage-key-file",
        "do not grant terminal, MCP or file export",
    ] {
        assert!(syntax.contains(phrase), "{phrase}: {syntax}");
    }
    assert_eq!(calls.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn help_values_compose_in_a_draft_and_success_closes_the_error_branch() {
    let (mut workspace, calls) = workspace();
    let mut draft = workspace.draft().unwrap();
    for source in [
        ":help > commands",
        "catalog echo value:$commands > copied",
        ":help node *> problem",
        "catalog echo value:$problem > handled",
    ] {
        let Preparation::Change(change) = draft.prepare(&statement(source)).unwrap() else {
            panic!("declaration")
        };
        draft.stage(change).unwrap();
    }
    assert!(workspace.runtime().graph().is_empty());
    workspace
        .commit_batch(draft.finish(), Duration::ZERO)
        .unwrap();
    run_all(&mut workspace).await;
    let commands = workspace.resolve("commands").unwrap().node;
    let copy = workspace.resolve("copied").unwrap().node;
    assert_eq!(
        workspace
            .runtime()
            .value_of(&commands)
            .cloned()
            .map(local_output)
            .as_ref(),
        workspace.runtime().value_of(&copy)
    );
    assert!(
        matches!(workspace.runtime().value_of(&commands).unwrap().shape(), Shape::Record(record) if record.name() == "wes.Help")
    );
    let error = workspace.resolve("problem").unwrap();
    let handled = workspace.resolve("handled").unwrap().node;
    assert_eq!(workspace.runtime().output(&error), OutputState::Closed);
    assert!(workspace.runtime().value_of(&handled).is_none());
    assert_eq!(calls.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn cancelled_help_never_publishes_a_value_and_cannot_be_changed_as_a_provider_call() {
    let (mut workspace, _) = workspace();
    let node = commit(&mut workspace, ":help calc > help").unwrap();
    let work = ticket(workspace.start(Duration::ZERO));
    assert!(workspace.enter(&work.run));
    let token = CancellationToken::new();
    token.cancel();
    let run = work.run.clone();
    let result = TaskExecutor::ephemeral().execute(work, token).await;
    assert!(matches!(result.outcome, Outcome::Cancelled(_)));
    workspace.complete(&run, result.outcome, Duration::ZERO);
    assert_eq!(
        workspace.runtime().output(&OutputRef::data(node.clone())),
        OutputState::Closed
    );
    assert!(workspace.runtime().value_of(&node).is_none());
    let Preparation::Meta(meta) = workspace
        .prepare(&statement(":change $help value:other"))
        .unwrap()
    else {
        panic!("control continuation")
    };
    assert!(workspace.prepare_control(meta).is_err());
}

#[tokio::test]
async fn unknown_help_is_rejected_before_allocation_and_reserved_commands_remain_describable() {
    let (mut workspace, _) = workspace();
    let error = workspace
        .prepare(&statement(":help missing > unknown"))
        .unwrap_err();
    assert_eq!(code(error), "RES004");
    assert!(workspace.runtime().graph().is_empty());
    assert!(workspace.resolve("unknown").is_none());
    let reserved = commit(&mut workspace, ":help node > reserved").unwrap();
    run_all(&mut workspace).await;
    let Data::Record(fields) = workspace.runtime().value_of(&reserved).unwrap().data() else {
        panic!("help")
    };
    assert_eq!(fields["invocation"], Data::Option(None));
}

#[tokio::test]
async fn provider_help_is_composable_metadata_and_never_invokes_a_provider() {
    let (mut workspace, calls) = workspace();
    let overview = commit(&mut workspace, ":help catalog > overview").unwrap();
    let operation = commit(&mut workspace, ":help catalog echo > operation").unwrap();
    run_all(&mut workspace).await;
    let Data::Record(fields) = workspace.runtime().value_of(&overview).unwrap().data() else {
        panic!()
    };
    assert_eq!(fields["provider"], Data::Text("catalog".into()));
    let Data::List(capabilities) = &fields["children"] else {
        panic!()
    };
    assert_eq!(capabilities.len(), 2);
    let Data::Record(fields) = workspace.runtime().value_of(&operation).unwrap().data() else {
        panic!()
    };
    let Data::Record(fields) = &fields["invocation"] else {
        panic!("invocation")
    };
    assert_eq!(fields["capability"], Data::Text("echo".into()));
    assert_eq!(fields["safety"], Data::Text("SAFE".into()));
    assert_eq!(
        fields["result"],
        Data::Record(indexmap::IndexMap::from([(
            "kind".to_string(),
            Data::Text("unknown".into())
        )]))
    );
    assert_eq!(calls.load(Ordering::SeqCst), 0);
    commit(&mut workspace, "catalog echo value:$operation > copied");
    run_all(&mut workspace).await;
    assert_eq!(calls.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn nested_operation_help_uses_alias_and_rules_and_meta_names_keep_precedence() {
    use wes_core::capability::{DeclaredRule, Rule, RuleBasis};
    let (mut workspace, calls) = workspace();
    let mut cap = Capability::new(["items", "list"], Shape::Unknown, Safety::Unsafe);
    cap.summary = "Lists synthetic items".into();
    cap.parameters.push(Parameter::new(
        "format",
        Shape::Primitive(Primitive::Text),
        false,
    ));
    cap.rules.push(DeclaredRule {
        rule: Rule::OneOf {
            key: "format".into(),
            values: ["json".into()].into(),
        },
        basis: RuleBasis::Documented { note: None },
    });
    let description =
        ProviderDescription::new("renamed", [cap], vec!["credential-name".into()]).unwrap();
    workspace
        .register_provider(description, Arc::new(Echo(calls.clone())))
        .unwrap();
    workspace
        .register_provider(
            metadata().renamed("import").unwrap(),
            Arc::new(Echo(calls.clone())),
        )
        .unwrap();
    let operation = commit(&mut workspace, ":help renamed items list > operation").unwrap();
    let meta = commit(&mut workspace, ":help command:import > meta").unwrap();
    run_all(&mut workspace).await;
    let Data::Record(fields) = workspace.runtime().value_of(&operation).unwrap().data() else {
        panic!()
    };
    assert_eq!(fields["provider"], Data::Text("renamed".into()));
    let Data::Record(fields) = &fields["invocation"] else {
        panic!("invocation")
    };
    assert_eq!(
        fields["summary"],
        Data::Text("Lists synthetic items".into())
    );
    assert_eq!(
        fields["rules"],
        Data::List(vec![Data::Text("'format' must be one of [json]".into())])
    );
    assert!(!format!("{fields:?}").contains("credential-name"));
    let Data::Record(fields) = workspace.runtime().value_of(&meta).unwrap().data() else {
        panic!()
    };
    let Data::Record(fields) = &fields["invocation"] else {
        panic!("invocation")
    };
    assert_eq!(fields["command"], Data::Text(":import".into()));
    assert_eq!(calls.load(Ordering::SeqCst), 0);
}

#[test]
fn invalid_provider_help_is_rejected_before_node_allocation() {
    let (workspace, calls) = workspace();
    for (source, expected) in [
        (":help missing", "RES004"),
        (":help catalog missing", "RES005"),
        (":help catalog echo extra", "RES005"),
        (":help import extra", "RES005"),
    ] {
        assert_eq!(
            code(workspace.prepare(&statement(source)).unwrap_err()),
            expected
        );
    }
    assert!(
        workspace
            .prepare(&statement(":help catalog echo value:secret"))
            .is_err()
    );
    assert!(workspace.runtime().graph().is_empty());
    assert_eq!(calls.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn errors_help_is_discoverable_and_describes_causes_without_execution() {
    let (mut workspace, calls) = workspace();
    let node = commit(&mut workspace, ":help errors").unwrap();
    run_all(&mut workspace).await;
    let Data::Record(help) = workspace.runtime().value_of(&node).unwrap().data() else {
        panic!("help record");
    };
    assert!(matches!(&help["codes"], Data::List(rows) if rows.len() >= 17));
    assert_eq!(calls.load(Ordering::SeqCst), 0);
}
