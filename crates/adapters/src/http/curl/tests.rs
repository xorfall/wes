use super::super::{auth::Redactor, transport::Transport};
use super::*;
use std::{os::unix::fs::PermissionsExt, time::Duration};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use wes_engine::environments::{EnvironmentLoader, Registry};

struct Fixture {
    root: tempfile::TempDir,
    binding: Binding,
    authority: Authority,
}
impl Fixture {
    fn new(remote: bool, variables: serde_json::Value) -> Self {
        let root = tempfile::tempdir().unwrap();
        let target = if remote {
            let client = root.path().join("ssh");
            std::fs::write(&client, format!("#!/bin/sh\nprintf '%s\\n' \"$@\" > '{}'\nfor last; do :; done\nexec /bin/sh -c \"$last\"\n", root.path().join("argv").display())).unwrap();
            std::fs::set_permissions(&client, std::fs::Permissions::from_mode(0o700)).unwrap();
            std::fs::write(root.path().join("key"), b"synthetic key").unwrap();
            std::fs::write(root.path().join("hosts"), b"synthetic host").unwrap();
            serde_json::json!({"kind":"ssh", "client":client, "host":"fixture.invalid", "user":"qa", "identity_file":root.path().join("key"), "known_hosts":root.path().join("hosts"), "shell":"posix", "inherit":"remote", "env":variables})
        } else {
            serde_json::json!({"kind":"local", "env":variables})
        };
        let yaml = serde_json::json!({"version":1, "targets":{"test":target}, "environments":{"qa":{"imports":{"http":{"source":{"kind":"builtin","name":"http"},"bind":{"target":"test","transport":"curl"}}}}}});
        let loader = crate::environments::LocalEnvironments::new(root.path()).unwrap();
        let loaded = loader.capture_text(&yaml.to_string(), None, None).unwrap();
        let mut registry = Registry::default();
        let plan = registry
            .plan(
                &wes_core::environments::Package::parse(&loaded.yaml).unwrap(),
                &loaded.sources,
            )
            .unwrap();
        registry.apply(plan).unwrap();
        Self {
            root,
            binding: registry.inspect("qa").unwrap().bind("http").unwrap(),
            authority: Authority::default(),
        }
    }
    async fn send(
        &self,
        request: Request,
        config: HttpConfig,
    ) -> Result<Response, InvocationError> {
        send(
            &self.binding,
            &self.authority,
            request,
            config,
            CancellationToken::new(),
        )
        .await
    }
}
async fn server(responses: Vec<Vec<u8>>) -> (String, tokio::task::JoinHandle<Vec<Vec<u8>>>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let task = tokio::spawn(async move {
        let mut requests = vec![];
        for response in responses {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut data = vec![];
            while !data.ends_with(b"\r\n\r\n") {
                data.push(socket.read_u8().await.unwrap());
            }
            let length = String::from_utf8_lossy(&data)
                .lines()
                .find_map(|line| {
                    line.to_ascii_lowercase()
                        .strip_prefix("content-length:")
                        .map(|v| v.trim().parse::<usize>().unwrap())
                })
                .unwrap_or(0);
            let start = data.len();
            data.resize(start + length, 0);
            socket.read_exact(&mut data[start..]).await.unwrap();
            requests.push(data);
            socket.write_all(&response).await.unwrap();
        }
        requests
    });
    (url, task)
}
fn reply(status: &str, headers: &str, body: &[u8]) -> Vec<u8> {
    let mut out = format!(
        "HTTP/1.1 {status}\r\nContent-Length: {}\r\nConnection: close\r\n{headers}\r\n",
        body.len()
    )
    .into_bytes();
    out.extend_from_slice(body);
    out
}

