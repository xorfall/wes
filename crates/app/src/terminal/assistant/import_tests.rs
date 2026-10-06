#[tokio::test(flavor = "multi_thread")]
async fn mcp_import_help_and_rejections_expose_the_real_contract_without_private_detail() {
    let root = tempfile::tempdir().unwrap();
    let home = root.path().join("home");
    let runtime = launch(RuntimeOptions::new(home.clone(), root.path().into())).await.unwrap();
    let manager = Manager::default();
    let terminal = test_terminal(&manager, &runtime, &home, root.path(), &uuid::Uuid::new_v4().to_string()).await;
    let _stop_on_failure = terminal.stopped.clone().drop_guard();
    let app = &runtime.handle;
    for command in ["arguments", "errors"] {
        let help=invoke(&terminal,app,"help",json!({"command":command})).await;
        assert_eq!(help["path"],command);
    }
    let tooling=invoke(&terminal,app,"view_toolchain",json!({})).await;
    assert_eq!(tooling["embeddedCompilerSources"],true);
    assert!(tooling["scope"].as_str().unwrap().contains("backend host"));
    let root_help = invoke(&terminal, app, "help", json!({"command":"import","depth":1})).await;
    let openapi = root_help["children"].as_array().unwrap().iter().find(|child| child["name"] == "openapi").unwrap();
    assert!(openapi["summary"].as_str().unwrap().contains("OpenAPI"));
    for kind in ["spec", "openapi"] {
        let help = invoke(&terminal, app, "help", json!({"command":"import","tail":[kind]})).await;
        let endpoint = help["invocation"]["parameters"].as_array().unwrap().iter().find(|parameter| parameter["name"] == "endpoint").unwrap();
        assert_eq!(endpoint["required"], true);
        assert_eq!(help["requirements"][0]["arguments"], json!(["file","url"]));
        let context = invoke(&terminal, app, "workspace_context", json!({})).await;
        let reply = invoke(&terminal, app, "execute", json!({"source":format!(":import {kind} file:PRIVATE_ABSENT_FILE"),"context":context["context"],"request_id":format!("missing-{kind}"),"wait_ms":1000})).await;
        assert!(reply["execution"]["diagnostics"].as_array().unwrap().iter().any(|diagnostic| diagnostic["code"] == "IMP001" && diagnostic["message"].as_str().unwrap().contains("endpoint")), "{reply}");
        assert!(!reply["execution"]["diagnostics"].as_array().unwrap().iter().any(|diagnostic| diagnostic["message"].as_str().unwrap().contains("PRIVATE")));
    }
    terminal.stopped.cancel();
    drop(terminal);
    runtime.shutdown().await.unwrap();
}
