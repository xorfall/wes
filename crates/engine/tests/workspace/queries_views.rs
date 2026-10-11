use super::*;

#[tokio::test]
async fn binding_a_completed_value_during_refresh_captures_its_producing_run() {
    let (mut workspace, calls) = workspace();
    let source = commit(
        &mut workspace,
        ":calc { return {view:\"metric\", value:1}; } > sample",
    )
    .unwrap();
    run_all(&mut workspace).await;
    let old_run = workspace.runtime().value_run(&source).unwrap().clone();
    let pending = refresh(&mut workspace, &source);
    assert!(!pending.is_empty());
    assert_ne!(workspace.runtime().run_of(&source), Some(&old_run));
    let view = commit(&mut workspace, ":view create Metric input:$sample > chart").unwrap();
    run_all(&mut workspace).await;
    let frame = workspace.view_frame(&view).unwrap();
    let input = frame.instances[0].input.as_ref().unwrap();
    assert_eq!(input.source().unwrap().run(), Some(&old_run));
    assert_eq!(fields(input.value().unwrap())["value"], Data::Int(1));
    assert_eq!(calls.load(Ordering::SeqCst), 0);
    workspace.close();
}

#[tokio::test]
async fn invalid_view_inputs_keep_contract_issues_and_failed_rebinds_are_atomic() {
    let (mut workspace, calls) = workspace();
    commit(
        &mut workspace,
        ":calc { return {view: \"metric\", value: 1250}; } > sample",
    )
    .unwrap();
    commit(
        &mut workspace,
        ":calc { return {view: \"wrong-card\", value: \"bad\"}; } > invalid",
    )
    .unwrap();
    run_all(&mut workspace).await;
    let chart = commit(&mut workspace, ":view create Metric input:$sample > chart").unwrap();
    let create = commit(
        &mut workspace,
        ":view create Metric input:$invalid > rejected *> problem",
    )
    .unwrap();
    run_all(&mut workspace).await;
    let before = workspace.view_frame(&chart).unwrap().instances.remove(0);
    let bind = commit(
        &mut workspace,
        ":view bind $chart input:$invalid revision:0",
    )
    .unwrap();
    run_all(&mut workspace).await;
    for node in [create, bind] {
        let error = workspace.runtime().error_of(&node).unwrap();
        assert_eq!(error.code(), "TYP005");
        assert_eq!(error.message(), "View input does not satisfy Metric");
        assert_eq!(error.issues().len(), 2);
        assert!(error.issues().iter().any(|issue| issue.path == "/view"));
        assert!(error.issues().iter().any(|issue| issue.path == "/value"));
    }
    let read = commit(&mut workspace, ":read $problem").unwrap();
    run_all(&mut workspace).await;
    assert_eq!(
        fields(workspace.runtime().value_of(&read).unwrap())["code"],
        Data::Text("TYP005".into())
    );
    let after = workspace.view_frame(&chart).unwrap().instances.remove(0);
    assert_eq!(after.revision, before.revision);
    assert!(std::ptr::eq(
        after.input.unwrap().value().unwrap().data(),
        before.input.unwrap().value().unwrap().data()
    ));
    assert_eq!(calls.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn view_discovery_is_a_normal_result_without_renderer_or_provider_execution() {
    let (mut workspace, calls) = workspace();
    let listed = commit(&mut workspace, ":list views").unwrap();
    let inspected = commit(
        &mut workspace,
        ":inspect view:Timeline > timeline_definition",
    )
    .unwrap();
    let duration = commit(&mut workspace, ":inspect view:Metric").unwrap();
    let missing = commit(&mut workspace, ":inspect view:MissingView").unwrap();
    run_all(&mut workspace).await;
    assert_eq!(
        workspace.runtime().value_of(&listed).unwrap().shape(),
        &Shape::List(Box::new(Shape::Primitive(Primitive::Text)))
    );
    let Data::List(names) = workspace.runtime().value_of(&listed).unwrap().data() else {
        panic!("list")
    };
    assert!(names.contains(&Data::Text("Timeline".into())));
    assert!(names.contains(&Data::Text("Metric".into())));
    let d = fields(workspace.runtime().value_of(&inspected).unwrap());
    assert_eq!(d["name"], Data::Text("Timeline".into()));
    assert_eq!(d["outputScope"], Data::Text("instance".into()));
    assert_eq!(d["execution"], Data::Text("none".into()));
    assert!(workspace.runtime().value_of(&duration).is_some());
    assert!(workspace.runtime().value_of(&missing).is_none());
    assert_eq!(calls.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn removing_a_source_does_not_rebind_a_view_to_a_reused_name() {
    let (mut workspace, _) = workspace();
    commit(
        &mut workspace,
        ":calc { return {view: \"metric\", value: 1000}; } > sample",
    )
    .unwrap();
    run_all(&mut workspace).await;
    let view = commit(&mut workspace, ":view create Metric input:$sample > chart").unwrap();
    run_all(&mut workspace).await;
    assert!(workspace.view_frame(&view).is_ok());
    let Preparation::Meta(meta) = workspace
        .prepare(&statement(":remove $sample scope:downstream"))
        .unwrap()
    else {
        panic!("control")
    };
    let control = workspace.prepare_control(meta).unwrap();
    workspace.apply_control(control, Duration::ZERO).unwrap();
    commit(
        &mut workspace,
        ":calc { return {view: \"metric\", value: 9000}; } > sample",
    )
    .unwrap();
    run_all(&mut workspace).await;
    let unavailable = workspace.view_frame(&view).unwrap().instances.remove(0);
    assert!(unavailable.input.as_ref().unwrap().value().is_none());
    assert!(
        unavailable
            .input_problem
            .as_ref()
            .unwrap()
            .contains("removed")
    );
    commit(&mut workspace, ":view bind $chart input:$sample").unwrap();
    run_all(&mut workspace).await;
    assert_eq!(
        fields(
            workspace.view_frame(&view).unwrap().instances[0]
                .input
                .as_ref()
                .unwrap()
                .value()
                .unwrap()
        )["value"],
        Data::Int(9000)
    );
}

#[tokio::test]
async fn instances_are_normal_nodes_and_edits_update_the_original_view() {
    let (mut workspace, calls) = workspace();
    commit(
        &mut workspace,
        ":calc { return {view: \"metric\", value: 1250}; } > sample",
    )
    .unwrap();
    commit(
        &mut workspace,
        ":calc { return {view: \"dashboard\", title: \"Overview\"}; } > settings",
    )
    .unwrap();
    run_all(&mut workspace).await;
    let view = commit(&mut workspace, ":view create Metric input:$sample > chart").unwrap();
    let dashboard = commit(
        &mut workspace,
        ":view create Dashboard input:$settings > dashboard",
    )
    .unwrap();
    let unnamed = commit(&mut workspace, ":view create Metric").unwrap();
    run_all(&mut workspace).await;
    assert!(workspace.runtime().value_of(&unnamed).is_some());
    let value = workspace.runtime().value_of(&view).unwrap().clone();
    assert_eq!(
        value.shape(),
        &Shape::Meta(wes_core::MetaType::ViewInstance)
    );
    assert!(value.management_authority().is_some());
    let connected = commit(&mut workspace, ":view connect $chart to:$dashboard").unwrap();
    run_all(&mut workspace).await;
    assert!(
        workspace.runtime().value_of(&connected).is_some(),
        "{:?}",
        workspace.runtime().error_of(&connected)
    );
    let frame = workspace.view_frame(&dashboard).unwrap();
    assert_eq!(frame.instances.len(), 2);
    assert_eq!(frame.instances[0].members["members"], vec![view.clone()]);
    assert_eq!(workspace.view_frame(&connected).unwrap().root, dashboard);
    let inspected = commit(&mut workspace, ":inspect $dashboard").unwrap();
    run_all(&mut workspace).await;
    assert_eq!(
        match &fields(workspace.runtime().value_of(&inspected).unwrap())["view"] {
            Data::Record(view) => view["revision"].clone(),
            _ => panic!("view metadata"),
        },
        Data::Text("1".into())
    );
    commit(
        &mut workspace,
        ":calc { return {view: \"metric\", value: 2500}; } > next",
    )
    .unwrap();
    run_all(&mut workspace).await;
    let changed = commit(&mut workspace, ":view bind $chart input:$next revision:0").unwrap();
    run_all(&mut workspace).await;
    assert!(workspace.runtime().value_of(&changed).is_some());
    assert_eq!(
        workspace.view_frame(&view).unwrap().instances[0].revision,
        1
    );
    let conflict = commit(&mut workspace, ":view bind $chart input:$sample revision:0").unwrap();
    run_all(&mut workspace).await;
    assert!(workspace.runtime().error_of(&conflict).is_some());
    assert_eq!(
        workspace.view_frame(&view).unwrap().instances[0].revision,
        1
    );
    assert_eq!(calls.load(Ordering::SeqCst), 0);
}

#[tokio::test(start_paused = true)]
async fn view_mounts_share_observation_lifetime_and_expiry_never_restarts_sources() {
    use wes_engine::views::MountAction;
    let (mut workspace, calls) = workspace();
    commit(
        &mut workspace,
        ":calc { return {view: \"metric\", value: 1000}; } > sample",
    )
    .unwrap();
    run_all(&mut workspace).await;
    let node = commit(&mut workspace, ":view create Metric input:$sample > chart").unwrap();
    run_all(&mut workspace).await;
    let identity = workspace.view_frame(&node).unwrap().instances[0]
        .identity
        .to_string();
    for (command, active) in [(":view start $chart", true), (":view stop $chart", false)] {
        let receipt = commit(&mut workspace, command).unwrap();
        run_all(&mut workspace).await;
        assert!(workspace.runtime().error_of(&receipt).is_none());
        assert!(workspace.runtime().value_of(&receipt).is_some());
        assert_eq!(
            workspace.view_frame(&node).unwrap().instances[0].observing,
            active
        );
    }

    let a = workspace
        .view_mount(&node, &identity, MountAction::Open)
        .unwrap()
        .unwrap();
    let b = workspace
        .view_mount(&node, &identity, MountAction::Open)
        .unwrap()
        .unwrap();
    workspace
        .view_mount(&node, &identity, MountAction::Start)
        .unwrap();
    let frame = workspace.view_frame(&node).unwrap();
    assert!(frame.instances[0].observing);
    assert_eq!(frame.instances[0].revision, 0);
    workspace
        .view_mount(&node, &identity, MountAction::Close(a.clone()))
        .unwrap();
    assert!(workspace.view_frame(&node).unwrap().instances[0].observing);
    assert!(
        workspace
            .view_mount(&node, &identity, MountAction::Touch(a))
            .is_err()
    );
    workspace
        .view_mount(&node, &identity, MountAction::Close(b))
        .unwrap();
    assert!(!workspace.view_frame(&node).unwrap().instances[0].observing);
    let c = workspace
        .view_mount(&node, &identity, MountAction::Open)
        .unwrap()
        .unwrap();
    assert!(workspace.view_frame(&node).unwrap().instances[0].observing);
    tokio::time::advance(Duration::from_secs(4)).await;
    let expired = workspace
        .view_mount(&node, &identity, MountAction::Touch(c))
        .unwrap_err();
    assert!(expired.contains("display session expired or closed"));
    assert!(!expired.contains("does not belong to this workspace"));
    assert!(!workspace.view_frame(&node).unwrap().instances[0].observing);
    let reopened = workspace
        .view_mount(&node, &identity, MountAction::Open)
        .unwrap()
        .unwrap();
    assert!(workspace.view_frame(&node).unwrap().instances[0].observing);
    workspace
        .view_mount(&node, &identity, MountAction::Close(reopened))
        .unwrap();
    workspace
        .view_mount(&node, &identity, MountAction::Stop)
        .unwrap();
    let stopped = workspace
        .view_mount(&node, &identity, MountAction::Open)
        .unwrap()
        .unwrap();
    assert!(!workspace.view_frame(&node).unwrap().instances[0].observing);
    workspace
        .view_mount(&node, &identity, MountAction::Close(stopped))
        .unwrap();
    assert!(
        workspace
            .view_mount(&node, "different-instance", MountAction::Open)
            .is_err()
    );
    assert_eq!(calls.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn pin_refuses_an_existing_result_name_and_direct_execution_cannot_claim_retention() {
    let (mut workspace, calls) = workspace();
    let source = commit(
        &mut workspace,
        ":calc { return {view:\"metric\",value:4}; } > sample",
    )
    .unwrap();
    run_all(&mut workspace).await;
    let view = commit(&mut workspace, ":view create Metric input:$sample > chart").unwrap();
    run_all(&mut workspace).await;
    let existing = workspace.resolve("sample").unwrap();
    let refused = workspace
        .prepare(&statement(":view pin $chart > sample"))
        .unwrap_err();
    assert!(format!("{refused:?}").contains("NAM003"));
    assert_eq!(workspace.resolve("sample"), Some(existing));
    let pin = commit(&mut workspace, ":view pin $chart > pinned").unwrap();
    run_all(&mut workspace).await;
    assert!(workspace.runtime().value_of(&pin).is_none());
    assert!(
        workspace
            .runtime()
            .error_of(&pin)
            .unwrap()
            .message()
            .contains("storage owner")
    );
    assert_eq!(
        workspace.view_frame(&view).unwrap().instances[0].revision,
        0
    );
    assert!(workspace.runtime().value_of(&source).is_some());
    assert_eq!(calls.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn partial_public_frames_cannot_authorize_coordinated_state_after_source_classification_changes()
 {
    let (mut workspace, calls) = workspace();
    let source = commit(&mut workspace, "catalog echo value:0 > sample").unwrap();
    let first = ticket(workspace.start(Duration::ZERO));
    let first = workspace.enter_ticket(first).unwrap().unwrap();
    let range: wes_core::Interval = "2030-01-01T00:00:00Z/2030-01-01T01:00:00Z".parse().unwrap();
    let input = Value::new(
        wes_views::named("Timeline").unwrap().input().shape(),
        Data::Record(
            [
                ("view".into(), Data::Text("timeline".into())),
                ("id".into(), Data::Text("synthetic".into())),
                ("title".into(), Data::Text("Signals".into())),
                ("range".into(), Data::Interval(range)),
                ("coverage".into(), Data::Interval(range)),
                ("omitted".into(), Data::Int(0)),
                ("sourceError".into(), Data::Text("".into())),
                ("series".into(), Data::List(vec![])),
                ("events".into(), Data::List(vec![])),
            ]
            .into(),
        ),
        Provenance::default(),
    )
    .unwrap();
    workspace.complete(&first.run, Outcome::Produced(input.clone()), Duration::ZERO);
    let group = commit(&mut workspace, ":view create TimelineGroup > group").unwrap();
    let child = commit(
        &mut workspace,
        ":view create Timeline input:$sample > child",
    )
    .unwrap();
    run_all(&mut workspace).await;
    commit(&mut workspace, ":view connect $child to:$group");
    run_all(&mut workspace).await;
    let before = workspace.view_frame(&group).unwrap();
    let identity = before.instances[0].identity.clone();
    assert!(workspace.view_interaction(&group, &identity).is_ok());
    let changed = ticket(refresh(&mut workspace, &source));
    let changed = workspace.enter_ticket(changed).unwrap().unwrap();
    let restricted = input.with_provenance(Provenance::default().with_policy(
        &wes_core::flow::FlowPolicy::default().confidential(wes_core::flow::Residence::Retainable),
    ));
    workspace.complete(&changed.run, Outcome::Produced(restricted), Duration::ZERO);
    let after = workspace.view_frame(&group).unwrap();
    assert_ne!(before.authority_epoch, after.authority_epoch);
    assert!(
        after
            .instances
            .iter()
            .find(|v| v.id == child)
            .unwrap()
            .input
            .is_none()
    );
    assert!(workspace.view_interaction(&group, &identity).is_err());
    assert!(workspace.view_frame(&child).is_err());
    assert_eq!(calls.load(Ordering::SeqCst), 0);
}
