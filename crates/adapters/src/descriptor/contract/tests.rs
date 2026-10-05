use super::*;
use serde_json::json;
use std::sync::atomic::{AtomicUsize, Ordering};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpListener,
};
use wes_core::{Provenance, Value};
use wes_engine::{
    credentials::{CredentialError, Secret},
    driver::CancellationToken,
    providers::{Call, InvocationError, Invoker},
    runtime::{Effect, ExecutionTraits, Runtime},
};

#[derive(Default)]
struct Secrets(AtomicUsize);
impl Credentials for Secrets {
    fn lookup(&self, _: &str) -> std::result::Result<Option<Secret>, CredentialError> {
        self.0.fetch_add(1, Ordering::SeqCst);
        Ok(None)
    }
}
fn doc() -> serde_json::Value {
    json!({"version":1,"provider":"fixture","servers":[{"url":"https://ignored.invalid"}],
    "types":{"Item":{"base":"Record","fields":{"id":"Int","note":{"type":"Option<Text>","optional":true},"labels":"Map<Text,Int>"}},"Limit":{"base":"Int","min":1,"max":10}},
    "operations":[{"path":["fetch"],"method":"GET","route":"/items/{id}","auth":[],
        "parameters":[{"name":"id","wire":"id","location":"path","type":"Text","required":true,"encoding":"scalar"},
        {"name":"limit","wire":"limit","location":"query","type":"Limit","required":false,"encoding":"scalar"}],
        "responses":{"200":"Item"}}]})
}
fn reading(
    doc: &serde_json::Value,
    endpoint: Option<&str>,
    secrets: Arc<Secrets>,
) -> Result<Reading> {
    super::super::read_bound(
        &serde_json::to_vec(doc).unwrap(),
        None,
        secrets,
        HttpConfig::default(),
        wes_engine::imports::ImportMode::Replay,
        endpoint,
    )
}
fn value(data: Data) -> Value {
    Value::new(Shape::Unknown, data, Provenance::default()).unwrap()
}

#[tokio::test]
async fn draft_unknown_inputs_and_output_keep_transport_checks_at_invocation() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let endpoint = format!("http://{}", listener.local_addr().unwrap());
    let (draft, _) =
        crate::api_library::draft::from_descriptor(&serde_json::to_vec(&doc()).unwrap()).unwrap();
    let mut draft: serde_json::Value = serde_json::from_str(&draft).unwrap();
    for param in draft["operations"][0]["parameters"].as_array_mut().unwrap() {
        param["required"] = serde_json::Value::Null;
    }
    draft["operations"][0]["responses"][0]
        .as_object_mut()
        .unwrap()
        .remove("type");
    let checked = crate::api_library::draft::validate(&draft.to_string());
    assert!(checked.valid, "{:?}", checked.diagnostics);
    let r = reading(
        &checked.descriptor.unwrap(),
        Some(&endpoint),
        Arc::default(),
    )
    .unwrap();
    assert_eq!(
        r.description.capability(&["fetch".into()]).unwrap().result,
        crate::http::response::shape()
    );
    // Missing path values are still rejected before sending, even when undocumented.
    assert!(
        r.invoker
            .invoke(call(&r, IndexMap::new()), CancellationToken::new())
            .await
            .is_err()
    );
    let server = tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.unwrap();
        let mut request = Vec::new();
        while !request.windows(4).any(|w| w == b"\r\n\r\n") {
            let mut buf = [0; 4096];
            let n = socket.read(&mut buf).await.unwrap();
            assert!(n > 0);
            request.extend_from_slice(&buf[..n]);
        }
        assert!(
            String::from_utf8(request)
                .unwrap()
                .starts_with("GET /items/one HTTP/1.1")
        );
        let body = r#"{"unexpected":[1,true,null]}"#;
        socket.write_all(format!("HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",body.len()).as_bytes()).await.unwrap();
    });
    let arguments = IndexMap::from([("id".into(), value(Data::Text("one".into())))]);
    assert!(
        r.invoker
            .invoke(call(&r, arguments), CancellationToken::new())
            .await
            .is_ok()
    );
    server.await.unwrap();
}
fn call(reading: &Reading, arguments: IndexMap<String, Value>) -> Call {
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
            .capability(&["fetch".into()])
            .unwrap()
            .clone(),
        arguments,
    }
}
#[test]
fn contract_is_inert_and_requires_environment_destination_and_explicit_security() {
    let secrets = Arc::new(Secrets::default());
    assert!(reading(&doc(), None, secrets.clone()).is_err());
    assert!(reading(&doc(), Some("http://127.0.0.1:1/base"), secrets.clone()).is_ok());
    for (key, value) in [
        ("version", json!(2)),
        ("base", json!("https://untrusted.invalid")),
        ("unknown", json!(true)),
    ] {
        let mut d = doc();
        d[key] = value;
        assert!(reading(&d, Some("http://127.0.0.1:1"), secrets.clone()).is_err());
    }
    let mut d = doc();
    d["operations"][0].as_object_mut().unwrap().remove("auth");
    assert!(reading(&d, Some("http://127.0.0.1:1"), secrets.clone()).is_err());
    let duplicate = br#"{"version":1,"version":1}"#;
    assert!(
        super::super::read_bound(
            duplicate,
            None,
            secrets.clone(),
            HttpConfig::default(),
            wes_engine::imports::ImportMode::Replay,
            Some("http://127.0.0.1:1")
        )
        .is_err()
    );
    assert_eq!(secrets.0.load(Ordering::SeqCst), 0);
}

