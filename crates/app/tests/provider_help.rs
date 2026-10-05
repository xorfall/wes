//! Exercise shipped provider metadata and environment scoping without any service calls.
use std::{
    sync::atomic::{AtomicUsize, Ordering},
    time::Duration,
};
use wes::runtime::{RuntimeOptions, launch};
use wes_core::Data;
use wes_engine::{session::SessionHandle, source::SourceInput};
static NEXT_CELL: AtomicUsize = AtomicUsize::new(0);

async fn submit(session: &SessionHandle, source: &str) -> wes_engine::session::SubmissionResult {
    let result = session
        .submit(
            SourceInput::new(
                format!("help-test-{}", NEXT_CELL.fetch_add(1, Ordering::Relaxed)),
                source.into(),
            )
            .unwrap(),
        )
        .await
        .unwrap();
    tokio::time::timeout(Duration::from_secs(10), session.wait_idle())
        .await
        .unwrap()
        .unwrap();
    result.as_ref().clone()
}
async fn help(session: &SessionHandle, source: &str) -> Data {
    let reply = submit(session, source).await;
    assert!(!reply.nodes.is_empty(), "{source}: {reply:?}");
    let snapshot = session.snapshot().await.unwrap();
    snapshot
        .execution
        .values
        .get(&reply.nodes[0])
        .unwrap_or_else(|| panic!("{source}: {snapshot:?}"))
        .data()
        .clone()
}
fn field(data: &Data, key: &str) -> Data {
    let Data::Record(fields) = data else {
        panic!("expected help record")
    };
    if let Some(value) = fields.get(key) {
        return value.clone();
    }
    field(&fields["invocation"], key)
}
fn has_error(reply: &wes_engine::session::SubmissionResult) -> bool {
    reply
        .diagnostics
        .diagnostics
        .iter()
        .any(|d| d.severity == wes_language::Severity::Error)
}

#[tokio::test]
async fn actual_http_help_and_managed_aliases_are_offline_and_survive_disabled_restart() {
    let root = tempfile::tempdir().unwrap();
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    let endpoint = format!("http://{}", listener.local_addr().unwrap());
    let descriptor = include_str!("../../../examples/http-domain/sensor.json");
    std::fs::write(root.path().join("sensor.json"), descriptor).unwrap();
    std::fs::write(
        root.path().join("other.json"),
        descriptor.replace("sample", "alternate"),
    )
    .unwrap();
    std::fs::write(root.path().join("environments.yaml"), format!(
        "version: 1\ntargets: {{local: {{kind: local}}}}\nenvironments:\n  first:\n    imports:\n      api:\n        source: {{kind: spec, file: sensor.json}}\n        bind: {{target: local, endpoint: '{endpoint}'}}\n  second:\n    imports:\n      api:\n        source: {{kind: spec, file: other.json}}\n        bind: {{target: local, endpoint: '{endpoint}'}}\n"
    )).unwrap();
    let home = root.path().join("home");
    let runtime = launch(RuntimeOptions::new(home.clone(), root.path().into()))
        .await
        .unwrap();
    let session = runtime.handle.current().unwrap().session;
    let overview = help(&session, ":help http > httpHelp").await;
    assert_eq!(field(&overview, "provider"), Data::Text("http".into()));
    let details = help(&session, ":help http request > requestHelp").await;
    let (metadata, _) = wes_adapters::http::direct(Default::default()).unwrap();
    assert_eq!(
        field(&details, "summary"),
        Data::Text(
            metadata
                .capabilities()
                .next()
                .unwrap()
                .summary
                .clone()
                .into()
        )
    );
    let Data::List(parameters) = field(&details, "parameters") else {
        panic!("parameters")
    };
    assert_eq!(parameters.len(), 4);
    assert_eq!(field(&details, "safety"), Data::Text("UNSAFE".into()));
    let plan = session
        .plan_environment_file("environments.yaml".into(), false)
        .await
        .unwrap();
    session.apply_environments(plan).await.unwrap();
    assert!(!has_error(&submit(&session, ":env use \"first\"").await));
    let first = help(&session, ":help api sample > firstHelp").await;
    assert_eq!(field(&first, "provider"), Data::Text("api".into()));
    assert!(has_error(&submit(&session, ":help http").await));
    assert!(!has_error(&submit(&session, ":env use \"second\"").await));
    assert!(has_error(&submit(&session, ":help api sample").await));
    let second = help(&session, ":help api alternate > secondHelp").await;
    assert_eq!(field(&second, "capability"), Data::Text("alternate".into()));
    assert!(!has_error(&submit(&session, ":env clear").await));
    assert!(has_error(&submit(&session, ":help api").await));
    assert!(has_error(&submit(&session, ":help http").await));
    runtime.shutdown().await.unwrap();
    let runtime = launch(RuntimeOptions::new(home, root.path().into()))
        .await
        .unwrap();
    let session = runtime.handle.current().unwrap().session;
    assert!(!has_error(&submit(&session, ":env use \"first\"").await));
    // Restored managed environments start disabled; help only needs retained metadata.
    assert_eq!(
        help(&session, ":help api sample > restoredHelp").await,
        first
    );
    assert_eq!(
        listener.accept().unwrap_err().kind(),
        std::io::ErrorKind::WouldBlock
    );
    runtime.shutdown().await.unwrap();
}

#[tokio::test]
async fn workspace_help_exposes_target_defaults_nominal_plan_and_complete_usage() {
    let root = tempfile::tempdir().unwrap();
    let runtime = launch(RuntimeOptions::new(
        root.path().join("home"),
        root.path().into(),
    ))
    .await
    .unwrap();
    let session = runtime.handle.current().unwrap().session;
    let family = help(&session, ":help workspace").await;
    let Data::List(children) = field(&family, "children") else {
        panic!("children")
    };
    assert!(
        children
            .iter()
            .any(|c| field(c, "name") == Data::Text("plan".into()))
    );
    assert!(
        children
            .iter()
            .any(|c| field(c, "name") == Data::Text("delete".into()))
    );
    let plan = help(&session, ":help workspace plan").await;
    assert_eq!(
        field(&plan, "usage"),
        Data::Text(":workspace plan delete [workspace:\"name\"] [> plan]".into())
    );
    assert_eq!(
        field(&plan, "planType"),
        Data::Text("WorkspaceDeletePlan".into())
    );
    let Data::List(parameters) = field(&plan, "parameters") else {
        panic!("parameters")
    };
    assert_eq!(parameters.len(), 1);
    assert_eq!(
        field(&parameters[0], "name"),
        Data::Text("workspace".into())
    );
    assert_eq!(field(&parameters[0], "required"), Data::Bool(false));
    let Data::Text(rules) = field(&plan, "semantics") else {
        panic!("semantics")
    };
    assert!(rules.contains("defaults to the issuing workspace"));
    assert!(rules.contains("missing name is never created"));
    let delete = help(&session, ":help workspace delete").await;
    let Data::Text(usage) = field(&delete, "usage") else {
        panic!("usage")
    };
    assert!(usage.starts_with(":workspace delete $plan"));
    assert!(usage.contains("stop:") && usage.contains("protected:"));
    assert_eq!(
        field(&delete, "planType"),
        Data::Text("WorkspaceDeletePlan".into())
    );
    runtime.shutdown().await.unwrap();
}
