#![cfg(unix)]
use serde_json::json;
use std::{os::unix::fs::PermissionsExt, time::Duration};
use wes::{
    api_library::{ApiLibrary, parse_request},
    runtime::{RuntimeOptions, launch},
};
use wes_engine::source::SourceInput;
#[tokio::test(flavor = "multi_thread")]
async fn describe_is_normal_runtime_work_and_import_is_a_separate_action() {
    let root = tempfile::tempdir().unwrap();
    let base = root.path().canonicalize().unwrap();
    let home = base.join("home");
    let answer = base.join("answer.json");
    let (draft, evidence) = wes_adapters::api_library::draft::from_descriptor(include_bytes!(
        "../../../examples/api-import/inventory.json"
    ))
    .unwrap();
    std::fs::write(&answer, serde_json::to_vec(&json!({"draft":serde_json::from_str::<serde_json::Value>(&draft).unwrap(),"source":evidence})).unwrap()).unwrap();
    let source = base.join("openapi.json");
    std::fs::write(
        &source,
        include_bytes!("../../../examples/api-import/openapi.json"),
    )
    .unwrap();
    let executable = base.join("extract");
    std::fs::write(
        &executable,
        format!(
            "#!/bin/sh\n/bin/cat >/dev/null\n/bin/cat '{}'\n",
            answer.display()
        ),
    )
    .unwrap();
    std::fs::set_permissions(&executable, std::fs::Permissions::from_mode(0o700)).unwrap();
    drop(wes::data_home::DataHome::open(&home).unwrap());
    let service = ApiLibrary::new(home.clone());
    let revision = service
        .perform(parse_request(br#"{"action":"status"}"#).unwrap())
        .unwrap()["revision"]
        .clone();
    let request = json!({"action":"configure","expectedRevision":revision,"settings":{"localDirectory":home.join("api-library"),"extractor":executable}});
    tokio::task::spawn_blocking(move || {
        service.perform(parse_request(&serde_json::to_vec(&request).unwrap()).unwrap())
    })
    .await
    .unwrap()
    .unwrap();
    let runtime = launch(RuntimeOptions::new(home.clone(), base.clone()))
        .await
        .unwrap();
    let session = runtime.handle.current().unwrap().session;
    let submitted = session
        .submit(
            SourceInput::new(
                "describe-fixture".into(),
                ":describe file:openapi.json provider:inventory out:export.json > specification"
                    .into(),
            )
            .unwrap(),
        )
        .await
        .unwrap();
    assert!(!submitted.accepted.is_empty(), "{submitted:?}");
    tokio::time::timeout(Duration::from_secs(10), session.wait_idle())
        .await
        .unwrap()
        .unwrap();
    let snapshot = session.snapshot().await.unwrap();
    let value = snapshot
        .execution
        .values
        .get(submitted.nodes.last().unwrap())
        .unwrap_or_else(|| panic!("{snapshot:?}"));
    let json: serde_json::Value = serde_json::from_slice(
        &wes_adapters::codec::encode_json(value.data(), Default::default()).unwrap(),
    )
    .unwrap();
    assert_eq!(json["operationCount"], 3);
    let exported = std::fs::read_to_string(base.join("export.json")).unwrap();
    let descriptor: serde_json::Value = serde_json::from_str(&exported).unwrap();
    assert_eq!(descriptor["source"]["location"], "openapi.json");
    assert!(!exported.contains(&base.to_string_lossy().to_string()));
    let imported = session
        .submit(
            SourceInput::new(
                "import-fixture".into(),
                ":import spec file:export.json as:inventory endpoint:\"http://127.0.0.1:1\" replace:false".into(),
            )
            .unwrap(),
        )
        .await
        .unwrap();
    assert!(!imported.accepted.is_empty(), "{imported:?}");
    session.wait_idle().await.unwrap();
    let duplicate=session.submit(SourceInput::new("duplicate-import".into(),":import spec file:export.json as:inventory endpoint:\"http://127.0.0.1:1\" replace:false".into()).unwrap()).await;
    assert!(
        duplicate.as_ref().map_or(true, |r| r
            .diagnostics
            .diagnostics
            .iter()
            .any(|d| d.severity == wes_language::Severity::Error)),
        "existing alias needs explicit replacement: {duplicate:?}"
    );
    let omitted = session
        .submit(
            SourceInput::new(
                "omitted-replace".into(),
                ":import spec file:export.json as:inventory endpoint:\"http://127.0.0.1:2\"".into(),
            )
            .unwrap(),
        )
        .await;
    assert!(
        omitted.as_ref().map_or(true, |r| r.accepted.is_empty()),
        "{omitted:?}"
    );
    let replacement=session.submit(SourceInput::new("replace-import".into(),":import spec file:export.json as:inventory endpoint:\"http://127.0.0.1:2\" replace:true".into()).unwrap()).await.unwrap();
    assert!(!replacement.accepted.is_empty(), "{replacement:?}");
    for text in [
        ":describe file:openapi.json url:\"http://127.0.0.1:1\" provider:inventory",
        ":describe file:openapi.json",
        ":calc { return 1 } | :describe file:openapi.json provider:inventory",
    ] {
        let rejected = session
            .submit(SourceInput::new(uuid::Uuid::new_v4().to_string(), text.into()).unwrap())
            .await
            .unwrap();
        assert!(
            rejected
                .diagnostics
                .diagnostics
                .iter()
                .any(|d| d.severity == wes_language::Severity::Error),
            "{rejected:?}"
        );
    }
    runtime.shutdown().await.unwrap();
    std::fs::remove_file(executable).unwrap();
    let reopened = launch(RuntimeOptions::new(home, base)).await.unwrap();
    let restored = reopened.handle.current().unwrap().session;
    let inspected = restored
        .submit(SourceInput::new("inspect-restored".into(), ":help inventory".into()).unwrap())
        .await
        .unwrap();
    assert!(!inspected.accepted.is_empty(), "{inspected:?}");
    restored.wait_idle().await.unwrap();
    reopened.shutdown().await.unwrap();
}
