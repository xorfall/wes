use super::source_control::source_control;
use super::*;
use wes::retention::ManagementError;
async fn preview(app: &ApplicationHandle) -> wes::workspace_deletion::Preview {
    app.preview_workspace_deletion(app.current().unwrap().generation, "qa".into())
        .await
        .unwrap()
}
async fn delete(
    app: &ApplicationHandle,
    p: wes::workspace_deletion::Preview,
    stop: bool,
) -> Result<(), ManagementError> {
    tokio::time::timeout(
        Duration::from_secs(10),
        app.delete_workspace(
            app.current().unwrap().generation,
            "qa".into(),
            p.token,
            stop,
            false,
        ),
    )
    .await
    .unwrap()
}
#[tokio::test]
async fn deletion_is_pure_until_confirmed_invalidates_handles_and_last_workspace_stays_absent_on_restart()
 {
    let root = temp();
    let calls = Arc::new(AtomicUsize::new(0));
    let configuration = || {
        config(
            root.path(),
            calls.clone(),
            Arc::new(AtomicBool::new(false)),
            None,
        )
    };
    let (app, task) = wes::open(configuration()).await.unwrap();
    let old = app.bound("default").unwrap();
    let old_id = app.current().unwrap().identity;
    accepted(submit(&app, "qa", ":help").await.as_ref());
    idle(&app).await;
    let p = preview(&app).await;
    assert_eq!(p.cells, 1);
    assert!(p.blockers.is_empty());
    assert!(old.current().is_ok());
    assert_eq!(calls.load(Ordering::SeqCst), 0);
    delete(&app, p, false).await.unwrap();
    assert!(app.current().is_err());
    assert!(old.current().is_err());
    assert!(app.subscribe_sessions().borrow().is_empty());
    assert!(app.subscribe_workspace_names().borrow().is_empty());
    assert!(
        app.open_workspace(WorkspaceName::new("default".into()).unwrap(), false)
            .await
            .is_err()
    );
    app.shutdown().await;
    task.join().await.unwrap();
    let (app, task) = wes::open(configuration()).await.unwrap();
    assert!(app.current().is_err());
    assert!(app.subscribe_workspace_names().borrow().is_empty());
    let fresh = app
        .open_workspace(WorkspaceName::new("default".into()).unwrap(), true)
        .await
        .unwrap();
    assert_ne!(fresh.identity, old_id);
    assert!(
        app.open_workspace_identity(WorkspaceName::new("default".into()).unwrap(), false, old_id)
            .await
            .is_err()
    );
    assert!(fresh.session.observe().await.unwrap().cells.is_empty());
    assert!(
        old.submit(SourceInput::new("stale".into(), "catalog echo value:wrong".into()).unwrap())
            .await
            .is_err()
    );
    assert_eq!(calls.load(Ordering::SeqCst), 0);
    app.shutdown().await;
    task.join().await.unwrap();
}
#[tokio::test]
async fn stale_preview_refuses_and_deleting_workspace_removes_only_owned_sandboxes() {
    let root = temp();
    let (app, task) = wes::open(config_for_reopen(root.path())).await.unwrap();
    let peer = app
        .open_workspace(WorkspaceName::new("peer".into()).unwrap(), true)
        .await
        .unwrap();
    let bound = app.bound("peer").unwrap();
    accepted(
        submit(&bound, "peer", "catalog echo value:survivor > result")
            .await
            .as_ref(),
    );
    idle(&bound).await;
    let p = preview(&app).await;
    accepted(
        submit(&app, "changed", "catalog echo value:changed")
            .await
            .as_ref(),
    );
    idle(&app).await;
    assert!(matches!(
        delete(&app, p, false).await,
        Err(ManagementError::Stale)
    ));
    let sandbox = submit(
        &app,
        "sandbox",
        ":sandbox { catalog echo value:one > value } > dashboard",
    )
    .await;
    assert!(sandbox.sandbox.is_some());
    let p = preview(&app).await;
    assert_eq!(p.sandboxes, vec!["dashboard"]);
    assert!(delete(&app, p, false).await.is_err());
    let p = preview(&app).await;
    delete(&app, p, true).await.unwrap();
    assert_eq!(app.current().unwrap().generation, peer.generation);
    assert!(
        peer.session
            .snapshot()
            .await
            .unwrap()
            .names
            .contains_key("result")
    );
    let restored = app
        .open_workspace(WorkspaceName::new("default".into()).unwrap(), true)
        .await
        .unwrap();
    assert!(
        restored
            .session
            .sandbox_lifecycle()
            .await
            .unwrap()
            .definitions
            .is_empty()
    );
    app.shutdown().await;
    task.join().await.unwrap();
}
#[tokio::test]
async fn cleanup_failure_does_not_resurrect_and_unknown_files_are_preserved() {
    let root = temp();
    let configuration = || config_for_reopen(root.path());
    let (app, task) = wes::open(configuration()).await.unwrap();
    let folder = std::fs::read_dir(root.path().join("workspaces"))
        .unwrap()
        .map(|e| e.unwrap().path())
        .find(|p| {
            p.file_name()
                .unwrap()
                .to_str()
                .unwrap()
                .starts_with("generation-")
        })
        .unwrap();
    std::fs::write(folder.join("unowned"), b"preserve").unwrap();
    let p = preview(&app).await;
    assert!(matches!(
        delete(&app, p, false).await,
        Err(ManagementError::CleanupPending)
    ));
    assert!(app.current().is_err());
    assert_eq!(std::fs::read(folder.join("unowned")).unwrap(), b"preserve");
    assert!(
        app.open_workspace(WorkspaceName::new("default".into()).unwrap(), true)
            .await
            .is_err()
    );
    app.shutdown().await;
    task.join().await.unwrap();
    // Remove only our synthetic obstruction; startup then resumes the committed cleanup.
    std::fs::remove_file(folder.join("unowned")).unwrap();
    let (app, task) = wes::open(configuration()).await.unwrap();
    assert!(app.current().is_err());
    app.open_workspace(WorkspaceName::new("default".into()).unwrap(), true)
        .await
        .unwrap();
    app.shutdown().await;
    task.join().await.unwrap();
}

