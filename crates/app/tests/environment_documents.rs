//! Current-document editing through real admission, isolated storage and synthetic provider inputs.
#[path = "support/fixtures.rs"]
mod fixtures;
#[path = "support/shell.rs"]
mod shell;
use std::path::Path;
use wes::runtime::{RuntimeOptions, launch};
use wes_core::Data;
use wes_engine::{session::SessionHandle, source::SourceInput};
use wes_language::Severity;
fn options(root: &Path) -> RuntimeOptions {
    let mut options = RuntimeOptions::new(root.join("home"), root.into());
    options.docker_candidates = Some(vec![root.join("missing.sock")]);
    options
}
async fn submit(
    session: &SessionHandle,
    source: &str,
    document: Option<String>,
) -> std::sync::Arc<wes_engine::session::SubmissionResult> {
    let input = SourceInput::new(uuid::Uuid::new_v4().to_string(), source.into())
        .unwrap()
        .with_document(document)
        .unwrap();
    let result = session.submit(input).await.unwrap();
    session.wait_idle().await.unwrap();
    result
}
fn accepted(result: &std::sync::Arc<wes_engine::session::SubmissionResult>) {
    assert!(
        result
            .diagnostics
            .diagnostics
            .iter()
            .all(|d| d.severity != Severity::Error),
        "{result:?}"
    );
}
async fn apply(
    session: &SessionHandle,
    document: &wes_engine::environments::EnvironmentDocument,
    source: String,
) {
    accepted(
        &submit(
            session,
            &format!(
                ":env plan source:\"\" origin:{} > proposal",
                serde_json::to_string(&document.origin).unwrap()
            ),
            Some(source),
        )
        .await,
    );
    accepted(&submit(session, ":env apply $proposal", None).await);
}
#[tokio::test]
async fn current_documents_promote_default_preserve_other_owners_and_replay_captured_imports() {
    let root = tempfile::tempdir().unwrap();
    fixtures::echo(root.path());
    let runtime = launch(options(root.path())).await.unwrap();
    let session = runtime.handle.current().unwrap().session;
    let docs = session.environment_documents().await.unwrap();
    assert_eq!(docs.len(), 1);
    assert_eq!(docs[0].name, "default");
    let package = wes_core::environments::Package::parse(&docs[0].source).unwrap();
    assert_eq!(package.definitions()["default"].imports.len(), 3);
    assert!(
        package.definitions()["default"]
            .imports
            .contains_key(shell::NAME)
    );
    assert_eq!(
        package.definitions()["default"].imports["docker"].binding_mode,
        wes_core::environments::BindingMode::Automatic
    );
    let before = submit(&session, &format!("{} > before", shell::print("old")), None).await;
    accepted(&before);
    // Another owner is allowed to call its (different) target local.
    let other = format!(
        "version: 1\npackage: work\ntargets: {{local: {{kind: local, cwd: {}}}}}\nenvironments:\n  dev: {{imports: {{shell: {{source: {{kind: builtin, name: sh}}, bind: {{target: local}}}}}}}}\n",
        serde_json::to_string(root.path().to_str().unwrap()).unwrap()
    );
    accepted(
        &submit(
            &session,
            ":env plan source:\"\" origin:\"editor:new\" > other_plan",
            Some(other),
        )
        .await,
    );
    accepted(&submit(&session, ":env apply $other_plan", None).await);
    let docs = session.environment_documents().await.unwrap();
    assert_eq!(docs.len(), 2);
    let default = docs.iter().find(|d| d.name == "default").unwrap();
    assert!(default.source.contains("local_1"));
    apply(&session, default, default.source.clone()).await;
    let stale = session
        .submit(
            SourceInput::new(
                "stale-editor".into(),
                format!(
                    ":env plan source:\"\" origin:{} > stale",
                    serde_json::to_string(&default.origin).unwrap()
                ),
            )
            .unwrap()
            .with_document(Some(default.source.clone()))
            .unwrap(),
        )
        .await;
    assert!(format!("{stale:?}").contains("changed since"));
    accepted(&submit(&session, &format!("{} > after", shell::print("new")), None).await);
    accepted(
        &submit(
            &session,
            ":import process bin:./example-echo.bin as:echo\necho run args:captured > imported",
            None,
        )
        .await,
    );
    let docs = session.environment_documents().await.unwrap();
    let default = docs.iter().find(|d| d.name == "default").unwrap();
    assert!(default.source.contains("echo"));
    apply(&session, default, default.source.clone()).await;
    // Applying the editable default document replaces configured handles with explicit recipes.
    // That capture change requires a new branch, even when its shell destination looks the same.
    let refused = submit(&session, ":refresh $before", None).await;
    assert!(
        refused
            .diagnostics
            .diagnostics
            .iter()
            .any(|d| d.code == "ENV039")
    );
    let state = session.snapshot().await.unwrap();
    let Data::Record(fields) = state.execution.values[before.nodes.last().unwrap()].data() else {
        panic!()
    };
    assert_eq!(fields["stdout"], Data::Bytes(b"old".to_vec().into()));
    let revisions = session.environment_revisions().await.unwrap();
    runtime.shutdown().await.unwrap();
    let runtime = launch(options(root.path())).await.unwrap();
    let session = runtime.handle.current().unwrap().session;
    assert_eq!(session.environment_revisions().await.unwrap(), revisions);
    assert_eq!(session.environment_documents().await.unwrap().len(), 2);
    accepted(&submit(&session, ":env enable \"default\"", None).await);
    // Applying the editable default document replaces configured handles with explicit recipes.
    // That capture change requires a new branch, even when its shell destination looks the same.
    let refused = submit(&session, ":refresh $before", None).await;
    assert!(
        refused
            .diagnostics
            .diagnostics
            .iter()
            .any(|d| d.code == "ENV039")
    );
    runtime.shutdown().await.unwrap();
}