#[tokio::test]
async fn local_internal_curl_and_ssh_preserve_binary_body_status_and_duplicate_headers() {
    for kind in 0..3 {
        let f = Fixture::new(kind == 2, serde_json::json!({}));
        let binary = b"\0\xff\n\r\"$HOME; 'payload";
        let (url, task) = server(vec![reply(
            "418 Teapot",
            "Set-Cookie: a=1\r\nSet-Cookie: b=2\r\n",
            binary,
        )])
        .await;
        let mut request = Request::new(
            Method::POST,
            format!("{url}/?token=synthetic-secret").parse().unwrap(),
        );
        request.headers_mut().insert(
            "authorization",
            HeaderValue::from_static("Bearer synthetic-private"),
        );
        *request.body_mut() = Some(binary.to_vec().into());
        let transport = if kind == 0 {
            Transport::Internal
        } else {
            Transport::bound(&f.binding, f.authority.clone()).unwrap()
        };
        let config = HttpConfig::default();
        let response = transport
            .send(
                &crate::http::client_builder(config).build().unwrap(),
                request,
                config,
                CancellationToken::new(),
                None,
                &Redactor::empty(),
            )
            .await
            .unwrap();
        assert_eq!(response.status, 418);
        assert_eq!(response.body, binary);
        assert_eq!(response.headers.get_all("set-cookie").iter().count(), 2);
        let sent = task.await.unwrap();
        assert!(sent[0].ends_with(binary));
        assert!(String::from_utf8_lossy(&sent[0]).contains("Bearer synthetic-private"));
        if kind == 2 {
            let argv = std::fs::read_to_string(f.root.path().join("argv")).unwrap();
            for secret in ["synthetic-secret", "synthetic-private", "$HOME;", &url] {
                assert!(!argv.contains(secret));
            }
            assert!(!argv.lines().any(|line| line == "-n"));
        }
    }
}

#[tokio::test]
async fn redirects_preserve_307_body_change_303_method_and_refuse_other_origins() {
    let f = Fixture::new(false, serde_json::json!({}));
    for status in ["307 Temporary Redirect", "303 See Other"] {
        let (url, task) = server(vec![
            reply(status, "Location: /final\r\n", b""),
            reply("200 OK", "", b"done"),
        ])
        .await;
        let mut req = Request::new(Method::POST, url.parse().unwrap());
        *req.body_mut() = Some(b"payload".to_vec().into());
        assert_eq!(
            f.send(req, HttpConfig::default()).await.unwrap().body,
            b"done"
        );
        let requests = task.await.unwrap();
        if status.starts_with("307") {
            assert!(requests[1].starts_with(b"POST /final"));
            assert!(requests[1].ends_with(b"payload"));
        } else {
            assert!(requests[1].starts_with(b"GET /final"));
            assert!(!requests[1].ends_with(b"payload"));
        }
    }
    let (url, task) = server(vec![reply(
        "302 Found",
        "Location: http://127.0.0.1:1/secret\r\n",
        b"",
    )])
    .await;
    assert!(
        f.send(
            Request::new(Method::GET, url.parse().unwrap()),
            HttpConfig::default()
        )
        .await
        .unwrap_err()
        .to_string()
        .contains("redirect was rejected")
    );
    assert_eq!(task.await.unwrap().len(), 1);
}

#[tokio::test]
async fn missing_helper_ambient_config_limits_and_head_have_clear_outcomes() {
    let f = Fixture::new(false, serde_json::json!({"PATH":"/does-not-exist"}));
    let error = f
        .send(
            Request::new(Method::GET, "http://127.0.0.1:1/private".parse().unwrap()),
            HttpConfig::default(),
        )
        .await
        .unwrap_err()
        .to_string();
    assert!(error.contains("install the missing tool"));
    assert!(!error.contains("/private"));
    let f = Fixture::new(false, serde_json::json!({}));
    std::fs::write(
        f.root.path().join(".curlrc"),
        "url = http://127.0.0.1:1/forbidden\ninsecure\n",
    )
    .unwrap();
    let f = Fixture::new(
        false,
        serde_json::json!({"CURL_HOME":f.root.path(),"http_proxy":"http://127.0.0.1:1"}),
    );
    let (url, task) = server(vec![reply("200 OK", "", b"123456")]).await;
    let mut config = HttpConfig::default();
    config.response.bytes = 3;
    assert!(
        f.send(Request::new(Method::GET, url.parse().unwrap()), config)
            .await
            .unwrap_err()
            .to_string()
            .contains("exceeds its byte budget")
    );
    task.await.unwrap();
    let (url, task) = server(vec![
        b"HTTP/1.1 200 OK\r\nContent-Length: 1000\r\nConnection: close\r\n\r\n".to_vec(),
    ])
    .await;
    let result = f
        .send(
            Request::new(Method::HEAD, url.parse().unwrap()),
            HttpConfig::default(),
        )
        .await
        .unwrap();
    assert!(result.body.is_empty());
    task.await.unwrap();
}

