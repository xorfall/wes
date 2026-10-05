use super::*;
use wes_engine::streams::{self, Limits, Phase, StreamHandle, StreamTask};
fn cap() -> EventContract {
    EventContract {
        kind: "Int",
        streaming: true,
    }
}
fn sse(body: &str) -> Response {
    Response { prefix: format!("HTTP/1.1 200 OK\r\nContent-Type: text/event-stream; charset=utf-8\r\nConnection: close\r\n\r\n{body}").into_bytes(), gate: None, tail: vec![] }
}
fn start(
    cap: Arc<Capability>,
    invoker: HttpInvoker,
    arguments: IndexMap<String, Value>,
) -> (StreamHandle, StreamTask) {
    streams::spawn(
        call(cap, arguments),
        Arc::new(invoker),
        Provenance::default(),
        Limits::default(),
        CancellationToken::new(),
    )
    .unwrap()
}
async fn joined(handle: &StreamHandle, task: StreamTask) -> Arc<streams::Snapshot> {
    tokio::time::timeout(Duration::from_secs(3), task.join())
        .await
        .unwrap()
        .unwrap();
    handle.snapshot()
}
async fn phase(handle: &StreamHandle, wanted: Phase) {
    let mut updates = handle.subscribe();
    tokio::time::timeout(Duration::from_secs(3), async {
        while updates.borrow_and_update().phase != wanted {
            updates.changed().await.unwrap();
        }
    })
    .await
    .unwrap();
}
fn code(snapshot: &streams::Snapshot, expected: &str) {
    let Phase::Failed(error) = &snapshot.phase else {
        panic!("failed: {:?}", snapshot.phase)
    };
    assert_eq!(error.code(), expected);
    assert!(!error.message().contains("fixture-private"));
}
#[tokio::test]
async fn streams_preserve_post_body_query_auth_and_fresh_shared_credentials() {
    let mut server = Server::start(vec![sse("data: 1\n\n"), sse("data: 2\n\n")]).await;
    let reads = Arc::new(AtomicUsize::new(0));
    let captured = reads.clone();
    let credentials = Arc::new(MemoryCredentials::with_environment(
        CredentialLimits::default(),
        move |_| {
            Ok(Some(SecretString::from(format!(
                "fixture-{}",
                captured.fetch_add(1, Ordering::SeqCst) + 1
            ))))
        },
    ));
    let (cap, invoker) = provider(
        &server.base,
        Some(Auth::Several(vec![
            Auth::Header {
                name: "X-Key".into(),
                scheme: "".into(),
                secret: "token".into(),
            },
            Auth::Query {
                parameter: "key".into(),
                secret: "token".into(),
            },
        ])),
        credentials,
        HttpConfig::default(),
        cap(),
        operation("POST", "/events", &["q"]),
    );
    for n in 1..=2 {
        let (handle, task) = start(
            cap.clone(),
            invoker.clone(),
            args(&[("q", Data::Text("a b".into())), ("data", Data::Int(3))]),
        );
        let snapshot = joined(&handle, task).await;
        assert_eq!(snapshot.phase, Phase::Ended);
        assert_eq!(snapshot.window.data(), &Data::List(vec![Data::Int(n)]));
        let request = server.next().await;
        assert!(request.starts_with(&format!("POST /events?q=a+b&key=fixture-{n} HTTP/1.1")));
        assert!(request.contains(&format!("x-key: fixture-{n}\r\n")));
        assert!(request.contains("accept: text/event-stream\r\n"));
        assert!(request.ends_with("3"));
    }
    assert_eq!(reads.load(Ordering::SeqCst), 2);
    server.finish().await;
}
#[tokio::test]
async fn split_unicode_multiline_data_and_invalid_items_preserve_valid_events_and_counts() {
    let all = "\u{feff}data: \"Türkçe 🚀\"\r\rdata: invalid-fixture-private\n\ndata: {\ndata: \"n\":2}\n\ndata: 3";
    let split = all
        .as_bytes()
        .iter()
        .position(|&byte| byte == 0xf0)
        .unwrap()
        + 2;
    let gate = Arc::new(Notify::new());
    let mut response = sse("");
    response.prefix.extend_from_slice(&all.as_bytes()[..split]);
    response.gate = Some(gate.clone());
    response.tail = all.as_bytes()[split..].to_vec();
    let mut server = Server::start(vec![response]).await;
    let (cap, invoker) = provider(
        &server.base,
        None,
        secrets(),
        HttpConfig::default(),
        EventContract {
            kind: "Unknown",
            ..cap()
        },
        operation("GET", "/events", &[]),
    );
    let (handle, task) = start(cap, invoker, IndexMap::new());
    server.next().await;
    phase(&handle, Phase::Open).await;
    assert_eq!(handle.snapshot().window.data(), &Data::List(vec![]));
    gate.notify_one();
    let snapshot = joined(&handle, task).await;
    assert_eq!(snapshot.phase, Phase::Ended);
    assert_eq!(snapshot.rejected, 1);
    assert_eq!(snapshot.omitted, 0);
    assert_eq!(
        snapshot.window.data(),
        &Data::List(vec![
            Data::Text("Türkçe 🚀".into()),
            Data::Record([("n".into(), Data::Int(2))].into())
        ])
    );
    server.finish().await;
}
#[tokio::test]
async fn streaming_budgets_apply_per_event_and_open_stream_has_no_finite_body_timeout() {
    let gate = Arc::new(Notify::new());
    let mut response = sse("");
    response.gate = Some(gate.clone());
    response.tail = b"data: 1\n\ndata: 2\n\ndata: 3\n\n".to_vec();
    let server = Server::start(vec![response]).await;
    let mut config = HttpConfig {
        request_timeout: Duration::from_millis(50),
        ..HttpConfig::default()
    };
    config.response.bytes = 8;
    let (cap, invoker) = provider(
        &server.base,
        None,
        secrets(),
        config,
        cap(),
        operation("GET", "/events", &[]),
    );
    let (handle, task) = start(cap, invoker, IndexMap::new());
    phase(&handle, Phase::Open).await;
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert_eq!(handle.snapshot().phase, Phase::Open);
    gate.notify_one();
    let snapshot = joined(&handle, task).await;
    assert_eq!(snapshot.phase, Phase::Ended);
    assert_eq!(
        snapshot.window.data(),
        &Data::List(vec![Data::Int(1), Data::Int(2), Data::Int(3)])
    );
    server.finish().await;
}
#[tokio::test]
async fn oversized_lines_and_multiline_events_fail_but_do_not_erase_last_good_window() {
    for body in [
        "data: 1\n\ndata: 12345",
        "data: 1\n\ndata: 1234\ndata: 1234\ndata:\ndata:\n",
    ] {
        let server = Server::start(vec![sse(body)]).await;
        let mut config = HttpConfig::default();
        config.response.bytes = 10;
        let (cap, invoker) = provider(
            &server.base,
            None,
            secrets(),
            config,
            cap(),
            operation("GET", "/events", &[]),
        );
        let (handle, task) = start(cap, invoker, IndexMap::new());
        let snapshot = joined(&handle, task).await;
        code(&snapshot, "HTTP006");
        assert_eq!(snapshot.window.data(), &Data::List(vec![Data::Int(1)]));
        server.finish().await;
    }
}
#[tokio::test]
async fn status_and_content_type_failures_are_visible_and_204_ends_without_reconnecting() {
    for (response, expected) in [
        (reply(500, "fixture-private failure"), Some("HTTP008")),
        (reply(200, "data: 1\n\n"), Some("HTTP007")),
        (reply(204, ""), None),
    ] {
        let mut server = Server::start(vec![response]).await;
        let store = secrets();
        store
            .remember("token".into(), SecretString::from("fixture-private"))
            .unwrap();
        let (cap, invoker) = provider(
            &server.base,
            Some(Auth::Header {
                name: "X-Key".into(),
                scheme: "".into(),
                secret: "token".into(),
            }),
            store,
            HttpConfig::default(),
            cap(),
            operation("GET", "/events", &[]),
        );
        let (handle, task) = start(cap, invoker, IndexMap::new());
        let snapshot = joined(&handle, task).await;
        if let Some(expected) = expected {
            code(&snapshot, expected);
        } else {
            assert_eq!(snapshot.phase, Phase::Ended);
        }
        server.next().await;
        assert_eq!(server.connections.load(Ordering::SeqCst), 1);
        server.finish().await;
    }
}
#[tokio::test]
async fn header_timeout_and_cancellation_while_waiting_for_headers_or_body_join() {
    for opened in [false, true] {
        let response = if opened {
            sse("")
        } else {
            Response {
                prefix: vec![],
                gate: None,
                tail: vec![],
            }
        };
        let mut server = Server::start(vec![Response {
            gate: Some(Arc::new(Notify::new())),
            ..response
        }])
        .await;
        let (cap, invoker) = provider(
            &server.base,
            None,
            secrets(),
            HttpConfig::default(),
            cap(),
            operation("GET", "/events", &[]),
        );
        let (handle, task) = start(cap, invoker, IndexMap::new());
        server.next().await;
        if opened {
            phase(&handle, Phase::Open).await;
        }
        handle.cancel();
        assert_eq!(joined(&handle, task).await.phase, Phase::Cancelled);
        server.finish().await;
    }
    let server = Server::start(vec![Response {
        prefix: vec![],
        gate: Some(Arc::new(Notify::new())),
        tail: vec![],
    }])
    .await;
    let (cap, invoker) = provider(
        &server.base,
        None,
        secrets(),
        HttpConfig {
            request_timeout: Duration::from_millis(50),
            ..HttpConfig::default()
        },
        cap(),
        operation("GET", "/events", &[]),
    );
    let (handle, task) = start(cap, invoker, IndexMap::new());
    code(joined(&handle, task).await.as_ref(), "HTTP004");
    server.finish().await;
}
#[tokio::test]
async fn cancellation_joins_stream_preparation_and_never_sends_late_request() {
    let server = Server::start(vec![sse("data: 1\n\n")]).await;
    let (release, waiting) = std::sync::mpsc::channel();
    let waiting = std::sync::Mutex::new(waiting);
    let entered = Arc::new(Notify::new());
    let entering = entered.clone();
    let credentials = Arc::new(MemoryCredentials::with_environment(
        CredentialLimits::default(),
        move |_| {
            entering.notify_one();
            waiting
                .lock()
                .unwrap()
                .recv_timeout(Duration::from_secs(3))
                .unwrap();
            Ok(Some(SecretString::from("fixture-private")))
        },
    ));
    let (cap, invoker) = provider(
        &server.base,
        Some(Auth::Query {
            parameter: "key".into(),
            secret: "token".into(),
        }),
        credentials,
        HttpConfig::default(),
        cap(),
        operation("GET", "/events", &[]),
    );
    let (handle, task) = start(cap, invoker, IndexMap::new());
    tokio::time::timeout(Duration::from_secs(1), entered.notified())
        .await
        .unwrap();
    handle.cancel();
    phase(&handle, Phase::Closing).await;
    release.send(()).unwrap();
    assert_eq!(joined(&handle, task).await.phase, Phase::Cancelled);
    assert_eq!(server.connections.load(Ordering::SeqCst), 0);
    server.finish().await;
}
#[tokio::test]
async fn streaming_redirects_preserve_same_origin_auth_and_refuse_cross_origin() {
    let mut server = Server::start(vec![Response { prefix: b"HTTP/1.1 307 Redirect\r\nLocation: /next\r\nContent-Length: 0\r\nConnection: close\r\n\r\n".to_vec(), gate: None, tail: vec![] }, sse("data: 1\n\n")]).await;
    let store = secrets();
    store
        .remember("token".into(), SecretString::from("fixture-private"))
        .unwrap();
    let auth = Some(Auth::Header {
        name: "X-Key".into(),
        scheme: "".into(),
        secret: "token".into(),
    });
    let (capability, invoker) = provider(
        &server.base,
        auth.clone(),
        store.clone(),
        HttpConfig::default(),
        cap(),
        operation("POST", "/events", &[]),
    );
    let (handle, task) = start(capability, invoker, args(&[("data", Data::Int(3))]));
    assert_eq!(joined(&handle, task).await.phase, Phase::Ended);
    server.next().await;
    let request = server.next().await;
    assert!(request.starts_with("POST /next HTTP/1.1"));
    assert!(request.contains("x-key: fixture-private\r\n"));
    assert!(request.ends_with("3"));
    server.finish().await;
    let target = Server::start(vec![sse("data: 9\n\n")]).await;
    let mut redirect = reply(307, "");
    redirect.prefix = format!("HTTP/1.1 307 Redirect\r\nLocation: {}/events\r\nContent-Length: 0\r\nConnection: close\r\n\r\n", target.base).into_bytes();
    let source = Server::start(vec![redirect]).await;
    let (cap, invoker) = provider(
        &source.base,
        auth,
        store,
        HttpConfig::default(),
        cap(),
        operation("GET", "/events", &[]),
    );
    let (handle, task) = start(cap, invoker, IndexMap::new());
    code(joined(&handle, task).await.as_ref(), "HTTP005");
    assert_eq!(target.connections.load(Ordering::SeqCst), 0);
    source.finish().await;
    target.finish().await;
}
