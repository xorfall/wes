use crate::{
    credentials::{CredentialLimits, MemoryCredentials},
    http::*,
};
use indexmap::IndexMap;
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
    sync::{Notify, mpsc},
    task::JoinHandle,
};
use wes_core::{
    Data, ErrorValue, Provenance, Shape, Value,
    capability::{Capability, Parameter, Safety},
};
use wes_engine::trace::Traces;
use wes_engine::{
    credentials::{Credentials, SecretString},
    driver::CancellationToken,
    providers::{Call, InvocationError, Invoker},
    runtime::{Effect, ExecutionTraits, Runtime},
};

struct Response {
    prefix: Vec<u8>,
    gate: Option<Arc<Notify>>,
    tail: Vec<u8>,
}
fn reply(status: u16, body: &str) -> Response {
    Response {
        prefix: format!(
            "HTTP/1.1 {status} Fixture\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        )
        .into_bytes(),
        gate: None,
        tail: vec![],
    }
}
struct Server {
    base: String,
    requests: mpsc::Receiver<Vec<u8>>,
    stop: CancellationToken,
    task: Option<JoinHandle<()>>,
    connections: Arc<AtomicUsize>,
}
impl Server {
    async fn start(responses: Vec<Response>) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let (tx, requests) = mpsc::channel(32);
        let stop = CancellationToken::new();
        let stopping = stop.clone();
        let connections = Arc::new(AtomicUsize::new(0));
        let accepted = connections.clone();
        let task = tokio::spawn(async move {
            let serving = async move {
                for response in responses {
                    let (mut stream, _) = listener.accept().await.unwrap();
                    accepted.fetch_add(1, Ordering::SeqCst);
                    let mut received = Vec::new();
                    loop {
                        let mut buf = [0u8; 4096];
                        let n = stream.read(&mut buf).await.unwrap();
                        if n == 0 {
                            break;
                        }
                        received.extend_from_slice(&buf[..n]);
                        assert!(received.len() < 256 * 1024, "fixture request budget");
                        if let Some(end) = received.windows(4).position(|w| w == b"\r\n\r\n") {
                            let header =
                                String::from_utf8_lossy(&received[..end]).to_ascii_lowercase();
                            let length = header
                                .lines()
                                .find_map(|line| {
                                    line.strip_prefix("content-length:")
                                        .map(|v| v.trim().parse::<usize>().unwrap())
                                })
                                .unwrap_or(0);
                            if received.len() >= end + 4 + length {
                                break;
                            }
                        }
                    }
                    tx.send(received).await.unwrap();
                    // Disconnects are expected in cancellation/size-limit tests.
                    if stream.write_all(&response.prefix).await.is_err() {
                        continue;
                    }
                    if let Some(gate) = response.gate {
                        gate.notified().await;
                    }
                    let _ = stream.write_all(&response.tail).await;
                }
            };
            tokio::select! { _=stopping.cancelled()=>{}, _=tokio::time::timeout(Duration::from_secs(5),serving)=>{} }
        });
        Self {
            base,
            requests,
            stop,
            task: Some(task),
            connections,
        }
    }
    async fn next(&mut self) -> String {
        String::from_utf8(
            tokio::time::timeout(Duration::from_secs(3), self.requests.recv())
                .await
                .unwrap()
                .unwrap(),
        )
        .unwrap()
    }
    async fn finish(mut self) {
        self.stop.cancel();
        self.task.take().unwrap().await.unwrap();
    }
}
impl Drop for Server {
    fn drop(&mut self) {
        self.stop.cancel();
    }
}
fn secrets() -> Arc<MemoryCredentials> {
    Arc::new(MemoryCredentials::with_environment(
        CredentialLimits::default(),
        |_| Ok(None),
    ))
}
fn value(data: Data) -> Value {
    Value::new(Shape::Unknown, data, Provenance::default()).unwrap()
}
fn args(items: &[(&str, Data)]) -> IndexMap<String, Value> {
    items
        .iter()
        .map(|(k, v)| (k.to_string(), value(v.clone())))
        .collect()
}
#[derive(Clone, Copy)]
struct EventContract {
    kind: &'static str,
    streaming: bool,
}
fn finite() -> EventContract {
    EventContract {
        kind: "Unknown",
        streaming: false,
    }
}
fn contract(kind: &str) -> Arc<wes_core::contracts::Contract> {
    let mut types = wes_core::contracts::ContractRegistry::new();
    types
        .load(r#"{"version":1,"types":{"Item":{"base":"Record","fields":{"id":"Int"}}}}"#)
        .unwrap();
    types.resolve(kind).unwrap()
}
type OperationInput = (String, String, Vec<explicit::Argument>);
fn operation(method: &str, route: &str, query: &[&str]) -> OperationInput {
    let mut names = query.to_vec();
    names.sort();
    let mut arguments = names
        .into_iter()
        .map(|name| explicit::Argument {
            name: name.into(),
            wire: name.into(),
            location: "query".into(),
            encoding: "scalar".into(),
            required: false,
            contract: contract("Text"),
        })
        .collect::<Vec<_>>();
    if method == "POST" {
        arguments.push(explicit::Argument {
            name: "data".into(),
            wire: "body".into(),
            location: "body".into(),
            encoding: "json".into(),
            required: false,
            contract: contract("Unknown"),
        });
    }
    (method.into(), route.into(), arguments)
}
fn build(
    base: &str,
    auth: Option<Auth>,
    credentials: Arc<dyn Credentials>,
    config: HttpConfig,
    response: EventContract,
    (method, route, arguments): OperationInput,
) -> Result<(wes_core::capability::ProviderDescription, HttpInvoker), HttpConfigError> {
    let mut responses = std::collections::BTreeMap::from([(
        200,
        explicit::Response {
            contract: Some(contract(response.kind)),
        },
    )]);
    if response.streaming {
        responses.insert(204, explicit::Response { contract: None });
    }
    let mut cap = Capability::new(
        ["items"],
        if response.streaming {
            contract(response.kind).shape()
        } else {
            super::response::shape()
        },
        if method == "GET" {
            Safety::Safe
        } else {
            Safety::Unsafe
        },
    );
    cap.streaming = response.streaming;
    cap.parameters = arguments
        .iter()
        .map(|arg| Parameter::new(&arg.name, arg.contract.shape(), arg.required))
        .collect();
    let operation = explicit::Operation::new(method, route, arguments, responses, auth)?;
    let mut builder = HttpProvider::new(base, credentials, config)?;
    builder.offer_explicit(cap, operation)?;
    builder.build("fixture")
}
fn provider(
    base: &str,
    auth: Option<Auth>,
    credentials: Arc<dyn Credentials>,
    config: HttpConfig,
    response: EventContract,
    operation: OperationInput,
) -> (Arc<Capability>, HttpInvoker) {
    let (description, invoker) =
        build(base, auth, credentials, config, response, operation).unwrap();
    (
        description.capability(&["items".into()]).unwrap().clone(),
        invoker,
    )
}
fn call(capability: Arc<Capability>, arguments: IndexMap<String, Value>) -> Call {
    let mut runtime = Runtime::new();
    runtime
        .add(
            (),
            [],
            ExecutionTraits {
                pure: false,
                repeatable: true,
                bounded: true,
            },
        )
        .unwrap();
    let run = runtime
        .start(Duration::ZERO)
        .into_iter()
        .find_map(|effect| match effect {
            Effect::Spawn(ticket) => Some(ticket.run),
            _ => None,
        })
        .unwrap();
    Call {
        authority: Default::default(),
        run,
        capability,
        arguments,
    }
}
async fn invoke(
    cap: Arc<Capability>,
    invoker: &HttpInvoker,
    arguments: IndexMap<String, Value>,
) -> Result<Value, InvocationError> {
    tokio::time::timeout(
        Duration::from_secs(3),
        invoker.invoke(call(cap, arguments), CancellationToken::new()),
    )
    .await
    .unwrap()
}
fn failure(result: Result<Value, InvocationError>) -> ErrorValue {
    match result {
        Err(InvocationError::Failed(error)) => error,
        other => panic!("expected failure, got {other:?}"),
    }
}

#[tokio::test]
async fn undeclared_arguments_are_rejected_before_sending_a_request() {
    let server = Server::start(vec![]).await;
    let (cap, invoker) = provider(
        &server.base,
        None,
        secrets(),
        HttpConfig::default(),
        finite(),
        operation("GET", "/items", &["q"]),
    );
    let error = failure(
        invoke(
            cap,
            &invoker,
            args(&[("undeclared", Data::Text("private-value".into()))]),
        )
        .await,
    );
    assert_eq!(error.code(), "HTTP001");
    assert!(!format!("{error:?}").contains("private-value"));
    assert!(server.requests.is_empty());
    server.finish().await;
}

#[tokio::test]
async fn get_preserves_base_path_explicit_query_and_does_not_trust_response_metadata() {
    let mut server = Server::start(vec![reply(
        200,
        r#"{"id":42,"metadata":{"source":"fixture","number":3}}"#,
    )])
    .await;
    let cap = EventContract {
        kind: "Item",
        ..finite()
    };
    let (cap, invoker) = provider(
        &format!("{}/api/", server.base),
        None,
        secrets(),
        HttpConfig::default(),
        cap,
        operation("GET", "/items", &["z", "a"]),
    );
    let result = invoke(
        cap,
        &invoker,
        args(&[
            ("z", Data::Text("+/?&=~*😀".into())),
            ("a", Data::Text("two words".into())),
        ]),
    )
    .await
    .unwrap();
    assert_eq!(result.provenance().fact("source"), None);
    let Data::Record(fields) = result.data() else {
        panic!("HTTP envelope")
    };
    assert!(matches!(&fields["body"], Data::Record(body) if body.contains_key("metadata")));
    assert_eq!(result.provenance().fact("number"), None);
    let received = server.next().await;
    assert!(
        received.starts_with(
            "GET /api/items?a=two+words&z=%2B%2F%3F%26%3D%7E*%F0%9F%98%80 HTTP/1.1\r\n"
        ),
        "{received}"
    );
    assert!(!received.contains("not-on-wire"));
    assert!(!received.to_ascii_lowercase().contains("content-type:"));
    server.finish().await;
}

#[tokio::test]
async fn post_splits_query_and_sorted_json_body_without_metadata() {
    let mut server = Server::start(vec![reply(200, "true")]).await;
    let (cap, invoker) = provider(
        &server.base,
        None,
        secrets(),
        HttpConfig::default(),
        finite(),
        operation("POST", "/items", &["q"]),
    );
    invoke(
        cap,
        &invoker,
        args(&[
            ("q", Data::Text("books".into())),
            (
                "data",
                Data::Record(
                    [
                        ("z".into(), Data::Decimal("1e4".parse().unwrap())),
                        ("a".into(), Data::Bytes(vec![1, 2, 3].into())),
                    ]
                    .into(),
                ),
            ),
        ]),
    )
    .await
    .unwrap();
    let received = server.next().await;
    assert!(received.starts_with("POST /items?q=books HTTP/1.1\r\n"));
    assert!(received.contains("content-type: application/json\r\n"));
    assert!(received.ends_with(r#"{"a":"AQID","z":10000}"#));
    server.finish().await;
}

#[tokio::test]
async fn nested_composite_auth_injects_each_destination_and_lists_unique_names() {
    let mut server = Server::start(vec![reply(200, "{}")]).await;
    let store = secrets();
    store
        .remember("token".into(), SecretString::from("fixture token"))
        .unwrap();
    store
        .remember("password".into(), SecretString::from("fixture-pass"))
        .unwrap();
    let auth = Auth::Several(vec![
        Auth::Header {
            name: "X-Key".into(),
            scheme: "Bearer".into(),
            secret: "token".into(),
        },
        Auth::Several(vec![
            Auth::Query {
                parameter: "key".into(),
                secret: "token".into(),
            },
            Auth::Basic {
                user: "reader".into(),
                secret: "password".into(),
            },
        ]),
    ]);
    let (description, invoker) = build(
        &server.base,
        Some(auth),
        store,
        HttpConfig::default(),
        finite(),
        operation("GET", "/items", &[]),
    )
    .unwrap();
    assert_eq!(description.secrets(), &["token", "password"]);
    let cap = description.capability(&["items".into()]).unwrap().clone();
    invoke(cap, &invoker, IndexMap::new()).await.unwrap();
    let received = server.next().await;
    assert!(received.starts_with("GET /items?key=fixture+token HTTP/1.1"));
    assert!(received.contains("x-key: Bearer fixture token\r\n"));
    assert!(received.contains("authorization: Basic cmVhZGVyOmZpeHR1cmUtcGFzcw==\r\n"));
    assert!(!format!("{invoker:?}").contains("fixture token"));
    server.finish().await;
}

#[tokio::test]
async fn missing_invalid_and_colliding_credentials_fail_before_a_request() {
    let server = Server::start(vec![reply(200, "{}")]).await;
    let store = secrets();
    let auth = Auth::Header {
        name: "X-Key".into(),
        scheme: String::new(),
        secret: "token".into(),
    };
    let (cap, invoker) = provider(
        &server.base,
        Some(auth),
        store.clone(),
        HttpConfig::default(),
        finite(),
        operation("GET", "/items", &[]),
    );
    let error = failure(invoke(cap.clone(), &invoker, IndexMap::new()).await);
    assert_eq!(error.code(), "HTTP002");
    assert!(error.message().contains("token"));
    store
        .remember(
            "token".into(),
            SecretString::from("private\r\nInjected: bad"),
        )
        .unwrap();
    let error = failure(invoke(cap, &invoker, IndexMap::new()).await);
    assert_eq!(error.code(), "HTTP001");
    assert!(!format!("{error:?}").contains("private"));
    store
        .remember("token".into(), SecretString::from("private-value"))
        .unwrap();
    assert!(
        build(
            &server.base,
            Some(Auth::Query {
                parameter: "q".into(),
                secret: "token".into()
            }),
            store,
            HttpConfig::default(),
            finite(),
            operation("GET", "/items", &["q"])
        )
        .is_err()
    );
    assert!(server.requests.is_empty());
    server.finish().await;
}

#[tokio::test]
async fn received_error_status_is_data_without_retry_and_trace_is_redacted() {
    for status in [400, 429, 500] {
        let secret = "fixture +/secret";
        let body = "fixture +/secret fixture+%2B%2Fsecret cmVhZGVyOmZpeHR1cmUgKy9zZWNyZXQ= ordinary explanation";
        let mut server = Server::start(vec![reply(status, body)]).await;
        let store = secrets();
        store
            .remember("token".into(), SecretString::from(secret))
            .unwrap();
        let (cap, invoker) = provider(
            &server.base,
            Some(Auth::Basic {
                user: "reader".into(),
                secret: "token".into(),
            }),
            store,
            HttpConfig::default(),
            finite(),
            operation("POST", "/items", &[]),
        );
        let request = call(cap, IndexMap::new());
        let run = request.run.clone();
        let traces = Traces::default();
        let sink = traces.begin(&run, "http", Provenance::default()).unwrap();
        let result = invoker
            .invoke_observed(request, CancellationToken::new(), sink)
            .await
            .unwrap();
        let Data::Record(fields) = result.data() else {
            panic!()
        };
        assert_eq!(fields["status"], Data::Int(status.into()));
        assert_eq!(fields["originalBody"], Data::Bytes(body.as_bytes().into()));
        let trace = format!("{:?}", traces.get(run.node(), None).unwrap().data());
        assert!(!trace.contains(secret));
        assert!(!trace.contains("fixture+%2B"));
        assert!(!trace.contains("cmVhZGVyOmZpeHR1cmUgKy9zZWNyZXQ="));
        server.next().await;
        assert!(server.requests.is_empty());
        server.finish().await;
    }
}

#[tokio::test]
async fn secrets_crossing_excerpt_boundary_or_surrounded_by_significant_space_do_not_leak() {
    for secret in [" leading-private ", "fixture-long-private"] {
        let body = format!("{}{secret} tail", "x".repeat(495));
        let server = Server::start(vec![reply(400, &body)]).await;
        let store = secrets();
        store
            .remember("token".into(), SecretString::from(secret))
            .unwrap();
        let (cap, invoker) = provider(
            &server.base,
            Some(Auth::Query {
                parameter: "key".into(),
                secret: "token".into(),
            }),
            store,
            HttpConfig::default(),
            finite(),
            operation("GET", "/items", &[]),
        );
        let request = call(cap, IndexMap::new());
        let run = request.run.clone();
        let traces = Traces::default();
        let sink = traces.begin(&run, "http", Provenance::default()).unwrap();
        invoker
            .invoke_observed(request, CancellationToken::new(), sink)
            .await
            .unwrap();
        let trace = format!("{:?}", traces.get(run.node(), None).unwrap().data());
        assert!(!trace.contains("private"));
        server.finish().await;
    }
}

#[tokio::test]
async fn response_limits_cover_content_length_chunked_and_exact_boundary() {
    for response in [reply(200,"12345"), Response { prefix:b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\nConnection: close\r\n\r\n5\r\n12345\r\n0\r\n\r\n".to_vec(),gate:None,tail:vec![] }] {
        let server=Server::start(vec![response]).await;
        let mut config=HttpConfig::default(); config.response.bytes=4;
        let (cap,invoker)=provider(&server.base,None,secrets(),config,finite(),operation("GET","/items",&[]));
        assert_eq!(failure(invoke(cap,&invoker,IndexMap::new()).await).code(),"HTTP006"); server.finish().await;
    }
    let server = Server::start(vec![reply(200, "true")]).await;
    let mut config = HttpConfig::default();
    config.response.bytes = 4;
    let (cap, invoker) = provider(
        &server.base,
        None,
        secrets(),
        config,
        finite(),
        operation("GET", "/items", &[]),
    );
    assert!(invoke(cap, &invoker, IndexMap::new()).await.is_ok());
    server.finish().await;
}

#[tokio::test]
async fn malformed_or_wrongly_typed_responses_do_not_echo_their_contents() {
    for body in [
        "fixture-private-not-json",
        r#"{"id":"fixture-private"}"#,
        r#"{"id":1,"id":2}"#,
    ] {
        let server = Server::start(vec![reply(200, body)]).await;
        let cap = EventContract {
            kind: "Item",
            ..finite()
        };
        let (cap, invoker) = provider(
            &server.base,
            None,
            secrets(),
            HttpConfig::default(),
            cap,
            operation("GET", "/items", &[]),
        );
        let result = invoke(cap, &invoker, IndexMap::new()).await.unwrap();
        let Data::Record(fields) = result.data() else {
            panic!()
        };
        assert_eq!(fields["originalBody"], Data::Bytes(body.as_bytes().into()));
        let Data::Record(validation) = &fields["validation"] else {
            panic!()
        };
        assert_ne!(validation["state"], Data::Text("validated".into()));
        assert!(!format!("{:?}", validation).contains("fixture-private"));
        server.finish().await;
    }
}

#[tokio::test]
async fn same_origin_redirect_preserves_auth_but_never_adds_referer() {
    let mut server=Server::start(vec![Response { prefix:b"HTTP/1.1 302 Found\r\nLocation: /next\r\nContent-Length: 0\r\nConnection: close\r\n\r\n".to_vec(),gate:None,tail:vec![] },reply(200,"true")]).await;
    let store = secrets();
    store
        .remember("token".into(), SecretString::from("fixture-private"))
        .unwrap();
    let (cap, invoker) = provider(
        &server.base,
        Some(Auth::Header {
            name: "X-Key".into(),
            scheme: String::new(),
            secret: "token".into(),
        }),
        store,
        HttpConfig::default(),
        finite(),
        operation("GET", "/items", &[]),
    );
    invoke(cap, &invoker, IndexMap::new()).await.unwrap();
    server.next().await;
    let redirected = server.next().await;
    assert!(redirected.starts_with("GET /next HTTP/1.1"));
    assert!(redirected.contains("x-key: fixture-private\r\n"));
    assert!(!redirected.contains("referer:"));
    server.finish().await;
}

#[tokio::test]
async fn cross_origin_redirect_never_contacts_the_second_server() {
    let other = Server::start(vec![reply(200, "true")]).await;
    let location = format!(
        "HTTP/1.1 307 Redirect\r\nLocation: {}/stolen\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
        other.base
    );
    let server = Server::start(vec![Response {
        prefix: location.into_bytes(),
        gate: None,
        tail: vec![],
    }])
    .await;
    let store = secrets();
    store
        .remember("token".into(), SecretString::from("fixture-private"))
        .unwrap();
    let (cap, invoker) = provider(
        &server.base,
        Some(Auth::Query {
            parameter: "key".into(),
            secret: "token".into(),
        }),
        store,
        HttpConfig::default(),
        finite(),
        operation("POST", "/items", &[]),
    );
    let error = failure(invoke(cap, &invoker, IndexMap::new()).await);
    assert_eq!(error.code(), "HTTP005");
    assert!(!format!("{error:?}").contains("fixture-private"));
    assert!(other.requests.is_empty());
    assert_eq!(other.connections.load(Ordering::SeqCst), 0);
    server.finish().await;
    other.finish().await;
}

#[tokio::test]
async fn cancel_before_entry_and_during_body_is_distinct_from_transport_timeout() {
    let gate = Arc::new(Notify::new());
    let mut server = Server::start(vec![Response {
        prefix: b"HTTP/1.1 200 OK\r\nContent-Length: 4\r\nConnection: close\r\n\r\nt".to_vec(),
        gate: Some(gate),
        tail: b"rue".to_vec(),
    }])
    .await;
    let (cap, invoker) = provider(
        &server.base,
        None,
        secrets(),
        HttpConfig::default(),
        finite(),
        operation("GET", "/items", &[]),
    );
    let cancelled = CancellationToken::new();
    cancelled.cancel();
    assert!(matches!(
        invoker
            .invoke(call(cap.clone(), IndexMap::new()), cancelled)
            .await,
        Err(InvocationError::Cancelled)
    ));
    assert!(server.requests.is_empty());
    let cancellation = CancellationToken::new();
    let running = tokio::spawn(invoker.invoke(call(cap, IndexMap::new()), cancellation.clone()));
    server.next().await;
    cancellation.cancel();
    assert!(matches!(
        tokio::time::timeout(Duration::from_secs(1), running)
            .await
            .unwrap()
            .unwrap(),
        Err(InvocationError::Cancelled)
    ));
    server.finish().await;

    let server = Server::start(vec![Response {
        prefix: b"HTTP/1.1 200 OK\r\nContent-Length: 4\r\n\r\nt".to_vec(),
        gate: Some(Arc::new(Notify::new())),
        tail: vec![],
    }])
    .await;
    let config = HttpConfig {
        request_timeout: Duration::from_millis(50),
        ..HttpConfig::default()
    };
    let (cap, invoker) = provider(
        &server.base,
        None,
        secrets(),
        config,
        finite(),
        operation("GET", "/items", &[]),
    );
    assert_eq!(
        failure(invoke(cap, &invoker, IndexMap::new()).await).code(),
        "HTTP004"
    );
    server.finish().await;
}

#[tokio::test]
async fn request_url_header_body_and_aggregate_work_budgets_fail_before_io() {
    let server = Server::start(vec![reply(200, "true")]).await;
    let mut cases = vec![];
    let config = HttpConfig {
        url_bytes: server.base.len() + 10,
        ..HttpConfig::default()
    };
    cases.push((
        config,
        operation("GET", "/items", &["q"]),
        args(&[("q", Data::Text("very long query".into()))]),
    ));
    let mut config = HttpConfig::default();
    config.request.bytes = 5;
    cases.push((
        config,
        operation("POST", "/items", &[]),
        args(&[("data", Data::Text("oversized".into()))]),
    ));
    let mut config = HttpConfig::default();
    config.request.nodes = 4;
    cases.push((
        config,
        {
            let mut op = operation("GET", "/items", &["a", "b"]);
            for arg in &mut op.2 {
                arg.encoding = "repeat".into();
                arg.contract = contract("List<Int>");
            }
            op
        },
        args(&[
            ("a", Data::List(vec![Data::Int(1)])),
            ("b", Data::List(vec![Data::Int(2)])),
        ]),
    ));
    for (config, binding, arguments) in cases {
        let (cap, invoker) = provider(&server.base, None, secrets(), config, finite(), binding);
        assert_eq!(
            failure(invoke(cap, &invoker, arguments).await).code(),
            "HTTP001"
        );
    }
    let store = secrets();
    store
        .remember("token".into(), SecretString::from("fixture-private"))
        .unwrap();
    let config = HttpConfig {
        header_bytes: 4,
        ..HttpConfig::default()
    };
    let (cap, invoker) = provider(
        &server.base,
        Some(Auth::Header {
            name: "X-Key".into(),
            scheme: String::new(),
            secret: "token".into(),
        }),
        store,
        config,
        finite(),
        operation("GET", "/items", &[]),
    );
    assert_eq!(
        failure(invoke(cap, &invoker, IndexMap::new()).await).code(),
        "HTTP001"
    );
    assert!(server.requests.is_empty());
    server.finish().await;
}

#[test]
fn malformed_configuration_and_duplicate_auth_destinations_are_rejected() {
    for base in [
        "file:///tmp/fixture",
        "http://user:fixture-private@example.test",
        "http://example.test/?key=private",
        "http://example.test/#fragment",
        "http://example.test/\r\n",
        "http://example.test\\path",
    ] {
        let error = HttpProvider::new(base, secrets(), HttpConfig::default())
            .err()
            .unwrap();
        assert!(!format!("{error:?}").contains("fixture-private"));
    }
    for auth in [
        Auth::Header {
            name: "Host".into(),
            scheme: String::new(),
            secret: "key".into(),
        },
        Auth::Several(vec![]),
        Auth::Several(vec![
            Auth::Basic {
                user: "reader".into(),
                secret: "a".into(),
            },
            Auth::Header {
                name: "Authorization".into(),
                scheme: String::new(),
                secret: "b".into(),
            },
        ]),
    ] {
        assert!(
            build(
                "http://example.test",
                Some(auth),
                secrets(),
                HttpConfig::default(),
                finite(),
                operation("GET", "/items", &[])
            )
            .is_err()
        );
    }
    for path in [
        "https://other.test/",
        "/items?key=private",
        "/items#fragment",
        "/bad\\path",
        "/../outside",
        "/%2E%2e/outside",
    ] {
        assert!(
            build(
                "http://example.test/api",
                None,
                secrets(),
                HttpConfig::default(),
                finite(),
                operation("GET", path, &[])
            )
            .is_err()
        );
    }
    let mut builder =
        HttpProvider::new("http://example.test/api", secrets(), HttpConfig::default()).unwrap();
    for duplicate in [false, true] {
        let (method, route, args) = operation("GET", "/items", &[]);
        let op = explicit::Operation::new(method, route, args, Default::default(), None).unwrap();
        let cap = Capability::new(["items"], super::response::shape(), Safety::Safe);
        assert_eq!(builder.offer_explicit(cap, op).is_err(), duplicate);
    }
}

#[tokio::test]
async fn each_request_refreshes_credentials_but_shared_names_are_read_once() {
    let mut server = Server::start(vec![reply(200, "true"), reply(200, "true")]).await;
    let reads = Arc::new(AtomicUsize::new(0));
    let captured = reads.clone();
    let store = Arc::new(MemoryCredentials::with_environment(
        CredentialLimits::default(),
        move |_| {
            Ok(Some(SecretString::from(format!(
                "fixture-{}",
                captured.fetch_add(1, Ordering::SeqCst) + 1
            ))))
        },
    ));
    let auth = Auth::Several(vec![
        Auth::Header {
            name: "X-One".into(),
            scheme: String::new(),
            secret: "token".into(),
        },
        Auth::Header {
            name: "X-Two".into(),
            scheme: String::new(),
            secret: "token".into(),
        },
    ]);
    let (cap, invoker) = provider(
        &server.base,
        Some(auth),
        store,
        HttpConfig::default(),
        finite(),
        operation("GET", "/items", &[]),
    );
    for n in 1..=2 {
        invoke(cap.clone(), &invoker, IndexMap::new())
            .await
            .unwrap();
        let received = server.next().await;
        assert!(received.contains(&format!("x-one: fixture-{n}\r\n")));
        assert!(received.contains(&format!("x-two: fixture-{n}\r\n")));
    }
    assert_eq!(reads.load(Ordering::SeqCst), 2);
    server.finish().await;
}

#[tokio::test]
async fn cancellation_joins_blocked_preparation_without_sending_its_late_request() {
    let server = Server::start(vec![reply(200, "true")]).await;
    let (release, waiting) = std::sync::mpsc::channel();
    let waiting = std::sync::Mutex::new(waiting);
    let entered = Arc::new(Notify::new());
    let entering = entered.clone();
    let store = Arc::new(MemoryCredentials::with_environment(
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
        store,
        HttpConfig::default(),
        finite(),
        operation("GET", "/items", &[]),
    );
    let cancellation = CancellationToken::new();
    let running = tokio::spawn(invoker.invoke(call(cap, IndexMap::new()), cancellation.clone()));
    tokio::time::timeout(Duration::from_secs(1), entered.notified())
        .await
        .unwrap();
    cancellation.cancel();
    assert!(!running.is_finished());
    release.send(()).unwrap();
    assert!(matches!(
        running.await.unwrap(),
        Err(InvocationError::Cancelled)
    ));
    assert_eq!(server.connections.load(Ordering::SeqCst), 0);
    server.finish().await;
}

#[tokio::test]
async fn redirect_loops_and_malformed_transport_fail_without_exposing_request_urls() {
    let redirect = || {
        Response { prefix:b"HTTP/1.1 302 Found\r\nLocation: /items\r\nContent-Length: 0\r\nConnection: close\r\n\r\n".to_vec(),gate:None,tail:vec![] }
    };
    let server = Server::start((0..10).map(|_| redirect()).collect()).await;
    let (cap, invoker) = provider(
        &server.base,
        None,
        secrets(),
        HttpConfig::default(),
        finite(),
        operation("GET", "/items", &[]),
    );
    assert_eq!(
        failure(invoke(cap, &invoker, IndexMap::new()).await).code(),
        "HTTP005"
    );
    assert_eq!(server.connections.load(Ordering::SeqCst), 10);
    server.finish().await;
    let server = Server::start(vec![Response {
        prefix: b"not an HTTP response\r\n\r\n".to_vec(),
        gate: None,
        tail: vec![],
    }])
    .await;
    let store = secrets();
    store
        .remember("token".into(), SecretString::from("fixture-private"))
        .unwrap();
    let (cap, invoker) = provider(
        &server.base,
        Some(Auth::Query {
            parameter: "key".into(),
            secret: "token".into(),
        }),
        store,
        HttpConfig::default(),
        finite(),
        operation("GET", "/items", &[]),
    );
    let error = failure(invoke(cap, &invoker, IndexMap::new()).await);
    assert_eq!(error.code(), "HTTP003");
    assert!(!format!("{error:?}").contains("fixture-private"));
    assert_eq!(server.connections.load(Ordering::SeqCst), 1);
    server.finish().await;
}

#[tokio::test]
async fn typed_template_runtime_and_real_http_share_the_existing_execution_boundary() {
    use wes_engine::{
        driver::Executor,
        runtime::Outcome,
        tasks::TaskExecutor,
        workspace::{Preparation, Workspace},
    };
    use wes_language::{SourceText, Span, parse};
    let mut server = Server::start(vec![reply(
        200,
        r#"{"items":[1,2],"metadata":{"source":"fixture"}}"#,
    )])
    .await;
    let (description, invoker) = build(
        &server.base,
        None,
        secrets(),
        HttpConfig::default(),
        finite(),
        operation("GET", "/items", &["q"]),
    )
    .unwrap();
    let mut workspace =
        Workspace::local(wes_engine::providers::LocalScope::new("fixture").unwrap());
    workspace
        .register_provider(description, Arc::new(invoker))
        .unwrap();
    let types = workspace
        .prepare_type_package(
            "types:\n  Category:\n    base: Text\n    enum: [books]\n",
            Span::at(0),
        )
        .unwrap();
    workspace.commit(types).unwrap();
    for source in [
        ":def search(kind: Category) as fixture items q:?kind",
        "search kind:books > found",
    ] {
        let parsed = parse(&SourceText::new("fixture", source));
        assert!(parsed.diagnostics.is_empty());
        let Preparation::Change(change) = workspace.prepare(&parsed.script.statements[0]).unwrap()
        else {
            panic!("staged change")
        };
        workspace.commit(change).unwrap();
    }
    let invalid = parse(&SourceText::new("fixture", "search kind:music"));
    assert!(workspace.prepare(&invalid.script.statements[0]).is_err());
    assert_eq!(server.connections.load(Ordering::SeqCst), 0);
    let ticket = workspace
        .start(Duration::ZERO)
        .into_iter()
        .find_map(|effect| match effect {
            Effect::Spawn(ticket) => Some(ticket),
            _ => None,
        })
        .unwrap();
    assert!(workspace.enter(&ticket.run));
    let run = ticket.run.clone();
    let report = TaskExecutor::ephemeral()
        .execute(ticket, CancellationToken::new())
        .await;
    assert!(matches!(&report.outcome, Outcome::Produced(_)));
    let output = workspace.resolve("found").unwrap();
    assert_eq!(
        workspace.typing(&output).unwrap().provenance.fact("source"),
        None
    );
    workspace.complete(&run, report.outcome, Duration::from_millis(1));
    assert_eq!(
        workspace.typing(&output).unwrap().provenance.fact("source"),
        None
    );
    assert!(
        server
            .next()
            .await
            .starts_with("GET /items?q=books HTTP/1.1")
    );
    server.finish().await;
}

#[path = "tests/streams.rs"]
mod streams;

#[tokio::test]
async fn traced_transport_timeout_redaction_and_bounded_body_evidence() {
    use wes_engine::trace::Traces;
    let secret = "inspection-canary-123";
    let credentials = secrets();
    credentials
        .remember("token".into(), SecretString::from(secret.to_owned()))
        .unwrap();
    let mut server = Server::start(vec![reply(
        200,
        &format!("{{\"message\":\"{secret}{}\"}}", "x".repeat(6000)),
    )])
    .await;
    let (cap, invoker) = provider(
        &server.base,
        Some(Auth::Header {
            name: "Authorization".into(),
            scheme: "Bearer".into(),
            secret: "token".into(),
        }),
        credentials,
        HttpConfig::default(),
        finite(),
        operation("GET", "/items", &[]),
    );
    let request_call = call(cap, IndexMap::new());
    let run = request_call.run.clone();
    let traces = Traces::default();
    let sink = traces.begin(&run, "http", Provenance::default()).unwrap();
    let result = invoker
        .invoke_observed(request_call, CancellationToken::new(), sink)
        .await
        .unwrap();
    assert!(format!("{:?}", result.data()).contains(secret));
    let shown = format!("{:?}", traces.get(run.node(), None).unwrap().data());
    assert!(!shown.contains(secret));
    assert!(shown.contains("REDACTED"));
    assert!(shown.contains("Http") || shown.contains("http.body"));
    assert!(shown.len() < 15_000);
    server.requests.recv().await.unwrap();
    server.finish().await;
    let gate = Arc::new(Notify::new());
    let mut server = Server::start(vec![Response {
        prefix: b"HTTP/1.1 200 OK\r\nContent-Length: 10\r\n\r\nx".to_vec(),
        gate: Some(gate.clone()),
        tail: b"123456789".to_vec(),
    }])
    .await;
    let (cap, invoker) = provider(
        &server.base,
        None,
        secrets(),
        HttpConfig {
            request_timeout: Duration::from_millis(80),
            ..HttpConfig::default()
        },
        finite(),
        operation("GET", "/items", &[]),
    );
    let request_call = call(cap, IndexMap::new());
    let run = request_call.run.clone();
    let traces = Traces::default();
    let sink = traces.begin(&run, "http", Provenance::default()).unwrap();
    assert_eq!(
        failure(
            invoker
                .invoke_observed(request_call, CancellationToken::new(), sink)
                .await
        )
        .code(),
        "HTTP004"
    );
    let trace = format!("{:?}", traces.get(run.node(), None).unwrap().data());
    assert!(trace.contains("http.response"));
    assert!(trace.contains("http.progress"));
    assert!(!trace.contains("http.body"));
    gate.notify_one();
    server.requests.recv().await.unwrap();
    server.finish().await;
}

#[tokio::test]
async fn direct_http_keeps_empty_binary_and_success_json_as_body_bytes() {
    let responses = vec![
        reply(204, ""),
        Response {
            prefix: b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\n\x00\xff"
                .to_vec(),
            gate: None,
            tail: vec![],
        },
        reply(200, "{\"n\":1}"),
    ];
    let mut server = Server::start(responses).await;
    let (description, invoker) = direct(HttpConfig::default()).unwrap();
    let cap = description.capability(&["request".into()]).unwrap().clone();
    for (status, body) in [
        (204, vec![]),
        (200, vec![0, 255]),
        (200, b"{\"n\":1}".to_vec()),
    ] {
        let result = invoker
            .invoke(
                call(
                    cap.clone(),
                    args(&[("url", Data::Text(server.base.clone().into()))]),
                ),
                CancellationToken::new(),
            )
            .await
            .unwrap();
        let Data::Record(fields) = result.data() else {
            panic!()
        };
        assert_eq!(fields["status"], Data::Int(status));
        assert_eq!(fields["originalBody"], Data::Bytes(body.into()));
        server.requests.recv().await.unwrap();
    }
    server.finish().await;
}

#[test]
fn credential_denial_is_distinct_from_store_failure_and_invalid_material() {
    use wes_engine::credentials::{CredentialError, Secret};
    struct Port(CredentialError);
    impl Credentials for Port {
        fn lookup(&self, _: &str) -> Result<Option<Secret>, CredentialError> {
            Err(self.0.clone())
        }
    }
    let auth = auth::CompiledAuth::new(Some(Auth::Header {
        name: "Authorization".into(),
        scheme: "Bearer".into(),
        secret: "token".into(),
    }))
    .unwrap();
    for (error, denied) in [
        (CredentialError::AccessDenied("inventory".into()), true),
        (CredentialError::Unavailable, false),
    ] {
        let failure = match auth.inject(
            &Port(error),
            &mut Default::default(),
            &mut vec![],
            8192,
            8192,
        ) {
            Err(error) => match error.error() {
                InvocationError::Failed(value) => value,
                other => panic!("{other:?}"),
            },
            Ok(_) => panic!("credential failure expected"),
        };
        assert_eq!(failure.code(), "HTTP002");
        assert_eq!(
            failure.message().contains("--grant-provider inventory"),
            denied
        );
        assert_eq!(
            failure.message().contains("access has not been granted"),
            denied
        );
    }
    struct Empty;
    impl Credentials for Empty {
        fn lookup(&self, _: &str) -> Result<Option<Secret>, CredentialError> {
            Ok(Some(Arc::new(SecretString::from(""))))
        }
    }
    let error = match auth.inject(&Empty, &mut Default::default(), &mut vec![], 8192, 8192) {
        Err(error) => match error.error() {
            InvocationError::Failed(value) => value,
            other => panic!("{other:?}"),
        },
        Ok(_) => panic!("invalid material expected"),
    };
    assert!(error.message().contains("material is invalid"));
    assert!(!error.message().contains("--grant-provider"));
}