struct RunningCall {
    entered: tokio::sync::mpsc::UnboundedSender<()>,
    exited: Arc<AtomicBool>,
    uncertain: bool,
}
impl Invoker for RunningCall {
    fn invoke(&self, call: Call, cancelled: CancellationToken) -> InvocationFuture {
        let entered = self.entered.clone();
        let exited = self.exited.clone();
        let uncertain = self.uncertain;
        Box::pin(async move {
            let _ = entered.send(());
            cancelled.cancelled().await;
            exited.store(true, Ordering::SeqCst);
            if uncertain {
                Err(wes_engine::providers::InvocationError::Failed(
                    wes_core::ErrorValue::new(
                        wes_core::ErrorId::new("synthetic").unwrap(),
                        wes_core::ErrorValue::REMOTE_OUTCOME_UNKNOWN,
                        "Synthetic remote outcome unknown",
                        vec![],
                        None,
                    )
                    .unwrap(),
                ))
            } else {
                Ok(call.arguments["value"].clone())
            }
        })
    }
}
#[tokio::test]
async fn stop_and_delete_joins_entered_work_and_uncertain_outcomes_keep_the_workspace() {
    for (uncertain, sandbox) in [(false, false), (true, false), (true, true)] {
        let root = temp();
        let (entered, mut receive) = tokio::sync::mpsc::unbounded_channel();
        let exited = Arc::new(AtomicBool::new(false));
        let invoker = Arc::new(RunningCall {
            entered,
            exited: exited.clone(),
            uncertain,
        });
        let mut configuration = config_for_reopen(root.path());
        configuration.workspace = Arc::new(move || {
            let mut w =
                Workspace::local(wes_engine::providers::LocalScope::new("fixture").unwrap());
            let mut cap = Capability::new(["wait"], Shape::Unknown, Safety::Safe);
            cap.parameters
                .push(Parameter::new("value", Shape::Unknown, true));
            w.register_provider(
                ProviderDescription::new("qa", [cap], vec![]).unwrap(),
                invoker.clone(),
            )?;
            Ok(w)
        });
        let (app, task) = wes::open(configuration).await.unwrap();
        let source = if sandbox {
            ":sandbox { qa wait value:fixture > output } > preview"
        } else {
            "qa wait value:fixture > output"
        };
        let result = submit(&app, "running", source).await;
        if sandbox {
            assert!(result.sandbox.is_some());
        } else {
            accepted(result.as_ref());
        }
        receive.recv().await.unwrap();
        let p = preview(&app).await;
        assert!(!p.running.is_empty() || !p.running_sandboxes.is_empty());
        assert!(!exited.load(Ordering::SeqCst));
        assert!(delete(&app, p, false).await.is_err());
        assert!(!exited.load(Ordering::SeqCst));
        let p = preview(&app).await;
        let result = delete(&app, p, true).await;
        assert!(exited.load(Ordering::SeqCst));
        if uncertain {
            assert!(result.is_err());
            assert_eq!(app.current().unwrap().name.as_str(), "default");
            assert!(!preview(&app).await.blockers.is_empty());
        } else {
            result.unwrap();
            assert!(app.current().is_err());
        }
        app.shutdown().await;
        task.join().await.unwrap();
    }
}

