use super::*;
use wes_core::{Primitive, Provenance, Value};
use wes_engine::{
    graph::OutputPort,
    runtime::{Outcome, RuntimeCode},
    workspace::ReplayWorkspace,
};

fn checked(source: &PreparedSource) {
    assert!(!source.accepted().is_empty(), "{:?}", source.diagnostics());
    assert!(
        !source
            .diagnostics()
            .diagnostics
            .iter()
            .any(|d| d.severity == Severity::Error),
        "{:?}",
        source.diagnostics()
    );
}
fn data(workspace: &Workspace, name: &str) -> Data {
    workspace
        .runtime()
        .value_of(&workspace.resolve(name).unwrap().node)
        .unwrap()
        .data()
        .clone()
}

#[tokio::test]
async fn typed_pipeline_uses_real_edges_lexical_input_and_no_hidden_names() {
    let (mut workspace, calls) = workspace();
    let text = ":calc { return {x: [1,2,3], y: 'hello'}; } > raw\n| catalog echo value:input.x > echoed\n| :calc { return input.map(x => x * 2); } > result *> problem";
    let prepared = plan(&workspace, text, no_files()).await;
    checked(&prepared);
    assert_eq!(prepared.accepted().len(), 1);
    assert_eq!(prepared.nodes().count(), 3);
    assert_eq!(prepared.record().unwrap().replay, text);
    assert!(prepared.record().unwrap().calculation_package.is_some());
    prepared.commit(&mut workspace, Duration::ZERO).unwrap();
    assert_eq!(workspace.bindings().names().len(), 4);
    let nodes = workspace.runtime().graph().nodes().collect::<Vec<_>>();
    assert_eq!(
        nodes[1].dependencies().get(nodes[0].id()),
        Some(&OutputPort::Data)
    );
    assert_eq!(
        nodes[2].dependencies().get(nodes[1].id()),
        Some(&OutputPort::Data)
    );
    run_all(&mut workspace).await;
    assert_eq!(
        data(&workspace, "result"),
        Data::List(vec![Data::Int(2), Data::Int(4), Data::Int(6)])
    );
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    let unrelated = plan(&workspace, ":calc { return input; }", no_files()).await;
    assert!(unrelated.nodes().next().is_none());
}

#[tokio::test]
async fn provider_to_provider_template_and_literal_input_are_distinct() {
    let (mut workspace, calls) = workspace();
    let prepared = plan(&workspace, ":def echo as catalog echo value:?value\ncatalog echo value:\"a | b\" > first | echo value:input > second | catalog echo value:\"input.x\" > literal", no_files()).await;
    checked(&prepared);
    prepared.commit(&mut workspace, Duration::ZERO).unwrap();
    run_all(&mut workspace).await;
    assert_eq!(data(&workspace, "second"), Data::Text("a | b".into()));
    assert_eq!(data(&workspace, "literal"), Data::Text("input.x".into()));
    assert_eq!(calls.load(Ordering::SeqCst), 3);
}

#[tokio::test]
async fn failure_skips_downstream_even_when_input_is_unused_and_error_handler_is_explicit() {
    let (mut workspace, calls) = workspace();
    let prepared = plan(&workspace, ":calc { return 1 / 0; } > source *> sourceError | catalog echo value:unused > never *> notAnError | :calc { return 99; } > later\n$sourceError | :calc { return input.code; } > handled", no_files()).await;
    checked(&prepared);
    prepared.commit(&mut workspace, Duration::ZERO).unwrap();
    run_all(&mut workspace).await;
    assert_eq!(calls.load(Ordering::SeqCst), 0);
    for name in ["never", "later"] {
        assert_eq!(
            workspace
                .runtime()
                .graph()
                .node(&workspace.resolve(name).unwrap().node)
                .unwrap()
                .state(),
            NodeState::Skipped
        );
    }
    assert!(matches!(data(&workspace, "handled"), Data::Text(_)));
    assert!(
        workspace
            .runtime()
            .error_of(&workspace.resolve("never").unwrap().node)
            .is_none()
    );
}