#[test]
fn contract_stream_metadata_requires_event_contracts_and_preserves_finite_default() {
    let secrets = Arc::new(Secrets::default());
    let finite = reading(&doc(), Some("http://127.0.0.1:1"), secrets.clone()).unwrap();
    assert!(
        !finite
            .description
            .capability(&["fetch".into()])
            .unwrap()
            .streaming
    );
    let mut stream = doc();
    stream["operations"][0]["stream"] = json!(true);
    stream["operations"][0]["responses"]["204"] = json!(null);
    let valid = reading(&stream, Some("http://127.0.0.1:1"), secrets.clone()).unwrap();
    let cap = valid.description.capability(&["fetch".into()]).unwrap();
    assert!(cap.streaming);
    assert!(matches!(cap.result, Shape::Record(_)));
    for responses in [
        json!({"204":null}),
        json!({"200":null}),
        json!({"200":"Item","205":null}),
    ] {
        let mut invalid = stream.clone();
        invalid["operations"][0]["responses"] = responses;
        assert!(reading(&invalid, Some("http://127.0.0.1:1"), secrets.clone()).is_err());
    }
    stream["operations"][0]["method"] = json!("HEAD");
    assert!(reading(&stream, Some("http://127.0.0.1:1"), secrets.clone()).is_err());
    stream["operations"][0]["method"] = json!("GET");
    stream["operations"][0]["stream"] = json!("true");
    assert!(reading(&stream, Some("http://127.0.0.1:1"), secrets.clone()).is_err());
    assert_eq!(secrets.0.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn contract_sse_checks_status_media_full_event_contracts_and_optional_fields() {
    use wes_engine::streams::{self, Phase};
    for (status, media, body, success, count, rejected) in [
        (
            200,
            "text/event-stream",
            "data: {\"id\":2,\"labels\":{}}\n\ndata: {\"id\":0,\"labels\":{}}\n\ndata: invalid\n\ndata: {\"id\":3,\"note\":\"ok\",\"labels\":{}}\n\n",
            true,
            2,
            2,
        ),
        (201, "text/event-stream", "data: {}\n\n", false, 0, 0),
        (200, "application/json", "{}", false, 0, 0),
        (204, "", "", true, 0, 0),
    ] {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let mut definition = doc();
        definition["types"]["Item"]["fields"]["id"] = json!("Limit");
        definition["operations"][0]["stream"] = json!(true);
        definition["operations"][0]["responses"]["204"] = json!(null);
        let r = reading(
            &definition,
            Some(&format!("http://{}", listener.local_addr().unwrap())),
            Arc::default(),
        )
        .unwrap();
        let server = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut request = Vec::new();
            while !request.ends_with(b"\r\n\r\n") {
                request.push(socket.read_u8().await.unwrap());
            }
            let request = String::from_utf8(request).unwrap().to_lowercase();
            assert!(
                request.starts_with("get /items/demo?limit=2 http/1.1\r\n"),
                "{request}"
            );
            assert!(request.contains("accept: text/event-stream\r\n"));
            socket.write_all(format!("HTTP/1.1 {status} Fixture\r\nContent-Type: {media}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",body.len()).as_bytes()).await.unwrap();
        });
        let invocation = call(
            &r,
            IndexMap::from([
                ("id".into(), value(Data::Text("demo".into()))),
                ("limit".into(), value(Data::Int(2))),
            ]),
        );
        let (handle, task) = streams::spawn(
            invocation,
            Arc::new(r.invoker),
            Provenance::default(),
            streams::Limits::default(),
            CancellationToken::new(),
        )
        .unwrap();
        tokio::time::timeout(std::time::Duration::from_secs(5), task.join())
            .await
            .unwrap()
            .unwrap();
        server.await.unwrap();
        let snapshot = handle.snapshot();
        assert_eq!(
            snapshot.phase == Phase::Ended,
            success,
            "{:?}",
            snapshot.phase
        );
        if !success {
            assert!(matches!(&snapshot.phase, Phase::Failed(e) if e.code() == "HTTP007"));
        }
        assert_eq!(snapshot.rejected, rejected);
        let Data::List(rows) = snapshot.window.data() else {
            panic!("window")
        };
        assert_eq!(rows.len(), count);
        if count > 0 {
            let Data::Record(first) = &rows[0] else {
                panic!("record")
            };
            assert_eq!(first["id"], Data::Int(2));
            assert!(!first.contains_key("note")); // Optional fields may be absent entirely.
            let Data::Record(second) = &rows[1] else {
                panic!("record")
            };
            assert_eq!(
                second["note"],
                Data::Option(Some(Box::new(Data::Text("ok".into()))))
            );
        }
    }
}
#[test]
fn invalid_bindings_and_auth_collisions_fail_at_import() {
    for route in [
        "/items/{missing}",
        "/items/prefix{id}",
        "/../items/{id}",
        "/items/{id}?x=1",
    ] {
        let mut d = doc();
        d["operations"][0]["route"] = json!(route);
        assert!(
            reading(&d, Some("http://127.0.0.1:1"), Arc::default()).is_err(),
            "{route}"
        );
    }
    for header in [
        "Host",
        "Authorization",
        "Content-Length",
        "Connection",
        "Cookie",
    ] {
        let mut d = doc();
        d["operations"][0]["parameters"].as_array_mut().unwrap().push(json!({"name":"header","wire":header,"location":"header","type":"Text","required":false,"encoding":"scalar"}));
        assert!(
            reading(&d, Some("http://127.0.0.1:1"), Arc::default()).is_err(),
            "{header}"
        );
    }
    let mut d = doc();
    d["operations"][0]["auth"] = json!([{"query":"limit","secret":"token"}]);
    assert!(reading(&d, Some("http://127.0.0.1:1"), Arc::default()).is_err());
}
#[tokio::test]
async fn invalid_contract_or_dot_argument_fails_before_network() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let r = reading(
        &doc(),
        Some(&format!("http://{}", listener.local_addr().unwrap())),
        Arc::default(),
    )
    .unwrap();
    for (id, limit) in [("..", 2), (".", 2), ("ok", 0), ("ok", 11)] {
        let args = IndexMap::from([
            ("id".into(), value(Data::Text(id.into()))),
            ("limit".into(), value(Data::Int(limit))),
        ]);
        let error = r
            .invoker
            .invoke(call(&r, args), CancellationToken::new())
            .await
            .unwrap_err();
        assert!(format!("{error:?}").contains("HTTP001"));
    }
    assert!(
        tokio::time::timeout(std::time::Duration::from_millis(20), listener.accept())
            .await
            .is_err()
    );
}
#[tokio::test]
async fn exact_status_json_media_and_nested_contracts_are_checked() {
    for (status, media, body, ok) in [
        (
            200,
            "application/json",
            r#"{"id":7,"labels":{"a":2}}"#,
            true,
        ),
        (
            200,
            "application/json; charset=utf-8",
            r#"{"id":7,"note":null,"labels":{"a":2}}"#,
            true,
        ),
        (
            200,
            "application/json",
            r#"{"id":7,"note":"present","labels":{}}"#,
            true,
        ),
        (200, "application/json", r#"{"id":"7","labels":{}}"#, false),
        (
            200,
            "application/json",
            r#"{"id":7,"labels":{"a":"wrong"}}"#,
            false,
        ),
        (
            200,
            "application/json",
            r#"{"id":7,"note":2,"labels":{}}"#,
            false,
        ),
        (200, "text/html", r#"{"id":7,"labels":{}}"#, false),
        (201, "application/json", r#"{"id":7,"labels":{}}"#, false),
        (
            200,
            "application/json",
            r#"{"id":7,"id":8,"labels":{}}"#,
            false,
        ),
    ] {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let endpoint = format!("http://{}/api", listener.local_addr().unwrap());
        let server = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut request = vec![];
            loop {
                let mut b = [0; 4096];
                let n = socket.read(&mut b).await.unwrap();
                assert!(n > 0);
                request.extend_from_slice(&b[..n]);
                if request.windows(4).any(|w| w == b"\r\n\r\n") {
                    break;
                }
            }
            let req = String::from_utf8(request).unwrap();
            assert!(
                req.starts_with("GET /api/items/a%2Fb%25%3F HTTP/1.1"),
                "{req}"
            );
            socket.write_all(format!("HTTP/1.1 {status} OK\r\nContent-Type: {media}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",body.len()).as_bytes()).await.unwrap();
        });
        let r = reading(&doc(), Some(&endpoint), Arc::default()).unwrap();
        let args = IndexMap::from([("id".into(), value(Data::Text("a/b%?".into())))]);
        let result = r
            .invoker
            .invoke(call(&r, args), CancellationToken::new())
            .await;
        let result = result.unwrap();
        assert_eq!(validated(&result), ok, "{body}: {result:?}");
        server.await.unwrap();
    }
}
#[test]
fn generated_example_imports_with_all_operations() {
    let document: serde_json::Value = serde_json::from_str(include_str!(
        "../../../../../examples/api-import/inventory.json"
    ))
    .unwrap();
    let r = reading(&document, Some("http://127.0.0.1:1/v1"), Arc::default()).unwrap();
    assert_eq!(r.description.capabilities().len(), 3);
}

#[tokio::test]
async fn per_operation_auth_is_not_sent_to_public_calls_and_failures_redact_it() {
    use wes_engine::credentials::SecretString;
    struct Supplied;
    impl Credentials for Supplied {
        fn lookup(&self, name: &str) -> std::result::Result<Option<Secret>, CredentialError> {
            assert_eq!(name, "token");
            Ok(Some(Arc::new(SecretString::from(
                "fixture-sensitive-token",
            ))))
        }
    }
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let endpoint = format!("http://{}", listener.local_addr().unwrap());
    let server = tokio::spawn(async move {
        for private in [false, true] {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut request = vec![];
            loop {
                let mut buf = [0; 4096];
                let n = socket.read(&mut buf).await.unwrap();
                assert!(n > 0);
                request.extend_from_slice(&buf[..n]);
                if request.windows(4).any(|w| w == b"\r\n\r\n") {
                    break;
                }
            }
            let request = String::from_utf8(request).unwrap().to_ascii_lowercase();
            assert_eq!(
                request.contains("authorization: bearer fixture-sensitive-token"),
                private
            );
            let (status, body) = if private {
                (403, "fixture-sensitive-token")
            } else {
                (200, r#"{"id":1,"labels":{}}"#)
            };
            socket.write_all(format!("HTTP/1.1 {status} Result\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",body.len()).as_bytes()).await.unwrap();
        }
    });
    let mut d = doc();
    let mut private = d["operations"][0].clone();
    private["path"] = json!(["private"]);
    private["auth"] = json!([{"header":"Authorization","scheme":"Bearer","secret":"token"}]);
    d["operations"].as_array_mut().unwrap().push(private);
    let r = super::super::read_bound(
        &serde_json::to_vec(&d).unwrap(),
        None,
        Arc::new(Supplied),
        HttpConfig::default(),
        wes_engine::imports::ImportMode::Replay,
        Some(&endpoint),
    )
    .unwrap();
    assert_eq!(r.description.secrets(), &["token"]);
    for private in [false, true] {
        let args = IndexMap::from([("id".into(), value(Data::Text("one".into())))]);
        let mut call = call(&r, args);
        if private {
            call.capability = r
                .description
                .capability(&["private".into()])
                .unwrap()
                .clone();
        }
        let result = r.invoker.invoke(call, CancellationToken::new()).await;
        if private {
            let result = result.unwrap();
            let Data::Record(fields) = result.data() else {
                panic!()
            };
            assert_eq!(fields["status"], Data::Int(403));
            assert!(!format!("{:?}", fields["validation"]).contains("fixture-sensitive-token"));
        } else {
            assert!(result.is_ok());
        }
    }
    server.await.unwrap();
}

#[test]
fn request_codec_keeps_native_options_out_of_the_http_wire_format() {
    let data = Data::Record(IndexMap::from([
        ("absent".into(), Data::Option(None)),
        (
            "present".into(),
            Data::Option(Some(Box::new(Data::Text("yes".into())))),
        ),
        (
            "literal".into(),
            Data::Record(IndexMap::from([("kind".into(), Data::Text("none".into()))])),
        ),
    ]));
    let bytes = crate::codec::encode_request_data(&data, Limits::default()).unwrap();
    let json: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(
        json,
        serde_json::json!({"absent":null,"present":"yes","literal":{"kind":"none"}})
    );
}

#[tokio::test]
async fn union_contracts_validate_body_and_response_through_explicit_http() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let endpoint = format!("http://{}", listener.local_addr().unwrap());
    let mut document = doc();
    document["types"] = json!({"Payload":{"base":"Union<Text, List<Int>>"}});
    document["operations"][0] = json!({"path":["fetch"],"method":"POST","route":"/items","auth":[],"parameters":[{"name":"body","wire":"body","location":"body","type":"Payload","required":true,"encoding":"json"}],"responses":{"200":"Payload"}});
    let r = reading(&document, Some(&endpoint), Arc::default()).unwrap();
    let server = tokio::spawn(async move {
        for response in [r#""accepted""#, "[1,2]", "true"] {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut request = Vec::new();
            loop {
                let mut buf = [0; 1024];
                let n = socket.read(&mut buf).await.unwrap();
                assert!(n > 0);
                request.extend_from_slice(&buf[..n]);
                if let Some(end) = request.windows(4).position(|w| w == b"\r\n\r\n") {
                    let header = String::from_utf8_lossy(&request[..end]).to_lowercase();
                    let length: usize = header
                        .lines()
                        .find_map(|line| line.strip_prefix("content-length: "))
                        .unwrap()
                        .parse()
                        .unwrap();
                    if request.len() >= end + 4 + length {
                        assert_eq!(&request[end + 4..], b"[1,2]");
                        assert!(header.contains("content-type: application/json"));
                        break;
                    }
                }
            }
            socket.write_all(format!("HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{response}", response.len()).as_bytes()).await.unwrap();
        }
    });
    for valid in [true, true, false] {
        let args = IndexMap::from([(
            "body".into(),
            value(Data::List(vec![Data::Int(1), Data::Int(2)])),
        )]);
        assert_eq!(
            validated(
                &r.invoker
                    .invoke(call(&r, args), CancellationToken::new())
                    .await
                    .unwrap()
            ),
            valid
        );
    }
    server.await.unwrap();
    // Invalid request must fail before any attempt to connect to the now-closed listener.
    let args = IndexMap::from([("body".into(), value(Data::Bool(true)))]);
    let error = r
        .invoker
        .invoke(call(&r, args), CancellationToken::new())
        .await
        .unwrap_err();
    assert!(matches!(
        error,
        wes_engine::providers::InvocationError::Failed(_)
    ));
}

#[test]
fn imported_information_survives_rename_and_has_no_credential_authority() {
    let mut d = doc();
    d["notes"] = json!([{"kind":"constraint","target":"#/types/Item","description":"Account-specific relationships apply.","enforcement":"not-checked-locally"}]);
    d["source"] = json!({"provenance":{"status":"stale","entries":[{"target":"#/notes/0","basis":"inferred"}]}});
    d["diagnostics"] = json!(["Example service"]);
    let credentials = Arc::new(Secrets::default());
    let read = reading(&d, Some("http://127.0.0.1:1"), credentials.clone()).unwrap();
    let renamed = read.description.renamed("other").unwrap();
    assert_eq!(renamed.information(), read.description.information());
    assert_eq!(
        renamed.information_value().unwrap().shape(),
        read.description.information_value().unwrap().shape()
    );
    let Data::Record(info) = renamed.information().unwrap() else {
        panic!()
    };
    assert!(matches!(&info["notes"], Data::List(notes) if notes.len() == 1));
    assert!(matches!(&info["evidence"], Data::Record(_)));
    assert_eq!(credentials.0.load(Ordering::SeqCst), 0);
    d["notes"][0]["enforcement"] = json!("server-validated");
    assert!(reading(&d, Some("http://127.0.0.1:1"), credentials).is_err());
}

fn auth_document() -> serde_json::Value {
    serde_json::from_str(include_str!(
        "../../../../../examples/http-auth-alternatives/service.json"
    ))
    .unwrap()
}
#[derive(Default)]
struct AuthSecrets {
    requested: std::sync::Mutex<Vec<String>>,
    missing: Option<String>,
}
impl Credentials for AuthSecrets {
    fn lookup(&self, name: &str) -> std::result::Result<Option<Secret>, CredentialError> {
        self.requested.lock().unwrap().push(name.into());
        Ok((self.missing.as_deref() != Some(name)).then(|| {
            Arc::new(wes_engine::credentials::SecretString::from(format!(
                "synthetic-{name}"
            )))
        }))
    }
}
fn auth_read(
    doc: &serde_json::Value,
    selected: Option<Vec<&str>>,
    endpoint: &str,
    secrets: Arc<AuthSecrets>,
) -> Result<Reading> {
    let choices = selected
        .map(|v| {
            BTreeMap::from([(
                "listItems".into(),
                v.into_iter().map(str::to_owned).collect(),
            )])
        })
        .unwrap_or_default();
    super::super::read_selected(
        &serde_json::to_vec(doc).unwrap(),
        None,
        secrets,
        Default::default(),
        wes_engine::imports::ImportMode::Replay,
        Some(endpoint),
        &choices,
    )
}
fn auth_call(reading: &Reading) -> Call {
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
            .capability(&["listItems".into()])
            .unwrap()
            .clone(),
        arguments: IndexMap::new(),
    }
}
#[tokio::test]
async fn environment_choice_injects_only_selected_secrets_and_preserves_info() {
    use base64::Engine;
    for (selection, slots) in [
        (vec!["apiSecret", "apiKey"], vec!["apiKey", "apiSecret"]),
        (vec!["basic"], vec!["basic.password", "basic.username"]),
    ] {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let secrets = Arc::new(AuthSecrets::default());
        let r = auth_read(
            &auth_document(),
            Some(selection),
            &format!("http://{}", listener.local_addr().unwrap()),
            secrets.clone(),
        )
        .unwrap();
        assert!(
            secrets.requested.lock().unwrap().is_empty(),
            "import looked up credentials"
        );
        let information = format!("{:?}", r.description.information());
        assert!(
            information.contains("selected")
                && information.contains("apiKey")
                && information.contains("basic.username")
        );
        assert!(!information.contains("synthetic-"));
        let server = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut request = vec![];
            while !request.windows(4).any(|w| w == b"\r\n\r\n") {
                let mut b = [0; 1024];
                let n = socket.read(&mut b).await.unwrap();
                assert!(n > 0);
                request.extend_from_slice(&b[..n]);
            }
            socket
                .write_all(b"HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: 2\r\nConnection: close\r\n\r\n[]")
                .await
                .unwrap();
            String::from_utf8(request).unwrap()
        });
        r.invoker
            .invoke(auth_call(&r), CancellationToken::new())
            .await
            .unwrap();
        let request = server.await.unwrap().to_ascii_lowercase();
        let mut requested = secrets.requested.lock().unwrap().clone();
        requested.sort();
        assert_eq!(requested, slots);
        if slots[0] == "apiKey" {
            assert!(
                request.contains("x-api-key: synthetic-apikey")
                    && request.contains("x-api-secret: synthetic-apisecret")
            );
            assert!(!request.contains("authorization:"));
        } else {
            let encoded = base64::engine::general_purpose::STANDARD
                .encode("synthetic-basic.username:synthetic-basic.password")
                .to_ascii_lowercase();
            assert!(request.contains(&format!("authorization: basic {encoded}")));
            assert!(!request.contains("x-api-key:"));
        }
    }
}
#[tokio::test]
async fn missing_choice_or_credentials_never_sends_and_never_falls_back() {
    for (selection, missing) in [
        (None, None),
        (Some(vec!["apiKey", "apiSecret"]), Some("apiSecret")),
        (Some(vec!["basic"]), Some("basic.username")),
    ] {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let secrets = Arc::new(AuthSecrets {
            missing: missing.map(str::to_owned),
            ..Default::default()
        });
        let r = auth_read(
            &auth_document(),
            selection.clone(),
            &format!("http://{}", listener.local_addr().unwrap()),
            secrets.clone(),
        )
        .unwrap();
        let err = r
            .invoker
            .invoke(auth_call(&r), CancellationToken::new())
            .await
            .unwrap_err();
        if selection.is_none() {
            assert!(err.to_string().contains("bind.auth"), "{err}");
            assert!(secrets.requested.lock().unwrap().is_empty());
        }
        assert!(
            tokio::time::timeout(std::time::Duration::from_millis(20), listener.accept())
                .await
                .is_err()
        );
    }
}
#[test]
fn every_auth_alternative_is_validated_and_anonymous_requires_explicit_choice() {
    let endpoint = "http://127.0.0.1:1";
    for selection in [
        vec!["bogus"],
        vec!["apiKey"],
        vec!["basic", "basic"],
        vec![],
    ] {
        assert!(auth_read(&auth_document(), Some(selection), endpoint, Arc::default()).is_err());
    }
    let mut d = auth_document();
    let op = d["operations"]
        .as_array_mut()
        .unwrap()
        .iter_mut()
        .find(|op| op["path"][0] == "listItems")
        .unwrap();
    op["authOptions"][1]["auth"][0] = json!({"header":"Host","secret":"bad"});
    assert!(
        auth_read(
            &d,
            Some(vec!["apiKey", "apiSecret"]),
            endpoint,
            Arc::default()
        )
        .is_err(),
        "invalid unselected auth was ignored"
    );
    let op = d["operations"]
        .as_array_mut()
        .unwrap()
        .iter_mut()
        .find(|op| op["path"][0] == "listItems")
        .unwrap();
    op["authOptions"][1] = json!({"schemes":[],"auth":[]});
    let r = auth_read(&d, Some(vec![]), endpoint, Arc::default()).unwrap();
    assert!(r.description.secrets().is_empty());
    assert!(
        auth_read(&d, None, endpoint, Arc::default())
            .unwrap()
            .warnings
            .iter()
            .any(|s| s.contains("choice required"))
    );
}