#[tokio::test]
async fn typed_plan_read_inspect_and_apply_never_invalidate_their_own_snapshot() {
    use wes_core::MetaType;
    let root = temp();
    let (app, task) = wes::open(config_for_reopen(root.path())).await.unwrap();
    let before = app.current().unwrap();
    let reply = source_control(
        &app,
        "plan",
        "qa",
        include_str!("../../../../examples/workspace-deletion/plan.wes"),
    )
    .await
    .unwrap();
    assert_eq!(reply.nodes.len(), 1);
    assert!(reply.recorded);
    let value = before.session.snapshot().await.unwrap().execution.values[&reply.nodes[0]].clone();
    assert!(
        source_control(&app, "plan", "qa", ":workspace delete $plan")
            .await
            .is_err()
    );
    let retry = source_control(
        &app,
        "plan",
        "qa",
        include_str!("../../../../examples/workspace-deletion/plan.wes"),
    )
    .await
    .unwrap();
    assert_eq!(
        retry.nodes, reply.nodes,
        "a conflicting destructive request must not overwrite or poison the original cell"
    );

    assert_eq!(value.shape(), &Shape::Meta(MetaType::WorkspaceDeletePlan));
    let Data::Record(fields) = value.data() else {
        panic!("structured projection")
    };
    assert_eq!(fields["workspace"], Data::Text("default".into()));
    assert!(
        !fields.contains_key("token"),
        "projection must not export the capability"
    );
    for (id, command) in [
        ("read", ":read $plan"),
        (
            "inspect",
            include_str!("../../../../examples/workspace-deletion/inspect.wes"),
        ),
    ] {
        let read = source_control(&app, id, "qa", command).await.unwrap();
        assert_eq!(read.nodes.len(), 1);
        if id == "read" {
            assert_eq!(
                before.session.snapshot().await.unwrap().execution.values[&read.nodes[0]],
                value
            );
        }
    }
    assert_eq!(before.session.observe().await.unwrap().cells.len(), 3);
    // Normal submission dedup returns the original node and never creates a second plan.
    let repeated = source_control(
        &app,
        "plan",
        "qa",
        include_str!("../../../../examples/workspace-deletion/plan.wes"),
    )
    .await
    .unwrap();
    assert_eq!(repeated.nodes, reply.nodes);
    let deleted = source_control(
        &app,
        "delete",
        "qa",
        include_str!("../../../../examples/workspace-deletion/delete.wes"),
    )
    .await
    .unwrap();
    assert!(!deleted.accepted.is_empty());
    assert!(app.current().is_err());
    assert!(before.session.observe().await.is_err());
    app.shutdown().await;
    task.join().await.unwrap();
}
#[tokio::test]
async fn plans_reject_wrong_types_clients_and_mutation_and_can_be_replanned() {
    let root = temp();
    let (app, task) = wes::open(config_for_reopen(root.path())).await.unwrap();
    accepted(
        submit(&app, "output", "catalog echo value:fake > ordinary")
            .await
            .as_ref(),
    );
    idle(&app).await;
    for (id, text) in [
        ("ordinary", ":workspace delete $ordinary"),
        ("literal", ":workspace delete \"token\""),
        ("missing", ":workspace delete $missing"),
    ] {
        assert!(
            source_control(&app, id, "qa", text).await.is_err(),
            "{text}"
        );
        assert!(app.current().is_ok());
    }
    source_control(&app, "plan", "qa", ":workspace plan delete > proposed")
        .await
        .unwrap();
    source_control(&app, "other-read", "other", ":inspect $proposed")
        .await
        .unwrap();
    assert!(
        source_control(&app, "other-delete", "other", ":workspace delete $proposed")
            .await
            .is_err()
    );
    accepted(
        submit(&app, "mutation", "catalog echo value:changed > changed")
            .await
            .as_ref(),
    );
    idle(&app).await;
    let stale = source_control(&app, "stale", "qa", ":workspace delete $proposed")
        .await
        .unwrap_err();
    assert!(stale.to_string().contains("STO"), "{stale}");
    assert!(
        source_control(&app, "consumed", "qa", ":workspace delete $proposed")
            .await
            .is_err()
    );
    source_control(
        &app,
        "fresh-plan",
        "qa",
        ":workspace plan delete > proposed",
    )
    .await
    .unwrap();
    source_control(&app, "fresh-delete", "qa", ":workspace delete $proposed")
        .await
        .unwrap();
    assert!(app.current().is_err());
    app.shutdown().await;
    task.join().await.unwrap();
}
#[tokio::test]
async fn plans_are_not_restored_and_source_never_grants_cooperative_deletion_authority() {
    let root = temp();
    let config = || config_for_reopen(root.path());
    let (app, task) = wes::open(config()).await.unwrap();
    source_control(&app, "plan", "qa", ":workspace plan delete > proposed")
        .await
        .unwrap();
    for text in [
        ":workspace plan delete > agent_plan",
        ":workspace delete $proposed",
    ] {
        let reply = app
            .submit(
                SourceInput::new(format!("agent-{text}"), text.into())
                    .unwrap()
                    .with_client("agent".into())
                    .unwrap()
                    .cooperative(),
            )
            .await;
        if let Ok(reply) = reply {
            idle(&app).await;
            let snapshot = app.current().unwrap().session.snapshot().await.unwrap();
            assert!(
                reply
                    .nodes
                    .iter()
                    .all(|id| !snapshot.execution.values.contains_key(id))
            );
        }
    }
    assert!(app.current().is_ok());
    app.shutdown().await;
    task.join().await.unwrap();
    let (app, task) = wes::open(config()).await.unwrap();
    assert!(
        source_control(&app, "gone", "qa", ":workspace delete $proposed")
            .await
            .is_err()
    );
    let restored = app.current().unwrap().session.observe().await.unwrap();
    assert!(restored.cells.iter().any(|c| c.input.cell() == "plan"));
    app.shutdown().await;
    task.join().await.unwrap();
}

