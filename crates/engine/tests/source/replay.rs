use super::*;
use wes_core::{Primitive, Provenance, Value};
use wes_engine::{
    graph::NodeId,
    history::CommandRecord,
    runtime::{RestoredState, RunId, RuntimeCode},
    source::prepare_replay,
    workspace::{Preparation, ReplayWorkspace},
};

fn command(text: &str, ids: &[&str]) -> CommandRecord {
    CommandRecord {
        source_name: "fixture.wes".into(),
        source_start: wes_language::Position { line: 1, column: 1 },
        changed_nodes: vec![],
        document: None,
        revision_of: None,
        environments: None,
        cell: "historical-cell".into(),
        text: text.into(),
        replay: text.into(),
        nodes: ids.iter().map(|id| NodeId::new(*id).unwrap()).collect(),
        type_sources: IndexMap::new(),
        calculation_package: None,
        imports: vec![],
    }
}
async fn apply(builder: &mut ReplayWorkspace, command: &CommandRecord) {
    let prepared = builder
        .prepare(command, CancellationToken::new())
        .await
        .unwrap();
    assert_eq!(
        prepared.nodes().collect::<Vec<_>>(),
        command.nodes.iter().collect::<Vec<_>>()
    );
    let applied = builder.apply(prepared).unwrap();
    assert!(applied.effects.is_empty());
    assert!(builder.workspace().runtime().is_idle());
}

#[tokio::test]
async fn unused_stored_import_evidence_is_not_silently_ignored() {
    use wes_engine::imports::{ImportRecipe, ImportRequest, ImportSnapshot};
    let (base, _) = workspace();
    let mut builder = ReplayWorkspace::new(base).unwrap();
    let mut record = command("catalog echo value:historic > output", &["id1"]);
    record.imports.push(ImportSnapshot::new(
        ImportRequest::new("fixture".into(), None, IndexMap::new()).unwrap(),
        ImportRecipe::new("fixture/v1".into(), "evidence".into()).unwrap(),
    ));
    assert!(
        builder
            .prepare(&record, CancellationToken::new())
            .await
            .is_err()
    );
    assert!(builder.workspace().resolve("output").is_none());
    assert!(builder.workspace().runtime().is_idle());
}
fn statement(text: &str) -> wes_language::Statement {
    wes_language::parse(&SourceText::new("test", text))
        .script
        .statements
        .into_iter()
        .next()
        .unwrap()
}

