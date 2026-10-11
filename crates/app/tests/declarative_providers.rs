//! Public runtime acceptance: native synthetic executable, isolated homes, no accounts/services.
#[path = "support/fixtures.rs"]
mod fixtures;
use std::{path::Path, sync::Arc, time::Duration};
use wes::runtime::{RuntimeOptions, launch};
use wes_core::Data;
use wes_engine::{
    session::{SessionHandle, SubmissionResult},
    source::SourceInput,
};

async fn submit(session: &SessionHandle, source: &str) -> Arc<SubmissionResult> {
    let result = tokio::time::timeout(
        Duration::from_secs(15),
        session.submit(SourceInput::new(uuid::Uuid::new_v4().to_string(), source.into()).unwrap()),
    )
    .await
    .unwrap()
    .unwrap();
    tokio::time::timeout(Duration::from_secs(15), session.wait_idle())
        .await
        .unwrap()
        .unwrap();
    result
}
fn accepted(result: &Arc<SubmissionResult>) {
    assert!(
        result
            .diagnostics
            .diagnostics
            .iter()
            .all(|d| d.severity != wes_language::Severity::Error),
        "{result:?}"
    );
}
fn definition(value: i64) -> String {
    serde_json::json!({"version":1,"provider":"fixture","types":{"Sample":{"base":"Record","fields":{"value":"Int"}}},
        "backend":{"kind":"process","bin":"example-echo.bin"},"operations":[{
            "path":["records","view"],"argv":[format!("{{\"value\":{value}}}")],"result":{"format":"json","type":"Sample"}
        }]}).to_string()
}
fn options(root: &Path) -> RuntimeOptions {
    let mut options = RuntimeOptions::new(root.join("home"), root.into());
    options.docker_candidates = Some(vec![root.join("unavailable.sock")]);
    options
}
async fn result_value(session: &SessionHandle, result: &SubmissionResult) -> wes_core::Value {
    let snapshot = session.snapshot().await.unwrap();
    snapshot
        .execution
        .values
        .get(result.nodes.last().unwrap())
        .unwrap_or_else(|| panic!("{snapshot:?}"))
        .clone()
}
#[tokio::test]
async fn yaml_import_is_discoverable_typed_revisioned_and_restores_without_inputs() {
    let root = tempfile::tempdir().unwrap();
    let executable = fixtures::echo(root.path());
    let file = root.path().join("tool.yaml");
    std::fs::write(&file, definition(1)).unwrap();
    let runtime = launch(options(root.path())).await.unwrap();
    let session = runtime.handle.current().unwrap().session;
    let imported = submit(
        &session,
        ":import provider file:tool.yaml as:tool\ntool records view > original",
    )
    .await;
    accepted(&imported);
    let original = result_value(&session, &imported).await;
    assert_eq!(
        original.data(),
        &Data::Record([("value".into(), Data::Int(1))].into_iter().collect())
    );
    assert!(original.metadata().is_some());
    let help = submit(&session, ":help tool records view").await;
    accepted(&help);
    let help = result_value(&session, &help).await;
    let help = String::from_utf8(
        wes_adapters::codec::encode_json(help.data(), Default::default()).unwrap(),
    )
    .unwrap();
    assert!(
        help.contains("tool records view") && help.contains("UNSAFE") && help.contains("Sample"),
        "{help}"
    );
    let revisions = session.environment_revisions().await.unwrap();
    let node = imported.nodes.last().unwrap().clone();
    let run = session.snapshot().await.unwrap().execution.runs[&node].clone();
    std::fs::write(&file, definition(2)).unwrap();
    let no_replace = submit(&session, ":import provider file:tool.yaml as:tool").await;
    assert!(
        no_replace
            .diagnostics
            .diagnostics
            .iter()
            .any(|d| d.severity == wes_language::Severity::Error)
    );
    assert_eq!(session.environment_revisions().await.unwrap(), revisions);
    let replaced = submit(
        &session,
        ":import provider file:tool.yaml as:tool replace:true\ntool records view > updated",
    )
    .await;
    accepted(&replaced);
    assert_eq!(
        result_value(&session, &replaced).await.data(),
        &Data::Record([("value".into(), Data::Int(2))].into_iter().collect())
    );
    assert_ne!(session.environment_revisions().await.unwrap(), revisions);
    assert_eq!(session.snapshot().await.unwrap().execution.runs[&node], run);
    let stale = submit(&session, ":refresh $original").await;
    assert!(
        stale
            .diagnostics
            .diagnostics
            .iter()
            .any(|d| d.code == "ENV039"),
        "{stale:?}"
    );
    assert_eq!(
        session.snapshot().await.unwrap().execution.values[&node].data(),
        original.data()
    );
    let docs = session.environment_documents().await.unwrap();
    assert!(
        docs.iter()
            .any(|d| d.source.contains("provider") && d.source.contains("tool.yaml"))
    );
    runtime.shutdown().await.unwrap();
    std::fs::remove_file(file).unwrap();
    std::fs::remove_file(executable).unwrap();
    let reopened = launch(options(root.path())).await.unwrap();
    let session = reopened.handle.current().unwrap().session;
    let snapshot = session.snapshot().await.unwrap();
    assert_eq!(snapshot.execution.runs[&node], run);
    assert_eq!(snapshot.execution.values[&node].data(), original.data());
    assert!(snapshot.execution.values[&node].metadata().is_some());
    let inspected = submit(&session, ":inspect tool records view").await;
    accepted(&inspected);
    reopened.shutdown().await.unwrap();
}
#[tokio::test]
async fn managed_provider_uses_existing_environment_activation_and_update_rules() {
    let root = tempfile::tempdir().unwrap();
    fixtures::echo(root.path());
    std::fs::write(root.path().join("tool.yaml"), definition(3)).unwrap();
    std::fs::write(
        root.path().join("env.yaml"),
        r#"version: 1
package: custom-tools
targets: {local: {kind: local}}
environments:
  demo:
    imports:
      tool:
        source: {kind: provider, file: tool.yaml}
        bind: {target: local}
"#,
    )
    .unwrap();
    let runtime = launch(options(root.path())).await.unwrap();
    let session = runtime.handle.current().unwrap().session;
    for source in [
        ":env plan file:env.yaml > proposed",
        ":env apply $proposed",
        ":env use \"demo\"",
        ":env enable \"demo\"",
    ] {
        accepted(&submit(&session, source).await);
    }
    let first = submit(&session, "tool records view > first").await;
    accepted(&first);
    assert_eq!(
        result_value(&session, &first).await.data(),
        &Data::Record([("value".into(), Data::Int(3))].into_iter().collect())
    );
    accepted(&submit(&session, ":env disable \"demo\"").await);
    let denied = submit(&session, "tool records view > disabled").await;
    let snapshot = session.snapshot().await.unwrap();
    assert!(
        denied
            .diagnostics
            .diagnostics
            .iter()
            .any(|d| d.severity == wes_language::Severity::Error)
            || denied
                .nodes
                .iter()
                .any(|n| snapshot.execution.errors.contains_key(n)),
        "{denied:?}"
    );
    accepted(&submit(&session, ":env enable \"demo\"").await);
    std::fs::write(root.path().join("tool.yaml"), definition(4)).unwrap();
    accepted(
        &submit(
            &session,
            ":import provider file:tool.yaml as:tool replace:true",
        )
        .await,
    );
    let stale_context = submit(&session, "tool records view > stale_context").await;
    assert!(
        stale_context
            .diagnostics
            .diagnostics
            .iter()
            .any(|d| d.code == "ENV008")
    );
    accepted(&submit(&session, ":env use \"demo\"").await);
    let second = submit(&session, "tool records view > second").await;
    accepted(&second);
    assert_eq!(
        result_value(&session, &second).await.data(),
        &Data::Record([("value".into(), Data::Int(4))].into_iter().collect())
    );
    runtime.shutdown().await.unwrap();
}
