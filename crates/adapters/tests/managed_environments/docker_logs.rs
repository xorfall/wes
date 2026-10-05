use super::*;
use serde_json::json;
use std::sync::Mutex;
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::UnixListener,
};

#[derive(Clone)]
struct Response {
    tty: bool,
    body: Vec<u8>,
    status: u16,
    id: String,
    stall: bool,
    broken_body: bool,
    hold: bool,
    opening_stall: bool,
}
impl Default for Response {
    fn default() -> Self {
        Self {
            tty: false,
            body: frame(b"2026-09-24T00:00:00Z INFO ready\nERROR fixture\n"),
            status: 200,
            id: "a".repeat(64),
            stall: false,
            broken_body: false,
            hold: false,
            opening_stall: false,
        }
    }
}
fn frame(body: &[u8]) -> Vec<u8> {
    let mut wire = vec![1, 0, 0, 0];
    wire.extend_from_slice(&(body.len() as u32).to_be_bytes());
    wire.extend_from_slice(body);
    wire
}
struct Fake {
    _root: tempfile::TempDir,
    socket: String,
    requests: Arc<Mutex<Vec<String>>>,
    response: Arc<Mutex<Response>>,
    waiting: Arc<tokio::sync::Notify>,
    closed: Arc<tokio::sync::Notify>,
    task: tokio::task::JoinHandle<()>,
}
impl Drop for Fake {
    fn drop(&mut self) {
        self.task.abort();
    }
}
impl Fake {
    fn new() -> Self {
        let root = tempfile::Builder::new()
            .prefix("wes-dlog-")
            .tempdir_in("/tmp")
            .unwrap();
        let socket = root.path().join("d.sock");
        let listener = UnixListener::bind(&socket).unwrap();
        let requests = Arc::new(Mutex::new(vec![]));
        let response = Arc::new(Mutex::new(Response::default()));
        let waiting = Arc::new(tokio::sync::Notify::new());
        let closed = Arc::new(tokio::sync::Notify::new());
        let (rq, rs, wait, close) = (
            requests.clone(),
            response.clone(),
            waiting.clone(),
            closed.clone(),
        );
        let task = tokio::spawn(async move {
            loop {
                let (mut stream, _) = listener.accept().await.unwrap();
                let mut header = vec![];
                while !header.ends_with(b"\r\n\r\n") {
                    let mut byte = [0];
                    if stream.read_exact(&mut byte).await.is_err() {
                        break;
                    }
                    header.push(byte[0]);
                    assert!(header.len() < 16384);
                }
                let request = String::from_utf8(header).unwrap();
                let version = request.starts_with("GET /version ");
                let logs = request.contains("/logs?");
                rq.lock().unwrap().push(request);
                let response = rs.lock().unwrap().clone();
                let body = if version {
                    json!({"ApiVersion":"1.47","MinAPIVersion":"1.24"})
                        .to_string()
                        .into_bytes()
                } else if logs {
                    response.body.clone()
                } else {
                    json!({"Id":response.id,"Config":{"Tty":response.tty,"Env":["DO_NOT_EXPORT"]}})
                        .to_string()
                        .into_bytes()
                };
                let code = if version { 200 } else { response.status };
                if logs && response.opening_stall {
                    wait.notify_one();
                    let mut byte = [0];
                    let _ = stream.read(&mut byte).await;
                    close.notify_one();
                    continue;
                }
                let length = body.len() + usize::from(logs && response.broken_body);
                let reply = if logs && response.hold {
                    format!("HTTP/1.1 {code} Result\r\nConnection: close\r\n\r\n")
                } else {
                    format!(
                        "HTTP/1.1 {code} Result\r\nContent-Length: {length}\r\nConnection: close\r\n\r\n"
                    )
                };
                if stream.write_all(reply.as_bytes()).await.is_err() {
                    continue;
                }
                if logs && response.stall {
                    wait.notify_one();
                    let mut b = [0];
                    let _ = stream.read(&mut b).await;
                    close.notify_one();
                } else {
                    // Header and UTF-8 can split at arbitrary HTTP boundaries.
                    for chunk in body[..body.len().min(80)]
                        .chunks(3)
                        .chain(body[body.len().min(80)..].chunks(4096))
                    {
                        if stream.write_all(chunk).await.is_err() {
                            break;
                        }
                    }
                    if logs && response.hold {
                        wait.notify_one();
                        let mut byte = [0];
                        let _ = stream.read(&mut byte).await;
                        close.notify_one();
                    }
                }
            }
        });
        Self {
            _root: root,
            socket: socket.to_string_lossy().into_owned(),
            requests,
            response,
            waiting,
            closed,
            task,
        }
    }
    fn recipe(&self, private: bool, timeout: u64) -> String {
        format!(
            "version: 1\ntargets: {{local: {{kind: local}}}}\nenvironments:\n  dev:\n    imports:\n      docker:\n        source: {{kind: docker, socket: '{}'}}\n        bind: {{target: local, output: {}, timeout_ms: {timeout}}}\n",
            self.socket,
            if private { "private" } else { "public" }
        )
    }
}
fn query(tail: &str) -> String {
    format!("docker logs container:{} {tail} > logs", "a".repeat(64))
}
fn fields(value: &Value) -> &indexmap::IndexMap<String, Data> {
    let Data::Record(fields) = value.data() else {
        panic!("envelope")
    };
    fields
}
async fn assert_error(f: &Fixture, code: &str) {
    let s = f.handle.snapshot().await.unwrap();
    assert!(
        s.execution.errors.values().any(|e| e.code() == code),
        "{code}: {:?}",
        s.execution.errors
    );
}
#[tokio::test]
async fn finite_logs_use_exact_identity_and_one_owned_operation_then_refresh_summary() {
    let fake = Fake::new();
    let f = Fixture::new(&fake.recipe(false, 15000));
    f.install().await;
    assert!(fake.requests.lock().unwrap().is_empty());
    f.submit("one", ":env use \"dev\"").await;
    f.submit("one", &query("since:1 until:2000000000")).await;
    let logs = f.value("logs").await;
    assert_eq!(fields(&logs)["returned"], Data::Int(2));
    assert_eq!(fields(&logs)["truncated"], Data::Bool(false));
    assert!(!format!("{logs:?}").contains("DO_NOT_EXPORT"));
    {
        let requests = fake.requests.lock().unwrap();
        assert_eq!(
            requests.len(),
            3,
            "one negotiation + internal inspect + finite read"
        );
        let path = requests[2].split_whitespace().nth(1).unwrap();
        let url = url::Url::parse(&format!("http://localhost{path}")).unwrap();
        let q: std::collections::BTreeMap<_, _> = url.query_pairs().collect();
        for (key, val) in [
            ("tail", "200"),
            ("since", "1"),
            ("until", "2000000000"),
            ("follow", "false"),
            ("timestamps", "true"),
            ("stdout", "true"),
            ("stderr", "true"),
        ] {
            assert_eq!(q[key], val);
        }
    }
    f.submit(
        "one",
        ":calc { return {lines: length($logs.rows), truncated: $logs.truncated}; } > summary",
    )
    .await;
    let before = f.handle.snapshot().await.unwrap().names["logs"]
        .node
        .clone();
    fake.response.lock().unwrap().body = frame(b"next\n");
    f.submit(
        "one",
        include_str!("../../../../examples/docker-logs/refresh.wes"),
    )
    .await;
    assert_eq!(fields(&f.value("summary").await)["lines"], Data::Int(1));
    assert_eq!(
        f.handle.snapshot().await.unwrap().names["logs"].node,
        before
    );
    assert_eq!(
        fake.requests.lock().unwrap().len(),
        5,
        "refresh reuses negotiation; calc does no I/O"
    );
    f.stop().await;
}
#[tokio::test]
async fn invalid_selection_is_inert_and_missing_or_mismatched_id_never_redirects() {
    let fake = Fake::new();
    let f = Fixture::new(&fake.recipe(false, 15000));
    f.install().await;
    f.submit("one", ":env use \"dev\"").await;
    for args in ["tail:0", "tail:5001", "since:-1", "since:5 until:4"] {
        f.submit("one", &query(args)).await;
    }
    f.submit("one", "docker logs container:api > bad").await;
    assert!(fake.requests.lock().unwrap().is_empty());
    assert_error(&f, "DOCKER_ARGUMENT").await;
    fake.response.lock().unwrap().id = "b".repeat(64);
    f.submit("one", &query("")).await;
    assert_error(&f, "DOCKER_IDENTITY").await;
    assert_eq!(fake.requests.lock().unwrap().len(), 2);
    fake.response.lock().unwrap().status = 404;
    f.submit("one", &query("")).await;
    assert_error(&f, "DOCKER_MISSING").await;
    assert_eq!(fake.requests.lock().unwrap().len(), 3);
    f.stop().await;
}
#[tokio::test]
async fn tty_limits_and_private_provenance_remain_explicit() {
    let fake = Fake::new();
    let f = Fixture::new(&fake.recipe(true, 15000));
    f.install().await;
    f.submit("one", ":env use \"dev\"").await;
    {
        let mut r = fake.response.lock().unwrap();
        r.tty = true;
        r.body = b"one\ntwo\nthree\n".to_vec();
    }
    f.submit("one", &query("tail:2")).await;
    let value = f.value("logs").await;
    assert!(value.provenance().policy().is_private());
    assert_eq!(fields(&value)["returned"], Data::Int(2));
    assert_eq!(
        fields(&value)["truncation"],
        Data::Option(Some(Box::new(Data::Text("line_limit".into()))))
    );
    fake.response.lock().unwrap().body = vec![b'x'; 1024 * 1024 + 1];
    f.submit("one", &query("")).await;
    assert_eq!(
        fields(&f.value("logs").await)["truncation"],
        Data::Option(Some(Box::new(Data::Text("byte_limit".into()))))
    );
    let before = fake.requests.lock().unwrap().len();
    f.submit("one", ":env disable \"dev\"").await;
    f.submit("one", ":refresh $logs").await;
    assert_eq!(fake.requests.lock().unwrap().len(), before);
    f.stop().await;
}
#[tokio::test]
async fn damaged_frames_and_transport_never_publish_partial_success() {
    let fake = Fake::new();
    let f = Fixture::new(&fake.recipe(false, 15000));
    f.install().await;
    f.submit("one", ":env use \"dev\"").await;
    fake.response.lock().unwrap().body.pop();
    f.submit("one", &query("")).await;
    assert_error(&f, "DOCKER_LOG_FRAME").await;
    {
        let mut r = fake.response.lock().unwrap();
        r.body = frame(b"hello\n");
        r.broken_body = true;
    }
    f.submit("one", &query("")).await;
    assert_error(&f, "DOCKER_TRANSPORT").await;
    f.stop().await;
}
#[tokio::test]
async fn deadline_and_cancellation_close_the_only_reader_without_retrying() {
    for cancel in [false, true] {
        let fake = Fake::new();
        let f = Fixture::new(&fake.recipe(false, if cancel { 15000 } else { 250 }));
        f.install().await;
        f.submit("one", ":env use \"dev\"").await;
        fake.response.lock().unwrap().stall = true;
        let input = SourceInput::new("log-request".into(), query(""))
            .unwrap()
            .with_client("one".into())
            .unwrap();
        let submitted = f.handle.submit(input).await.unwrap();
        assert!(
            !submitted
                .diagnostics
                .diagnostics
                .iter()
                .any(|d| d.severity == wes_language::Severity::Error)
        );
        tokio::time::timeout(std::time::Duration::from_secs(2), fake.waiting.notified())
            .await
            .unwrap();
        if cancel {
            f.submit("one", ":cancel $logs").await;
        } else {
            f.handle.wait_idle().await.unwrap();
            assert_error(&f, "DOCKER_TIMEOUT").await;
        }
        tokio::time::timeout(std::time::Duration::from_secs(2), fake.closed.notified())
            .await
            .unwrap();
        assert_eq!(fake.requests.lock().unwrap().len(), 3);
        f.stop().await;
    }
}

