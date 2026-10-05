//! End-to-end effects through the real session with synthetic Unix daemons and actors.
#![cfg(unix)]
use serde_json::{Value as Json, json};
use std::{
    sync::{Arc, Mutex},
    time::Duration,
};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use wes::runtime::{RuntimeOptions, launch};
use wes_core::Data;
use wes_engine::{
    session::{SessionHandle, SubmissionResult},
    source::SourceInput,
};
const IMAGE: &str = "sha256:bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";
fn id() -> String {
    "a".repeat(64)
}
#[derive(Default)]
struct State {
    requests: Vec<(String, Json)>,
    lose_create: bool,
    hold_create: bool,
    malformed: bool,
    volumes: bool,
    conflict: bool,
    pull_error: bool,
    running: bool,
    warnings: Vec<String>,
}
struct Daemon {
    path: String,
    state: Arc<Mutex<State>>,
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
    let state = Arc::new(Mutex::new(State::default()));
    let shared = state.clone();
    let task = tokio::spawn(async move {
        let mut children = tokio::task::JoinSet::new();
        loop {
            tokio::select! {
                accepted = listener.accept() => {
                    let (mut socket, _) = accepted.unwrap(); let shared = shared.clone();
                    children.spawn(async move {
                        let mut head = vec![];
                        while !head.ends_with(b"\r\n\r\n") { let mut b=[0]; if socket.read_exact(&mut b).await.is_err() { return; } head.push(b[0]); assert!(head.len()<16384); }
                        let head = String::from_utf8(head).unwrap();
                        let line = head.lines().next().unwrap().to_owned();
                        let path = line.split_whitespace().nth(1).unwrap();
                        let len = head.lines().find_map(|l| l.to_ascii_lowercase().strip_prefix("content-length:").map(|v| v.trim().parse::<usize>().unwrap())).unwrap_or(0);
                        assert!(len <= 128*1024); let mut body = vec![0;len]; socket.read_exact(&mut body).await.unwrap();
                        let body: Json = if body.is_empty() { Json::Null } else { serde_json::from_slice(&body).unwrap() };
                        let (status, body, hold) = {
                            let mut s = shared.lock().unwrap(); s.requests.push((line.clone(), body));
                            if path.starts_with("/v1.45/containers/create?") && s.lose_create { return; }
                            if path == "/version" { (200,json!({"ApiVersion":"1.45","MinAPIVersion":"1.24"}).to_string(),false) }
                            else if path.starts_with("/v1.45/images/create?") { (200, if s.pull_error { "{\"error\":\"PRIVATE_ERROR_NOT_EXPORTED\"}\n".into() } else { "{\"status\":\"Downloading\"}\n{\"status\":\"Done\"}\n".into() },false) }
                            else if path.starts_with("/v1.45/images/") { (200,json!({"Id":IMAGE,"Config":{"Volumes": if s.volumes { json!({"/data":{}}) } else { Json::Null }}}).to_string(),false) }
                            else if path.starts_with("/v1.45/containers/create?") { (201, if s.malformed { "{}".into() } else { json!({"Id":id(),"Warnings":s.warnings}).to_string() },s.hold_create) }
                            else if path.ends_with("/start") { s.running=true; (204,String::new(),false) }
                            else if path.contains("/stop?") { s.running=false; (204,String::new(),false) }
                            else if line.starts_with("DELETE ") { (if s.conflict {409} else {204},String::new(),false) }
                            else if path.ends_with("/json") { (200,json!({"Id":id(),"Name":"/fixture","Image":IMAGE,"Created":"2026-09-25T00:00:00Z","State":{"Status":if s.running {"running"} else {"exited"},"Running":s.running,"ExitCode":0,"OOMKilled":false},"RestartCount":0,"Config":{"Tty":true}}).to_string(),false) }
                            else if path.contains("/logs?") { (200,"wes-fixture\n".into(),false) }
                            else { panic!("unexpected fixture request: {line}") }
                        };
                        if hold { let mut b=[0]; let _ = socket.read(&mut b).await; return; }
                        let _ = socket.write_all(format!("HTTP/1.1 {status} Result\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",body.len()).as_bytes()).await;
                    });
                }
                Some(result) = children.join_next(), if !children.is_empty() => { result.unwrap(); }
            }
        }
    });
    Daemon { path, state, task }
}
fn source(cell: &str, actor: Option<&str>, text: &str) -> SourceInput {
    let input = SourceInput::new(cell.into(), text.into()).unwrap();
    match actor {
        Some(who) => input.with_client(who.into()).unwrap().cooperative(),
        None => input,
    }
}
async fn run(h: &SessionHandle, actor: Option<&str>, text: &str) -> Arc<SubmissionResult> {
    let reply = h
        .submit(source(&uuid::Uuid::new_v4().to_string(), actor, text))
        .await
        .unwrap();
    assert!(
        !reply
            .diagnostics
            .diagnostics
            .iter()
            .any(|d| d.severity == wes_language::Severity::Error),
        "{text}: {reply:?}"
    );
    tokio::time::timeout(Duration::from_secs(10), h.wait_idle())
        .await
        .unwrap_or_else(|_| panic!("did not settle after {text}: {reply:?}"))
        .unwrap();
    reply
}
async fn success(h: &SessionHandle, actor: Option<&str>, text: &str) -> Arc<SubmissionResult> {
    let reply = run(h, actor, text).await;
    let s = h.snapshot().await.unwrap();
    assert!(
        s.execution.values.contains_key(reply.nodes.last().unwrap()),
        "{text}: {s:?}"
    );
    reply
}
async fn error(h: &SessionHandle, actor: Option<&str>, text: &str, code: &str) {
    let reply = run(h, actor, text).await;
    let s = h.snapshot().await.unwrap();
    assert!(
        !s.execution.values.contains_key(reply.nodes.last().unwrap()),
        "unexpected success {text}"
    );
    assert_eq!(
        s.execution
            .errors
            .get(reply.nodes.last().unwrap())
            .map(|e| e.code()),
        Some(code),
        "{s:?}"
    );
}
async fn connect(h: &SessionHandle, d: &Daemon) {
    run(
        h,
        None,
        &format!(
            ":import docker socket:{} as:docker replace:true",
            serde_json::to_string(&d.path).unwrap()
        ),
    )
    .await;
    for who in ["a", "b"] {
        h.observe_actor(who.into(), "terminal".into())
            .await
            .unwrap();
    }
}
async fn argv(h: &SessionHandle) {
    success(
        h,
        None,
        ":calc { return [\"/bin/true\"]; } > raw\n:type check $raw as:\"List<Text>\" > argv",
    )
    .await;
}
fn create() -> String {
    format!("docker container create image:{IMAGE} argv:$argv")
}