#[tokio::test]
async fn dynamic_missing_field_or_type_failure_never_enters_the_receiver() {
    for source in [
        ":calc { let value={x:1}; return value; } | catalog echo value:input.missing > failed *> reason | catalog echo value:never > later",
        ":calc { let value=7; return value; } | typed echo value:input > failed *> reason | catalog echo value:never > later",
        "catalog echo value:text > untyped | typed echo value:input > failed *> reason | catalog echo value:never > later",
    ] {
        let (mut workspace, calls) = workspace();
        let mut cap = Capability::new(["echo"], Shape::Primitive(Primitive::Text), Safety::Safe);
        cap.parameters = vec![Parameter::new(
            "value",
            Shape::Primitive(Primitive::Text),
            true,
        )];
        workspace
            .register_provider(
                ProviderDescription::new("typed", [cap], vec![]).unwrap(),
                Arc::new(Echo(calls.clone())),
            )
            .unwrap();
        let prepared = plan(&workspace, source, no_files()).await;
        checked(&prepared);
        prepared.commit(&mut workspace, Duration::ZERO).unwrap();
        run_all(&mut workspace).await;
        assert_eq!(
            calls.load(Ordering::SeqCst),
            usize::from(source.starts_with("catalog"))
        );
        assert_eq!(
            workspace
                .runtime()
                .graph()
                .node(&workspace.resolve("failed").unwrap().node)
                .unwrap()
                .state(),
            NodeState::Failed
        );
        assert_eq!(
            workspace
                .runtime()
                .graph()
                .node(&workspace.resolve("later").unwrap().node)
                .unwrap()
                .state(),
            NodeState::Skipped
        );
    }
}

#[tokio::test]
async fn rejected_pipeline_rolls_back_nodes_bindings_and_replay_but_not_independent_statements() {
    for bad in [
        "catalog echo value:yes > leaked | absent operation",
        "catalog echo value:yes > leaked | :import spec file:never.json",
        "catalog echo value:yes > leaked | :calc { const input=2; return input; }",
        "catalog echo value:yes > leaked | catalog echo value:input..field",
        "catalog echo value:yes > leaked | :calc { return $leaked::error; }",
        "catalog echo value:yes > leaked | :workspace save \"forbidden\"",
    ] {
        let (mut workspace, calls) = workspace();
        let source =
            format!("catalog echo value:before > before\n{bad}\ncatalog echo value:after > after");
        let prepared = plan(&workspace, &source, no_files()).await;
        assert_eq!(prepared.nodes().count(), 2, "{bad}");
        assert_eq!(prepared.accepted().len(), 2);
        assert!(!prepared.record().unwrap().replay.contains("leaked"));
        prepared.commit(&mut workspace, Duration::ZERO).unwrap();
        assert!(workspace.resolve("leaked").is_none());
        run_all(&mut workspace).await;
        assert_eq!(calls.load(Ordering::SeqCst), 2);
    }
}