#[tokio::test]
async fn request_diagnostics_preserve_contract_paths_without_values_or_network() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let secrets = Arc::new(Secrets::default());
    let mut d = doc();
    d["types"]["Feed"] = json!({"base":"Text", "enum":["primary", "backup"]});
    d["types"]["Body"] = json!({"base":"Record", "fields":{"count":"Limit", "mode":"Feed", "labels":"Map<Text,Int>"}});
    d["operations"][0]["method"] = json!("POST");
    d["operations"][0]["auth"] = json!([{"header":"X-Api-Key","secret":"syntheticKey"}]);
    let params = d["operations"][0]["parameters"].as_array_mut().unwrap();
    params.push(json!({"name":"feed", "wire":"feed", "location":"query", "type":"Feed", "required":false,"encoding":"scalar"}));
    params.push(json!({"name":"body", "wire":"body", "location":"body", "type":"Body", "required":false,"encoding":"json"}));
    params.push(json!({"name":"note", "wire":"X-Note", "location":"header", "type":"Text", "required":false,"encoding":"scalar"}));
    let r = reading(
        &d,
        Some(&format!("http://{}", listener.local_addr().unwrap())),
        secrets.clone(),
    )
    .unwrap();
    for (raw, path, code, message) in [
        (
            json!({}),
            "/arguments/id",
            "HTTP_REQUIRED_ARGUMENT",
            "required",
        ),
        (
            json!({"id":".."}),
            "/arguments/id",
            "HTTP_PATH_SEGMENT",
            "dot segment",
        ),
        (
            json!({"id":"ok","feed":"DO_NOT_ECHO_ENUM_VALUE"}),
            "/arguments/feed",
            "TYP005",
            "allowed values",
        ),
        (
            json!({"id":"ok","limit":0}),
            "/arguments/limit",
            "TYP005",
            "bounds",
        ),
        (
            json!({"id":"ok","body":{"count":0,"mode":"DO_NOT_ECHO_BODY_VALUE","labels":{"DO_NOT_ECHO_MAP_KEY":"DO_NOT_ECHO_MAP_VALUE"}}}),
            "/arguments/body/count",
            "TYP005",
            "bounds",
        ),
        (
            json!({"id":"ok","body":{"count":1,"mode":"primary","labels":{}}}),
            "/arguments",
            "HTTP002",
            "credential",
        ),
        (
            json!({"id":"ok","note":"DO_NOT_ECHO_HEADER\r\n"}),
            "/arguments/note",
            "HTTP_HEADER_VALUE",
            "header",
        ),
    ] {
        let Data::Record(args) = crate::codec::decode_json_preserving(
            &serde_json::to_vec(&raw).unwrap(),
            Limits::default(),
        )
        .unwrap() else {
            panic!()
        };
        let args = args
            .into_iter()
            .map(|(key, data)| (key, value(data)))
            .collect();
        let InvocationError::Failed(error) = r
            .invoker
            .invoke(call(&r, args), CancellationToken::new())
            .await
            .unwrap_err()
        else {
            panic!()
        };
        if code == "HTTP002" {
            assert_eq!(error.code(), code);
            continue;
        }
        assert_eq!(error.code(), "HTTP001");
        assert!(
            error.issues().iter().any(|issue| issue.path == path
                && issue.code == code
                && issue.message.contains(message)),
            "{error:?}"
        );
        assert!(!format!("{error:?}").contains("DO_NOT_ECHO"));
        if raw.get("body").is_some() {
            assert_eq!(error.issues().len(), 3);
            assert_eq!(error.issues()[1].path, "/arguments/body/mode");
            assert_eq!(error.issues()[2].path, "/arguments/body/labels/*");
        }
        let private = error.with_policy(&wes_core::flow::FlowPolicy::default().private());
        assert!(private.issues().is_empty());
        assert!(!private.message().contains("/arguments"));
    }
    assert_eq!(
        secrets.0.load(Ordering::SeqCst),
        1,
        "only the valid request reaches credentials"
    );
    assert!(
        tokio::time::timeout(std::time::Duration::from_millis(20), listener.accept())
            .await
            .is_err()
    );
}

