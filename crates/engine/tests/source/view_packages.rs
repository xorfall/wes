use super::*;
use wes_engine::workspace::Preparation;
fn artifact() -> String {
    let manifest = r#"{"name":"TaskBadge","id":"task-badge","summary":"Task count","renderer":"View.tsx","input":"TaskBadge","outputs":{},"interaction":null}"#;
    let types = "types: {TaskBadge: {base: Record, fields: {count: Int}}}";
    let package = wes_views::Package::parse(manifest, types).unwrap();
    // Domain fixture; installation must not evaluate renderer code.
    format!(
        r#"{{"format":1,"sdk":{},"manifest":{:?},"types":{:?},"definition":{:?},"javascript":"throw new Error('not executed');","css":""}}"#,
        wes_views::sdk_version(),
        manifest,
        types,
        package.digest
    )
}
#[tokio::test]
async fn installed_views_are_workspace_scoped_atomic_discoverable_and_captured_for_replay() {
    let (mut w, calls) = workspace();
    let source = artifact();
    let captured = source.clone();
    let text = ":package load path:\"badge.wes-view.json\"\n:view create TaskBadge > badge\n:list views > views\n:inspect view:TaskBadge > schema";
    let p = plan(&w, text, capture(move |_, _| Ok(captured.clone()))).await;
    assert_eq!(p.accepted().len(), 4, "{:?}", p.diagnostics());
    assert!(!w.view_catalogue().contains_key("TaskBadge"));
    let saved = p.record().unwrap().clone();
    p.commit(&mut w, Duration::ZERO).unwrap();
    run_all(&mut w).await;
    let package = w.view_catalogue().get("TaskBadge").unwrap();
    let digest = package.artifact.as_ref().unwrap().clone();
    assert!(w.view_catalogue().artifact(&digest).is_some());
    assert!(w.contracts().resolve("TaskBadge").is_ok());
    let listed = w.bindings().names().get("views").unwrap();
    let value = w.runtime().value_of(&listed.node).unwrap();
    assert_eq!(
        value.shape(),
        &Shape::List(Box::new(Shape::Primitive(wes_core::Primitive::Text)))
    );
    let Data::List(names) = value.data() else {
        panic!("view names")
    };
    assert!(names.contains(&Data::Text("TaskBadge".into())));
    assert_eq!(calls.load(Ordering::SeqCst), 0);
    let (empty, _) = workspace();
    assert!(!empty.view_catalogue().contains_key("TaskBadge"));
    let mut replay = wes_engine::workspace::ReplayWorkspace::new(empty).unwrap();
    let prepared = replay
        .prepare(&saved, CancellationToken::new())
        .await
        .unwrap();
    assert!(replay.apply(prepared).unwrap().effects.is_empty());
    assert!(
        replay
            .workspace()
            .view_catalogue()
            .artifact(&digest)
            .is_some()
    );
    let again = source.clone();
    let p = plan(
        &w,
        ":package load path:\"again.json\"",
        capture(move |_, _| Ok(again.clone())),
    )
    .await;
    assert_eq!(p.accepted().len(), 1);
    p.commit(&mut w, Duration::ZERO).unwrap();
    let changed = source.replace("not executed", "different code");
    let p = plan(
        &w,
        ":package load path:\"replacement.json\"",
        capture(move |_, _| Ok(changed.clone())),
    )
    .await;
    assert_eq!(p.accepted().len(), 0);
    assert!(w.view_catalogue().artifact(&digest).is_some());
}
#[tokio::test]
async fn importing_identical_workspace_types_is_allowed_but_conflicts_leave_no_view() {
    for (definition, expected) in [("Int", 1), ("Text", 0)] {
        let (mut w, _) = workspace();
        let p = w
            .prepare_type_package(
                &format!("types: {{TaskBadge: {{base: Record, fields: {{count: {definition}}}}}}}"),
                wes_language::Span::at(0),
            )
            .unwrap();
        w.commit(p).unwrap();
        let compiled = artifact();
        let p = plan(
            &w,
            ":package load path:\"badge.json\"",
            capture(move |_, _| Ok(compiled.clone())),
        )
        .await;
        assert_eq!(p.accepted().len(), expected);
        p.commit(&mut w, Duration::ZERO).unwrap();
        assert_eq!(w.view_catalogue().contains_key("TaskBadge"), expected == 1);
    }
}

#[tokio::test]
async fn creating_and_binding_literal_structures_use_the_same_captured_view_contract() {
    let (mut workspace, calls) = workspace();
    let compiled = artifact();
    let prepared = plan(&workspace, ":package load path:\"badge.wes-view.json\"\n:view create TaskBadge input:{count:7} > badge\n:view bind $badge input:{count:\"8\"}", capture(move |_, _| Ok(compiled.clone()))).await;
    assert_eq!(prepared.accepted().len(), 3, "{:?}", prepared.diagnostics());
    prepared.commit(&mut workspace, Duration::ZERO).unwrap();
    run_all(&mut workspace).await;
    let view = workspace.resolve("badge").unwrap();
    let frame = workspace.view_frame(&view.node).unwrap();
    let input = frame.instances[0].input.as_ref().unwrap();
    assert!(matches!(
        input.binding,
        wes_engine::views::InputBinding::Unlinked
    ));
    assert_eq!(
        input.value().unwrap().data(),
        &Data::Record([("count".into(), Data::Int(8))].into())
    );
    assert_eq!(calls.load(Ordering::SeqCst), 0);
    let refused = plan(
        &workspace,
        ":calc {return 1;} > x\n:view bind $badge input:{count:$x}",
        no_files(),
    )
    .await;
    assert_eq!(refused.accepted().len(), 1);
    assert!(
        refused
            .diagnostics()
            .diagnostics
            .iter()
            .any(|d| d.code == "VIE003")
    );
}