#[tokio::test]
async fn actual_example_and_actor_resource_scope_survive_alias_changes_but_not_restore() {
    let root = tempfile::Builder::new()
        .prefix("dl-")
        .tempdir_in("/tmp")
        .unwrap();
    let d = daemon(root.path(), "one");
    let other = daemon(root.path(), "two");
    let home = root.path().join("home");
    let runtime = launch(RuntimeOptions::new(home.clone(), root.path().into()))
        .await
        .unwrap();
    let h = runtime.handle.current().unwrap().session;
    for command in [
        ":help docker image pull",
        ":help docker container create",
        ":help docker container remove",
    ] {
        success(&h, None, command).await;
    }
    assert!(d.state.lock().unwrap().requests.is_empty());
    connect(&h, &d).await;
    let example = include_str!("../../../examples/docker-lifecycle/lifecycle.wes");
    let (create_phase, rest) = example.split_once("docker container start").unwrap();
    success(&h, Some("a"), create_phase).await;
    let started = success(
        &h,
        Some("a"),
        &format!("docker container start{}", rest.lines().next().unwrap()),
    )
    .await;
    let s = h.snapshot().await.unwrap();
    let Data::Record(created) = s.execution.values[&s.names["created"].node].data() else {
        panic!()
    };
    assert_eq!(created["container"], Data::Text(id().into()));
    assert!(format!("{:?}", s.execution.values).contains("wes-fixture"));
    let before = d.state.lock().unwrap().requests.len();
    error(
        &h,
        Some("b"),
        &format!("docker container stop container:{}", id()),
        "AUT001",
    )
    .await;
    assert_eq!(
        d.state.lock().unwrap().requests.len(),
        before,
        "denial requires no daemon preflight"
    );
    // Reuse the same trusted scoped-work host API as existing cooperative acceptance fixtures.
    h.grant_work("b".into(), vec![started.cell.clone()])
        .await
        .unwrap();
    run(&h, Some("b"), &format!(":refresh ${}", started.nodes[0])).await;
    assert_eq!(
        d.state.lock().unwrap().requests.len(),
        before,
        "refresh must use requester, not cell owner"
    );
    let repeated = source(
        "repeat-foreign",
        Some("b"),
        &format!("docker container start{}", rest.lines().next().unwrap()),
    )
    .with_repeat(started.cell.clone(), true)
    .unwrap();
    assert!(h.submit(repeated).await.unwrap().repeated_run.is_some());
    h.wait_idle().await.unwrap();
    assert_eq!(
        d.state.lock().unwrap().requests.len(),
        before,
        "repeat work grant is not resource scope"
    );
    // A different imported connection cannot inherit A's ownership even if its daemon returns the same ID.
    run(
        &h,
        Some("a"),
        &format!(
            ":import docker socket:{} as:other",
            serde_json::to_string(&other.path).unwrap()
        ),
    )
    .await;
    error(
        &h,
        Some("a"),
        &format!("other container stop container:{}", id()),
        "AUT001",
    )
    .await;
    assert!(other.state.lock().unwrap().requests.is_empty());
    success(
        &h,
        Some("a"),
        include_str!("../../../examples/docker-lifecycle/cleanup.wes"),
    )
    .await;
    let requests = d.state.lock().unwrap().requests.clone();
    let payload = &requests
        .iter()
        .find(|(r, _)| r.starts_with("POST /v1.45/containers/create?"))
        .unwrap()
        .1;
    assert_eq!(payload["Image"], IMAGE);
    assert_eq!(payload["HostConfig"]["NetworkMode"], "none");
    assert_eq!(payload["HostConfig"]["Memory"], 256 * 1024 * 1024);
    assert_eq!(payload["HostConfig"]["CapDrop"], json!(["ALL"]));
    assert_eq!(
        payload["Entrypoint"],
        json!(["/bin/sh", "-c", "echo wes-fixture; sleep 30"])
    );
    assert!(requests.last().unwrap().0.contains("force=false&v=false"));
    error(
        &h,
        Some("a"),
        &format!("docker container start container:{}", id()),
        "AUT001",
    )
    .await;
    // Local user may deliberately select an existing full ID; external ownership isn't graph ownership.
    success(
        &h,
        None,
        &format!("docker container stop container:{}", id()),
    )
    .await;
    // Restore test needs a live creation receipt, not the already removed one.
    success(&h, Some("a"), &format!("{} > retained", create())).await;
    runtime.shutdown().await.unwrap();
    let before = d.state.lock().unwrap().requests.len();
    let reopened = launch(RuntimeOptions::new(home, root.path().into()))
        .await
        .unwrap();
    let h = reopened.handle.current().unwrap().session;
    assert_eq!(d.state.lock().unwrap().requests.len(), before);
    run(&h, None, ":env enable \"default\"").await;
    h.observe_actor("a".into(), "terminal".into())
        .await
        .unwrap();
    error(
        &h,
        Some("a"),
        &format!("docker container stop container:{}", id()),
        "AUT001",
    )
    .await;
    assert_eq!(d.state.lock().unwrap().requests.len(), before);
    reopened.shutdown().await.unwrap();
}

