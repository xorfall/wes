use super::*;
use serde_json::json;
use std::{
    sync::{
        Mutex,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::UnixListener,
};
struct Fake {
    _root: tempfile::TempDir,
    socket: String,
    requests: Arc<Mutex<Vec<String>>>,
    closed: Arc<AtomicUsize>,
    task: tokio::task::JoinHandle<()>,
}
impl Drop for Fake {
    fn drop(&mut self) {
        self.task.abort();
    }
}
impl Fake {
    fn new(body: Vec<u8>, opening_stall: bool, status: u16) -> Self {
        let root = tempfile::Builder::new()
            .prefix("dmetric-")
            .tempdir_in("/tmp")
            .unwrap();
        let socket = root.path().join("d.sock");
        let listener = UnixListener::bind(&socket).unwrap();
        let requests = Arc::new(Mutex::new(vec![]));
        let closed = Arc::new(AtomicUsize::new(0));
        let (rq, done) = (requests.clone(), closed.clone());
        let task = tokio::spawn(async move {
            let mut children = tokio::task::JoinSet::new();
            loop {
                let (mut socket, _) = listener.accept().await.unwrap();
                let (rq, done, body) = (rq.clone(), done.clone(), body.clone());
                children.spawn(async move {
                    let mut header=vec![];
                    while !header.ends_with(b"\r\n\r\n") {let mut b=[0];if socket.read_exact(&mut b).await.is_err(){return;}header.push(b[0]);assert!(header.len()<16384);}
                    let request=String::from_utf8(header).unwrap();
                    let stream=request.contains("/stats?") || request.contains("/events?");
                    let version=request.starts_with("GET /version ");
                    rq.lock().unwrap().push(request);
                    if stream && opening_stall {let _=socket.read(&mut [0]).await;done.fetch_add(1,Ordering::SeqCst);return;}
                    let body=if version {json!({"ApiVersion":"1.45","MinAPIVersion":"1.24"}).to_string().into_bytes()}
                        else if stream {body} else {json!({"Id":"a".repeat(64)}).to_string().into_bytes()};
                    let code=if stream {status} else {200};
                    let header=if stream && code==200 {format!("HTTP/1.1 {code} OK\r\nConnection: close\r\n\r\n")}
                        else {format!("HTTP/1.1 {code} Result\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",body.len())};
                    if socket.write_all(header.as_bytes()).await.is_err(){return;}
                    for part in body.chunks(31) {if socket.write_all(part).await.is_err(){return;}}
                    if stream && code==200 {let _=socket.read(&mut [0]).await;done.fetch_add(1,Ordering::SeqCst);}
                });
                while children.try_join_next().is_some() {}
            }
        });
        Self {
            _root: root,
            socket: socket.to_string_lossy().into_owned(),
            requests,
            closed,
            task,
        }
    }
    fn recipe(&self, timeout: u64, private: bool) -> String {
        format!(
            "version: 1\ntargets: {{local: {{kind: local}}}}\nenvironments:\n  dev:\n    imports:\n      docker:\n        source: {{kind: docker, socket: '{}'}}\n        bind: {{target: local, timeout_ms: {timeout}, output: {}}}\n",
            self.socket,
            if private { "private" } else { "public" }
        )
    }
    async fn closed(&self, count: usize) {
        tokio::time::timeout(Duration::from_secs(5), async {
            while self.closed.load(Ordering::SeqCst) < count {
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await
        .unwrap();
    }
}
fn body(kind: &str, n: usize) -> Vec<u8> {
    (0..n).map(|_| if kind=="stats" {json!({"id":"a".repeat(64),"read":"2026-09-24T00:00:01Z","preread":"2026-09-24T00:00:00Z","cpu_stats":{"cpu_usage":{"total_usage":200},"system_cpu_usage":2000,"online_cpus":4},"precpu_stats":{"cpu_usage":{"total_usage":100},"system_cpu_usage":1000},"memory_stats":{}})}
        else {json!({"Type":"container","Action":"new_future_action","Actor":{"ID":"a".repeat(64),"Attributes":{"secret":"withheld"}},"timeNano":123})}.to_string()+"\n").collect::<String>().into_bytes()
}
async fn rows(f: &Fixture, n: usize) -> Value {
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let s = f.handle.snapshot().await.unwrap();
            if let Some(name) = s.names.get("live")
                && let Some(v) = s.execution.values.get(&name.node)
                && matches!(v.data(),Data::List(rows) if (n == 0 || rows.len()==n) && rows.last().is_some_and(|row| matches!(row, Data::Record(fields) if fields.get("sequence") == Some(&Data::Int(if n == 1 {1} else {510})))))
            {
                return v.clone();
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap()
}
#[tokio::test]
async fn both_metrics_streams_use_bounded_windows_stop_values_and_explicit_restart() {
    for kind in ["stats", "events"] {
        let fake = Fake::new(body(kind, 510), false, 200);
        let f = Fixture::new(&fake.recipe(15000, false));
        f.install().await;
        f.submit("one", ":env use \"dev\"").await;
        f.submit(
            "one",
            &format!("docker {kind} container:{} > live", "a".repeat(64)),
        )
        .await;
        let value = rows(&f, if kind == "stats" { 0 } else { 500 }).await;
        let Data::List(list) = value.data() else {
            panic!()
        };
        let Data::Record(first) = &list[0] else {
            panic!()
        };
        assert!(!list.is_empty() && list.len() <= 500);
        let Data::Record(last) = list.last().unwrap() else {
            panic!()
        };
        assert_eq!(last["sequence"], Data::Int(510));
        assert_eq!(first["sequence"], Data::Int(511 - list.len() as i64));
        assert!(!format!("{value:?}").contains("withheld"));
        let before = f.handle.snapshot().await.unwrap();
        let node = before.names["live"].node.clone();
        let run = before.execution.runs[&node].clone();
        f.submit("one", ":cancel $live").await;
        fake.closed(1).await;
        assert_eq!(
            f.handle.snapshot().await.unwrap().execution.evidence_values[&node]
                .value
                .data(),
            value.data()
        );
        let first_requests = if kind == "stats" { 2 } else { 3 };
        assert_eq!(fake.requests.lock().unwrap().len(), first_requests);
        let request = fake.requests.lock().unwrap().last().unwrap().clone();
        if kind == "stats" {
            assert!(request.contains("stream=true"));
        } else {
            let path = request.split_whitespace().nth(1).unwrap();
            let url = url::Url::parse(&format!("http://localhost{path}")).unwrap();
            let filters = url
                .query_pairs()
                .find(|(k, _)| k == "filters")
                .unwrap()
                .1
                .to_string();
            assert_eq!(
                serde_json::from_str::<serde_json::Value>(&filters).unwrap(),
                json!({"type":["container"],"container":["a".repeat(64)]})
            );
            assert!(!url.query_pairs().any(|(k, _)| k == "since"));
        }
        f.submit("one", ":refresh $live").await;
        rows(&f, if kind == "stats" { 0 } else { 500 }).await;
        assert_ne!(
            f.handle.snapshot().await.unwrap().execution.runs[&node],
            run
        );
        assert_eq!(fake.requests.lock().unwrap().len(), first_requests * 2 - 1);
        f.submit("one", ":env disable \"dev\"").await;
        fake.closed(2).await;
        f.handle.wait_idle().await.unwrap();
        let s = f.handle.snapshot().await.unwrap();
        assert!(s.execution.errors.values().any(|e| e.code() == "ENV020"));
        f.submit("one", ":env enable \"dev\"").await;
        assert_eq!(fake.requests.lock().unwrap().len(), first_requests * 2 - 1);
        f.stop().await;
    }
}
#[tokio::test]
async fn metrics_opening_deadline_and_private_lifetime_are_shared() {
    for kind in ["stats", "events"] {
        let fake = Fake::new(vec![], true, 200);
        let f = Fixture::new(&fake.recipe(150, false));
        f.install().await;
        f.submit("one", ":env use \"dev\"").await;
        f.submit(
            "one",
            &format!("docker {kind} container:{} > live", "a".repeat(64)),
        )
        .await;
        fake.closed(1).await;
        assert!(
            f.handle
                .snapshot()
                .await
                .unwrap()
                .execution
                .errors
                .values()
                .any(|e| e.code() == "DOCKER_TIMEOUT")
        );
        f.stop().await;
        let fake = Fake::new(body(kind, 1), false, 200);
        let f = Fixture::new(&fake.recipe(150, true));
        f.install().await;
        f.submit("one", ":env use \"dev\"").await;
        f.submit(
            "one",
            &format!("docker {kind} container:{} > live", "a".repeat(64)),
        )
        .await;
        assert!(rows(&f, 1).await.provenance().policy().is_private());
        tokio::time::sleep(Duration::from_millis(200)).await;
        assert_eq!(
            f.handle.snapshot().await.unwrap().execution.streaming.len(),
            1
        );
        f.submit("one", ":timeout $live after:PT0.05S").await;
        fake.closed(1).await;
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
}
#[tokio::test]
async fn metric_stream_malformed_input_and_http_failures_are_diagnostic() {
    for (body, status, code) in [
        (b"{broken}\n".to_vec(), 200, "DOCKER_STREAM_RECORD"),
        (vec![], 404, "DOCKER_MISSING"),
    ] {
        let fake = Fake::new(body, false, status);
        let f = Fixture::new(&fake.recipe(15000, false));
        f.install().await;
        f.submit("one", ":env use \"dev\"").await;
        f.submit(
            "one",
            &format!("docker stats container:{} > live", "a".repeat(64)),
        )
        .await;
        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                if f.handle
                    .snapshot()
                    .await
                    .unwrap()
                    .execution
                    .errors
                    .values()
                    .any(|e| e.code() == code)
                {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();
        f.stop().await;
    }
}