#[tokio::test]
async fn source_stop_consent_joins_work_and_preserves_uncertain_outcomes() {
    for uncertain in [false, true] {
        let root = temp();
        let (entered, mut receive) = tokio::sync::mpsc::unbounded_channel();
        let exited = Arc::new(AtomicBool::new(false));
        let invoker = Arc::new(RunningCall {
            entered,
            exited: exited.clone(),
            uncertain,
        });
        let mut configuration = config_for_reopen(root.path());
        configuration.workspace = Arc::new(move || {
            let mut w =
                Workspace::local(wes_engine::providers::LocalScope::new("fixture").unwrap());
            let mut cap = Capability::new(["wait"], Shape::Unknown, Safety::Safe);
            cap.parameters
                .push(Parameter::new("value", Shape::Unknown, true));
            w.register_provider(
                ProviderDescription::new("qa", [cap], vec![]).unwrap(),
                invoker.clone(),
            )?;
            Ok(w)
        });
        let (app, task) = wes::open(configuration).await.unwrap();
        accepted(
            submit(&app, "running", "qa wait value:fixture > output")
                .await
                .as_ref(),
        );
        receive.recv().await.unwrap();
        source_control(&app, "plan", "qa", ":workspace plan delete > plan")
            .await
            .unwrap();
        assert!(
            source_control(&app, "refuse-stop", "qa", ":workspace delete $plan")
                .await
                .is_err()
        );
        assert!(!exited.load(Ordering::SeqCst));
        source_control(&app, "replan", "qa", ":workspace plan delete > plan")
            .await
            .unwrap();
        let result = source_control(&app, "stop", "qa", ":workspace delete $plan stop:true").await;
        assert!(exited.load(Ordering::SeqCst));
        if uncertain {
            assert!(result.is_err());
            assert!(app.current().is_ok());
        } else {
            result.unwrap();
            assert!(app.current().is_err());
        }
        app.shutdown().await;
        task.join().await.unwrap();
    }
}

