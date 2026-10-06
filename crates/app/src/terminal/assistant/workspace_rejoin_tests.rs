#[tokio::test(flavor = "multi_thread")]
async fn workspace_rejoin_after_restart_preserves_work_without_replay_or_restored_authority() {
    let root = tempfile::tempdir().unwrap();
    let home = root.path().join("home");
    let history = uuid::Uuid::new_v4().to_string();
    let runtime = launch(RuntimeOptions::new(home.clone(), root.path().into()))
        .await
        .unwrap();
    let manager = Manager::default();
    let terminal = test_terminal(&manager, &runtime, &home, root.path(), &history).await;
    let _stop_on_failure = terminal.stopped.clone().drop_guard();
    let joined = invoke(
        &terminal,
        &runtime.handle,
        "workspace_open",
        json!({"name":"analysis","create":true}),
    )
    .await;
    let submitted = invoke(
        &terminal,
        &runtime.handle,
        "execute",
        json!({"workspace":"analysis","context":joined["context"],"request_id":"original","source":":calc { return 42; } > retained","wait_ms":1000}),
    )
    .await;
    assert_eq!(submitted["execution"]["settled"], true, "{submitted}");
    let before = invoke(
        &terminal,
        &runtime.handle,
        "workspace_snapshot",
        json!({"workspace":"analysis"}),
    )
    .await;
    manager.shutdown().await;
    runtime.shutdown().await.unwrap();
    drop(terminal);

    let runtime = launch(RuntimeOptions::new(home.clone(), root.path().into()))
        .await
        .unwrap();
    let manager = Manager::default();
    let terminal = test_terminal(&manager, &runtime, &home, root.path(), &history).await;
    let _stop_on_failure = terminal.stopped.clone().drop_guard();
    let rejected = perform(
        &terminal,
        &runtime.handle,
        &[
            json!({"name":"value_read","arguments":{"workspace":"analysis","name":"retained"}})
                .to_string(),
        ],
    )
    .await
    .unwrap_err();
    let recovery = json!({"name":"workspace_open","arguments":{"name":"analysis","create":false}});
    assert!(
        rejected.stderr.contains(&recovery.to_string()),
        "{}",
        rejected.stderr
    );
    assert!(
        rejected
            .stderr
            .contains("does not restore grants or replay work")
    );
    assert!(rejected.stderr.contains("original request_id"));
    assert!(terminal.assistant.workspaces.lock().unwrap().is_empty());

    let joined = perform(&terminal, &runtime.handle, &[recovery.to_string()])
        .await
        .unwrap_or_else(|error| panic!("{}", error.stderr));
    assert_ne!(joined["context"], submitted["context"]);
    assert_eq!(joined["executeArguments"]["workspace"], "analysis");
    let after = invoke(
        &terminal,
        &runtime.handle,
        "workspace_snapshot",
        json!({"workspace":"analysis"}),
    )
    .await;
    assert_eq!(after["total"], before["total"]);
    let value = invoke(
        &terminal,
        &runtime.handle,
        "value_read",
        json!({"workspace":"analysis","name":"retained"}),
    )
    .await;
    assert_eq!(value, json!(42));
    let original = invoke(
        &terminal,
        &runtime.handle,
        "execution_read",
        json!({"workspace":"analysis","request_id":"original"}),
    )
    .await;
    assert_eq!(original["restored"], true);
    let protected = invoke(
        &terminal,
        &runtime.handle,
        "execute",
        json!({"workspace":"analysis","context":joined["context"],"request_id":"overwrite","source":":calc { return 7; } > retained","wait_ms":1000}),
    )
    .await;
    assert!(
        protected["execution"]["diagnostics"]
            .as_array()
            .unwrap()
            .iter()
            .any(|diagnostic| diagnostic["code"] == "AUT001"),
        "{protected}"
    );
    assert!(
        protected["execution"]["nodes"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    assert_eq!(
        invoke(
            &terminal,
            &runtime.handle,
            "value_read",
            json!({"workspace":"analysis","name":"retained"})
        )
        .await,
        json!(42)
    );
    manager.shutdown().await;
    runtime.shutdown().await.unwrap();
}

#[test]
fn rejoin_and_replacement_rules_are_discoverable_before_execution() {
    let tools = super::super::protocol::registry()["tools"]
        .as_array()
        .unwrap();
    let open = tools
        .iter()
        .find(|tool| tool["name"] == "workspace_open")
        .unwrap();
    let description = open["description"].as_str().unwrap();
    assert!(description.contains("live terminal session"));
    assert!(description.contains("create:false"));
    assert!(description.contains("does not replay requests or restore prior grants"));
    assert_eq!(
        open["inputSchema"]["properties"]["create"]["default"],
        false
    );
    assert!(
        super::super::GUIDE
            .contains("replace:true expresses replacement intent; it never grants authority")
    );
    let recovery = rejoin_required("analysis", "Workspace was reloaded.");
    assert!(recovery.stderr.contains("\"create\":false"));
    assert!(recovery.stderr.contains("execution_read"));
}
