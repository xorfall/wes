//! End-to-end finite inspection through the shipped runtime, actual HTTP and acknowledged storage.
use std::{
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpListener,
    sync::Notify,
};
use wes::runtime::{RuntimeOptions, launch};
use wes_core::{Data, Value};
use wes_engine::{graph::NodeId, session::SessionHandle, source::SourceInput};
fn data(value: &Value) -> serde_json::Value {
    serde_json::from_slice(
        &wes_adapters::codec::encode_json(value.data(), Default::default()).unwrap(),
    )
    .unwrap()
}
async fn submit(session: &SessionHandle, cell: &str, text: &str) -> NodeId {
    let result = session
        .submit(SourceInput::new(cell.into(), text.into()).unwrap())
        .await
        .unwrap();
    assert!(!result.nodes.is_empty(), "{text}: {result:?}");
    result.nodes[0].clone()
}
async fn idle(session: &SessionHandle) {
    tokio::time::timeout(Duration::from_secs(10), session.wait_idle())
        .await
        .unwrap()
        .unwrap();
}
#[tokio::test(flavor = "multi_thread")]
async fn live_query_failure_cancel_and_restart_keep_attempts_without_more_requests() {
    let root = tempfile::tempdir().unwrap();
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let calls = Arc::new(AtomicUsize::new(0));
    let count = calls.clone();
    let released = Arc::new(Notify::new());
    let gate = released.clone();
    let serving = tokio::spawn(async move {
        for index in 0..3 {
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut request = vec![0u8; 8192];
            let _ = stream.read(&mut request).await.unwrap();
            count.fetch_add(1, Ordering::SeqCst);
            if index == 1 {
                stream.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 4\r\nContent-Type: application/json\r\nConnection: close\r\n\r\noops").await.unwrap();
            } else {
                let payload = br#"{"reading":42}"#;
                let headers = format!(
                    "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nContent-Type: application/json\r\nConnection: close\r\n\r\n",
                    payload.len()
                );
                stream.write_all(headers.as_bytes()).await.unwrap();
                stream
                    .write_all(&payload[..payload.len() - 3])
                    .await
                    .unwrap();
                gate.notified().await;
                let _ = stream.write_all(&payload[payload.len() - 3..]).await;
            }
        }
    });
    let descriptor = serde_json::json!({"version":1,"provider":"sensor","types":{"Sample":{"base":"Record","fields":{"reading":{"type":"Int","optional":false}}}},"operations":[{"path":["sample"],"method":"GET","route":"/sample","auth":[],"parameters":[],"responses":{"200":"Sample"}}]});
    std::fs::write(root.path().join("sensor.json"), descriptor.to_string()).unwrap();
    let home = root.path().join("home");
    let runtime = launch(RuntimeOptions::new(home.clone(), root.path().into()))
        .await
        .unwrap();
    let session = runtime.handle.current().unwrap().session;
    session
        .submit(
            SourceInput::new(
                "import".into(),
                format!(":import spec file:sensor.json endpoint:\"http://{address}\" as:sensor"),
            )
            .unwrap(),
        )
        .await
        .unwrap();
    idle(&session).await;
    let node = submit(&session, "sample", "@trace(http) sensor sample > sample").await;
    let web = runtime.serve(0, None).await.unwrap();
    let client = reqwest::Client::builder().no_proxy().build().unwrap();
    let generation = runtime.handle.current().unwrap().generation;
    let trace_url = format!("http://{}/traces/{node}", web.address());
    assert_eq!(client.get(&trace_url).send().await.unwrap().status(), 409);
    let live = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let response = client
                .get(&trace_url)
                .header("X-Wes-Session", &generation)
                .send()
                .await
                .unwrap();
            if response.status() == 200 {
                let json: serde_json::Value =
                    serde_json::from_slice(&response.bytes().await.unwrap()).unwrap();
                if json["data"]["events"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .any(|e| e["kind"] == "http.progress")
                {
                    break json;
                }
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    assert_eq!(live["data"]["state"], "running");
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    released.notify_one();
    idle(&session).await;
    let snapshot = session.snapshot().await.unwrap();
    assert_eq!(
        data(&snapshot.execution.values[&node])["body"]["reading"],
        42
    );
    let trace = submit(&session, "inspect", ":read trace:$sample > inspection").await;
    idle(&session).await;
    let stored = data(&session.snapshot().await.unwrap().execution.values[&trace]);
    assert_eq!(stored["state"], "completed");
    assert_eq!(stored["persistence"], "recorded");
    let view = submit(
        &session,
        "view",
        ":calc pure { return {trace: $inspection}; } > shown",
    )
    .await;
    idle(&session).await;
    assert_eq!(
        data(&session.snapshot().await.unwrap().execution.values[&view])["trace"],
        stored
    );
    let failed = submit(&session, "bad", "@trace(http) sensor sample > bad").await;
    idle(&session).await;
    assert_eq!(
        data(&session.snapshot().await.unwrap().execution.values[&failed])["validation"]["state"],
        "unreadable"
    );
    let failed_trace = submit(&session, "badtrace", ":read trace:$bad > badtrace").await;
    idle(&session).await;
    assert_eq!(
        data(&session.snapshot().await.unwrap().execution.values[&failed_trace])["state"],
        "completed"
    );
    let cancelled = submit(
        &session,
        "cancelled",
        "@trace(http) sensor sample > cancelled",
    )
    .await;
    tokio::time::timeout(Duration::from_secs(5), async {
        while calls.load(Ordering::SeqCst) < 3 {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    session.cancel(cancelled.clone()).await.unwrap();
    idle(&session).await;
    released.notify_one();
    serving.await.unwrap();
    let cancelled_trace = submit(
        &session,
        "canceltrace",
        ":read trace:$cancelled > canceltrace",
    )
    .await;
    idle(&session).await;
    assert_eq!(
        data(&session.snapshot().await.unwrap().execution.values[&cancelled_trace])["state"],
        "cancelled"
    );
    web.shutdown().await.unwrap();
    runtime.shutdown().await.unwrap();
    let restored = launch(RuntimeOptions::new(home, root.path().into()))
        .await
        .unwrap();
    let session = restored.handle.current().unwrap().session;
    let observed = session.observe().await.unwrap();
    assert_eq!(data(&observed.traces.get(&node, None).unwrap()), stored);
    let query = submit(
        &session,
        "restored",
        &format!(
            ":read trace:$sample run:\"{}\" > restored",
            stored["run"].as_str().unwrap()
        ),
    )
    .await;
    idle(&session).await;
    assert_eq!(
        data(&session.snapshot().await.unwrap().execution.values[&query]),
        stored
    );
    assert_eq!(calls.load(Ordering::SeqCst), 3);
    restored.shutdown().await.unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn direct_http_status_headers_body_and_rejected_annotations() {
    let root = tempfile::tempdir().unwrap();
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.unwrap();
        let mut request = vec![0u8; 8192];
        let n = stream.read(&mut request).await.unwrap();
        let request = String::from_utf8_lossy(&request[..n]);
        assert!(request.starts_with("POST /orders"));
        assert!(request.contains("hello"));
        stream.write_all(b"HTTP/1.1 422 Invalid\r\nContent-Length: 3\r\nContent-Type: text/plain\r\nSet-Cookie: a=1\r\nSet-Cookie: b=2\r\nConnection: close\r\n\r\nbad").await.unwrap();
    });
    let runtime = launch(RuntimeOptions::new(
        root.path().join("home"),
        root.path().into(),
    ))
    .await
    .unwrap();
    let session = runtime.handle.current().unwrap().session;
    for (i, source) in [
        "@trace(tcp) http request url:\"http://127.0.0.1:1\"",
        "@trace(http) @trace(http) http request url:\"http://127.0.0.1:1\"",
        "@trace(http) sh run cmd:ignored",
        "@trace(http) :help",
    ]
    .iter()
    .enumerate()
    {
        let reply = session
            .submit(SourceInput::new(format!("invalid{i}"), source.to_string()).unwrap())
            .await;
        assert!(
            reply.is_err() || reply.unwrap().nodes.is_empty(),
            "{source}"
        );
    }
    let node=submit(&session,"direct",&format!("@trace(http) http request method:POST url:\"http://{address}/orders\" body:hello > response")).await;
    idle(&session).await;
    server.await.unwrap();
    let value = session.snapshot().await.unwrap().execution.values[&node].clone();
    let json = data(&value);
    assert_eq!(json["status"], 422);
    assert_eq!(
        json["headers"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|h| h["name"] == "set-cookie")
            .count(),
        2
    );
    let Data::Record(fields) = value.data() else {
        panic!()
    };
    assert_eq!(fields["body"], Data::Text("bad".into()));
    // The server has exited: decoding a received error body performs no second request.
    let decoded = submit(
        &session,
        "decode-error",
        ":calc { return text($response.body); } > error_text",
    )
    .await;
    idle(&session).await;
    assert_eq!(
        session.snapshot().await.unwrap().execution.values[&decoded].data(),
        &Data::Text("bad".into())
    );
    let malformed = submit(
        &session,
        "parse-error",
        ":calc { return parseJson(text($response.body)); } > invalid_json",
    )
    .await;
    idle(&session).await;
    assert_eq!(
        session.snapshot().await.unwrap().execution.errors[&malformed].code(),
        "CAL016"
    );
    let trace = data(
        &session
            .observe()
            .await
            .unwrap()
            .traces
            .get(&node, None)
            .unwrap(),
    );
    let written = trace.to_string();
    assert!(!written.contains("a=1"));
    assert!(!written.contains("b=2"));
    assert_eq!(trace["state"], "completed");
    runtime.shutdown().await.unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn private_sensor_source_and_views_are_live_only_and_do_not_restore() {
    let root = tempfile::tempdir().unwrap();
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.unwrap();
        let mut request = [0; 4096];
        let count = stream.read(&mut request).await.unwrap();
        assert!(
            std::str::from_utf8(&request[..count])
                .unwrap()
                .starts_with("GET /history ")
        );
        let body = r#"[{"time":"09:00","reading":18.25}]"#;
        let response = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        );
        stream.write_all(response.as_bytes()).await.unwrap();
    });
    std::fs::write(
        root.path().join("sensor.json"),
        include_str!("../../../examples/http-inspection/sensor.json"),
    )
    .unwrap();
    std::fs::write(
        root.path().join("environments.yaml"),
        include_str!("../../../examples/http-inspection/environments.yaml")
            .replace("http://127.0.0.1:8765", &format!("http://{address}")),
    )
    .unwrap();
    let home = root.path().join("home");
    let runtime = launch(RuntimeOptions::new(home.clone(), root.path().into()))
        .await
        .unwrap();
    let session = runtime.handle.current().unwrap().session;
    let plan = session
        .plan_environment_file("environments.yaml".into(), false)
        .await
        .unwrap();
    session.apply_environments(plan).await.unwrap();
    let selected = session
        .submit(SourceInput::new("select".into(), ":env use \"private\"".into()).unwrap())
        .await
        .unwrap();
    assert!(
        !selected
            .diagnostics
            .diagnostics
            .iter()
            .any(|d| d.severity == wes_language::Severity::Error),
        "{selected:?}"
    );
    submit(
        &session,
        "history",
        include_str!("../../../examples/http-inspection/history.wes"),
    )
    .await;
    idle(&session).await;
    server.await.unwrap();
    let snapshot = session.snapshot().await.unwrap();
    for name in ["readings", "reading_table", "reading_chart"] {
        let value = &snapshot.execution.values[&snapshot.names[name].node];
        assert!(value.provenance().policy().is_private());
    }
    // Inspect native data without invoking the public codec, which must reject private values.
    let Data::Record(chart) =
        snapshot.execution.values[&snapshot.names["reading_chart"].node].data()
    else {
        panic!()
    };
    let Data::List(points) = &chart["points"] else {
        panic!()
    };
    assert_eq!(points.len(), 1);
    drop(snapshot);
    runtime.shutdown().await.unwrap();
    let runtime = launch(RuntimeOptions::new(home, root.path().into()))
        .await
        .unwrap();
    let snapshot = runtime
        .handle
        .current()
        .unwrap()
        .session
        .snapshot()
        .await
        .unwrap();
    for name in ["readings", "reading_table", "reading_chart"] {
        assert!(
            !snapshot
                .execution
                .values
                .contains_key(&snapshot.names[name].node)
        );
        assert_eq!(
            snapshot.execution.stale_reasons[&snapshot.names[name].node],
            wes_engine::runtime::StaleReason::RestoreNotRetained
        );
    }
    runtime.shutdown().await.unwrap();
}
