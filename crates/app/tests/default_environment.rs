//! Real runtime, private homes and loopback-only providers; no live user data/services.
#[path = "support/fixtures.rs"]
mod fixtures;
#[path = "support/shell.rs"]
mod shell;
use std::{sync::Arc, time::Duration};
use wes::runtime::{RuntimeOptions, launch};
use wes_core::Data;
use wes_engine::{
    session::{SessionHandle, SubmissionResult},
    source::SourceInput,
};

async fn submit(session: &SessionHandle, source: &str) -> Arc<SubmissionResult> {
    let result = tokio::time::timeout(
        Duration::from_secs(10),
        session.submit(SourceInput::new(uuid::Uuid::new_v4().to_string(), source.into()).unwrap()),
    )
    .await
    .unwrap()
    .unwrap();
    tokio::time::timeout(Duration::from_secs(10), session.wait_idle())
        .await
        .unwrap()
        .unwrap();
    result
}
fn accepted(result: &SubmissionResult) {
    assert!(!result.accepted.is_empty(), "{result:?}");
    assert!(
        !result
            .diagnostics
            .diagnostics
            .iter()
            .any(|d| d.severity == wes_language::Severity::Error),
        "{result:?}"
    );
}
async fn value(session: &SessionHandle, source: &str) -> Data {
    let result = submit(session, source).await;
    accepted(&result);
    let snapshot = session.snapshot().await.unwrap();
    snapshot
        .execution
        .values
        .get(result.nodes.last().unwrap())
        .unwrap_or_else(|| panic!("{source}: {snapshot:?}"))
        .data()
        .clone()
}
fn field(data: &Data, name: &str) -> Data {
    let Data::Record(fields) = data else {
        panic!("expected record: {data:?}")
    };
    fields[name].clone()
}
/// The named providers and the host's shell provider, in a listing's order.
fn listed(others: &[&str]) -> Vec<String> {
    let mut names: Vec<String> = others.iter().map(|name| (*name).to_owned()).collect();
    names.push(shell::NAME.into());
    names.sort();
    names
}
fn providers(data: Data, environment: &str) -> Vec<String> {
    let Data::List(rows) = data else {
        panic!("expected provider rows")
    };
    rows.iter()
        .map(|row| {
            assert_eq!(field(row, "environment"), Data::Text(environment.into()));
            let Data::Text(name) = field(row, "provider") else {
                panic!("provider name")
            };
            name.to_string()
        })
        .collect()
}

#[tokio::test]
async fn current_contract_example_reopens_retained_value_with_its_producing_run() {
    let root = tempfile::tempdir().unwrap();
    let home = root.path().join("home");
    let runtime = launch(RuntimeOptions::new(home.clone(), root.path().into()))
        .await
        .unwrap();
    let session = runtime.handle.current().unwrap().session;
    let result = submit(
        &session,
        include_str!("../../../examples/current-contracts/main.wes"),
    )
    .await;
    accepted(&result);
    let node = result.nodes.last().unwrap().clone();
    let snapshot = session.snapshot().await.unwrap();
    let expected = Data::List(vec![Data::Int(1), Data::Int(2), Data::Int(3)]);
    assert_eq!(snapshot.execution.values[&node].data(), &expected);
    let run = snapshot.execution.runs[&node].clone();
    runtime.shutdown().await.unwrap();

    let reopened = launch(RuntimeOptions::new(home, root.path().into()))
        .await
        .unwrap();
    let session = reopened.handle.current().unwrap().session;
    let snapshot = session.snapshot().await.unwrap();
    assert_eq!(snapshot.execution.runs[&node], run);
    assert_eq!(snapshot.execution.values[&node].data(), &expected);
    assert_eq!(value(&session, ":read $sample").await, expected);
    reopened.shutdown().await.unwrap();
}

