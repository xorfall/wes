use super::*;

#[tokio::test]
async fn opened_names_are_isolated_reused_and_joined_without_changing_default() {
    let root = temp();
    let (app, task) = wes::open(config_for_reopen(root.path())).await.unwrap();
    let default = app.current().unwrap();
    let name = WorkspaceName::new("second".into()).unwrap();
    assert!(app.open_workspace(name.clone(), false).await.is_err());
    let second = app.open_workspace(name.clone(), true).await.unwrap();
    let same = app.open_workspace(name, true).await.unwrap();
    assert_eq!(second.generation, same.generation);
    assert_eq!(app.current().unwrap().generation, default.generation);
    let bound = app.bound("second").unwrap();
    accepted(
        submit(&bound, "same-cell", "catalog echo value:second > local")
            .await
            .as_ref(),
    );
    accepted(
        submit(&app, "same-cell", "catalog echo value:first > local")
            .await
            .as_ref(),
    );
    idle(&bound).await;
    idle(&app).await;
    let first = default.session.snapshot().await.unwrap();
    let second_state = second.session.snapshot().await.unwrap();
    assert_eq!(
        first.execution.values.values().next().unwrap().data(),
        &Data::Text("first".into())
    );
    assert_eq!(
        second_state
            .execution
            .values
            .values()
            .next()
            .unwrap()
            .data(),
        &Data::Text("second".into())
    );
    assert!(
        submit(&bound, "foreign-load", ":workspace load \"default\"")
            .await
            .accepted
            .is_empty()
    );
    assert!(
        submit(&bound, "collision", ":workspace save \"default\"")
            .await
            .accepted
            .is_empty()
    );
    assert_eq!(app.current().unwrap().generation, default.generation);
    accepted(
        submit(&app, "select-second", ":workspace load \"second\"")
            .await
            .as_ref(),
    );
    assert_eq!(app.current().unwrap().generation, second.generation);
    assert_eq!(
        app.bound("default").unwrap().current().unwrap().generation,
        default.generation
    );
    accepted(
        submit(&bound, "reload", ":workspace load \"second\"")
            .await
            .as_ref(),
    );
    let reloaded = bound.current().unwrap();
    assert_ne!(reloaded.generation, second.generation);
    assert!(app.session_for_generation(&second.generation).is_err());
    assert!(app.session_for_generation(&default.generation).is_ok());
    app.shutdown().await;
    task.join().await.unwrap();
    assert!(bound.current().is_err());
    assert!(app.session_for_generation(&default.generation).is_err());
    for name in ["default", "second"] {
        let mut configuration = config_for_reopen(root.path());
        configuration.initial = WorkspaceName::new(name.into()).unwrap();
        let (app, task) = wes::open(configuration).await.unwrap();
        assert!(
            app.current()
                .unwrap()
                .session
                .snapshot()
                .await
                .unwrap()
                .names
                .contains_key("local")
        );
        app.shutdown().await;
        task.join().await.unwrap();
    }
}

#[tokio::test]
async fn live_session_capacity_refuses_new_names_but_reuses_existing_ones() {
    let root = temp();
    let (app, task) = wes::open(config_for_reopen(root.path())).await.unwrap();
    for index in 1..16 {
        app.open_workspace(WorkspaceName::new(format!("peer-{index}")).unwrap(), true)
            .await
            .unwrap();
    }
    assert!(matches!(
        app.open_workspace(WorkspaceName::new("overflow".into()).unwrap(), true)
            .await,
        Err(wes::ApplicationError::Capacity)
    ));
    app.open_workspace(WorkspaceName::new("peer-1".into()).unwrap(), true)
        .await
        .unwrap();
    assert_eq!(app.subscribe_sessions().borrow().len(), 16);
    accepted(
        submit(&app, "still-usable", "catalog echo value:ok")
            .await
            .as_ref(),
    );
    app.shutdown().await;
    task.join().await.unwrap();
}

struct Gated {
    entered: tokio::sync::mpsc::UnboundedSender<String>,
    permits: Arc<tokio::sync::Semaphore>,
}
impl Invoker for Gated {
    fn invoke(&self, call: Call, cancelled: CancellationToken) -> InvocationFuture {
        let value = call.arguments["value"].clone();
        let Data::Text(label) = value.data() else {
            panic!("synthetic text argument")
        };
        self.entered.send(label.to_string()).unwrap();
        let permits = self.permits.clone();
        Box::pin(async move {
            tokio::select! {
                _ = cancelled.cancelled() => {},
                permit = permits.acquire_owned() => { permit.unwrap().forget(); },
            }
            Ok(value)
        })
    }
}