#[tokio::test]
async fn cancellation_and_revocation_join_client_and_do_not_retry() {
    for revoke in [false, true] {
        let f = Fixture::new(true, serde_json::json!({}));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let token = CancellationToken::new();
        let request = Request::new(Method::GET, url.parse().unwrap());
        let result = send(
            &f.binding,
            &f.authority,
            request,
            HttpConfig::default(),
            token.clone(),
        );
        let interruption = async {
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut byte = [0];
            stream.read_exact(&mut byte).await.unwrap();
            if revoke {
                f.authority.disable("qa").unwrap();
            } else {
                token.cancel();
            }
            // Close the fixture's server to let the simulated remote curl terminate too.
            drop(stream);
        };
        let (result, ()) = tokio::time::timeout(Duration::from_secs(5), async {
            tokio::join!(result, interruption)
        })
        .await
        .unwrap();
        assert!(
            result
                .unwrap_err()
                .to_string()
                .contains("remote work may have run")
        );
    }
}

#[test]
fn input_escaping_and_header_framing_are_bounded() {
    assert_eq!(quoted("a\"\n\\"), "\"a\\\"\\n\\\\\"");
    let config = HttpConfig::default();
    let response = parse(
        b"HTTP/1.1 100 Continue\r\n\r\nHTTP/1.1 200 OK\r\nX-Test: a\r\nX-Test: b\r\n\r\n",
        vec![0, 255],
        config,
    )
    .unwrap();
    assert_eq!(response.headers.get_all("x-test").iter().count(), 2);
    for bytes in [
        b"HTTP/1.1 200 OK\r\nsecret".as_slice(),
        b"HTTP/1.1 101 Upgrade\r\n\r\n",
        b"HTTP/1.1 200 OK\r\n\r\nHTTP/1.1 200 OK\r\n\r\n",
    ] {
        assert!(parse(bytes, vec![], config).is_err());
    }
}

#[tokio::test]
async fn finite_spec_decodes_response_and_sends_scoped_auth_over_stdin() {
    use crate::credentials::{CredentialLimits, MemoryCredentials};
    use std::sync::Arc;
    use wes_engine::{
        credentials::SecretString,
        providers::{Call, Invoker},
        runtime::{Effect, ExecutionTraits, Runtime},
    };
    let f = Fixture::new(true, serde_json::json!({}));
    let (url, task) = server(vec![reply(
        "200 OK",
        "Content-Type: application/json\r\n",
        br#""decoded""#,
    )])
    .await;
    let credentials = Arc::new(MemoryCredentials::with_environment(
        CredentialLimits::default(),
        |_| Ok(Some(SecretString::from("synthetic-credential"))),
    ));
    let document = serde_json::json!({"version":1,"provider":"api","types":{},"operations":[{"path":["get"],"method":"GET","route":"/fixture","auth":[{"secret":"token","header":"X-Key","scheme":""}],"parameters":[],"responses":{"200":"Text"}}]});
    let reading = crate::descriptor::read_bound(
        &serde_json::to_vec(&document).unwrap(),
        None,
        credentials,
        HttpConfig::default(),
        wes_engine::imports::ImportMode::Replay,
        Some(&url),
    )
    .unwrap();
    let description = reading.description;
    let invoker = reading.invoker;
    let invoker =
        invoker.with_transport(Transport::bound(&f.binding, f.authority.clone()).unwrap());
    let mut runtime = Runtime::new();
    runtime
        .add(
            (),
            [],
            ExecutionTraits {
                pure: false,
                repeatable: false,
                bounded: true,
            },
        )
        .unwrap();
    let run = runtime
        .start(Duration::ZERO)
        .into_iter()
        .find_map(|e| {
            if let Effect::Spawn(ticket) = e {
                Some(ticket.run)
            } else {
                None
            }
        })
        .unwrap();
    let call = Call {
        run,
        authority: Default::default(),
        capability: description.capabilities().next().unwrap().clone(),
        arguments: Default::default(),
    };
    let result = invoker
        .invoke(call, CancellationToken::new())
        .await
        .unwrap();
    let wes_core::Data::Record(fields) = result.data() else {
        panic!()
    };
    assert_eq!(fields["body"], wes_core::Data::Text("decoded".into()));
    assert!(String::from_utf8_lossy(&task.await.unwrap()[0]).contains("synthetic-credential"));
    assert!(
        !std::fs::read_to_string(f.root.path().join("argv"))
            .unwrap()
            .contains("synthetic-credential")
    );
}