#[tokio::test]
async fn oversized_explicit_argument_reports_its_name_before_credentials_or_network() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let secrets = Arc::new(Secrets::default());
    let mut config = HttpConfig::default();
    config.request.bytes = 8;
    let r = super::super::read_bound(
        &serde_json::to_vec(&doc()).unwrap(),
        None,
        secrets.clone(),
        config,
        wes_engine::imports::ImportMode::Replay,
        Some(&format!("http://{}", listener.local_addr().unwrap())),
    )
    .unwrap();
    let args = IndexMap::from([(
        "id".into(),
        value(Data::Text("DO_NOT_ECHO_LONG_ARGUMENT".into())),
    )]);
    let InvocationError::Failed(error) = r
        .invoker
        .invoke(call(&r, args), CancellationToken::new())
        .await
        .unwrap_err()
    else {
        panic!()
    };
    assert_eq!(error.issues()[0].path, "/arguments/id");
    assert_eq!(error.issues()[0].code, "HTTP_REQUEST_BYTES");
    assert!(!format!("{error:?}").contains("DO_NOT_ECHO"));
    assert_eq!(secrets.0.load(Ordering::SeqCst), 0);
    assert!(
        tokio::time::timeout(std::time::Duration::from_millis(20), listener.accept())
            .await
            .is_err()
    );
}