fn follow_query() -> String {
    format!(
        "docker logs follow container:{} tail:200 > live_logs",
        "a".repeat(64)
    )
}
async fn live_rows(f: &Fixture, expected: usize) -> Value {
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        loop {
            let snapshot = f.handle.snapshot().await.unwrap();
            if let Some(named) = snapshot.names.get("live_logs")
                && let Some(value) = snapshot.execution.values.get(&named.node)
                && matches!(value.data(), Data::List(rows) if rows.len() == expected)
            {
                return value.clone();
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap()
}
async fn closed(fake: &Fake) {
    tokio::time::timeout(std::time::Duration::from_secs(3), fake.closed.notified())
        .await
        .unwrap();
}
#[tokio::test]
async fn follow_reuses_stream_window_preserves_cancelled_values_and_refresh_is_explicit() {
    let fake = Fake::new();
    {
        let mut response = fake.response.lock().unwrap();
        response.hold = true;
        response.body = frame(
            (1..=620)
                .map(|i| format!("line {i}\n"))
                .collect::<String>()
                .as_bytes(),
        );
    }
    let f = Fixture::new(&fake.recipe(false, 15000));
    f.install().await;
    f.submit("one", ":env use \"dev\"").await;
    f.submit("one", ":workspace policy mode:reactive").await;
    f.submit("one", &follow_query()).await;
    let value = live_rows(&f, 500).await;
    let Data::List(rows) = value.data() else {
        panic!("window")
    };
    let Data::Record(first) = &rows[0] else {
        panic!("row")
    };
    assert_eq!(first["sequence"], Data::Int(121));
    assert_eq!(first["text"], Data::Text("line 121".into()));
    assert!(format!("{:?}", value.provenance()).contains("omitted 120"));
    let before = f.handle.snapshot().await.unwrap();
    let node = before.names["live_logs"].node.clone();
    let run = before.execution.runs[&node].clone();
    f.submit("one", ":calc { return length($live_logs); } > count")
        .await;
    assert_eq!(f.value("count").await.data(), &Data::Int(500));
    assert_eq!(fake.requests.lock().unwrap().len(), 3);
    assert!(fake.requests.lock().unwrap()[2].contains("follow=true"));
    f.submit("one", ":cancel $live_logs").await;
    closed(&fake).await;
    let after = f.handle.snapshot().await.unwrap();
    assert_eq!(
        after.execution.stopped_values[&node].value.data(),
        value.data()
    );
    let count = &after.names["count"].node;
    assert_eq!(
        after.execution.stopped_values[count].value.data(),
        &Data::Int(500)
    );
    fake.response.lock().unwrap().body = frame(b"new run\n");
    f.submit("one", ":refresh $live_logs").await;
    let fresh = live_rows(&f, 1).await;
    let Data::List(rows) = fresh.data() else {
        panic!("window")
    };
    let Data::Record(row) = &rows[0] else {
        panic!("row")
    };
    assert_eq!(row["sequence"], Data::Int(1));
    let current = f.handle.snapshot().await.unwrap();
    assert_eq!(current.names["live_logs"].node, node);
    assert_ne!(current.execution.runs[&node], run);
    assert_eq!(fake.requests.lock().unwrap().len(), 5);
    f.submit("one", ":cancel $live_logs").await;
    closed(&fake).await;
    f.stop().await;
}
#[tokio::test]
async fn idle_follow_outlives_opening_timeout_but_disable_closes_it_without_polling() {
    let fake = Fake::new();
    fake.response.lock().unwrap().hold = true;
    let f = Fixture::new(&fake.recipe(false, 100));
    f.install().await;
    f.submit("one", ":env use \"dev\"").await;
    f.submit("one", &follow_query()).await;
    live_rows(&f, 2).await;
    tokio::time::sleep(std::time::Duration::from_millis(200)).await;
    assert_eq!(fake.requests.lock().unwrap().len(), 3);
    assert_eq!(
        f.handle.snapshot().await.unwrap().execution.streaming.len(),
        1
    );
    f.submit("one", ":env disable \"dev\"").await;
    closed(&fake).await;
    f.handle.wait_idle().await.unwrap();
    assert_error(&f, "ENV020").await;
    f.submit("one", ":env enable \"dev\"").await;
    assert_eq!(
        fake.requests.lock().unwrap().len(),
        3,
        "reenable cannot restart old lease"
    );
    f.submit("one", ":refresh $live_logs").await;
    live_rows(&f, 2).await;
    assert_eq!(fake.requests.lock().unwrap().len(), 5);
    f.submit("one", ":timeout $live_logs after:PT0.05S").await;
    closed(&fake).await;
    f.handle.wait_idle().await.unwrap();
    assert!(
        f.handle
            .snapshot()
            .await
            .unwrap()
            .execution
            .streaming
            .is_empty()
    );
    f.stop().await;
}
#[tokio::test]
async fn follow_opening_timeout_and_cancel_own_the_pending_socket() {
    for cancel in [false, true] {
        let fake = Fake::new();
        fake.response.lock().unwrap().opening_stall = true;
        let f = Fixture::new(&fake.recipe(false, if cancel { 15000 } else { 250 }));
        f.install().await;
        f.submit("one", ":env use \"dev\"").await;
        f.handle
            .submit(
                SourceInput::new("follow-opening".into(), follow_query())
                    .unwrap()
                    .with_client("one".into())
                    .unwrap(),
            )
            .await
            .unwrap();
        tokio::time::timeout(std::time::Duration::from_secs(3), fake.waiting.notified())
            .await
            .unwrap();
        if cancel {
            f.submit("one", ":cancel $live_logs").await;
        } else {
            f.handle.wait_idle().await.unwrap();
            assert_error(&f, "DOCKER_TIMEOUT").await;
        }
        closed(&fake).await;
        assert_eq!(fake.requests.lock().unwrap().len(), 3);
        f.stop().await;
    }
}
#[tokio::test]
async fn finite_follow_eof_and_private_policy_use_the_shared_decoder_and_window() {
    let fake = Fake::new();
    {
        let mut r = fake.response.lock().unwrap();
        r.tty = true;
        r.body = vec![b'x'; 70 * 1024];
        r.body.extend_from_slice(b"\nok\nlast");
    }
    let f = Fixture::new(&fake.recipe(true, 15000));
    f.install().await;
    f.submit("one", ":env use \"dev\"").await;
    f.submit("one", &follow_query()).await;
    let value = live_rows(&f, 3).await;
    assert!(value.provenance().policy().is_private());
    let Data::List(rows) = value.data() else {
        panic!("rows")
    };
    let Data::Record(first) = &rows[0] else {
        panic!("row")
    };
    assert_eq!(first["line_truncated"], Data::Bool(true));
    assert_eq!(first["stream"], Data::Text("tty".into()));
    let Data::Record(last) = &rows[2] else {
        panic!("row")
    };
    assert_eq!(last["partial"], Data::Bool(true));
    f.stop().await;
}