#[tokio::test]
async fn endpoint_reimport_changes_revision_without_retargeting_old_nodes_or_reducing_import_size_limit()
 {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    async fn endpoint(body: &'static str, calls: usize) -> (String, tokio::task::JoinHandle<()>) {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let task = tokio::spawn(async move {
            for _ in 0..calls {
                let (mut stream, _) =
                    tokio::time::timeout(Duration::from_secs(10), listener.accept())
                        .await
                        .unwrap()
                        .unwrap();
                let mut request = [0; 4096];
                assert!(stream.read(&mut request).await.unwrap() > 0);
                let body = format!("\"{body}\"");
                stream.write_all(format!("HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len()).as_bytes()).await.unwrap();
            }
        });
        (url, task)
    }
    let root = tempfile::tempdir().unwrap();
    let descriptor = r#"{"version":1,"provider":"fixture","types":{},"operations":[{"path":["read"],"method":"GET","route":"/read","auth":[],"parameters":[],"responses":{"200":"Text"}}]}"#;
    // Ordinary imports support 1 MiB: do not accidentally impose managed recipes' 256 KiB limit.
    std::fs::write(
        root.path().join("large.json"),
        format!("{}{descriptor}", " ".repeat(300 * 1024)),
    )
    .unwrap();
    let runtime = launch(RuntimeOptions::new(
        root.path().join("home"),
        root.path().into(),
    ))
    .await
    .unwrap();
    let session = runtime.handle.current().unwrap().session;
    let (first, first_server) = endpoint("first", 1).await;
    let (second, second_server) = endpoint("second", 1).await;
    let original = submit(
        &session,
        &format!(":import spec file:large.json endpoint:\"{first}\"\nfixture read"),
    )
    .await;
    accepted(&original);
    let node = original.nodes.last().unwrap();
    assert_eq!(
        field(
            session.snapshot().await.unwrap().execution.values[node].data(),
            "body"
        ),
        Data::Text("first".into())
    );
    let revision = session.environment_revisions().await.unwrap()["default"];
    assert_eq!(
        field(
            &value(
                &session,
                &format!(
                    ":import spec file:large.json endpoint:\"{second}\" replace:true\nfixture read"
                )
            )
            .await,
            "body"
        ),
        Data::Text("second".into())
    );
    assert_ne!(
        session.environment_revisions().await.unwrap()["default"],
        revision
    );
    let before = session.snapshot().await.unwrap().execution.runs[node].clone();
    let refused = submit(&session, &format!(":refresh ${node}")).await;
    assert!(
        refused
            .diagnostics
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.code == "ENV039")
    );
    let message = &refused
        .diagnostics
        .diagnostics
        .iter()
        .find(|d| d.code == "ENV039")
        .unwrap()
        .message;
    assert!(message.contains(&first), "{message}");
    assert!(!message.contains(&second), "{message}");
    let inspected = value(&session, &format!(":inspect ${node}")).await;
    let inspected = String::from_utf8(
        wes_adapters::codec::encode_json(&inspected, Default::default()).unwrap(),
    )
    .unwrap();
    assert!(inspected.contains(&first), "{inspected}");
    assert!(!inspected.contains("target local · target local"));
    assert!(refused.refreshed.is_empty());
    assert_eq!(
        session.snapshot().await.unwrap().execution.runs[node],
        before
    );
    assert_eq!(
        field(
            session.snapshot().await.unwrap().execution.values[node].data(),
            "body"
        ),
        Data::Text("first".into())
    );
    first_server.await.unwrap();
    second_server.await.unwrap();
    let node = node.clone();
    runtime.shutdown().await.unwrap();
    let reopened = launch(RuntimeOptions::new(
        root.path().join("home"),
        root.path().into(),
    ))
    .await
    .unwrap();
    let session = reopened.handle.current().unwrap().session;
    let inspected = value(&session, &format!(":inspect ${node}")).await;
    let text = String::from_utf8(
        wes_adapters::codec::encode_json(&inspected, Default::default()).unwrap(),
    )
    .unwrap();
    assert!(text.contains(&first));
    let refused = submit(&session, &format!(":refresh ${node}")).await;
    assert!(
        refused
            .diagnostics
            .diagnostics
            .iter()
            .any(|d| d.code == "ENV039" && d.message.contains(&first))
    );
    reopened.shutdown().await.unwrap();
}

#[tokio::test]
async fn fresh_default_executes_shell_and_http_and_discovery_owns_the_same_namespace() {
    let root = tempfile::tempdir().unwrap();
    let runtime = launch(RuntimeOptions::new(
        root.path().join("home"),
        root.path().into(),
    ))
    .await
    .unwrap();
    let session = runtime.handle.current().unwrap().session;
    let observation = session.observe().await.unwrap();
    assert_eq!(observation.default_environment.as_deref(), Some("default"));
    assert!(observation.environment_enabled["default"]);
    assert!(observation.environment_clients.is_empty());
    assert_eq!(
        providers(value(&session, ":list providers").await, "default"),
        listed(&["docker", "http"])
    );
    assert_eq!(
        field(
            &value(&session, &shell::print("default-ok")).await,
            "stdout"
        ),
        Data::Bytes(b"default-ok".to_vec().into())
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let (mut socket, _) = listener.accept().await.unwrap();
        let mut request = [0; 4096];
        assert!(socket.read(&mut request).await.unwrap() > 0);
        socket
            .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\nok")
            .await
            .unwrap();
    });
    let response = value(&session, &format!("http request url:\"http://{address}/\"")).await;
    assert_eq!(field(&response, "status"), Data::Int(200));
    tokio::time::timeout(Duration::from_secs(5), server)
        .await
        .unwrap()
        .unwrap();
    let observation = session.observe().await.unwrap();
    for node in observation.state.execution.graph.nodes() {
        if let Some(call) = node.payload().call() {
            assert_eq!(call.environment().unwrap().environment().name(), "default");
        }
    }
    runtime.shutdown().await.unwrap();
}

