//! Captured Docker connections, no Docker installation or real daemon access.
#![cfg(unix)]
#[path = "support/python.rs"]
mod python;
use std::{
    sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use wes::runtime::{RuntimeOptions, launch};
use wes_core::Data;
use wes_engine::{session::SessionHandle, source::SourceInput};

async fn submit(
    session: &SessionHandle,
    source: &str,
) -> Arc<wes_engine::session::SubmissionResult> {
    let reply = session
        .submit(SourceInput::new(uuid::Uuid::new_v4().to_string(), source.into()).unwrap())
        .await
        .unwrap();
    tokio::time::timeout(Duration::from_secs(10), session.wait_idle())
        .await
        .unwrap()
        .unwrap();
    assert!(
        !reply
            .diagnostics
            .diagnostics
            .iter()
            .any(|d| d.severity == wes_language::Severity::Error),
        "{source}: {reply:?}"
    );
    reply
}
async fn value(session: &SessionHandle, source: &str) -> Data {
    let reply = submit(session, source).await;
    let snapshot = session.snapshot().await.unwrap();
    snapshot
        .execution
        .values
        .get(reply.nodes.last().unwrap())
        .unwrap_or_else(|| panic!("{snapshot:?}"))
        .data()
        .clone()
}
struct Daemon {
    path: String,
    requests: Arc<Mutex<Vec<String>>>,
    closed: Arc<AtomicUsize>,
    task: tokio::task::JoinHandle<()>,
}
impl Drop for Daemon {
    fn drop(&mut self) {
        self.task.abort();
    }
}
fn daemon(root: &std::path::Path, name: &str) -> Daemon {
    let path = root
        .join(format!("{name}.sock"))
        .to_str()
        .unwrap()
        .to_owned();
    let listener = tokio::net::UnixListener::bind(&path).unwrap();
    let requests = Arc::new(Mutex::new(vec![]));
    let calls = requests.clone();
    let closed = Arc::new(AtomicUsize::new(0));
    let closed_stream = closed.clone();
    let name = name.to_owned();
    let task = tokio::spawn(async move {
        let mut tasks = tokio::task::JoinSet::new();
        loop {
            tokio::select! {
                accepted = listener.accept() => {
                    let (mut socket, _) = accepted.unwrap();
                    let calls = calls.clone(); let name = name.clone(); let closed = closed_stream.clone();
                    tasks.spawn(async move {
                        let mut bytes = vec![];
                        while !bytes.ends_with(b"\r\n\r\n") {
                            let mut byte = [0]; socket.read_exact(&mut byte).await.unwrap(); bytes.extend(byte);
                            assert!(bytes.len() < 8192);
                        }
                        let request = String::from_utf8(bytes).unwrap();
                        let path = request.split_whitespace().nth(1).unwrap();
                        calls.lock().unwrap().push(path.into());
                        if path.starts_with("/v1.45/events?") {
                            socket.write_all(b"HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nTransfer-Encoding: chunked\r\n\r\n").await.unwrap();
                            let mut byte = [0];
                            let _ = socket.read(&mut byte).await;
                            closed.fetch_add(1, Ordering::SeqCst); return;
                        }
                        let id = "a".repeat(64);
                        let body = if path == "/version" {
                            serde_json::json!({"ApiVersion":"1.54","MinAPIVersion":"1.40"})
                        } else if path.starts_with("/v1.45/containers/json?") {
                            serde_json::json!([{"Id":id,"Names":[format!("/{name}")],"ImageID":format!("sha256:{}", "b".repeat(64)),"State":"running","Created":1,"Labels":{}}])
                        } else if path == format!("/v1.45/containers/{id}/json") {
                            serde_json::json!({"Id":id,"Name":format!("/{name}"),"Image":format!("sha256:{}", "b".repeat(64)),"Created":"2026-09-25T00:00:00Z","State":{"Status":"running","Running":true,"ExitCode":0,"OOMKilled":false},"RestartCount":0,"Config":{"Tty":false}})
                        } else { panic!("unexpected fixture request: {path}") };
                        let body = body.to_string();
                        socket.write_all(format!("HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len()).as_bytes()).await.unwrap();
                    });
                }
                Some(result) = tasks.join_next(), if !tasks.is_empty() => { result.unwrap(); }
            }
        }
    });
    Daemon {
        path,
        requests,
        closed,
        task,
    }
}

#[tokio::test]
async fn default_help_import_completion_rebinding_restore_and_authority_share_one_owner() {
    // Short paths also fit macOS's Unix-socket address limit.
    let root = tempfile::Builder::new()
        .prefix("wes-dd-")
        .tempdir_in("/tmp")
        .unwrap();
    let a = daemon(root.path(), "first");
    let b = daemon(root.path(), "second");
    let home = root.path().join("home");
    let runtime = launch(isolated_options(home.clone(), root.path()))
        .await
        .unwrap();
    let session = runtime.handle.current().unwrap().session;
    for help in [
        ":help docker",
        ":help docker inspect",
        ":help import docker",
    ] {
        value(&session, help).await;
    }
    assert!(a.requests.lock().unwrap().is_empty());
    let reply = submit(&session, "docker containers").await;
    let snapshot = session.snapshot().await.unwrap();
    let errors = format!("{:?}", snapshot.execution);
    assert!(
        errors.contains("DOCKER_CONNECTION") && errors.contains("/edit env"),
        "{reply:?}: {errors}"
    );
    let first = value(
        &session,
        &format!(
            ":import docker socket:{} as:docker replace:true\ndocker containers > first_inventory",
            serde_json::to_string(&a.path).unwrap()
        ),
    )
    .await;
    assert_eq!(a.requests.lock().unwrap().len(), 2);
    let resources = wes_engine::completion::retained_resources(&session.observe().await.unwrap());
    assert_eq!(resources.len(), 1);
    assert_eq!(resources[0].items[0].label, "first");
    assert_eq!(resources[0].items[0].value, "a".repeat(64));
    assert_eq!(
        a.requests.lock().unwrap().len(),
        2,
        "completion performs no I/O"
    );
    value(
        &session,
        &format!(
            ":import docker socket:{} as:docker replace:true\ndocker containers > second_inventory",
            serde_json::to_string(&b.path).unwrap()
        ),
    )
    .await;
    assert_eq!(b.requests.lock().unwrap().len(), 2);
    refuse_changed_binding(&session, ":refresh $first_inventory").await;
    let snapshot = session.snapshot().await.unwrap();
    let first_node = snapshot.names["first_inventory"].clone();
    // Refused refresh preserves the original result and never enters either daemon.
    let Data::Record(before) = first else {
        panic!()
    };
    let Data::Record(after) = snapshot.execution.values[&first_node.node].data() else {
        panic!()
    };
    assert_eq!(before["rows"], after["rows"]);
    assert_eq!(a.requests.lock().unwrap().len(), 2);
    let event = session
        .submit(
            SourceInput::new(
                "events".into(),
                format!("docker events container:{} > events", "a".repeat(64)),
            )
            .unwrap(),
        )
        .await
        .unwrap();
    assert!(!event.nodes.is_empty(), "{event:?}");
    tokio::time::timeout(Duration::from_secs(10), async {
        while !b
            .requests
            .lock()
            .unwrap()
            .iter()
            .any(|p| p.starts_with("/v1.45/events?"))
        {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    submit(&session, ":env disable \"default\"").await;
    tokio::time::timeout(Duration::from_secs(10), async {
        while b.closed.load(Ordering::SeqCst) == 0 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    let count = b.requests.lock().unwrap().len();
    let _ = submit(&session, "docker containers").await;
    assert_eq!(b.requests.lock().unwrap().len(), count);
    runtime.shutdown().await.unwrap();
    let before_a = a.requests.lock().unwrap().len();
    let before_b = b.requests.lock().unwrap().len();
    let runtime = launch(isolated_options(home, root.path())).await.unwrap();
    let session = runtime.handle.current().unwrap().session;
    value(&session, ":help docker logs").await;
    assert_eq!(a.requests.lock().unwrap().len(), before_a);
    assert_eq!(b.requests.lock().unwrap().len(), before_b);
    // Re-enable cannot authorize an obsolete binding; unchanged captured work may refresh.
    submit(&session, ":env enable \"default\"").await;
    refuse_changed_binding(&session, ":refresh $first_inventory").await;
    assert_eq!(a.requests.lock().unwrap().len(), before_a);
    submit(&session, ":refresh $second_inventory").await;
    assert_eq!(b.requests.lock().unwrap().len(), before_b + 2);
    runtime.shutdown().await.unwrap();
}

#[test]
fn restart_investigation_measures_actual_mcp_calls_and_bytes() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let output = python::command()
        .arg(root.join("examples/docker-investigation/check.py"))
        .args(["--binary", env!("CARGO_BIN_EXE_wes")])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    println!("{}", String::from_utf8_lossy(&output.stdout));
}

fn isolated_options(home: std::path::PathBuf, base: &std::path::Path) -> RuntimeOptions {
    let mut options = RuntimeOptions::new(home, base.into());
    options.docker_candidates = Some(vec![base.join("missing.sock")]);
    options
}

#[tokio::test]
async fn automatic_default_is_lazy_and_pins_the_selected_socket_across_calls() {
    use std::os::unix::fs::symlink;
    let root = tempfile::Builder::new()
        .prefix("wes-auto-")
        .tempdir_in("/tmp")
        .unwrap();
    let a = daemon(root.path(), "first");
    let b = daemon(root.path(), "second");
    let link = root.path().join("local.sock");
    symlink(&a.path, &link).unwrap();
    let mut options = RuntimeOptions::new(root.path().join("home"), root.path().into());
    options.docker_candidates = Some(vec![link.clone()]);
    let runtime = launch(options).await.unwrap();
    let session = runtime.handle.current().unwrap().session;
    value(&session, ":help docker").await;
    assert_eq!(session.environment_documents().await.unwrap().len(), 1);
    assert!(a.requests.lock().unwrap().is_empty());
    value(&session, "docker containers > first").await;
    std::fs::remove_file(&link).unwrap(); // fixture symlink, never a user's socket
    symlink(&b.path, &link).unwrap();
    value(&session, "docker containers > second").await;
    assert_eq!(a.requests.lock().unwrap().len(), 3);
    assert!(b.requests.lock().unwrap().is_empty());
    runtime.shutdown().await.unwrap();
}

async fn refuse_changed_binding(session: &SessionHandle, source: &str) {
    let reply = session
        .submit(SourceInput::new(uuid::Uuid::new_v4().to_string(), source.into()).unwrap())
        .await
        .unwrap();
    assert!(
        reply
            .diagnostics
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.code == "ENV039"),
        "{reply:?}"
    );
    assert!(reply.refreshed.is_empty());
    session.wait_idle().await.unwrap();
}