#[tokio::test]
async fn exact_ids_and_recorded_controls_reconstruct_held_nodes_and_handlers_without_effects() {
    let (base, calls) = workspace();
    let mut builder = ReplayWorkspace::new(base).unwrap();
    let text = ":workspace policy mode:reactive\n:def echo as catalog echo value:?value\necho value:old > root *> problem\ncatalog echo value:$problem > handler\n:type check \"7\" as:Int > typed\n:change $root value:changed\n:timeout $root after:PT2S";
    let mut record = command(text, &["id9100", "legacy-handler", "id8500"]);
    record.changed_nodes = vec![NodeId::new("id9100").unwrap()];
    record.text = "original source includes rejected statements; do not parse this".into();
    apply(&mut builder, &record).await;
    assert_eq!(
        builder.workspace().resolve("root").unwrap().node,
        record.nodes[0]
    );
    assert_eq!(
        builder.workspace().resolve("handler").unwrap().node,
        record.nodes[1]
    );
    assert_eq!(
        builder.workspace().resolve("typed").unwrap().node,
        record.nodes[2]
    );
    let original_error = RuntimeCode::ExecutionFailed.error("historical failure", None);
    let run = RunId::new("historical-run").unwrap();
    builder
        .hydrate(
            &record.nodes[0],
            RestoredState::Failed(original_error.clone()),
            Some(run.clone()),
        )
        .unwrap();
    let typed = Value::new(
        Shape::Primitive(Primitive::Int),
        Data::Int(7.into()),
        Provenance::default().with_fact("source", "synthetic-history"),
    )
    .unwrap();
    builder
        .hydrate(
            &record.nodes[2],
            RestoredState::Ready(typed.clone()),
            Some(RunId::new("typed-run").unwrap()),
        )
        .unwrap();
    let mut restored = builder.finish();
    assert!(restored.start(Duration::ZERO).is_empty());
    assert_eq!(
        restored.runtime().error_of(&record.nodes[0]),
        Some(&original_error)
    );
    assert_eq!(restored.runtime().run_of(&record.nodes[0]), Some(&run));
    assert_eq!(restored.runtime().value_of(&record.nodes[2]), Some(&typed));
    assert_eq!(
        restored.data_typing(&record.nodes[2]).unwrap().provenance,
        typed.provenance().clone()
    );
    let new = plan(&restored, ":type check \"new\" as:Text > new", no_files()).await;
    assert_eq!(new.nodes().next().unwrap().as_str(), "id9101");
    new.commit(&mut restored, Duration::ZERO).unwrap();
    run_all(&mut restored).await;
    assert_eq!(calls.load(Ordering::SeqCst), 0); // Unrelated work cannot launch historical handlers.
    let Preparation::Meta(meta) = restored.prepare(&statement(":refresh $handler")).unwrap() else {
        panic!("refresh")
    };
    let control = restored.prepare_control(meta).unwrap();
    let effects = restored
        .apply_control(control, Duration::ZERO)
        .unwrap()
        .effects;
    for effect in effects {
        if let Effect::Spawn(ticket) = effect {
            let run = ticket.run.clone();
            assert_eq!(run.node(), &record.nodes[1]);
            assert!(restored.enter(&run));
            let report = TaskExecutor::ephemeral()
                .execute(ticket, CancellationToken::new())
                .await;
            restored.complete(&run, report.outcome, Duration::ZERO);
        }
    }
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    let Preparation::Meta(meta) = restored.prepare(&statement(":refresh $root")).unwrap() else {
        panic!("root refresh")
    };
    let control = restored.prepare_control(meta).unwrap();
    let effects = restored
        .apply_control(control, Duration::ZERO)
        .unwrap()
        .effects;
    assert!(effects.iter().any(|effect| matches!(effect, Effect::Watch(deadline) if deadline.remaining(Duration::ZERO) == Duration::from_secs(2))));
    for effect in effects {
        if let Effect::Spawn(ticket) = effect {
            assert_eq!(ticket.run.node(), &record.nodes[0]);
            let run = ticket.run.clone();
            assert!(restored.enter(&run));
            let report = TaskExecutor::ephemeral()
                .execute(ticket, CancellationToken::new())
                .await;
            restored.complete(&run, report.outcome, Duration::ZERO);
        }
    }
    assert_eq!(calls.load(Ordering::SeqCst), 2);
    assert_eq!(
        restored
            .runtime()
            .value_of(&record.nodes[0])
            .unwrap()
            .data(),
        &Data::Text("changed".into())
    );
}

#[tokio::test]
async fn failed_reconstruction_reserves_recorded_ids_and_keeps_structured_diagnostics() {
    let (base, calls) = workspace();
    let mut builder = ReplayWorkspace::new(base).unwrap();
    let error = builder
        .prepare(
            &command("missing call", &["id70000"]),
            CancellationToken::new(),
        )
        .await;
    let Err(SourceError::ReplayRejected(diagnostics)) = error else {
        panic!("structured replay rejection")
    };
    assert!(!diagnostics.diagnostics.is_empty());
    assert!(builder.workspace().runtime().graph().is_empty());
    let restored = builder.finish();
    let fresh = plan(&restored, "catalog echo value:new", no_files()).await;
    assert_eq!(fresh.nodes().next().unwrap().as_str(), "id70001");
    assert_eq!(calls.load(Ordering::SeqCst), 0);
    let (base, _) = workspace();
    let mut builder = ReplayWorkspace::new(base).unwrap();
    assert!(
        builder
            .prepare(
                &command("missing call", &["id9223372036854775807"]),
                CancellationToken::new()
            )
            .await
            .is_err()
    );
    let restored = builder.finish();
    assert!(
        restored
            .prepare(&statement("catalog echo value:new"))
            .is_err()
    ); // No wrapping/reuse.
}