fn validated(value: &wes_core::Value) -> bool {
    let Data::Record(fields) = value.data() else {
        panic!()
    };
    let Data::Record(validation) = &fields["validation"] else {
        panic!()
    };
    validation["state"] == Data::Text("validated".into())
}

#[test]
fn explicit_local_safety_overrides_method_without_changing_the_wire_request() {
    for (method, declared, expected) in [
        ("POST", Some("safe"), Safety::Safe),
        ("GET", Some("unsafe"), Safety::Unsafe),
        ("POST", None, Safety::Unsafe),
        ("GET", None, Safety::Safe),
    ] {
        let mut document = doc();
        document["operations"][0]["method"] = json!(method);
        if let Some(safety) = declared {
            document["operations"][0]["safety"] = json!(safety);
        }
        let read = reading(
            &document,
            Some("http://localhost:1"),
            Arc::new(Secrets::default()),
        )
        .unwrap();
        assert_eq!(
            read.description
                .capability(&["fetch".into()])
                .unwrap()
                .safety,
            expected
        );
        let Data::Record(info) = read.description.information().unwrap() else {
            panic!("information")
        };
        let Data::List(operations) = &info["safety"] else {
            panic!("operations")
        };
        let Data::Record(safety) = &operations[0] else {
            panic!("safety")
        };
        assert_eq!(
            safety["basis"],
            Data::Text(
                if declared.is_some() {
                    "explicit local contract"
                } else {
                    "HTTP method default"
                }
                .into()
            )
        );
        assert_eq!(document["operations"][0]["method"], method);
    }
    let mut invalid = doc();
    invalid["operations"][0]["safety"] = json!("idempotent");
    assert!(
        reading(
            &invalid,
            Some("http://localhost:1"),
            Arc::new(Secrets::default())
        )
        .err()
        .unwrap()
        .to_string()
        .contains("safe or unsafe")
    );
}

#[tokio::test]
async fn safe_post_declaration_preserves_post_on_the_wire() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let endpoint = format!("http://{}", listener.local_addr().unwrap());
    let server = tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.unwrap();
        let mut request = vec![];
        while !request.ends_with(b"\r\n\r\n") {
            request.push(socket.read_u8().await.unwrap());
        }
        assert!(request.starts_with(b"POST /items/one HTTP/1.1\r\n"));
        socket
            .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\n{}")
            .await
            .unwrap();
    });
    let mut document = doc();
    document["operations"][0]["method"] = json!("POST");
    document["operations"][0]["safety"] = json!("safe");
    let r = reading(&document, Some(&endpoint), Arc::new(Secrets::default())).unwrap();
    let arguments = IndexMap::from([("id".into(), value(Data::Text("one".into())))]);
    assert!(
        r.invoker
            .invoke(call(&r, arguments), CancellationToken::new())
            .await
            .is_ok()
    );
    server.await.unwrap();
}