#[tokio::test]
async fn remote_binding_requires_explicit_transport_without_discovery_io() {
    let f = Fixture::new(true, serde_json::json!({}));
    assert!(!f.root.path().join("argv").exists());
    let yaml = serde_json::json!({"version":1,"targets":{"remote":{"kind":"ssh","client":f.root.path().join("ssh"),"host":"fixture.invalid","user":"qa","identity_file":f.root.path().join("key"),"known_hosts":f.root.path().join("hosts"),"shell":"posix","inherit":"remote"}},"environments":{"qa":{"imports":{"http":{"source":{"kind":"builtin","name":"http"},"bind":{"target":"remote"}}}}}});
    let loader = crate::environments::LocalEnvironments::new(f.root.path()).unwrap();
    for transport in [None, Some("internal")] {
        let mut yaml = yaml.clone();
        if let Some(t) = transport {
            yaml["environments"]["qa"]["imports"]["http"]["bind"]["transport"] = t.into();
        }
        let loaded = loader.capture_text(&yaml.to_string(), None, None).unwrap();
        let mut registry = Registry::default();
        let plan = registry
            .plan(
                &wes_core::environments::Package::parse(&loaded.yaml).unwrap(),
                &loaded.sources,
            )
            .unwrap();
        registry.apply(plan).unwrap();
        let binding = registry.inspect("qa").unwrap().bind("http").unwrap();
        let error = loader.build("http", &binding).err().unwrap();
        assert!(error.message.contains("curl"));
        assert!(!f.root.path().join("argv").exists());
    }
}

#[tokio::test]
async fn curl_refuses_untrusted_tls_even_with_ambient_curl_config() {
    use std::sync::Arc;
    use tokio_rustls::{
        TlsAcceptor,
        rustls::{ServerConfig, pki_types::PrivatePkcs8KeyDer},
    };
    let cert = rcgen::generate_simple_self_signed(vec!["127.0.0.1".into()]).unwrap();
    let server = ServerConfig::builder()
        .with_no_client_auth()
        .with_single_cert(
            vec![cert.cert.der().clone()],
            PrivatePkcs8KeyDer::from(cert.signing_key.serialize_der()).into(),
        )
        .unwrap();
    let acceptor = TlsAcceptor::from(Arc::new(server));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("https://{}", listener.local_addr().unwrap());
    let serving = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        assert!(acceptor.accept(stream).await.is_err());
    });
    let f = Fixture::new(false, serde_json::json!({}));
    let error = f
        .send(
            Request::new(Method::GET, url.parse().unwrap()),
            HttpConfig::default(),
        )
        .await
        .unwrap_err();
    let InvocationError::Failed(error) = error else {
        panic!("failure")
    };
    assert_eq!(error.code(), "HTTP003");
    serving.await.unwrap();
}

#[tokio::test]
async fn helper_supports_ssh_style_socket_stderr_and_binary_stdout() {
    use std::{os::fd::OwnedFd, process::Stdio};
    let (url, task) = server(vec![reply(
        "200 OK",
        "X-Fixture: socket\r\n",
        &[0, 255, 42],
    )])
    .await;
    let (child_socket, parent_socket) = std::os::unix::net::UnixStream::pair().unwrap();
    parent_socket.set_nonblocking(true).unwrap();
    let mut reader = tokio::net::UnixStream::from_std(parent_socket).unwrap();
    let mut command = tokio::process::Command::new("/bin/sh");
    command
        .args(["-c", SCRIPT, "wes-http", "empty", "128"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::from(OwnedFd::from(child_socket)));
    let mut child = crate::process::serialized_spawn_async(|| command.spawn())
        .await
        .unwrap();
    drop(command);
    let input = encode(
        &Request::new(Method::GET, url.parse().unwrap()),
        HttpConfig::default(),
    )
    .unwrap();
    let token = CancellationToken::new();
    let capture = crate::process::capture_input(
        &mut child,
        tokio::time::Instant::now() + Duration::from_secs(5),
        65536,
        &token,
        Some(input),
    );
    let stderr = async {
        let mut bytes = vec![];
        reader.read_to_end(&mut bytes).await.unwrap();
        bytes
    };
    let (captured, headers) = tokio::join!(capture, stderr);
    let (status, body, _) = captured.ok().unwrap();
    assert!(status.success());
    let response = parse(&headers, body, HttpConfig::default()).unwrap();
    assert_eq!(response.body, [0, 255, 42]);
    assert_eq!(response.headers["x-fixture"], "socket");
    task.await.unwrap();
}
