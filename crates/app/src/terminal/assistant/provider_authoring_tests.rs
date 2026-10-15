#[tokio::test(flavor = "multi_thread")]
async fn mcp_provider_schema_matches_validation_without_execution_or_payload_disclosure() {
    use std::os::unix::fs::PermissionsExt;
    let root = tempfile::tempdir().unwrap();
    let home = root.path().join("home");
    let marker = root.path().join("unexpected-launch");
    let executable = root.path().join("fixture");
    std::fs::write(&executable, "#!/bin/sh\ntouch unexpected-launch\nexit 99\n").unwrap();
    std::fs::set_permissions(&executable, std::fs::Permissions::from_mode(0o700)).unwrap();
    let definition = serde_json::json!({
        "version":1,"provider":"sample","backend":{"kind":"process","bin":"./fixture"},
        "operations":[{"path":["read"],"argv":[],"result":{"format":"text"}}]
    });
    let file = root.path().join("provider.yaml");
    std::fs::write(&file, definition.to_string()).unwrap();
    let runtime = launch(RuntimeOptions::new(home.clone(), root.path().into())).await.unwrap();
    let manager = Manager::default();
    let terminal = test_terminal(&manager, &runtime, &home, root.path(), &uuid::Uuid::new_v4().to_string()).await;
    let _stop_on_failure = terminal.stopped.clone().drop_guard();
    let app = &runtime.handle;
    let schema = wes_core::package_schema::declarations().describe_root("provider").unwrap();
    for tail in [json!(["provider"]), json!(["plan","provider"])] {
        let help = invoke(&terminal, app, "help", json!({"command":"import","tail":tail})).await;
        assert_eq!(help["invocation"]["declaration"], schema, "{help}");
        assert!(serde_json::to_vec(&help).unwrap().len() < 64 * 1024);
        assert!(help["summary"].as_str().unwrap().contains("No executable is launched"));
    }
    let context = invoke(&terminal, app, "workspace_context", json!({})).await;
    let imported = invoke(&terminal, app, "execute", json!({
        "source":format!(":import provider file:{} as:sample",wes_language::quote_text(file.to_str().unwrap())),
        "context":context["context"],"request_id":"capture-provider","wait_ms":1000
    })).await;
    assert_eq!(imported["execution"]["settled"], true, "{imported}");
    assert!(imported["execution"]["diagnostics"].as_array().unwrap().is_empty(), "{imported}");
    // Discovery after capture must not need the files or reconstruct an execution.
    std::fs::remove_file(&file).unwrap();
    std::fs::remove_file(&executable).unwrap();
    let operation = invoke(&terminal, app, "help", json!({"provider":"sample","tail":["read"]})).await;
    assert_eq!(operation["invocation"]["capability"], "read", "{operation}");
    assert_eq!(operation["invocation"]["safety"], "UNSAFE", "{operation}");
    let mut invalid = definition;
    invalid["backend"]["kind"] = "private-value".into();
    invalid["backend"]["bin"] = "private-executable".into();
    std::fs::write(&file, invalid.to_string()).unwrap();
    let context = invoke(&terminal, app, "workspace_context", json!({})).await;
    let refused = invoke(&terminal, app, "execute", json!({
        "source":format!(":import provider file:{} as:rejected",wes_language::quote_text(file.to_str().unwrap())),
        "context":context["context"],"request_id":"refuse-provider","wait_ms":1000
    })).await;
    let diagnostics = refused["execution"]["diagnostics"].to_string();
    assert!(diagnostics.contains("valid kind values: process, session"), "{refused}");
    for secret in ["private-value","private-executable"] { assert!(!diagnostics.contains(secret)); }
    assert!(!marker.exists(), "import and help must never launch the fixture");
    manager.shutdown().await;
    runtime.shutdown().await.unwrap();
}
