//! Synthetic captured editor documents: normal session admission, archive and offline replay.
#[path = "support/python.rs"]
mod python;
use std::fs;
use wes::runtime::{RuntimeOptions, launch};
use wes_adapters::environments::LocalEnvironments;
use wes_engine::{environments::EnvironmentLoader, source::SourceInput};
use wes_language::Severity;

const TYPES: &str = include_str!("../../../examples/import-retention/catalog.yaml");
const ENV: &str =
    "# ĞğİıŞşÇçÖöÜü\nversion: 1\npackage: editor-test\nenvironments:\n  text_demo: {}\n";
fn document(cell: &str, command: &str, source: &str) -> SourceInput {
    SourceInput::new(cell.into(), command.into())
        .unwrap()
        .with_document(Some(source.into()))
        .unwrap()
}
fn accepted(reply: &wes_engine::session::SubmissionResult) {
    assert!(
        reply
            .diagnostics
            .diagnostics
            .iter()
            .all(|d| d.severity != Severity::Error),
        "{:?}",
        reply.diagnostics
    );
}

#[tokio::test]
async fn editor_documents_are_identity_bound_archived_and_restore_without_archives() {
    let temp = tempfile::tempdir().unwrap();
    let home = temp.path().join("home");
    let runtime = launch(RuntimeOptions::new(home.clone(), temp.path().into()))
        .await
        .unwrap();
    let session = runtime.handle.current().unwrap().session;
    let input = document(
        "types",
        ":package load source:\"\" origin:\"editor:fixed\"",
        TYPES,
    );
    accepted(&session.submit(input.clone()).await.unwrap());
    accepted(&session.submit(input.clone()).await.unwrap());
    assert!(
        session
            .submit(input.with_document(Some("types: {}".into())).unwrap())
            .await
            .is_err()
    );
    accepted(
        &session
            .submit(document(
                "env",
                ":env plan source:\"\" origin:\"editor:env\" > proposed",
                ENV,
            ))
            .await
            .unwrap(),
    );
    assert!(
        !session
            .observe()
            .await
            .unwrap()
            .environment_revisions
            .contains_key("text_demo")
    );
    accepted(
        &session
            .submit(SourceInput::new("apply".into(), ":env apply $proposed".into()).unwrap())
            .await
            .unwrap(),
    );
    assert!(
        session
            .observe()
            .await
            .unwrap()
            .environment_revisions
            .contains_key("text_demo")
    );
    runtime.shutdown().await.unwrap();
    let receipts: Vec<serde_json::Value> = fs::read_dir(home.join("imports/records"))
        .unwrap()
        .map(|p| serde_json::from_slice(&fs::read(p.unwrap().path()).unwrap()).unwrap())
        .collect();
    assert_eq!(receipts.len(), 2);
    assert!(receipts.iter().all(|r| {
        r["origin"]
            .as_str()
            .unwrap()
            .starts_with("wes-text:editor:")
    }));
    fs::remove_dir_all(home.join("imports")).unwrap();
    let runtime = launch(RuntimeOptions::new(home.clone(), temp.path().into()))
        .await
        .unwrap();
    let session = runtime.handle.current().unwrap().session;
    let state = session.observe().await.unwrap();
    assert!(state.types.iter().any(|t| t == "GreetingText"));
    assert!(state.environment_revisions.contains_key("text_demo"));
    assert_eq!(
        state
            .cells
            .iter()
            .find(|c| c.input.cell() == "types")
            .unwrap()
            .input
            .document(),
        Some(TYPES)
    );
    assert_eq!(
        state
            .cells
            .iter()
            .find(|c| c.input.cell() == "env")
            .unwrap()
            .input
            .document(),
        Some(ENV)
    );
    // Plans are ephemeral; restoring a document does not recreate or reapply a plan.
    let stale = session
        .submit(SourceInput::new("stale-apply".into(), ":env apply $proposed".into()).unwrap())
        .await;
    assert!(stale.as_ref().map_or(true, |r| {
        r.diagnostics
            .diagnostics
            .iter()
            .any(|d| d.severity == Severity::Error)
    }));
    assert!(!home.join("imports").exists());
    runtime.shutdown().await.unwrap();
}

#[test]
fn text_and_file_packages_share_relative_inputs_and_normalization() {
    let temp = tempfile::tempdir().unwrap();
    let base = temp.path().canonicalize().unwrap();
    fs::write(
        base.join("spec.json"),
        include_str!("../../../tests/fixtures/catalog.provider.json"),
    )
    .unwrap();
    let yaml = "version: 1\npackage: parity\ntargets: {local: {kind: local}}\nenvironments: {demo: {imports: {api: {source: {kind: spec, file: spec.json}, bind: {target: local, endpoint: 'https://example.invalid/api'}}}}}\n";
    fs::write(base.join("env.yaml"), yaml).unwrap();
    let loader = LocalEnvironments::new(&base).unwrap();
    assert!(
        loader
            .capture_text(yaml, None, None)
            .err()
            .unwrap()
            .message
            .contains("explicit absolute base")
    );
    assert!(loader.capture_text(yaml, None, Some(".")).is_err());
    let file = loader.capture("env.yaml").unwrap();
    let text = loader
        .capture_text(yaml, Some("editor:parity"), Some(base.to_str().unwrap()))
        .unwrap();
    assert_eq!(file.yaml, text.yaml);
    let sources = |s: &wes_core::environments::CapturedSources| {
        s.iter()
            .map(|(key, value)| {
                (
                    key.clone(),
                    value.format().to_owned(),
                    value.bytes().to_owned(),
                )
            })
            .collect::<Vec<_>>()
    };
    assert_eq!(sources(&file.sources), sources(&text.sources));
    assert!(loader.capture_text(ENV, Some("editor:pure"), None).is_ok());
    assert!(loader.capture_text(ENV, Some("bad\norigin"), None).is_err());
    let container = "version: 1\ntargets: {box: {kind: docker, socket: /synthetic/docker.sock, container: fixture, inherit: container}}\nenvironments: {demo: {imports: {tool: {source: {kind: process, bin: python}, bind: {target: box}}}}}\n";
    assert!(
        loader.capture_text(container, None, None).is_ok(),
        "container commands never resolve against host base"
    );
}