#[tokio::test]
async fn provider_capacity_is_shared_and_hidden_running_sessions_are_joined() {
    let root = temp();
    let (entered, mut entries) = tokio::sync::mpsc::unbounded_channel();
    let permits = Arc::new(tokio::sync::Semaphore::new(0));
    let invoker = Arc::new(Gated {
        entered,
        permits: permits.clone(),
    });
    let mut configuration = config_for_reopen(root.path());
    configuration.concurrency = NonZeroUsize::new(1).unwrap();
    configuration.workspace = Arc::new(move || {
        let mut workspace =
            Workspace::local(wes_engine::providers::LocalScope::new("fixture").unwrap());
        let mut capability = Capability::new(["echo"], Shape::Unknown, Safety::Safe);
        capability
            .parameters
            .push(Parameter::new("value", Shape::Unknown, true));
        workspace.register_provider(
            ProviderDescription::new("catalog", [capability], vec![]).unwrap(),
            invoker.clone(),
        )?;
        Ok(workspace)
    });
    let (app, task) = wes::open(configuration).await.unwrap();
    app.open_workspace(WorkspaceName::new("peer".into()).unwrap(), true)
        .await
        .unwrap();
    let peer = app.bound("peer").unwrap();
    accepted(
        submit(&app, "first", "catalog echo value:first")
            .await
            .as_ref(),
    );
    assert_eq!(entries.recv().await.unwrap(), "first");
    assert_eq!(app.subscribe_capacity().borrow().operations.used, 1);
    assert_eq!(
        *app.subscribe_capacity().borrow(),
        *peer.subscribe_capacity().borrow()
    );
    accepted(
        submit(&peer, "second", "catalog echo value:second")
            .await
            .as_ref(),
    );
    assert!(
        tokio::time::timeout(Duration::from_millis(30), entries.recv())
            .await
            .is_err()
    );
    permits.add_permits(1);
    assert_eq!(
        tokio::time::timeout(Duration::from_secs(5), entries.recv())
            .await
            .unwrap()
            .unwrap(),
        "second"
    );
    // No pane owns this handle now; the application still cancels and joins its provider.
    let capacity = app.subscribe_capacity();
    assert_eq!(capacity.borrow().operations.used, 1);
    drop(peer);
    app.shutdown().await;
    tokio::time::timeout(Duration::from_secs(5), task.join())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(capacity.borrow().operations.used, 0);
}

#[tokio::test]
async fn concurrent_workspace_example_runs_actual_files_and_restores_without_execution() {
    let root = temp();
    let (app, task) = wes::open(config_for_reopen(root.path())).await.unwrap();
    for (name, source, expected) in [
        (
            "left-demo",
            include_str!("../../../../examples/concurrent-workspaces/left.wes"),
            "left",
        ),
        (
            "right-demo",
            include_str!("../../../../examples/concurrent-workspaces/right.wes"),
            "right",
        ),
    ] {
        let opened = app
            .open_workspace(WorkspaceName::new(name.into()).unwrap(), true)
            .await
            .unwrap();
        assert_eq!(
            app.open_workspace(WorkspaceName::new(name.into()).unwrap(), true)
                .await
                .unwrap()
                .generation,
            opened.generation
        );
        let bound = app.bound(name).unwrap();
        for (index, line) in source
            .lines()
            .filter(|line| !line.trim().is_empty())
            .enumerate()
        {
            accepted(
                submit(&bound, &format!("example-{index}"), line)
                    .await
                    .as_ref(),
            );
        }
        idle(&bound).await;
        let state = bound.current().unwrap().session.snapshot().await.unwrap();
        assert!(state.names.contains_key("shared"));
        assert_eq!(
            state.execution.values.values().next().unwrap().data(),
            &Data::Text(expected.into())
        );
    }
    assert!(
        app.current()
            .unwrap()
            .session
            .snapshot()
            .await
            .unwrap()
            .names
            .is_empty()
    );
    app.shutdown().await;
    task.join().await.unwrap();
    for name in ["left-demo", "right-demo"] {
        let mut config = config_for_reopen(root.path());
        config.initial = WorkspaceName::new(name.into()).unwrap();
        let (app, task) = wes::open(config).await.unwrap();
        let current = app.current().unwrap();
        let observed = current.session.observe().await.unwrap();
        assert!(observed.state.names.contains_key("shared"));
        assert_eq!(observed.cells.len(), 1);
        assert!(
            observed.state.execution.values.is_empty(),
            "restoration must not execute recipes"
        );
        app.shutdown().await;
        task.join().await.unwrap();
    }
}

#[tokio::test]
async fn bound_management_refuses_a_neighbor_generation_before_admission() {
    use wes::retention::ManagementError;
    let root = temp();
    let (app, task) = wes::open(config_for_reopen(root.path())).await.unwrap();
    let original = app.current().unwrap().generation;
    app.open_workspace(WorkspaceName::new("neighbor".into()).unwrap(), true)
        .await
        .unwrap();
    let bound = app.bound("neighbor").unwrap();
    assert!(matches!(
        bound
            .work_history(original.clone(), "client".into(), "cell".into(), None)
            .await,
        Err(ManagementError::Stale)
    ));
    assert!(matches!(
        bound
            .protect_run(
                original.clone(),
                "client".into(),
                "cell".into(),
                "run".into()
            )
            .await,
        Err(ManagementError::Stale)
    ));
    assert!(matches!(
        bound
            .preview_delete_work(original.clone(), "client".into(), "cell".into())
            .await,
        Err(ManagementError::Stale)
    ));
    assert!(matches!(
        bound
            .delete_work(
                original.clone(),
                "client".into(),
                "token".into(),
                false,
                false
            )
            .await,
        Err(ManagementError::Stale)
    ));
    assert!(matches!(
        bound
            .confirm_release(original, "client".into(), "token".into())
            .await,
        Err(ManagementError::Stale)
    ));
    app.shutdown().await;
    task.join().await.unwrap();
}
