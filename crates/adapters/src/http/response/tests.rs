use super::*;
use crate::{credentials::MemoryCredentials, descriptor, http::HttpConfig};
use indexmap::IndexMap;
use serde_json::json;
use std::sync::Arc;
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpListener,
};
use wes_engine::runtime::{Effect, ExecutionTraits, Runtime};
use wes_engine::{
    driver::CancellationToken,
    imports::ImportMode,
    providers::{Call, InvocationError, Invoker},
};

fn spec() -> serde_json::Value {
    json!({"version":1,"provider":"fixture","types":{"Item":{"base":"Record","fields":{"amount":"Decimal"}}},"operations":[{"path":["get"],"method":"GET","route":"/","auth":[],"parameters":[],"responses":{"200":"Item","204":null,"304":null,"400":"Unknown"}}]})
}
fn build(document: &serde_json::Value, endpoint: &str, config: HttpConfig) -> descriptor::Reading {
    descriptor::read_bound(
        &serde_json::to_vec(document).unwrap(),
        None,
        Arc::new(MemoryCredentials::with_environment(
            Default::default(),
            |_| Ok(None),
        )),
        config,
        ImportMode::Replay,
        Some(endpoint),
    )
    .unwrap()
}
fn call(reading: &descriptor::Reading, arguments: IndexMap<String, Value>) -> Call {
    let mut rt = Runtime::new();
    rt.add(
        (),
        [],
        ExecutionTraits {
            pure: false,
            bounded: true,
            repeatable: true,
        },
    )
    .unwrap();
    let run = rt
        .start(std::time::Duration::ZERO)
        .into_iter()
        .find_map(|e| {
            if let Effect::Spawn(t) = e {
                Some(t.run)
            } else {
                None
            }
        })
        .unwrap();
    Call {
        authority: Default::default(),
        run,
        capability: reading
            .description
            .capability(&["get".into()])
            .unwrap()
            .clone(),
        arguments,
    }
}
fn fields(data: &Data) -> &IndexMap<String, Data> {
    let Data::Record(xs) = data else {
        panic!("record: {data:?}")
    };
    xs
}
fn text_at(data: &Data, field: &str) -> String {
    let Data::Text(s) = &fields(data)[field] else {
        panic!("text")
    };
    s.to_string()
}

#[tokio::test]
async fn all_received_statuses_keep_structural_bodies_headers_bytes_and_validation() {
    for (status, media, body, kind, state) in [
        (
            200,
            "application/json",
            br#"{"amount":1.2300000000000000001}"#.as_slice(),
            "json",
            "validated",
        ),
        (
            200,
            "application/json",
            br#"{"amount":"wrong"}"#,
            "json",
            "mismatch",
        ),
        (
            200,
            "application/json",
            br#"{"amount":1,"amount":2}"#,
            "bytes",
            "unreadable",
        ),
        (200, "application/json", b"{broken", "bytes", "unreadable"),
        (200, "text/html", b"<h1>oops</h1>", "text", "mismatch"),
        (204, "", b"", "empty", "validated"),
        (304, "", b"", "empty", "validated"),
        (302, "text/plain", b"moved", "text", "undocumented"),
        (
            400,
            "application/problem+json",
            br#"{"message":"bad input"}"#,
            "json",
            "validated",
        ),
        (
            422,
            "application/json",
            br#"{"reason":"invalid"}"#,
            "json",
            "undocumented",
        ),
        (
            500,
            "text/html",
            b"<script>neverRun()</script>",
            "text",
            "undocumented",
        ),
        (
            503,
            "application/octet-stream",
            b"\x00\xff\x80",
            "bytes",
            "undocumented",
        ),
        (
            201,
            "text/plain; charset=windows-1254",
            b"\xdd",
            "bytes",
            "unreadable",
        ),
        (201, "", b"opaque", "bytes", "undocumented"),
    ] {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let reading = build(
            &spec(),
            &format!("http://{}", listener.local_addr().unwrap()),
            HttpConfig::default(),
        );
        let invocation = call(&reading, IndexMap::new());
        let expected_shape = invocation.capability.result.clone();
        let server = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut request = vec![];
            while !request.ends_with(b"\r\n\r\n") {
                request.push(socket.read_u8().await.unwrap());
            }
            let header = if media.is_empty() {
                String::new()
            } else {
                format!("Content-Type: {media}\r\n")
            };
            socket.write_all(format!("HTTP/1.1 {status} Fixture\r\n{header}Set-Cookie: one\r\nSet-Cookie: two\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",body.len()).as_bytes()).await.unwrap();
            socket.write_all(body).await.unwrap();
        });
        let result = reading
            .invoker
            .invoke(invocation, CancellationToken::new())
            .await
            .unwrap();
        server.await.unwrap();
        assert!(result.shape().is_assignable_to(&expected_shape));
        assert_eq!(fields(result.data())["status"], Data::Int(status.into()));
        assert_eq!(text_at(result.data(), "bodyKind"), kind);
        assert_eq!(
            text_at(&fields(result.data())["validation"], "state"),
            state,
            "status {status} {media}"
        );
        assert_eq!(
            fields(result.data())["originalBody"],
            Data::Bytes(body.into())
        );
        let Data::List(headers) = &fields(result.data())["headers"] else {
            panic!()
        };
        assert_eq!(
            headers
                .iter()
                .filter(|h| text_at(h, "name") == "set-cookie")
                .count(),
            2
        );
        if state == "validated" && status == 200 {
            let meta =
                serde_json::to_value(result.metadata().expect("validated response metadata"))
                    .unwrap();
            assert_eq!(meta["contract"]["name"], "HttpResponse");
            assert_eq!(meta["fields"]["/f:body"]["contract"]["name"], "Item");
            assert_eq!(meta["fields"]["/f:body/f:amount"]["kind"], "decimal");
            let Data::Decimal(amount) = &fields(&fields(result.data())["body"])["amount"] else {
                panic!("exact Decimal")
            };
            assert_eq!(amount.to_string(), "1.2300000000000000001");
        }
    }
}