#[tokio::test]
async fn validation_conflict_lost_reply_and_cancel_are_bounded_without_retry() {
    let root = tempfile::Builder::new()
        .prefix("de-")
        .tempdir_in("/tmp")
        .unwrap();
    let d = daemon(root.path(), "d");
    let runtime = launch(RuntimeOptions::new(
        root.path().join("home"),
        root.path().into(),
    ))
    .await
    .unwrap();
    let h = runtime.handle.current().unwrap().session;
    connect(&h, &d).await;
    argv(&h).await;
    let before = d.state.lock().unwrap().requests.len();
    error(
        &h,
        Some("a"),
        "docker image pull reference:alpine",
        "DOCKER_ARGUMENT",
    )
    .await;
    error(
        &h,
        Some("a"),
        r#"docker image pull reference:"alpine:""#,
        "DOCKER_ARGUMENT",
    )
    .await;
    error(
        &h,
        Some("a"),
        "docker container create image:alpine argv:$argv",
        "DOCKER_ARGUMENT",
    )
    .await;
    error(
        &h,
        None,
        "docker container stop container:short-name",
        "DOCKER_ARGUMENT",
    )
    .await;
    assert_eq!(d.state.lock().unwrap().requests.len(), before);
    d.state.lock().unwrap().volumes = true;
    error(&h, Some("a"), &create(), "DOCKER_ARGUMENT").await;
    assert!(
        !d.state
            .lock()
            .unwrap()
            .requests
            .iter()
            .any(|(r, _)| r.starts_with("POST "))
    );
    d.state.lock().unwrap().volumes = false;
    d.state.lock().unwrap().lose_create = true;
    error(&h, Some("a"), &create(), "ENV036").await;
    assert_eq!(
        d.state
            .lock()
            .unwrap()
            .requests
            .iter()
            .filter(|(r, _)| r.starts_with("POST /v1.45/containers/create"))
            .count(),
        1
    );
    d.state.lock().unwrap().lose_create = false;
    d.state.lock().unwrap().malformed = true;
    error(&h, Some("a"), &create(), "ENV036").await;
    d.state.lock().unwrap().malformed = false;
    d.state.lock().unwrap().hold_create = true;
    let mut notices = h.subscribe_notices().unwrap();
    let submitted = h
        .submit(source("cancel-create", Some("a"), &create()))
        .await
        .unwrap();
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if d.state
                .lock()
                .unwrap()
                .requests
                .iter()
                .filter(|(r, _)| r.starts_with("POST /v1.45/containers/create"))
                .count()
                == 3
            {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    run(&h, Some("a"), &format!(":cancel ${}", submitted.nodes[0])).await;
    let notice = tokio::time::timeout(Duration::from_secs(5), notices.recv())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(notice.run.node(), &submitted.nodes[0]);
    assert_eq!(notice.error.code(), "ENV036");
    // Revoke authority and expire a deadline after dispatch; both retain uncertainty notices.
    for (index, control) in ["disable", "timeout"].into_iter().enumerate() {
        let submitted = h
            .submit(source(&format!("held-{control}"), Some("a"), &create()))
            .await
            .unwrap();
        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                if d.state
                    .lock()
                    .unwrap()
                    .requests
                    .iter()
                    .filter(|(r, _)| r.starts_with("POST /v1.45/containers/create"))
                    .count()
                    == 4 + index
                {
                    break;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        let command = if control == "disable" {
            ":env disable \"default\"".to_string()
        } else {
            format!(":timeout ${} after:PT0.001S", submitted.nodes[0])
        };
        run(&h, None, &command).await;
        let notice = tokio::time::timeout(Duration::from_secs(5), notices.recv())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(notice.run.node(), &submitted.nodes[0]);
        assert_eq!(notice.error.code(), "ENV036");
        if control == "disable" {
            run(&h, None, ":env enable \"default\"").await;
        }
    }
    d.state.lock().unwrap().hold_create = false;
    d.state.lock().unwrap().pull_error = true;
    error(
        &h,
        Some("a"),
        "docker image pull reference:\"missing:1\"",
        "DOCKER_PULL_FAILED",
    )
    .await;
    assert!(!format!("{:?}", h.snapshot().await.unwrap()).contains("PRIVATE_ERROR_NOT_EXPORTED"));
    d.state.lock().unwrap().warnings = vec!["⚠".repeat(500); 20];
    let receipt = success(&h, Some("a"), &create()).await;
    let snapshot = h.snapshot().await.unwrap();
    let Data::Record(data) = snapshot.execution.values[receipt.nodes.last().unwrap()].data() else {
        panic!()
    };
    assert_eq!(data["warnings_truncated"], Data::Bool(true));
    let Data::List(warnings) = &data["warnings"] else {
        panic!()
    };
    assert_eq!(warnings.len(), 16);
    assert!(
        warnings
            .iter()
            .all(|w| matches!(w, Data::Text(text) if text.len() <= 1024))
    );
    d.state.lock().unwrap().conflict = true;
    error(
        &h,
        None,
        &format!("docker container remove container:{}", id()),
        "DOCKER_HTTP",
    )
    .await;
    assert_eq!(
        d.state
            .lock()
            .unwrap()
            .requests
            .iter()
            .filter(|(r, _)| r.starts_with("POST /v1.45/containers/create"))
            .count(),
        6
    );
    runtime.shutdown().await.unwrap();
}
