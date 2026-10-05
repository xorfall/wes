// Uses the runnable editable-api-draft fixture, isolated from user data and services.
#[tokio::test(flavor = "multi_thread")]
async fn spec_mcp_saved_edits_require_grants_preserve_history_and_reject_stale_saves() {
    use crate::api_library::{ApiLibrary, Request, parse_request};
    use wes_adapters::api_library::{Library, PackageKey};
    let root = tempfile::tempdir().unwrap();
    let base = root.path().canonicalize().unwrap();
    let home = base.join("home");
    let runtime = launch(RuntimeOptions::new(home.clone(), base.clone()))
        .await
        .unwrap();
    let key = json!({"service":"items","apiVersion":"v1","scope":"default"});
    let native_key: PackageKey = serde_json::from_value(key.clone()).unwrap();
    let ready = include_str!("../../../../../examples/editable-api-draft/completed-draft.json");
    let service = ApiLibrary::new(home.clone());
    let initial = tokio::task::spawn_blocking(move || {
        let settings = service.perform(Request::Status).unwrap();
        let directory =
            std::path::Path::new(settings["settings"]["localDirectory"].as_str().unwrap());
        let mut library = Library::open(directory).unwrap();
        let result = library
            .create_draft(
                native_key,
                ready.into(),
                json!({"note":"synthetic evidence"}),
                b"synthetic",
                "fixture".into(),
            )
            .unwrap();
        library
            .create_draft(
                PackageKey {
                    service: "private_other".into(),
                    api_version: "v1".into(),
                    scope: "default".into(),
                },
                ready.into(),
                json!({}),
                b"private",
                "private origin".into(),
            )
            .unwrap();
        result
    })
    .await
    .unwrap();
    let manager = Manager::default();
    let services = crate::runtime::browser_services(home.clone(), base.clone()).unwrap();
    let web_library = services.api_library.unwrap();
    let current = runtime.handle.current().unwrap();
    let id = manager
        .start(
            services.terminal.unwrap(),
            current.clone(),
            "spec-test".into(),
            0,
            runtime.handle.clone(),
        )
        .await
        .unwrap();
    let terminal = manager
        .owned(&id, &current.generation, "spec-test")
        .unwrap();
    let _stop_on_failure = terminal.stopped.clone().drop_guard();
    let app = &runtime.handle;
    let denied =
        |name: &str, arguments: Value| vec![json!({"name":name,"arguments":arguments}).to_string()];
    let read = json!({"key":key,"revision":initial["draft"]["revision"]});
    assert_eq!(
        invoke(&terminal, app, "spec_list", json!({})).await["total"],
        0
    );
    assert!(
        perform(&terminal, app, &denied("spec_read", read.clone()))
            .await
            .unwrap_err()
            .stderr
            .contains("not shared")
    );
    assert!(
        perform(
            &terminal,
            app,
            &denied(
                "spec_save",
                json!({"key":key,"revision":read["revision"],"text":"{}"})
            )
        )
        .await
        .is_err()
    );
    assert!(
        perform(
            &terminal,
            app,
            &denied("spec_list", json!({"workspace":"default"}))
        )
        .await
        .is_err()
    );
    assert!(
        perform(&terminal, app, &denied("spec_list", json!({"limit":101})))
            .await
            .is_err()
    );
    let service = web_library;
    let host = |request: Value| {
        let service = service.clone();
        tokio::task::spawn_blocking(move || {
            service.perform(parse_request(&serde_json::to_vec(&request).unwrap()).unwrap())
        })
    };
    assert_eq!(
        host(json!({"action":"draftAccess","key":key,"enabled":true}))
            .await
            .unwrap()
            .unwrap()["enabled"],
        true
    );
    let listed = invoke(&terminal, app, "spec_list", json!({"limit":1})).await;
    assert_eq!(listed["total"], 1);
    assert!(!listed.to_string().contains("private_other"));
    let loaded = invoke(&terminal, app, "spec_read", read.clone()).await;
    assert_eq!(loaded["text"], ready);
    assert!(loaded["validation"].get("preview").is_none());
    assert!(loaded["evidence"].get("source").is_none());
    let mut evidence = read.clone();
    evidence["evidence"] = json!(true);
    assert_eq!(
        invoke(&terminal, app, "spec_read", evidence).await["evidence"]["source"]["note"],
        "synthetic evidence"
    );
    let edited = format!("{ready}\n");
    let save = json!({"key":key,"revision":read["revision"],"text":edited});
    let saved = invoke(&terminal, app, "spec_save", save.clone()).await;
    assert_eq!(saved["validation"]["valid"], true);
    assert_ne!(saved["draft"]["revision"], read["revision"]);
    assert!(saved.get("text").is_none());
    assert!(saved.get("descriptor").is_none());
    assert_eq!(saved["evidence"]["status"], "stale");
    assert!(
        perform(&terminal, app, &denied("spec_save", save))
            .await
            .unwrap_err()
            .stderr
            .contains("changed")
    );
    assert_eq!(
        invoke(&terminal, app, "spec_read", read.clone()).await["text"],
        ready
    );
    assert_eq!(
        invoke(&terminal, app, "spec_list", json!({"offset":1,"limit":1})).await["drafts"][0]["revision"],
        saved["draft"]["revision"]
    );
    // UI uses the same CAS: a concurrent unsaved buffer cannot overwrite an agent save.
    assert!(host(json!({"action":"saveDraft","key":key,"revision":read["revision"],"text":"user unsaved text"})).await.unwrap().unwrap_err().to_string().contains("changed"));
    let broken = invoke(
        &terminal,
        app,
        "spec_save",
        json!({"key":key,"revision":saved["draft"]["revision"],"text":"{"}),
    )
    .await;
    assert_eq!(broken["validation"]["valid"], false);
    assert!(broken["validation"]["diagnosticsTotal"].as_u64().unwrap() > 0);
    assert!(
        perform(
            &terminal,
            app,
            &denied(
                "spec_save",
                json!({"key":key,"revision":broken["draft"]["revision"],"text":"x".repeat(65537)})
            )
        )
        .await
        .is_err()
    );
    // The narrow service cannot escalate itself or leak settings, even off the MCP parser path.
    let agent = service.clone();
    let agent_key = serde_json::from_value(key.clone()).unwrap();
    tokio::task::spawn_blocking(move || {
        assert!(
            agent
                .perform_agent(Request::DraftAccess {
                    key: agent_key,
                    enabled: Some(true)
                })
                .is_err()
        );
        assert!(agent.perform_agent(Request::Status).is_err());
    })
    .await
    .unwrap();
    let fresh = ApiLibrary::new(home);
    assert_eq!(
        tokio::task::spawn_blocking(move || fresh.perform_agent(Request::ListDrafts))
            .await
            .unwrap()
            .unwrap()["drafts"],
        json!([])
    );
    host(json!({"action":"draftAccess","key":key,"enabled":false}))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        invoke(&terminal, app, "spec_list", json!({})).await["total"],
        0
    );
    assert!(
        perform(&terminal, app, &denied("spec_read", read))
            .await
            .is_err()
    );
    terminal.stopped.cancel();
    assert!(
        perform(&terminal, app, &denied("spec_list", json!({})))
            .await
            .is_err()
    );
    manager.shutdown().await;
    runtime.shutdown().await.unwrap();
}