#[tokio::test]
async fn identity_count_conflicts_and_semantic_partial_replays_never_mutate_the_builder() {
    for record in [
        command("catalog echo value:one", &["old-one", "old-two"]),
        command(
            "catalog echo value:one\ncatalog echo value:two",
            &["old-one"],
        ),
        command("catalog echo value:one\nmissing call", &["old-one"]),
        command(
            "catalog echo value:one\ncatalog echo value:two",
            &["same", "same"],
        ),
        command("catalog echo value:one > old-one", &["old-one"]),
        command(
            ":def usable as catalog echo value:?value\nmissing call",
            &[],
        ),
        command("catalog echo value:\"unclosed", &["old-one"]),
        command(":refresh $missing", &[]),
    ] {
        let (base, calls) = workspace();
        let builder = ReplayWorkspace::new(base).unwrap();
        assert!(
            prepare_replay(&record, builder.draft().unwrap(), CancellationToken::new())
                .await
                .is_err()
        );
        assert!(builder.workspace().runtime().graph().is_empty());
        assert!(!builder.workspace().templates().contains("usable"));
        assert_eq!(calls.load(Ordering::SeqCst), 0);
    }
}

#[tokio::test]
async fn deleted_historical_ids_cannot_be_reused_and_generator_keeps_its_high_water_mark() {
    let (base, calls) = workspace();
    let mut builder = ReplayWorkspace::new(base).unwrap();
    let mut record = command(
        "catalog echo value:one > root\ncatalog echo value:$root > child\n:change $root value:changed\n:node remove $root scope:downstream",
        &["id9999", "child-id"],
    );
    record.changed_nodes = vec![NodeId::new("id9999").unwrap()];
    apply(&mut builder, &record).await;
    assert!(builder.workspace().runtime().graph().is_empty());
    let reused = prepare_replay(
        &command("catalog echo value:new > new", &["id9999"]),
        builder.draft().unwrap(),
        CancellationToken::new(),
    )
    .await
    .unwrap();
    assert!(builder.apply(reused).is_err());
    assert!(builder.workspace().runtime().graph().is_empty());
    let restored = builder.finish();
    let new = plan(&restored, "catalog echo value:new", no_files()).await;
    assert_eq!(new.nodes().next().unwrap().as_str(), "id10000");
    assert_eq!(calls.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn replay_uses_only_captured_yaml_and_never_falls_back_to_live_sources() {
    let mut record = command(
        ":package load path:types.yaml\n:def choose(category: Category) as catalog echo value:?category\nchoose category:books > selected",
        &["kept-node"],
    );
    record.type_sources.insert(
        "types.yaml".into(),
        "types: {Category: {base: Text, enum: [books]}}".into(),
    );
    let (base, calls) = workspace();
    let mut builder = ReplayWorkspace::new(base).unwrap();
    apply(&mut builder, &record).await;
    assert!(builder.workspace().templates().contains("choose"));
    assert_eq!(
        builder
            .workspace()
            .resolve("selected")
            .unwrap()
            .node
            .as_str(),
        "kept-node"
    );
    assert_eq!(calls.load(Ordering::SeqCst), 0);
    for sources in [
        IndexMap::new(),
        IndexMap::from([(
            "types.yaml".into(),
            "types: {Category: {base: Text, enum: [games]}}".into(),
        )]),
    ] {
        record.type_sources = sources;
        let (base, _) = workspace();
        let builder = ReplayWorkspace::new(base).unwrap();
        assert!(
            prepare_replay(&record, builder.draft().unwrap(), CancellationToken::new())
                .await
                .is_err()
        );
        assert!(builder.workspace().runtime().graph().is_empty());
        assert!(!builder.workspace().templates().contains("choose"));
    }
}

#[tokio::test]
async fn replay_rejects_stale_foreign_and_post_hydration_batches() {
    let (base, _) = workspace();
    let mut first = ReplayWorkspace::new(base).unwrap();
    let (base, _) = workspace();
    let mut second = ReplayWorkspace::new(base).unwrap();
    let record = command("catalog echo value:one > first", &["first-id"]);
    let foreign = prepare_replay(&record, first.draft().unwrap(), CancellationToken::new())
        .await
        .unwrap();
    assert!(matches!(
        second.apply(foreign),
        Err(WorkspaceError::Obsolete)
    ));
    let stale = prepare_replay(
        &command("catalog echo value:two > second", &["second-id"]),
        first.draft().unwrap(),
        CancellationToken::new(),
    )
    .await
    .unwrap();
    apply(&mut first, &record).await;
    assert!(matches!(first.apply(stale), Err(WorkspaceError::Obsolete)));
    let later = prepare_replay(
        &command("catalog echo value:two > second", &["second-id"]),
        first.draft().unwrap(),
        CancellationToken::new(),
    )
    .await
    .unwrap();
    first
        .hydrate(&record.nodes[0], RestoredState::Stale, None)
        .unwrap();
    assert!(matches!(first.apply(later), Err(WorkspaceError::Obsolete)));
    assert!(first.draft().is_err());
    assert_eq!(first.workspace().runtime().graph().nodes().count(), 1);
}

#[tokio::test]
async fn reconstruction_cannot_take_over_a_nonempty_or_closed_workspace_and_cancelled_preparation_is_inert()
 {
    let (mut base, calls) = workspace();
    let prepared = plan(&base, "catalog echo value:one", no_files()).await;
    prepared.commit(&mut base, Duration::ZERO).unwrap();
    assert!(ReplayWorkspace::new(base).is_err());
    assert_eq!(calls.load(Ordering::SeqCst), 0);
    let mut closed = Workspace::local(wes_engine::providers::LocalScope::new("fixture").unwrap());
    closed.close();
    assert!(ReplayWorkspace::new(closed).is_err());
    let (base, _) = workspace();
    let builder = ReplayWorkspace::new(base).unwrap();
    let cancellation = CancellationToken::new();
    cancellation.cancel();
    assert!(matches!(
        prepare_replay(
            &command("catalog echo value:one", &["old"]),
            builder.draft().unwrap(),
            cancellation
        )
        .await,
        Err(SourceError::Cancelled)
    ));
    assert!(builder.workspace().runtime().graph().is_empty());
}

#[tokio::test]
async fn calculation_replay_uses_captured_grammar_and_stays_held() {
    let (base, calls) = workspace();
    let mut builder = ReplayWorkspace::new(base).unwrap();
    let mut record = command(":calc { yield 6*7; } > answer", &["calc-history"]);
    record.calculation_package = Some(
        wes_language::calc::Package::standard()
            .source()
            .replace("  return: return", "  yield: return"),
    );
    apply(&mut builder, &record).await;
    assert_eq!(calls.load(Ordering::SeqCst), 0);
    let mut restored = builder.finish();
    assert!(restored.start(Duration::ZERO).is_empty());
    let Preparation::Meta(meta) = restored.prepare(&statement(":refresh $answer")).unwrap() else {
        panic!()
    };
    let control = restored.prepare_control(meta).unwrap();
    for effect in restored
        .apply_control(control, Duration::ZERO)
        .unwrap()
        .effects
    {
        if let Effect::Spawn(ticket) = effect {
            let run = ticket.run.clone();
            assert!(restored.enter(&run));
            let report = TaskExecutor::ephemeral()
                .execute(ticket, CancellationToken::new())
                .await;
            restored.complete(&run, report.outcome, Duration::ZERO);
        }
    }
    assert_eq!(
        restored
            .runtime()
            .value_of(&record.nodes[0])
            .unwrap()
            .data(),
        &Data::Int(42)
    );
    assert_eq!(calls.load(Ordering::SeqCst), 0);
}
#[tokio::test]
async fn calculation_replay_refuses_missing_or_unknown_package_without_execution() {
    for package in [None, Some("version: 9000".to_owned())] {
        let (base, calls) = workspace();
        let mut builder = ReplayWorkspace::new(base).unwrap();
        let mut record = command(":calc { return 1; } > answer", &["calc-history"]);
        record.calculation_package = package;
        assert!(
            builder
                .prepare(&record, CancellationToken::new())
                .await
                .is_err()
        );
        assert!(builder.workspace().runtime().graph().is_empty());
        assert_eq!(calls.load(Ordering::SeqCst), 0);
    }
}

#[tokio::test]
async fn replay_defers_unknown_reference_shapes_but_dispatch_checks_hydrated_values() {
    for field in [false, true] {
        for compatible in [false, true] {
            let (mut base, calls) = workspace();
            let mut strict =
                Capability::new(["int"], Shape::Primitive(Primitive::Int), Safety::Safe);
            strict.parameters = vec![Parameter::new(
                "value",
                Shape::Primitive(Primitive::Int),
                true,
            )];
            base.register_provider(
                ProviderDescription::new("strict", [strict], vec![]).unwrap(),
                Arc::new(Echo(calls.clone())),
            )
            .unwrap();
            let mut calc = command(
                ":calc { let value=7; return value; } > input",
                &["input-node"],
            );
            calc.calculation_package =
                Some(wes_language::calc::Package::standard().source().into());
            let mut builder = ReplayWorkspace::new(base).unwrap();
            apply(&mut builder, &calc).await;
            let reference = if field { "$input.id" } else { "$input" };
            let text = format!("strict int value:{reference} > output");
            // Live admission remains strict even for a node awaiting hydration.
            assert!(builder.workspace().prepare(&statement(&text)).is_err());
            for invalid in ["strict int value:$absent", "strict int value:not_an_int"] {
                assert!(
                    builder
                        .prepare(&command(invalid, &["bad"]), CancellationToken::new())
                        .await
                        .is_err()
                );
            }
            apply(&mut builder, &command(&text, &["output-node"])).await;
            assert_eq!(calls.load(Ordering::SeqCst), 0);
            let shape = Shape::Primitive(if compatible {
                Primitive::Int
            } else {
                Primitive::Text
            });
            let data = if compatible {
                Data::Int(7.into())
            } else {
                Data::Text("wrong".into())
            };
            let value = if field {
                Value::new(
                    Shape::Record(
                        wes_core::RecordShape::new("Fixture", [("id".into(), shape)]).unwrap(),
                    ),
                    Data::Record(IndexMap::from([("id".into(), data)])),
                    Provenance::default(),
                )
                .unwrap()
            } else {
                Value::new(shape, data, Provenance::default()).unwrap()
            };
            builder
                .hydrate(&calc.nodes[0], RestoredState::Ready(value), None)
                .unwrap();
            let mut restored = builder.finish();
            assert!(restored.start(Duration::ZERO).is_empty());
            let Preparation::Meta(meta) = restored.prepare(&statement(":refresh $output")).unwrap()
            else {
                panic!("refresh")
            };
            let control = restored.prepare_control(meta).unwrap();
            let effects = restored
                .apply_control(control, Duration::ZERO)
                .unwrap()
                .effects;
            for effect in effects {
                if let Effect::Spawn(ticket) = effect {
                    let run = ticket.run.clone();
                    let ticket = restored.enter_ticket(ticket).unwrap().unwrap();
                    let report = TaskExecutor::ephemeral()
                        .execute(ticket, CancellationToken::new())
                        .await;
                    restored.complete(&run, report.outcome, Duration::ZERO);
                }
            }
            assert_eq!(calls.load(Ordering::SeqCst), usize::from(compatible));
            let output = restored.resolve("output").unwrap().node;
            assert_eq!(
                restored.runtime().graph().node(&output).unwrap().state(),
                if compatible {
                    NodeState::Ready
                } else {
                    NodeState::Failed
                }
            );
        }
    }
}

#[tokio::test]
async fn nested_reference_replay_restores_declarations_without_hydrated_shape_or_effects() {
    let (mut base, calls) = workspace();
    let body = Shape::Record(
        wes_core::RecordShape::new("Body", [("count".into(), Shape::Primitive(Primitive::Int))])
            .unwrap(),
    );
    let mut capability = Capability::new(["use"], Shape::Unknown, Safety::Safe);
    capability.parameters = vec![Parameter::new("value", body, true)];
    base.register_provider(
        ProviderDescription::new("typed", [capability], vec![]).unwrap(),
        Arc::new(Echo(calls.clone())),
    )
    .unwrap();
    let mut builder = ReplayWorkspace::new(base).unwrap();
    let record = command(
        "typed use value:{count:7} > source\ntyped use value:{count:$source.count} > result",
        &["first", "second"],
    );
    apply(&mut builder, &record).await;
    let restored = builder.finish();
    let result = restored.resolve("result").unwrap();
    assert_eq!(
        restored
            .runtime()
            .graph()
            .node(&result.node)
            .unwrap()
            .dependencies()[&NodeId::new("first").unwrap()],
        wes_engine::graph::OutputPort::Data
    );
    assert!(restored.runtime().is_idle());
    assert_eq!(calls.load(Ordering::SeqCst), 0);
}