#[tokio::test]
async fn editor_reuses_captured_sources_and_cooperative_imports_cannot_replace_an_existing_alias() {
    let root = tempfile::tempdir().unwrap();
    fixtures::echo(root.path());
    std::fs::write(root.path().join("fixture.json"), r#"{"version":1,"provider":"fixture","types":{},"operations":[{"path":["items"],"method":"GET","route":"/items","auth":[],"parameters":[],"responses":{"200":"Text"}}]}"#).unwrap();
    let runtime = launch(options(root.path())).await.unwrap();
    let session = runtime.handle.current().unwrap().session;
    accepted(
        &submit(
            &session,
            ":import spec file:fixture.json endpoint:http://127.0.0.1:1",
            None,
        )
        .await,
    );
    std::fs::remove_file(root.path().join("fixture.json")).unwrap();
    let doc = session.environment_documents().await.unwrap().remove(0);
    apply(&session, &doc, doc.source.clone()).await;
    let doc = session.environment_documents().await.unwrap().remove(0);
    apply(&session, &doc, doc.source.clone()).await;
    let actor = "synthetic-agent";
    session
        .observe_actor(actor.into(), "default".into())
        .await
        .unwrap();
    let send = |source: &str| {
        SourceInput::new(uuid::Uuid::new_v4().to_string(), source.into())
            .unwrap()
            .with_client(actor.into())
            .unwrap()
            .cooperative()
    };
    accepted(
        &session
            .submit(send(":import process bin:./example-echo.bin as:agent_echo"))
            .await
            .unwrap(),
    );
    let before = session.environment_revisions().await.unwrap();
    let denied = session
        .submit(send(":import process bin:./example-echo.bin as:agent_echo"))
        .await;
    assert!(
        format!("{denied:?}").contains("requires user authority"),
        "{denied:?}"
    );
    assert_eq!(session.environment_revisions().await.unwrap(), before);
    let doc = session.environment_documents().await.unwrap().remove(0);
    let input = send(&format!(
        ":env plan source:\"\" origin:{} > agent_plan",
        serde_json::to_string(&doc.origin).unwrap()
    ))
    .with_document(Some(
        doc.source.replace("protected: true", "protected: false"),
    ))
    .unwrap();
    accepted(&session.submit(input).await.unwrap());
    let denied = session.submit(send(":env apply $agent_plan")).await;
    assert!(
        format!("{denied:?}").contains("shared environment definitions"),
        "{denied:?}"
    );
    assert_eq!(session.environment_revisions().await.unwrap(), before);
    runtime.shutdown().await.unwrap();
}

#[tokio::test]
async fn runnable_builtin_recipe_uses_actual_example_files_without_docker_io() {
    let root = tempfile::tempdir().unwrap();
    let runtime = launch(options(root.path())).await.unwrap();
    let session = runtime.handle.current().unwrap().session;
    accepted(
        &submit(
            &session,
            ":env plan source:\"\" origin:\"editor:example\" > recipe",
            Some(include_str!("../../../examples/environment-editor/environments.yaml").into()),
        )
        .await,
    );
    accepted(&submit(&session, ":env apply $recipe", None).await);
    accepted(&submit(&session, ":env use \"demo\"", None).await);
    let result = submit(
        &session,
        if cfg!(windows) {
            include_str!("../../../examples/environment-editor/demo.windows.wes")
        } else {
            include_str!("../../../examples/environment-editor/demo.wes")
        },
        None,
    )
    .await;
    accepted(&result);
    let snapshot = session.snapshot().await.unwrap();
    let Data::Record(fields) = snapshot.execution.values[result.nodes.last().unwrap()].data()
    else {
        panic!()
    };
    assert_eq!(
        fields["stdout"],
        Data::Bytes(b"environment-editor-ok".to_vec().into())
    );
    runtime.shutdown().await.unwrap();
}
