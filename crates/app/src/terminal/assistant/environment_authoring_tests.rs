#[tokio::test(flavor = "multi_thread")]
async fn mcp_environment_help_supports_inline_plan_review_and_apply_without_a_file() {
    let root = tempfile::tempdir().unwrap();
    let home = root.path().join("home");
    let runtime = launch(RuntimeOptions::new(home.clone(), root.path().into()))
        .await
        .unwrap();
    let manager = Manager::default();
    let terminal = test_terminal(
        &manager,
        &runtime,
        &home,
        root.path(),
        &uuid::Uuid::new_v4().to_string(),
    )
    .await;
    let _stop_on_failure = terminal.stopped.clone().drop_guard();
    let app = &runtime.handle;
    let help = invoke(
        &terminal,
        app,
        "help",
        json!({"command":"env","tail":["plan"]}),
    )
    .await;
    assert_eq!(help["assistant_allowed"], true);
    let schema = &help["invocation"]["declaration"];
    assert_eq!(
        schema,
        &wes_core::package_schema::declarations()
            .describe_root("env")
            .unwrap()
    );
    assert!(serde_json::to_vec(&help).unwrap().len() < 64 * 1024);
    assert!(
        help["invocation"]["authoring"]
            .as_str()
            .unwrap()
            .contains("no filesystem write tool")
    );
    let context = invoke(&terminal, app, "workspace_context", json!({})).await;
    let planned = invoke(&terminal, app, "execute", json!({
        "source":help["invocation"]["examples"][0], "context":context["context"], "request_id":"inline-plan", "wait_ms":1000
    })).await;
    assert_eq!(planned["execution"]["settled"], true, "{planned}");
    assert_eq!(planned["plans"][0]["name"], "proposed", "{planned}");
    assert_eq!(
        planned["plans"][0]["changes"][0]["environment"], "sample",
        "{planned}"
    );
    let before = invoke(&terminal, app, "workspace_context", json!({})).await;
    assert!(before["environment"]["revisions"].get("sample").is_none());
    let applied = invoke(&terminal, app, "execute", json!({
        "source":help["invocation"]["examples"][1], "context":before["context"], "request_id":"inline-apply", "wait_ms":1000
    })).await;
    assert_eq!(applied["execution"]["settled"], true, "{applied}");
    let after = invoke(&terminal, app, "workspace_context", json!({})).await;
    assert!(
        after["environment"]["revisions"]["sample"].is_string(),
        "{applied}\n{after}"
    );
    assert!(applied["plans"].as_array().unwrap().is_empty());
    assert_eq!(
        std::fs::read_dir(root.path()).unwrap().count(),
        1,
        "only the isolated Wes home was created"
    );
    let invalid = invoke(&terminal, app, "execute", json!({
        "source":format!(":env plan source:{} > rejected", wes_language::quote_text("version: 1\nenvironments: {}\nprivate-field: private-value\n")),
        "context":after["context"], "request_id":"invalid-inline", "wait_ms":1000
    })).await;
    let failure = invalid["execution"].to_string();
    assert!(failure.contains("valid fields:"), "{invalid}");
    assert!(!failure.contains("private-field"));
    assert!(!failure.contains("private-value"));
    manager.shutdown().await;
    runtime.shutdown().await.unwrap();
}