#[tokio::test]
async fn cancellation_closes_the_pipeline_and_does_not_run_followers() {
    let (mut workspace, calls) = workspace();
    let prepared = plan(
        &workspace,
        "catalog echo value:first > root | catalog echo value:input > later",
        no_files(),
    )
    .await;
    checked(&prepared);
    prepared.commit(&mut workspace, Duration::ZERO).unwrap();
    let effects = workspace.start(Duration::ZERO);
    let ticket = effects
        .iter()
        .find_map(|effect| match effect {
            Effect::Spawn(ticket) => Some(ticket),
            _ => None,
        })
        .expect("source execution");
    let run = ticket.run.clone();
    assert!(workspace.enter(&run));
    let follow = workspace.complete(
        &run,
        Outcome::Cancelled(RuntimeCode::Cancelled.error("fixture cancellation", None)),
        Duration::ZERO,
    );
    assert!(follow.iter().all(|e| !matches!(e, Effect::Spawn(_))));
    assert_eq!(
        workspace
            .runtime()
            .graph()
            .node(&workspace.resolve("later").unwrap().node)
            .unwrap()
            .state(),
        NodeState::Skipped
    );
    assert_eq!(calls.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn pipeline_replay_preserves_exact_ids_and_never_invokes_providers() {
    let (workspace, _) = workspace();
    let source = ":calc { return {x: 9}; } > origin | catalog echo value:input.x | :calc { return input + 1; } > result";
    let prepared = plan(&workspace, source, no_files()).await;
    checked(&prepared);
    let record = prepared.record().unwrap().clone();
    let (base, calls) = super::workspace();
    let mut builder = ReplayWorkspace::new(base).unwrap();
    let replay = builder
        .prepare(&record, CancellationToken::new())
        .await
        .unwrap();
    assert_eq!(replay.nodes().cloned().collect::<Vec<_>>(), record.nodes);
    builder.apply(replay).unwrap();
    let mut restored = builder.finish();
    assert!(restored.start(Duration::ZERO).is_empty());
    assert_eq!(restored.resolve("result").unwrap().node, record.nodes[2]);
    assert_eq!(calls.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn replay_hydration_and_explicit_refresh_preserve_pipeline_activation() {
    use wes_engine::{runtime::RestoredState, workspace::Preparation};
    let (workspace, _) = workspace();
    let prepared = plan(
        &workspace,
        ":calc { return 1 / 0; } > source | catalog echo value:unused > later",
        no_files(),
    )
    .await;
    checked(&prepared);
    let record = prepared.record().unwrap().clone();
    let (base, calls) = super::workspace();
    let mut builder = ReplayWorkspace::new(base).unwrap();
    let replay = builder
        .prepare(&record, CancellationToken::new())
        .await
        .unwrap();
    builder.apply(replay).unwrap();
    builder
        .hydrate(
            &record.nodes[0],
            RestoredState::Failed(RuntimeCode::InputFailed.error("fixture", None)),
            None,
        )
        .unwrap();
    builder
        .hydrate(&record.nodes[1], RestoredState::Skipped, None)
        .unwrap();
    let mut restored = builder.finish();
    assert!(restored.start(Duration::ZERO).is_empty());
    let parsed = wes_language::parse(&SourceText::new("refresh", ":refresh $later"));
    let Preparation::Meta(meta) = restored.prepare(&parsed.script.statements[0]).unwrap() else {
        panic!("refresh");
    };
    let control = restored.prepare_control(meta).unwrap();
    let applied = restored.apply_control(control, Duration::ZERO).unwrap();
    assert!(
        applied
            .effects
            .iter()
            .all(|effect| !matches!(effect, Effect::Spawn(_)))
    );
    assert_eq!(
        restored
            .runtime()
            .graph()
            .node(&record.nodes[1])
            .unwrap()
            .state(),
        NodeState::Skipped
    );
    assert!(restored.runtime().error_of(&record.nodes[1]).is_none());
    assert_eq!(calls.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn piped_calc_preserves_provenance_and_input_is_an_immutable_local() {
    let (mut workspace, _) = workspace();
    let prepared = plan(
        &workspace,
        "catalog echo value:start > root | :calc { const f=() => input; return f(); } > result",
        no_files(),
    )
    .await;
    checked(&prepared);
    prepared.commit(&mut workspace, Duration::ZERO).unwrap();
    let effects = workspace.start(Duration::ZERO);
    let ticket = effects
        .iter()
        .find_map(|effect| match effect {
            Effect::Spawn(ticket) => Some(ticket),
            _ => None,
        })
        .expect("source execution");
    let run = ticket.run.clone();
    assert!(workspace.enter(&run));
    let value = Value::new(
        Shape::Primitive(Primitive::Int),
        Data::Int(42),
        Provenance::default().with_fact("fixture", "pipe"),
    )
    .unwrap();
    let effects = workspace.complete(&run, Outcome::Produced(value), Duration::ZERO);
    for effect in effects {
        if let Effect::Spawn(ticket) = effect {
            let run = ticket.run.clone();
            let ticket = workspace.enter_ticket(ticket).unwrap().unwrap();
            let result = TaskExecutor::ephemeral()
                .execute(ticket, CancellationToken::new())
                .await;
            workspace.complete(&run, result.outcome, Duration::ZERO);
        }
    }
    let value = workspace
        .runtime()
        .value_of(&workspace.resolve("result").unwrap().node)
        .unwrap();
    assert_eq!(value.data(), &Data::Int(42));
    assert_eq!(
        value.provenance(),
        &Provenance::default().with_fact("fixture", "pipe")
    );
    let bad = plan(
        &workspace,
        "$root | :calc { input = 3; return input; }",
        no_files(),
    )
    .await;
    assert_eq!(bad.nodes().count(), 0);
}

#[tokio::test]
async fn unsupported_stage_modes_and_statically_missing_fields_reject_the_entire_pipe() {
    let (mut workspace, calls) = workspace();
    let mut cap = Capability::new(["echo"], Shape::Primitive(Primitive::Text), Safety::Safe);
    cap.parameters = vec![Parameter::new(
        "value",
        Shape::Primitive(Primitive::Text),
        true,
    )];
    workspace
        .register_provider(
            ProviderDescription::new("typed", [cap.clone()], vec![]).unwrap(),
            Arc::new(Echo(calls.clone())),
        )
        .unwrap();
    cap.streaming = true;
    workspace
        .register_provider(
            ProviderDescription::new("stream", [cap], vec![]).unwrap(),
            Arc::new(Echo(calls.clone())),
        )
        .unwrap();
    for source in [
        "typed echo value:yes | catalog echo value:input.field",
        "catalog echo value:yes | stream echo value:input",
        "@interactive catalog echo value:yes | :calc { return input; }",
        "catalog echo value:yes | :list providers",
    ] {
        let prepared = plan(&workspace, source, no_files()).await;
        assert_eq!(prepared.nodes().count(), 0, "{source}");
        assert!(prepared.record().is_none());
        assert!(!prepared.diagnostics().diagnostics.is_empty());
    }
    assert_eq!(calls.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn unused_pipe_input_cannot_bypass_transfer_policy() {
    let (mut workspace, calls) = workspace();
    let prepared = plan(
        &workspace,
        "catalog echo value:start > root | catalog echo value:constant > receiver",
        no_files(),
    )
    .await;
    checked(&prepared);
    prepared.commit(&mut workspace, Duration::ZERO).unwrap();
    let effects = workspace.start(Duration::ZERO);
    let ticket = effects
        .iter()
        .find_map(|effect| match effect {
            Effect::Spawn(ticket) => Some(ticket),
            _ => None,
        })
        .expect("source execution");
    let run = ticket.run.clone();
    assert!(workspace.enter(&run));
    let value = Value::new(
        Shape::Primitive(Primitive::Text),
        Data::Text("private".into()),
        Provenance::default().with_policy(&wes_core::flow::FlowPolicy::default().unknown()),
    )
    .unwrap();
    let effects = workspace.complete(&run, Outcome::Produced(value), Duration::ZERO);
    for effect in effects {
        if let Effect::Spawn(ticket) = effect {
            let run = ticket.run.clone();
            let ticket = workspace.enter_ticket(ticket).unwrap().unwrap();
            let result = TaskExecutor::ephemeral()
                .execute(ticket, CancellationToken::new())
                .await;
            workspace.complete(&run, result.outcome, Duration::ZERO);
        }
    }
    assert_eq!(calls.load(Ordering::SeqCst), 0);
    let receiver = workspace.resolve("receiver").unwrap();
    assert_eq!(
        workspace
            .runtime()
            .graph()
            .node(&receiver.node)
            .unwrap()
            .state(),
        NodeState::Failed
    );
}

#[tokio::test]
async fn unused_typed_calc_pipe_input_keeps_dependency_and_private_transfer_policy() {
    for parameter in ["", "input: Text"] {
        let (mut workspace, calls) = workspace();
        let source = format!(
            ":def fixed({parameter}) -> Text as :calc pure {{ return 'constant'; }}\ncatalog echo value:start > root | fixed > local | catalog echo value:input > exported"
        );
        let prepared = plan(&workspace, &source, no_files()).await;
        checked(&prepared);
        prepared.commit(&mut workspace, Duration::ZERO).unwrap();
        let root = workspace.resolve("root").unwrap();
        let local = workspace.resolve("local").unwrap();
        assert_eq!(
            workspace
                .runtime()
                .graph()
                .node(&local.node)
                .unwrap()
                .dependencies()
                .get(&root.node),
            Some(&OutputPort::Data)
        );
        let effects = workspace.start(Duration::ZERO);
        let ticket = effects
            .into_iter()
            .find_map(|effect| {
                if let Effect::Spawn(ticket) = effect {
                    Some(ticket)
                } else {
                    None
                }
            })
            .unwrap();
        assert_eq!(ticket.run.node(), &root.node);
        let run = ticket.run.clone();
        assert!(workspace.enter(&run));
        let policy = wes_core::flow::FlowPolicy::default().unknown();
        let value = Value::new(
            Shape::Primitive(Primitive::Text),
            Data::Text("private synthetic control".into()),
            Provenance::default().with_policy(&policy),
        )
        .unwrap();
        let mut effects =
            VecDeque::from(workspace.complete(&run, Outcome::Produced(value), Duration::ZERO));
        while let Some(effect) = effects.pop_front() {
            if let Effect::Spawn(ticket) = effect {
                let run = ticket.run.clone();
                let ticket = workspace.enter_ticket(ticket).unwrap().unwrap();
                let report = TaskExecutor::ephemeral()
                    .execute(ticket, CancellationToken::new())
                    .await;
                effects.extend(workspace.complete(&run, report.outcome, Duration::ZERO));
            }
        }
        let output = workspace.runtime().value_of(&local.node).unwrap();
        assert_eq!(output.data(), &Data::Text("constant".into()));
        assert_eq!(output.provenance().policy(), &policy);
        let exported = workspace.resolve("exported").unwrap();
        assert_eq!(
            workspace
                .runtime()
                .graph()
                .node(&exported.node)
                .unwrap()
                .state(),
            NodeState::Failed
        );
        assert_eq!(calls.load(Ordering::SeqCst), 0);
    }
}

#[tokio::test]
async fn fork_predicate_and_unused_body_keep_private_control_and_block_external_transfer() {
    for parameter in ["", "input: Text"] {
        let (mut workspace, calls) = workspace();
        let source = format!(
            ":def always(input: Text) -> Bool as :calc pure {{ return true; }}\n:def fixed({parameter}) -> Text as :calc pure {{ return 'constant'; }}\ncatalog echo value:start > root | :fork {{ when always {{ fixed > local | catalog echo value:input > exported }} }}"
        );
        let prepared = plan(&workspace, &source, no_files()).await;
        checked(&prepared);
        prepared.commit(&mut workspace, Duration::ZERO).unwrap();
        let root = workspace.resolve("root").unwrap();
        let local = workspace.resolve("local").unwrap();
        let effects = workspace.start(Duration::ZERO);
        let ticket = effects
            .into_iter()
            .find_map(|effect| {
                if let Effect::Spawn(ticket) = effect {
                    Some(ticket)
                } else {
                    None
                }
            })
            .unwrap();
        assert_eq!(ticket.run.node(), &root.node);
        let run = ticket.run.clone();
        assert!(workspace.enter(&run));
        let policy = wes_core::flow::FlowPolicy::default().unknown();
        let value = Value::new(
            Shape::Primitive(Primitive::Text),
            Data::Text("private synthetic control".into()),
            Provenance::default().with_policy(&policy),
        )
        .unwrap();
        let mut effects =
            VecDeque::from(workspace.complete(&run, Outcome::Produced(value), Duration::ZERO));
        while let Some(effect) = effects.pop_front() {
            if let Effect::Spawn(ticket) = effect {
                let run = ticket.run.clone();
                let ticket = workspace.enter_ticket(ticket).unwrap().unwrap();
                let report = TaskExecutor::ephemeral()
                    .execute(ticket, CancellationToken::new())
                    .await;
                effects.extend(workspace.complete(&run, report.outcome, Duration::ZERO));
            }
        }
        let output = workspace.runtime().value_of(&local.node).unwrap();
        assert_eq!(output.data(), &Data::Text("constant".into()));
        assert_eq!(output.provenance().policy(), &policy);
        let exported = workspace.resolve("exported").unwrap();
        assert_eq!(
            workspace
                .runtime()
                .graph()
                .node(&exported.node)
                .unwrap()
                .state(),
            NodeState::Failed
        );
        assert_eq!(calls.load(Ordering::SeqCst), 0);
    }
}

#[tokio::test]
async fn fork_source_restores_the_same_branch_nodes_held_without_handlers_or_calls() {
    let (mut workspace, _) = workspace();
    let text = include_str!("../../../../examples/pipeline-forks/finite.wes");
    let prepared = plan(&workspace, text, no_files()).await;
    checked(&prepared);
    let record = prepared.record().unwrap().clone();
    let ids = prepared.nodes().cloned().collect::<Vec<_>>();
    prepared.commit(&mut workspace, Duration::ZERO).unwrap();
    run_all(&mut workspace).await;
    let mut replay = ReplayWorkspace::new(Workspace::local(
        wes_engine::providers::LocalScope::new("fixture").unwrap(),
    ))
    .unwrap();
    let prepared = replay
        .prepare(&record, CancellationToken::new())
        .await
        .unwrap();
    replay.apply(prepared).unwrap();
    let mut restored = replay.finish();
    assert!(restored.start(Duration::ZERO).is_empty());
    assert_eq!(
        restored
            .runtime()
            .graph()
            .nodes()
            .map(|n| n.id().clone())
            .collect::<Vec<_>>(),
        ids
    );
    assert!(restored.resolve("failureCode").is_some());
}

#[tokio::test]
async fn nested_pipeline_inputs_defer_unknown_types_until_the_captured_value_is_available() {
    for valid in [true, false] {
        let (mut workspace, calls) = workspace();
        let body = Shape::Record(
            wes_core::RecordShape::new(
                "Body",
                [("count".into(), Shape::Primitive(Primitive::Int))],
            )
            .unwrap(),
        );
        let mut capability = Capability::new(["use"], Shape::Unknown, Safety::Safe);
        capability.parameters = vec![Parameter::new("value", body, true)];
        workspace
            .register_provider(
                ProviderDescription::new("typed", [capability], vec![]).unwrap(),
                Arc::new(Echo(calls.clone())),
            )
            .unwrap();
        let text = if valid {
            ":calc { return {count:7}; } | catalog echo value:input | typed use value:{count:input.count} > result"
        } else {
            ":calc { return {count:\"seven\"}; } | catalog echo value:input | typed use value:{count:input.count} > result"
        };
        let prepared = plan(&workspace, text, no_files()).await;
        checked(&prepared);
        prepared.commit(&mut workspace, Duration::ZERO).unwrap();
        run_all(&mut workspace).await;
        let result = workspace.resolve("result").unwrap();
        if valid {
            assert!(workspace.runtime().value_of(&result.node).is_some());
            assert_eq!(calls.load(Ordering::SeqCst), 2);
        } else {
            assert_eq!(
                workspace
                    .runtime()
                    .graph()
                    .node(&result.node)
                    .unwrap()
                    .state(),
                NodeState::Failed
            );
            assert_eq!(calls.load(Ordering::SeqCst), 1);
            assert_eq!(
                workspace.runtime().error_of(&result.node).unwrap().code(),
                "RUN001"
            );
        }
    }
}