#[tokio::test]
async fn workspace_source_plan_expiry_does_not_refresh_silently() {
    let root = temp();
    let (app, task) = wes::open(config_for_reopen(root.path())).await.unwrap();
    source_control(&app, "plan", "qa", ":workspace plan delete > plan")
        .await
        .unwrap();
    // The application's monotonic clock shares Tokio's test clock. Expiry
    // must not require a two-minute real-time wait.
    tokio::time::pause();
    tokio::time::advance(Duration::from_secs(121)).await;
    let error = source_control(&app, "expired", "qa", ":workspace delete $plan")
        .await
        .unwrap_err();
    assert!(error.to_string().contains("STO003"), "{error}");
    assert!(app.current().is_ok());
    app.shutdown().await;
    task.join().await.unwrap();
}

#[tokio::test]
async fn explicit_saved_target_is_inspected_without_execution_or_selection_change() {
    let root = temp();
    let calls = Arc::new(AtomicUsize::new(0));
    let (app, task) = wes::open(config(
        root.path(),
        calls.clone(),
        Arc::new(AtomicBool::new(false)),
        None,
    ))
    .await
    .unwrap();
    accepted(
        submit(&app, "value", "catalog echo value:survivor > result")
            .await
            .as_ref(),
    );
    idle(&app).await;
    accepted(
        submit(&app, "save-target", ":workspace save \"demo\"")
            .await
            .as_ref(),
    );
    idle(&app).await;
    let issuer = app.current().unwrap();
    assert!(
        app.bound("demo").is_err(),
        "saved target should not be open yet"
    );
    let reply = source_control(
        &app,
        "target-plan",
        "qa",
        include_str!("../../../../examples/workspace-deletion/plan-target.wes"),
    )
    .await
    .unwrap();
    let snapshot = issuer.session.snapshot().await.unwrap();
    let Data::Record(fields) = snapshot.execution.values[&reply.nodes[0]].data() else {
        panic!("plan projection")
    };
    assert_eq!(fields["workspace"], Data::Text("demo".into()));
    assert_eq!(app.current().unwrap().generation, issuer.generation);
    assert_eq!(
        calls.load(Ordering::SeqCst),
        1,
        "opening for inspection must not invoke the provider"
    );
    assert!(app.bound("demo").is_ok());
    source_control(&app, "inspect-target", "qa", ":inspect $plan")
        .await
        .unwrap();
    source_control(&app, "delete-target", "qa", ":workspace delete $plan")
        .await
        .unwrap();
    assert_eq!(app.current().unwrap().generation, issuer.generation);
    assert!(
        issuer
            .session
            .snapshot()
            .await
            .unwrap()
            .names
            .contains_key("result")
    );
    assert!(
        !app.subscribe_workspace_names()
            .borrow()
            .iter()
            .any(|n| n.as_str() == "demo")
    );
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    app.shutdown().await;
    task.join().await.unwrap();
}

#[tokio::test]
async fn explicit_target_survives_unrelated_peer_history_and_selection_changes() {
    let root = temp();
    let (app, task) = wes::open(config_for_reopen(root.path())).await.unwrap();
    let issuer = app.bound("default").unwrap();
    let target = app
        .open_workspace(WorkspaceName::new("demo".into()).unwrap(), true)
        .await
        .unwrap();
    let other = app
        .open_workspace(WorkspaceName::new("other".into()).unwrap(), true)
        .await
        .unwrap();
    source_control(
        &issuer,
        "plan",
        "qa",
        ":workspace plan delete workspace:\"demo\" > plan",
    )
    .await
    .unwrap();
    accepted(
        submit(&app, "switch", ":workspace load \"other\"")
            .await
            .as_ref(),
    );
    idle(&issuer).await;
    assert_eq!(app.current().unwrap().generation, other.generation);
    // The name remains in its issuing workspace, rather than becoming a portable bearer token.
    assert!(
        source_control(&app, "foreign", "qa", ":workspace delete $plan")
            .await
            .is_err()
    );
    // Loading another peer changes history, but neither deletion effects nor shared references.
    source_control(&issuer, "delete", "qa", ":workspace delete $plan")
        .await
        .unwrap();
    assert!(target.session.observe().await.is_err());
    assert_eq!(app.current().unwrap().generation, other.generation);
    assert!(issuer.current().is_ok());
    app.shutdown().await;
    task.join().await.unwrap();
}