#[tokio::test]
async fn terminal_presentation_has_a_creation_gate_and_keeps_its_instance_on_source_refresh() {
    let (mut workspace, calls) = workspace();
    let compiled = artifact();
    let source = ":package load path:badge.json
:def badgeData(input:Int) -> TaskBadge as :calc { return {count:input}; }
:calc { return 7; } > raw | badgeData > mapped | :view create TaskBadge > shown";
    let prepared = plan(
        &workspace,
        source,
        capture(move |_, _| Ok(compiled.clone())),
    )
    .await;
    assert_eq!(prepared.accepted().len(), 3, "{:?}", prepared.diagnostics());
    let record = prepared.record().unwrap().clone();
    prepared.commit(&mut workspace, Duration::ZERO).unwrap();
    run_all(&mut workspace).await;
    let mapped = workspace.resolve("mapped").unwrap().node;
    let shown = workspace.resolve("shown").unwrap().node;
    let frame = workspace.view_frame(&shown).unwrap();
    let identity = frame.instances[0].identity.clone();
    assert_eq!(
        frame.instances[0]
            .input
            .as_ref()
            .unwrap()
            .source()
            .unwrap()
            .output
            .node,
        mapped
    );
    assert_eq!(
        workspace
            .runtime()
            .graph()
            .node(&shown)
            .unwrap()
            .dependencies()[&mapped],
        wes_engine::graph::OutputPort::Data
    );
    assert_eq!(
        workspace.runtime().dependency_lifetime(&shown),
        Some(wes_engine::runtime::DependencyLifetime::Creation)
    );
    let run = workspace.runtime().run_of(&shown).unwrap().clone();
    assert!(workspace.runtime().construction_complete(&shown));
    let parsed = wes_language::parse(&SourceText::new("refresh", ":refresh $raw"));
    let Preparation::Meta(meta) = workspace.prepare(&parsed.script.statements[0]).unwrap() else {
        panic!()
    };
    let control = workspace.prepare_control(meta).unwrap();
    let applied = workspace.apply_control(control, Duration::ZERO).unwrap();
    let mut work = VecDeque::from(applied.effects);
    while let Some(effect) = work.pop_front() {
        if let Effect::Spawn(ticket) = effect {
            let run = ticket.run.clone();
            let ticket = workspace.enter_ticket(ticket).unwrap().unwrap();
            let report = TaskExecutor::ephemeral()
                .execute(ticket, CancellationToken::new())
                .await;
            work.extend(workspace.complete(&run, report.outcome, Duration::ZERO));
        }
    }
    assert_eq!(
        workspace.view_frame(&shown).unwrap().instances[0].identity,
        identity
    );
    assert_eq!(workspace.runtime().run_of(&shown), Some(&run));
    assert_eq!(
        workspace.runtime().graph().node(&shown).unwrap().state(),
        NodeState::Ready
    );
    let parsed = wes_language::parse(&SourceText::new("repeat", ":refresh $shown"));
    let Preparation::Meta(meta) = workspace.prepare(&parsed.script.statements[0]).unwrap() else {
        panic!()
    };
    let control = workspace.prepare_control(meta).unwrap();
    assert!(workspace.apply_control(control, Duration::ZERO).is_err());
    assert_eq!(calls.load(Ordering::SeqCst), 0);
    let (base, _) = super::workspace();
    let mut replay = wes_engine::workspace::ReplayWorkspace::new(base).unwrap();
    let prepared = replay
        .prepare(&record, CancellationToken::new())
        .await
        .unwrap();
    assert!(replay.apply(prepared).unwrap().effects.is_empty());
    assert!(replay.finish().start(Duration::ZERO).is_empty());
}

#[tokio::test]
async fn malformed_presentation_stages_refuse_atomically_and_failed_sources_skip_creation() {
    let (mut workspace, calls) = workspace();
    let compiled = artifact();
    plan(
        &workspace,
        ":package load path:badge.json",
        capture(move |_, _| Ok(compiled.clone())),
    )
    .await
    .commit(&mut workspace, Duration::ZERO)
    .unwrap();
    let mut capability = Capability::new(["echo"], Shape::Unknown, Safety::Safe);
    capability.streaming = true;
    capability.parameters = vec![Parameter::new("value", Shape::Unknown, true)];
    workspace
        .register_provider(
            ProviderDescription::new("stream", [capability], vec![]).unwrap(),
            Arc::new(Echo(calls.clone())),
        )
        .unwrap();
    for source in [
        ":calc {return {count:7};} | :view create TaskBadge | :calc {return input;}",
        ":calc {return {count:7};} | :view create TaskBadge input:{count:8}",
        ":calc {return {count:7};} | @trace(http) :view create TaskBadge",
        "stream echo value:synthetic | :view create TaskBadge",
    ] {
        let prepared = plan(&workspace, source, no_files()).await;
        assert_eq!(
            prepared.nodes().count(),
            0,
            "{source}: {:?}",
            prepared.diagnostics()
        );
        assert!(prepared.record().is_none());
    }
    let prepared = plan(
        &workspace,
        ":calc {return 1 / 0;} | :view create TaskBadge > never",
        no_files(),
    )
    .await;
    assert_eq!(prepared.nodes().count(), 2, "{:?}", prepared.diagnostics());
    prepared.commit(&mut workspace, Duration::ZERO).unwrap();
    run_all(&mut workspace).await;
    let never = workspace.resolve("never").unwrap().node;
    assert_eq!(
        workspace.runtime().graph().node(&never).unwrap().state(),
        NodeState::Skipped
    );
    assert!(workspace.view_frame(&never).is_err());
    assert_eq!(calls.load(Ordering::SeqCst), 0);
}