#[tokio::test]
async fn text_archive_failure_rejects_both_operations_without_registry_mutation() {
    let temp = tempfile::tempdir().unwrap();
    let home = temp.path().join("home");
    let runtime = launch(RuntimeOptions::new(home.clone(), temp.path().into()))
        .await
        .unwrap();
    fs::write(home.join("imports"), "synthetic storage failure").unwrap();
    let session = runtime.handle.current().unwrap().session;
    let reply = session
        .submit(document("types", ":package load source:\"\"", TYPES))
        .await
        .unwrap();
    assert!(
        reply
            .diagnostics
            .diagnostics
            .iter()
            .any(|d| d.message.contains("could not be saved"))
    );
    assert!(
        session
            .submit(document("env", ":env plan source:\"\" > proposed", ENV))
            .await
            .unwrap_err()
            .to_string()
            .contains("could not be saved")
    );
    let state = session.observe().await.unwrap();
    assert!(!state.types.iter().any(|t| t == "GreetingText"));
    assert!(!state.environment_revisions.contains_key("text_demo"));
    runtime.shutdown().await.unwrap();
}

#[tokio::test]
async fn full_one_mib_document_is_accepted_without_source_escape_inflation() {
    let temp = tempfile::tempdir().unwrap();
    let runtime = launch(RuntimeOptions::new(
        temp.path().join("home"),
        temp.path().into(),
    ))
    .await
    .unwrap();
    let session = runtime.handle.current().unwrap().session;
    let mut text = String::from("types: {LargeText: {base: Text}}\n#");
    text.push_str(&"\\".repeat(wes_engine::source::max_source_bytes() - text.len()));
    let reply = tokio::time::timeout(
        std::time::Duration::from_secs(20),
        session.submit(document("large", ":package load source:\"\"", &text)),
    )
    .await
    .unwrap()
    .unwrap();
    accepted(&reply);
    assert!(
        session
            .observe()
            .await
            .unwrap()
            .types
            .iter()
            .any(|t| t == "LargeText")
    );
    runtime.shutdown().await.unwrap();
    let reopened = launch(RuntimeOptions::new(
        temp.path().join("home"),
        temp.path().into(),
    ))
    .await
    .unwrap();
    let state = reopened
        .handle
        .current()
        .unwrap()
        .session
        .observe()
        .await
        .unwrap();
    assert!(state.types.iter().any(|t| t == "LargeText"));
    assert_eq!(
        state
            .cells
            .iter()
            .find(|c| c.input.cell() == "large")
            .unwrap()
            .input
            .document(),
        Some(text.as_str())
    );
    reopened.shutdown().await.unwrap();
}

#[tokio::test]
async fn env_text_exclusivity_invalid_yaml_and_unapplied_plan_do_not_change_environments() {
    let temp = tempfile::tempdir().unwrap();
    let home = temp.path().join("home");
    let runtime = launch(RuntimeOptions::new(home.clone(), temp.path().into()))
        .await
        .unwrap();
    let session = runtime.handle.current().unwrap().session;
    for (n, text) in [
        ":env plan source:x file:x > p",
        ":env plan file:x base:/ > p",
        ":env plan source:\"version: 1\\nenvironments: [bad\" > p",
        ":env plan source:\"version: 1\\nenvironments: {}\" reconcile:file > p",
    ]
    .iter()
    .enumerate()
    {
        let result = session
            .submit(SourceInput::new(format!("bad{n}"), (*text).into()).unwrap())
            .await;
        assert!(
            result.as_ref().map_or(true, |r| r
                .diagnostics
                .diagnostics
                .iter()
                .any(|d| d.severity == Severity::Error)),
            "{text}"
        );
    }
    accepted(
        &session
            .submit(document(
                "unapplied",
                ":env plan source:\"\" > proposed",
                ENV,
            ))
            .await
            .unwrap(),
    );
    runtime.shutdown().await.unwrap();
    let runtime = launch(RuntimeOptions::new(home, temp.path().into()))
        .await
        .unwrap();
    assert!(
        !runtime
            .handle
            .current()
            .unwrap()
            .session
            .observe()
            .await
            .unwrap()
            .environment_revisions
            .contains_key("text_demo")
    );
    runtime.shutdown().await.unwrap();
}

#[test]
fn actual_document_example_runs_and_restores_its_checked_in_files() {
    let output = python::command()
        .arg(
            std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("../../examples/document-input/check.py"),
        )
        .args(["--binary", env!("CARGO_BIN_EXE_wes")])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}