#[tokio::test]
async fn explicit_targets_refuse_missing_names_and_changed_target_state() {
    let root = temp();
    let (app, task) = wes::open(config_for_reopen(root.path())).await.unwrap();
    let original = app.current().unwrap();
    for (id, text) in [
        (
            "absent",
            ":workspace plan delete workspace:\"absent\" > plan",
        ),
        (
            "invalid",
            ":workspace plan delete workspace:\"../outside\" > plan",
        ),
    ] {
        assert!(source_control(&app, id, "qa", text).await.is_err());
        assert_eq!(app.current().unwrap().generation, original.generation);
    }
    assert_eq!(
        app.subscribe_workspace_names().borrow().len(),
        1,
        "missing target was not created"
    );
    let target = app
        .open_workspace(WorkspaceName::new("demo".into()).unwrap(), true)
        .await
        .unwrap();
    source_control(
        &app,
        "plan",
        "qa",
        ":workspace plan delete workspace:\"demo\" > plan",
    )
    .await
    .unwrap();
    let bound = app.bound("demo").unwrap();
    accepted(
        submit(&bound, "target-change", "catalog echo value:changed")
            .await
            .as_ref(),
    );
    idle(&bound).await;
    assert!(
        source_control(&app, "changed", "qa", ":workspace delete $plan")
            .await
            .is_err()
    );
    assert_eq!(app.current().unwrap().generation, original.generation);
    assert!(target.session.observe().await.is_ok());
    app.shutdown().await;
    task.join().await.unwrap();
}

#[tokio::test]
async fn captured_target_identity_cannot_delete_a_recreated_workspace_with_the_same_name() {
    let root = temp();
    let (app, task) = wes::open(config_for_reopen(root.path())).await.unwrap();
    let issuer = app.current().unwrap();
    let target = app
        .open_workspace(WorkspaceName::new("demo".into()).unwrap(), true)
        .await
        .unwrap();
    source_control(
        &app,
        "plan",
        "qa",
        ":workspace plan delete workspace:\"demo\" > plan",
    )
    .await
    .unwrap();
    let target_handle = app.bound("demo").unwrap();
    let preview = target_handle
        .preview_workspace_deletion(target.generation.clone(), "another-user".into())
        .await
        .unwrap();
    target_handle
        .delete_workspace(
            target.generation,
            "another-user".into(),
            preview.token,
            false,
            false,
        )
        .await
        .unwrap();
    let recreated = app
        .open_workspace(WorkspaceName::new("demo".into()).unwrap(), true)
        .await
        .unwrap();
    assert_ne!(target.identity, recreated.identity);
    let error = source_control(&app, "old-plan", "qa", ":workspace delete $plan")
        .await
        .unwrap_err();
    assert!(error.to_string().contains("STO003"), "{error}");
    assert_eq!(app.current().unwrap().generation, issuer.generation);
    assert!(recreated.session.observe().await.is_ok());
    app.shutdown().await;
    task.join().await.unwrap();
}

#[tokio::test]
async fn unnamed_plan_is_a_cell_node_and_data_boundaries_do_not_call_providers() {
    let root = temp();
    let calls = Arc::new(AtomicUsize::new(0));
    let (app, task) = wes::open(config(
        root.path(),
        calls.clone(),
        Arc::new(AtomicBool::new(false)),
        None,
    ))
    .await
    .unwrap();
    let target = app
        .open_workspace(WorkspaceName::new("demo".into()).unwrap(), true)
        .await
        .unwrap();
    let reply = source_control(
        &app,
        "unnamed",
        "qa",
        include_str!("../../../../examples/workspace-deletion/plan-unnamed.wes"),
    )
    .await
    .unwrap();
    assert_eq!(reply.nodes.len(), 1);
    let node = &reply.nodes[0];
    let session = app.current().unwrap().session;
    let observed = session.observe().await.unwrap();
    assert!(observed.cells.iter().any(|c| c.input.cell() == "unnamed"));
    let plan = observed.state.execution.values[node].clone();
    assert!(plan.management_authority().is_some());
    let bytes = wes_adapters::codec::encode_display_value(&plan, Limits::default()).unwrap();
    assert!(!String::from_utf8_lossy(&bytes).contains(plan.management_authority().unwrap()));
    for (id, command) in [
        ("direct-data", format!("catalog echo value:${node}")),
        ("cast", format!(":type check ${node} as:\"Unknown\"")),
    ] {
        assert!(
            source_control(&app, id, "qa", &command).await.is_err(),
            "{command}"
        );
    }
    let read = source_control(&app, "read-alias", "qa", &format!(":read ${node} > copy"))
        .await
        .unwrap();
    assert_eq!(read.nodes.len(), 1);
    assert!(
        source_control(&app, "copied-data", "qa", "catalog echo value:$copy")
            .await
            .is_err()
    );
    assert_eq!(calls.load(Ordering::SeqCst), 0);
    // Failed non-observation commands can legitimately stale a preview; refresh issues a new one.
    source_control(&app, "refresh", "qa", &format!(":refresh ${node}"))
        .await
        .unwrap();
    source_control(&app, "inspect", "qa", &format!(":inspect ${node}"))
        .await
        .unwrap();
    source_control(&app, "delete", "qa", &format!(":workspace delete ${node}"))
        .await
        .unwrap();
    assert!(target.session.observe().await.is_err());
    assert!(
        session
            .observe()
            .await
            .unwrap()
            .cells
            .iter()
            .any(|c| c.input.cell() == "delete")
    );
    app.shutdown().await;
    task.join().await.unwrap();
}

