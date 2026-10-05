// Isolated MCP acceptance: no account, real API, or model process is used.
#[tokio::test(flavor = "multi_thread")]
async fn describe_through_mcp_preserves_setup_errors_idempotency_and_authority() {
    use crate::api_library::{ApiLibrary, parse_request};
    use std::os::unix::fs::PermissionsExt;

    let root = tempfile::tempdir().unwrap();
    let base = root.path().canonicalize().unwrap();
    let home = base.join("home");
    std::fs::write(
        base.join("openapi.json"),
        include_bytes!("../../../../../examples/api-import/openapi.json"),
    )
    .unwrap();
    let (draft, evidence) = wes_adapters::api_library::draft::from_descriptor(include_bytes!(
        "../../../../../examples/api-import/inventory.json"
    ))
    .unwrap();
    std::fs::write(
        base.join("answer.json"),
        serde_json::to_vec(&json!({
            "draft": serde_json::from_str::<Value>(&draft).unwrap(), "source": evidence
        }))
        .unwrap(),
    )
    .unwrap();
    std::fs::write(
        base.join("prose.txt"),
        "Synthetic prose is not an OpenAPI document.",
    )
    .unwrap();
    let executable = base.join("extract");
    std::fs::write(
        &executable,
        format!(
            "#!/bin/sh\nsource_text=$(/bin/cat)\ncase \"$source_text\" in Synthetic*) exit 21;; esac\necho called >> '{}'\n/bin/cat '{}'\n",
            base.join("calls").display(),
            base.join("answer.json").display()
        ),
    )
    .unwrap();
    std::fs::set_permissions(&executable, std::fs::Permissions::from_mode(0o700)).unwrap();
    drop(crate::data_home::DataHome::open(&home).unwrap());
    let library = ApiLibrary::new(home.clone());
    let revision = library.perform(parse_request(br#"{"action":"status"}"#).unwrap()).unwrap()["revision"].clone();
    let config = json!({"action":"configure","expectedRevision":revision,"settings":{"localDirectory":home.join("api-library"),"extractor":executable}});
    tokio::task::spawn_blocking(move || {
        library.perform(parse_request(&serde_json::to_vec(&config).unwrap()).unwrap())
    })
    .await
    .unwrap()
    .unwrap();
    let runtime = launch(RuntimeOptions::new(home.clone(), base.clone()))
        .await
        .unwrap();
    let manager = Manager::default();
    let terminal = test_terminal(
        &manager,
        &runtime,
        &home,
        &base,
        &uuid::Uuid::new_v4().to_string(),
    )
    .await;
    let _stop_on_failure = terminal.stopped.clone().drop_guard();
    let app = &runtime.handle;
    let context = invoke(&terminal, app, "workspace_context", json!({})).await;
    let source = ":describe file:openapi.json provider:inventory out:export.json > specification";
    assert_eq!(
        invoke(&terminal, app, "help", json!({"command":"describe"})).await["assistant_allowed"],
        true
    );
    assert_eq!(
        invoke(&terminal, app, "validate", json!({"source":source})).await["valid"],
        true
    );
    let args = json!({"source":source,"request_id":"describe","context":context["context"],"wait_ms":1000,"results":"full"});
    let submitted = invoke(&terminal, app, "execute", args.clone()).await;
    runtime
        .handle
        .current()
        .unwrap()
        .session
        .wait_idle()
        .await
        .unwrap();
    let result = invoke(
        &terminal,
        app,
        "execution_read",
        json!({"request_id":"describe","results":"full"}),
    )
    .await;
    assert_eq!(result["nodes"][0]["state"], "ready", "{result}");
    assert!(
        !result.to_string().contains("descriptorPath"),
        "private result leaked: {result}"
    );
    let exported = std::fs::read(base.join("export.json")).unwrap();
    wes_adapters::api_library::validate_descriptor(&exported).unwrap();
    let exported: Value = serde_json::from_slice(&exported).unwrap();
    let expected = wes_adapters::api_library::draft::validate(&draft)
        .descriptor
        .unwrap();
    for field in ["version", "provider", "types", "operations"] {
        assert_eq!(exported[field], expected[field]);
    }
    assert_eq!(
        invoke(&terminal, app, "execute", args).await["duplicate"],
        true
    );
    assert_eq!(
        std::fs::read_to_string(base.join("calls")).unwrap(),
        "called\n"
    );
    assert!(
        perform(
            &terminal,
            app,
            &[json!({"name":"value_read","arguments":{"name":"specification"}}).to_string()]
        )
        .await
        .is_err()
    );

    std::fs::write(base.join("protected.json"), "keep these bytes").unwrap();
    invoke(&terminal, app, "execute", json!({"source":":describe file:openapi.json provider:inventory out:protected.json", "request_id":"collision", "context":submitted["context"], "wait_ms":1000})).await;
    runtime
        .handle
        .current()
        .unwrap()
        .session
        .wait_idle()
        .await
        .unwrap();
    let collision = invoke(
        &terminal,
        app,
        "execution_read",
        json!({"request_id":"collision"}),
    )
    .await;
    assert_eq!(collision["nodes"][0]["state"], "failed", "{collision}");
    let saved = runtime
        .handle
        .current()
        .unwrap()
        .session
        .snapshot()
        .await
        .unwrap();
    let collision_node =
        wes_engine::graph::NodeId::new(collision["nodes"][0]["node"].as_str().unwrap()).unwrap();
    let error = &saved.execution.errors[&collision_node];
    assert_eq!(error.code(), "DSC006");
    assert!(error.message().contains("Draft saved; export failed"));
    assert!(error.message().contains("already exists"));
    assert!(!saved.execution.values.contains_key(&collision_node));
    assert_eq!(
        std::fs::read_to_string(base.join("protected.json")).unwrap(),
        "keep these bytes"
    );

    invoke(&terminal, app, "execute", json!({"source":":describe file:prose.txt provider:prose", "request_id":"unsupported-source", "context":submitted["context"], "wait_ms":1000})).await;
    runtime
        .handle
        .current()
        .unwrap()
        .session
        .wait_idle()
        .await
        .unwrap();
    let failed = invoke(
        &terminal,
        app,
        "execution_read",
        json!({"request_id":"unsupported-source"}),
    )
    .await;
    assert_eq!(failed["nodes"][0]["state"], "failed", "{failed}");
    assert!(
        failed
            .to_string()
            .contains("unsupported or invalid"),
        "{failed}"
    );

    let session = runtime.handle.current().unwrap().session;
    session
        .submit(SourceInput::new("user".into(), ":calc { return 7 } > protected".into()).unwrap())
        .await
        .unwrap();
    session.wait_idle().await.unwrap();
    let blocked = invoke(&terminal, app, "execute", json!({"source":":describe file:openapi.json provider:inventory > protected", "request_id":"protected", "context":submitted["context"], "wait_ms":1000})).await;
    assert!(blocked.to_string().contains("AUT001"), "{blocked}");
    for source in [
        r#":workspace save "synthetic""#,
        r#":workspace load "synthetic""#,
    ] {
        assert_eq!(
            invoke(&terminal, app, "validate", json!({"source":source})).await["valid"],
            false
        );
    }
    let imported = invoke(&terminal, app, "execute", json!({"source":":import spec file:export.json as:inventory endpoint:\"http://127.0.0.1:1\" replace:false", "request_id":"import", "context":submitted["context"], "wait_ms":1000})).await;
    assert!(
        !imported["execution"]["accepted"]
            .as_array()
            .unwrap()
            .is_empty(),
        "{imported}"
    );
    let context = invoke(&terminal, app, "workspace_context", json!({})).await;
    assert!(
        context["providers"]
            .as_array()
            .unwrap()
            .contains(&json!("inventory")),
        "{context}"
    );
    // A private report is user-readable; MCP receives only its opaque reference.
    std::fs::write(
        base.join("report.json"),
        serde_json::to_vec(
            &json!({"version":1,"message":"private-source-detail","issues":[],"omitted":0}),
        )
        .unwrap(),
    )
    .unwrap();
    std::fs::write(
        base.join("extract"),
        format!(
            "#!/bin/sh\n/bin/cat >/dev/null\n/bin/cat '{}'\nexit 21\n",
            base.join("report.json").display()
        ),
    )
    .unwrap();
    invoke(&terminal, app, "execute", json!({"source":":describe file:openapi.json provider:inventory", "request_id":"reported-failure", "context":context["context"], "wait_ms":1000})).await;
    runtime
        .handle
        .current()
        .unwrap()
        .session
        .wait_idle()
        .await
        .unwrap();
    let failure = invoke(
        &terminal,
        app,
        "execution_read",
        json!({"request_id":"reported-failure","results":"full"}),
    )
    .await;
    assert!(failure.to_string().contains("DSC_REPORT"), "{failure}");
    assert!(!failure.to_string().contains("private-source-detail"));
    manager.shutdown().await;
    runtime.shutdown().await.unwrap();
}