#[tokio::test]
async fn default_imports_keep_batch_semantics_captured_revisions_and_offline_reopening() {
    let root = tempfile::tempdir().unwrap();
    fixtures::echo(root.path());
    let home = root.path().join("home");
    let descriptor = root.path().join("fixture.json");
    std::fs::write(&descriptor, r#"{"version":1,"provider":"fixture","types":{},"operations":[{"path":["items"],"method":"GET","route":"/items","auth":[],"parameters":[],"responses":{"200":"Text"}}]}"#).unwrap();
    let runtime = launch(RuntimeOptions::new(home.clone(), root.path().into()))
        .await
        .unwrap();
    let session = runtime.handle.current().unwrap().session;
    let before = session.environment_revisions().await.unwrap()["default"];
    accepted(submit(&session, ":env use \"default\"").await.as_ref());
    let initial_context = wes_core::environments::EnvironmentContext {
        selected: Some("default".into()),
        revisions: session.environment_revisions().await.unwrap(),
    };
    let old = submit(&session, &shell::print("before")).await;
    accepted(&old);
    assert_eq!(
        field(
            &value(
                &session,
                include_str!("../../../examples/scripts/default-environment/default.wes")
            )
            .await,
            "stdout"
        ),
        Data::Bytes(b"import-ok\n".to_vec().into())
    );
    // Inferred descriptor alias and subsequent metadata query share the same atomic source draft.
    assert!(
        providers(
            value(
                &session,
                ":import spec file:fixture.json endpoint:http://127.0.0.1:1\n:list providers"
            )
            .await,
            "default"
        )
        .contains(&"fixture".into())
    );
    let after = session.environment_revisions().await.unwrap()["default"];
    assert_ne!(before, after);
    for source in [
        &shell::print("stale-must-not-run"),
        ":import process bin:./example-echo.bin as:stale",
    ] {
        let result = session
            .submit(
                SourceInput::new(uuid::Uuid::new_v4().to_string(), source.into())
                    .unwrap()
                    .with_environments(initial_context.clone())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert!(
            result.accepted.is_empty(),
            "stale context must not bind or import: {result:?}"
        );
    }
    let observation = session.observe().await.unwrap();
    let binding = observation
        .state
        .execution
        .graph
        .node(&old.nodes[0])
        .unwrap()
        .payload()
        .call()
        .unwrap()
        .environment()
        .unwrap();
    assert_eq!(binding.environment().revision(), before);
    assert_eq!(
        field(&value(&session, ":help fixture items").await, "provider"),
        Data::Text("fixture".into())
    );
    runtime.shutdown().await.unwrap();
    std::fs::remove_file(descriptor).unwrap(); // replay must not re-open importer input
    let runtime = launch(RuntimeOptions::new(home, root.path().into()))
        .await
        .unwrap();
    let session = runtime.handle.current().unwrap().session;
    assert_eq!(
        session.environment_revisions().await.unwrap()["default"],
        after
    );
    assert_eq!(
        providers(value(&session, ":list providers").await, "default"),
        listed(&["docker", "echo", "fixture", "http"])
    );
    assert_eq!(
        field(&value(&session, "echo run args:reopened").await, "stdout"),
        Data::Bytes(b"reopened\n".to_vec().into())
    );
    assert_eq!(
        field(&value(&session, &shell::print("reopened")).await, "stdout"),
        Data::Bytes(b"reopened".to_vec().into())
    );
    runtime.shutdown().await.unwrap();
}

#[tokio::test]
async fn explicit_environment_and_clear_never_fall_back_and_reopen_does_not_enable_user_environments()
 {
    let root = tempfile::tempdir().unwrap();
    let home = root.path().join("home");
    std::fs::write(
        root.path().join("env.yaml"),
        include_str!("../../../examples/scripts/default-environment/environments.yaml"),
    )
    .unwrap();
    fixtures::echo(root.path());
    let runtime = launch(RuntimeOptions::new(home.clone(), root.path().into()))
        .await
        .unwrap();
    let session = runtime.handle.current().unwrap().session;
    let plan = session
        .plan_environment_file("env.yaml".into(), false)
        .await
        .unwrap();
    session.apply_environments(plan).await.unwrap();
    assert_eq!(
        providers(value(&session, ":list providers").await, "default"),
        listed(&["docker", "http"])
    );
    accepted(submit(&session, ":env use \"pg-demo\"").await.as_ref());
    assert_eq!(
        field(
            &value(&session, "pg run args:environment-ok").await,
            "stdout"
        ),
        Data::Bytes(b"environment-ok\n".to_vec().into())
    );
    assert_eq!(
        providers(value(&session, ":list providers").await, "pg-demo"),
        ["pg"]
    );
    assert!(
        submit(&session, &shell::print("must-not-run"))
            .await
            .nodes
            .is_empty()
    );
    assert!(
        submit(&session, "http request url:\"http://127.0.0.1:1\"")
            .await
            .nodes
            .is_empty()
    );
    assert!(submit(&session, ":help http").await.nodes.is_empty());
    assert!(submit(&session, ":help docker").await.nodes.is_empty());
    accepted(submit(&session, ":env clear").await.as_ref());
    assert_eq!(value(&session, ":list providers").await, Data::List(vec![]));
    assert!(
        submit(&session, &shell::print("must-not-run"))
            .await
            .nodes
            .is_empty()
    );
    runtime.shutdown().await.unwrap();
    let runtime = launch(RuntimeOptions::new(home, root.path().into()))
        .await
        .unwrap();
    let session = runtime.handle.current().unwrap().session;
    let observation = session.observe().await.unwrap();
    assert!(observation.environment_enabled["default"]);
    assert!(!observation.environment_enabled["pg-demo"]);
    assert!(observation.environment_clients.is_empty());
    assert_eq!(
        field(&value(&session, &shell::print("ready")).await, "stdout"),
        Data::Bytes(b"ready".to_vec().into())
    );
    assert_eq!(
        providers(value(&session, ":list providers").await, "default"),
        listed(&["docker", "http"])
    );
    runtime.shutdown().await.unwrap();
}

#[cfg(unix)]
#[tokio::test]
async fn unknown_provider_revision_remains_an_explicit_reconstruction_error() {
    let root = tempfile::tempdir().unwrap();
    let home = root.path().join("home");
    let source = ":list providers > answer";
    let record = wes_engine::history::JournalEntry::Command(wes_engine::history::CommandRecord {
        source_name: "fixture.wes".into(),
        source_start: wes_language::Position { line: 1, column: 1 },
        changed_nodes: vec![],
        document: None,
        revision_of: None,
        environments: Some(wes_core::environments::EnvironmentContext {
            selected: Some("default".into()),
            revisions: [(
                "default".into(),
                format!("sha256:{}", "0".repeat(64)).parse().unwrap(),
            )]
            .into(),
        }),
        cell: "synthetic-revision".into(),
        text: source.into(),
        replay: source.into(),
        nodes: vec![wes_engine::graph::NodeId::new("id1000").unwrap()],
        type_sources: Default::default(),
        calculation_package: None,
        imports: vec![],
    });
    let journal = String::from_utf8(
        wes_adapters::codec::history::encode_journal(&record, Default::default()).unwrap(),
    )
    .unwrap()
        + "\n";
    let file = install_revision_fixture(&home, &journal, "00000000000000000000000000000002");
    let error = match launch(RuntimeOptions::new(home, root.path().into())).await {
        Ok(runtime) => {
            runtime.shutdown().await.unwrap();
            panic!("unknown revision accepted")
        }
        Err(error) => error,
    };
    let message = error.to_string();
    assert!(message.contains("historical command"), "{message}");
    assert!(message.contains("ENV008"), "{message}");
    assert!(
        !message.contains(":list providers"),
        "do not expose historical source in the startup error"
    );
    assert_eq!(std::fs::read_to_string(file).unwrap(), journal);
}

#[cfg(unix)]
fn install_revision_fixture(
    home: &std::path::Path,
    journal: &str,
    generation: &str,
) -> std::path::PathBuf {
    use std::os::unix::fs::PermissionsExt;
    use wes_adapters::{
        journal::{Durability, FileHistory, ReadLimits},
        workspaces::FileWorkspaces,
    };
    drop(wes::data_home::DataHome::open(home).unwrap());
    let directory = home.join("workspaces");
    drop(FileWorkspaces::open(&directory, ReadLimits::default(), Durability::File).unwrap());
    let path = directory.join(format!("generation-{generation}"));
    drop(FileHistory::open(&path, ReadLimits::default(), Durability::File).unwrap());
    let recovery = path.join("recovery.jsonl");
    std::fs::write(&recovery, "").unwrap();
    std::fs::set_permissions(&recovery, std::fs::Permissions::from_mode(0o600)).unwrap();
    let file = path.join("journal.jsonl");
    std::fs::write(&file, journal).unwrap();
    std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o600)).unwrap();
    let pointer = directory.join("workspace-64656661756c74");
    std::fs::write(
        &pointer,
        format!("wes.workspace\n1\n{generation}\n{generation}\n"),
    )
    .unwrap();
    std::fs::set_permissions(&pointer, std::fs::Permissions::from_mode(0o600)).unwrap();
    file
}
