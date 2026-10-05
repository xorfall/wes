//! Application acceptance uses an isolated data home and synthetic loopback HTTP.
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use wes::runtime::{RuntimeOptions, launch};
use wes_core::Data;
use wes_engine::{session::SessionHandle, source::SourceInput};

async fn submit(session: &SessionHandle, source: &str) -> Vec<wes_engine::graph::NodeId> {
    let reply = tokio::time::timeout(
        Duration::from_secs(10),
        session.submit(SourceInput::new(uuid::Uuid::new_v4().to_string(), source.into()).unwrap()),
    )
    .await
    .unwrap()
    .unwrap();
    assert!(
        !reply
            .diagnostics
            .diagnostics
            .iter()
            .any(|d| d.severity == wes_language::Severity::Error),
        "{reply:?}"
    );
    tokio::time::timeout(Duration::from_secs(10), session.wait_idle())
        .await
        .unwrap()
        .unwrap();
    reply.nodes.clone()
}

#[tokio::test]
async fn http_calc_http_calc_and_reopen_preserve_one_submission_without_reissuing_requests() {
    let root = tempfile::tempdir().unwrap();
    let home = root.path().join("home");
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        for path in ["/first", "/second"] {
            let (mut stream, _) = tokio::time::timeout(Duration::from_secs(10), listener.accept())
                .await
                .unwrap()
                .unwrap();
            let mut request = vec![];
            while !request.windows(4).any(|w| w == b"\r\n\r\n") {
                let mut buffer = [0; 1024];
                let n = tokio::time::timeout(Duration::from_secs(5), stream.read(&mut buffer))
                    .await
                    .unwrap()
                    .unwrap();
                assert!(n > 0 && request.len() < 8192);
                request.extend_from_slice(&buffer[..n]);
            }
            assert!(String::from_utf8_lossy(&request).starts_with(&format!("GET {path} ")));
            let (status, body) = if path == "/first" {
                (
                    "200 OK",
                    format!(r#"{{"next":"http://{address}/second","method":"GET"}}"#),
                )
            } else {
                ("404 Not Found", "{}".into())
            };
            stream.write_all(format!("HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len()).as_bytes()).await.unwrap();
        }
    });
    let runtime = launch(RuntimeOptions::new(home.clone(), root.path().into()))
        .await
        .unwrap();
    let session = runtime.handle.current().unwrap().session;
    let source = format!(
        "http request url:\"http://{address}/first\" > response *> firstError\n| :calc {{ const next = input.body; return {{next: check(\"Text\", next.next), method: check(\"Text\", next.method)}}; }}\n| http request url:input.next method:input.method > second *> secondError\n| :calc {{ return input.status; }} > result"
    );
    let nodes = submit(&session, &source).await;
    assert_eq!(nodes.len(), 4);
    let snapshot = session.snapshot().await.unwrap();
    assert_eq!(
        snapshot
            .execution
            .values
            .get(&snapshot.names["result"].node)
            .unwrap_or_else(|| panic!("pipeline did not publish: {snapshot:?}"))
            .data(),
        &Data::Int(404)
    );
    let rejected = session
        .submit(
            SourceInput::new("repeat".into(), source.clone())
                .unwrap()
                .with_repeat(
                    session
                        .observe()
                        .await
                        .unwrap()
                        .cells
                        .iter()
                        .find(|c| c.input.text() == source)
                        .unwrap()
                        .input
                        .cell()
                        .into(),
                    false,
                )
                .unwrap(),
        )
        .await;
    assert!(matches!(
        rejected,
        Err(wes_engine::session::SessionError::RepeatRefused(_))
    ));
    server.await.unwrap();
    runtime.shutdown().await.unwrap();
    let runtime = launch(RuntimeOptions::new(home, root.path().into()))
        .await
        .unwrap();
    let session = runtime.handle.current().unwrap().session;
    let snapshot = session.snapshot().await.unwrap();
    assert_eq!(snapshot.names["result"].node, nodes[3]);
    assert_eq!(snapshot.execution.values[&nodes[3]].data(), &Data::Int(404));
    assert_eq!(snapshot.execution.graph.len(), 4);
    runtime.shutdown().await.unwrap();
}