#[tokio::test]
async fn byte_limit_timeout_and_cancellation_remain_invocation_failures() {
    for size_limit in [true, false] {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let mut config = HttpConfig::default();
        config.request_timeout = std::time::Duration::from_millis(30);
        if size_limit {
            config.response.bytes = 2;
        }
        let reading = build(
            &spec(),
            &format!("http://{}", listener.local_addr().unwrap()),
            config,
        );
        let server = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            if size_limit {
                // The request is read first: closing a socket with unread input resets the
                // connection on some hosts, and the reply would be lost with it.
                let mut request = Vec::new();
                while !request.ends_with(b"\r\n\r\n") {
                    let mut byte = [0u8; 1];
                    if socket.read(&mut byte).await.unwrap() == 0 {
                        break;
                    }
                    request.push(byte[0]);
                }
                socket.write_all(b"HTTP/1.1 400 Bad\r\nContent-Length: 10\r\nConnection: close\r\n\r\n0123456789").await.unwrap();
            } else {
                tokio::time::sleep(std::time::Duration::from_secs(1)).await;
            }
        });
        let error = reading
            .invoker
            .invoke(call(&reading, IndexMap::new()), CancellationToken::new())
            .await
            .unwrap_err();
        let InvocationError::Failed(error) = error else {
            panic!()
        };
        assert_eq!(error.code(), if size_limit { "HTTP006" } else { "HTTP004" });
        server.abort();
    }
    let reading = build(&spec(), "http://127.0.0.1:1", HttpConfig::default());
    let token = CancellationToken::new();
    token.cancel();
    assert!(matches!(
        reading
            .invoker
            .invoke(call(&reading, IndexMap::new()), token)
            .await,
        Err(InvocationError::Cancelled)
    ));
}

#[test]
fn unknown_versions_and_bodyful_304_are_not_admitted() {
    let mut doc = spec();
    doc["version"] = json!(4);
    let read = |doc: &serde_json::Value| {
        descriptor::read_bound(
            &serde_json::to_vec(doc).unwrap(),
            None,
            Arc::new(MemoryCredentials::with_environment(
                Default::default(),
                |_| Ok(None),
            )),
            HttpConfig::default(),
            ImportMode::Replay,
            Some("http://127.0.0.1:1"),
        )
    };
    assert!(read(&doc).is_err());
    doc = spec();
    doc["operations"][0]["responses"]["304"] = json!("Item");
    assert!(read(&doc).is_err());
}
