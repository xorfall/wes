#[tokio::test(flavor = "multi_thread")]
async fn view_authoring_is_bounded_shared_product_context_without_execution_or_source_access() {
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
    let before = invoke(&terminal, app, "workspace_snapshot", json!({})).await;
    let overview = invoke(&terminal, app, "view_authoring", json!({})).await;
    let manifest: Value = serde_json::from_str(include_str!(
        "../../../../../packages/view-sdk/authoring.json"
    ))
    .unwrap();
    assert_eq!(overview["content"], manifest);
    for topic in ["overview", "sdk", "theme", "layout", "examples", "types"] {
        let reply = invoke(&terminal, app, "view_authoring", json!({"topic":topic})).await;
        assert!(reply.to_string().len() < 64 * 1024);
        assert_eq!(reply["sdk"], wes_views::sdk_version());
        assert_eq!(reply["version"], 1);
        match topic {
            "layout" => {
                assert_eq!(reply["content"]["rules"], manifest["layout"]);
                let layouts = reply["content"]["builtinLayouts"].as_array().unwrap();
                assert_eq!(layouts.len(), wes_views::catalogue().len());
                for package in wes_views::catalogue() {
                    let layout = layouts.iter().find(|item| item["name"] == package.manifest.name).unwrap();
                    assert_eq!(layout["layout"], package.description()["layout"]);
                    for tier in ["preview", "expanded", "window"] {
                        assert!(layout["layout"][tier]["placement"].is_object());
                    }
                }
                assert!(reply["content"]["rules"]["allocation"].as_str().unwrap().contains("feedback loop"));
            }
            "theme" => {
                let registry: Value = serde_json::from_str(include_str!(
                    "../../../../../packages/view-sdk/theme.json"
                ))
                .unwrap();
                assert_eq!(reply["content"], registry);
                assert!(!reply.to_string().contains("#"));
                assert!(reply["content"]["roles"].get("frame-focus").is_none());
            }
            "sdk" => {
                assert!(reply["content"]["commands"].as_str().unwrap().contains("ViewCommands"));
                assert!(reply["content"]["evidence"].as_str().unwrap().contains("ViewEvidence"));
                assert_eq!(
                reply["content"]["declarations"],
                include_str!("../../../../../packages/view-sdk/index.ts")
            );
            },
            "types" => {
                assert!(reply["content"]["inputRule"].as_str().unwrap().contains("Record"));
                let source = reply["content"]["example"].as_str().unwrap();
                wes_views::Package::parse(r#"{"name":"Forecast","id":"forecast","summary":"Synthetic input contract","renderer":"View.tsx","input":"ForecastInput","outputs":{}}"#,source).unwrap();
                assert!(reply["content"]["viewBoundary"].as_str().unwrap().contains("Map"));
            },
            "examples" => {
                assert_eq!(reply["content"]["files"]["View.tsx"], include_str!("../../../../../tools/view-package/template/View.tsx"));
                let example: Value = serde_json::from_str(reply["content"]["files"]["view.json"].as_str().unwrap()).unwrap();
                for tier in ["preview", "expanded", "window"] {
                    assert!(example["layout"][tier]["placement"].is_object());
                }
            },
            _ => (),
        }
    }
    assert_eq!(
        invoke(&terminal, app, "workspace_snapshot", json!({})).await,
        before
    );
    for arguments in [
        json!({"topic":"unknown"}),
        json!({"topic":3}),
        json!({"topic":"theme","extra":true}),
        json!({"workspace":"default"}),
    ] {
        assert!(
            perform(
                &terminal,
                app,
                &[json!({"name":"view_authoring","arguments":arguments}).to_string()]
            )
            .await
            .is_err()
        );
    }
    terminal.stopped.cancel();
    assert!(
        perform(
            &terminal,
            app,
            &[json!({"name":"view_authoring","arguments":{}}).to_string()]
        )
        .await
        .is_err()
    );
    manager.shutdown().await;
    runtime.shutdown().await.unwrap();
}