#[tokio::test]
async fn retained_plan_reopens_as_a_cell_without_authority_and_explicit_refresh_replans() {
    let root = temp();
    let calls = Arc::new(AtomicUsize::new(0));
    let values = TieredValues::open(
        &root.path().join("live"),
        &root.path().join("archive"),
        Limits::default(),
        Durability::File,
        None,
    )
    .unwrap();
    let (worker, writer) = spawn_store(values, StoreWorkerLimits::default()).unwrap();
    let configuration = || {
        config(
            root.path(),
            calls.clone(),
            Arc::new(AtomicBool::new(false)),
            Some(SessionStorage {
                worker: worker.clone(),
                auto_keep: AutoKeep::default(),
            }),
        )
    };
    let (app, task) = wes::open(configuration()).await.unwrap();
    app.open_workspace(WorkspaceName::new("demo".into()).unwrap(), true)
        .await
        .unwrap();
    let reply = source_control(
        &app,
        "plan-cell",
        "qa",
        ":workspace plan delete workspace:\"demo\" > plan",
    )
    .await
    .unwrap();
    idle(&app).await;
    let node = reply.nodes[0].clone();
    let observation = app.current().unwrap().session.observe().await.unwrap();
    let handle = observation.values.unwrap().outputs[&node]
        .handle()
        .unwrap()
        .clone();
    app.current()
        .unwrap()
        .session
        .keep(handle.clone())
        .await
        .unwrap();
    app.shutdown().await;
    task.join().await.unwrap();
    let (app, task) = wes::open(configuration()).await.unwrap();
    let observation = app.current().unwrap().session.observe().await.unwrap();
    assert!(
        observation
            .cells
            .iter()
            .any(|c| c.input.cell() == "plan-cell")
    );
    let value = &observation.state.execution.values[&node];
    assert_eq!(
        value.shape(),
        &Shape::Meta(wes_core::MetaType::WorkspaceDeletePlan)
    );
    assert!(value.management_authority().is_none());
    assert!(
        source_control(&app, "old", "qa", ":workspace delete $plan")
            .await
            .is_err()
    );
    source_control(&app, "refresh", "qa", ":refresh $plan")
        .await
        .unwrap();
    let refreshed = app.current().unwrap().session.observe().await.unwrap();
    assert_eq!(
        refreshed.state.execution.graph.node(&node).unwrap().state(),
        wes_engine::graph::NodeState::Ready
    );
    assert_ne!(
        refreshed.state.execution.runs.get(&node),
        observation.state.execution.runs.get(&node)
    );
    assert!(
        refreshed.state.execution.values[&node]
            .management_authority()
            .is_some()
    );
    source_control(&app, "inspect", "qa", ":inspect $plan")
        .await
        .unwrap();
    source_control(&app, "apply", "qa", ":workspace delete $plan")
        .await
        .unwrap();
    assert!(app.current().is_ok());
    assert_eq!(calls.load(Ordering::SeqCst), 0);
    assert!(
        worker.read(handle).await.unwrap().is_some(),
        "issuer's protected history survives deleting a different target"
    );
    app.shutdown().await;
    task.join().await.unwrap();
    worker.shutdown().await.unwrap();
    writer.join().await.unwrap();
}
