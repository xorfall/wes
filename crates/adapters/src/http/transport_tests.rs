//! Loopback TLS/H2 handshakes use isolated roots; no OS store/env is changed.
use super::*;
use crate::credentials::{CredentialLimits, MemoryCredentials};
use rcgen::{
    BasicConstraints, CertificateParams, ExtendedKeyUsagePurpose, IsCa, Issuer, KeyPair,
    KeyUsagePurpose,
};
use std::sync::atomic::{AtomicUsize, Ordering};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpListener,
    sync::mpsc,
    task::JoinHandle,
};
use tokio_rustls::{
    TlsAcceptor,
    rustls::{
        ServerConfig,
        pki_types::{CertificateDer, PrivatePkcs8KeyDer},
    },
};
use wes_core::Value;
use wes_engine::{
    credentials::SecretString,
    runtime::{Effect, ExecutionTraits, Runtime},
};

enum Behavior {
    Success,
    RefuseStream,
    Redirect(String),
}
struct Certificates {
    root: CertificateDer<'static>,
    leaf: CertificateDer<'static>,
    key: PrivatePkcs8KeyDer<'static>,
}
fn certificates(subject: &str, expired: bool) -> Certificates {
    let mut ca = CertificateParams::new(vec![]).unwrap();
    ca.is_ca = IsCa::Ca(BasicConstraints::Unconstrained);
    ca.key_usages = vec![
        KeyUsagePurpose::KeyCertSign,
        KeyUsagePurpose::CrlSign,
        KeyUsagePurpose::DigitalSignature,
    ];
    let ca_key = KeyPair::generate().unwrap();
    let root = ca.self_signed(&ca_key).unwrap();
    let issuer = Issuer::new(ca, ca_key);
    let mut leaf = CertificateParams::new(vec![subject.to_owned()]).unwrap();
    leaf.key_usages = vec![KeyUsagePurpose::DigitalSignature];
    leaf.extended_key_usages = vec![ExtendedKeyUsagePurpose::ServerAuth];
    if expired {
        leaf.not_before = rcgen::date_time_ymd(2000, 1, 1);
        leaf.not_after = rcgen::date_time_ymd(2001, 1, 1);
    }
    let key = KeyPair::generate().unwrap();
    let leaf = leaf.signed_by(&key, &issuer).unwrap();
    Certificates {
        root: root.der().clone(),
        leaf: leaf.der().clone(),
        key: PrivatePkcs8KeyDer::from(key.serialize_der()),
    }
}
struct TlsServer {
    base: String,
    root: CertificateDer<'static>,
    seen: mpsc::Receiver<String>,
    requests: Arc<AtomicUsize>,
    resets: Arc<AtomicUsize>,
    stop: CancellationToken,
    task: Option<JoinHandle<()>>,
}
impl TlsServer {
    async fn start(subject: &str, expired: bool, h2: bool, behavior: Behavior) -> Self {
        let Certificates { root, leaf, key } = certificates(subject, expired);
        let mut config = ServerConfig::builder()
            .with_no_client_auth()
            .with_single_cert(vec![leaf], key.into())
            .unwrap();
        config.alpn_protocols = vec![if h2 {
            b"h2".to_vec()
        } else {
            b"http/1.1".to_vec()
        }];
        let acceptor = TlsAcceptor::from(Arc::new(config));
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("https://{}", listener.local_addr().unwrap());
        let (send, seen) = mpsc::channel(32);
        let requests = Arc::new(AtomicUsize::new(0));
        let count = requests.clone();
        let resets = Arc::new(AtomicUsize::new(0));
        let reset_count = resets.clone();
        let stop = CancellationToken::new();
        let stopping = stop.clone();
        let task = tokio::spawn(async move {
            let serving = async move {
                loop {
                    let (tcp, _) = listener.accept().await.unwrap();
                    let Ok(mut tls) = acceptor.accept(tcp).await else {
                        continue;
                    };
                    if h2 {
                        assert_eq!(tls.get_ref().1.alpn_protocol(), Some(b"h2".as_slice()));
                        let Ok(mut connection) = h2::server::handshake(tls).await else {
                            continue;
                        };
                        while let Some(Ok((request, mut response))) = connection.accept().await {
                            count.fetch_add(1, Ordering::SeqCst);
                            send.send(format!(
                                "{} {} {:?}",
                                request.method(),
                                request.uri().path(),
                                request.headers()
                            ))
                            .await
                            .unwrap();
                            match &behavior {
                                Behavior::RefuseStream => {
                                    response.send_reset(h2::Reason::REFUSED_STREAM);
                                    reset_count.fetch_add(1, Ordering::SeqCst);
                                }
                                Behavior::Success => {
                                    let head = http::Response::builder()
                                        .status(200)
                                        .header("content-type", "application/json")
                                        .body(())
                                        .unwrap();
                                    response
                                        .send_response(head, false)
                                        .unwrap()
                                        .send_data(b"true".to_vec().into(), true)
                                        .unwrap();
                                }
                                Behavior::Redirect(location) => {
                                    let head = http::Response::builder()
                                        .status(307)
                                        .header("location", location)
                                        .body(())
                                        .unwrap();
                                    response.send_response(head, true).unwrap();
                                }
                            }
                        }
                    } else {
                        let mut received = Vec::new();
                        loop {
                            let mut block = [0u8; 1024];
                            let n = tls.read(&mut block).await.unwrap();
                            if n == 0 {
                                break;
                            }
                            received.extend_from_slice(&block[..n]);
                            assert!(received.len() < 16 * 1024);
                            if received.windows(4).any(|w| w == b"\r\n\r\n") {
                                break;
                            }
                        }
                        if !received.is_empty() {
                            count.fetch_add(1, Ordering::SeqCst);
                            send.send(String::from_utf8(received).unwrap())
                                .await
                                .unwrap();
                            tls.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 4\r\nConnection: close\r\n\r\ntrue").await.unwrap();
                            tls.flush().await.unwrap();
                        }
                    }
                }
            };
            tokio::select! { _=stopping.cancelled()=>{}, _=tokio::time::timeout(Duration::from_secs(5),serving)=>{} }
        });
        Self {
            base,
            root,
            seen,
            requests,
            resets,
            stop,
            task: Some(task),
        }
    }
    async fn finish(mut self) {
        self.stop.cancel();
        self.task.take().unwrap().await.unwrap();
    }
}
impl Drop for TlsServer {
    fn drop(&mut self) {
        self.stop.cancel();
    }
}
fn client(server: &TlsServer, trust: bool) -> (Arc<Capability>, HttpInvoker) {
    let credentials = Arc::new(MemoryCredentials::with_environment(
        CredentialLimits::default(),
        |_| Ok(Some(SecretString::from("fixture-private"))),
    ));
    let document = serde_json::json!({"version":1,"provider":"fixture","types":{},"operations":[{"path":["items"],"method":"GET","route":"/items","auth":[{"secret":"token","header":"X-Key","scheme":""}],"parameters":[],"responses":{"200":"Unknown"}}]});
    let reading = crate::descriptor::read_bound(
        &serde_json::to_vec(&document).unwrap(),
        None,
        credentials,
        HttpConfig::default(),
        wes_engine::imports::ImportMode::Replay,
        Some(&server.base),
    )
    .unwrap();
    let description = reading.description;
    let mut invoker = reading.invoker;
    // Keep production request/retry/redirect policy, but isolate the trust source.
    // Merging roots invokes the platform verifier (SecTrust on macOS), whose
    // synchronous evaluation can stall the test runtime before HTTP is reached.
    // Certificate signatures, hostnames and expiry are still verified. An unrelated
    // ephemeral CA exercises unknown-authority rejection without the OS trust store.
    let root = if trust {
        server.root.clone()
    } else {
        certificates("unrelated.example", false).root
    };
    let inner = Arc::get_mut(&mut invoker.inner).unwrap();
    inner.client = client_builder(inner.config)
        .tls_certs_only([reqwest::Certificate::from_der(root.as_ref()).unwrap()])
        .build()
        .unwrap();
    (
        description.capability(&["items".into()]).unwrap().clone(),
        invoker,
    )
}
async fn invoke(
    server: &TlsServer,
    capability: Arc<Capability>,
    invoker: HttpInvoker,
) -> Result<Value, InvocationError> {
    let mut runtime = Runtime::new();
    runtime
        .add(
            (),
            [],
            ExecutionTraits {
                pure: false,
                bounded: true,
                repeatable: true,
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
    tokio::time::timeout(
        Duration::from_secs(3),
        invoker.invoke(
            Call {
                authority: Default::default(),
                run,
                capability,
                arguments: IndexMap::new(),
            },
            CancellationToken::new(),
        ),
    )
    .await
    .unwrap_or_else(|_| {
        panic!(
            "TLS fixture invocation exceeded 3s: requests={}, resets={}, server_finished={}",
            server.requests.load(Ordering::SeqCst),
            server.resets.load(Ordering::SeqCst),
            server.task.as_ref().unwrap().is_finished(),
        )
    })
}

#[tokio::test]
async fn unknown_authority_wrong_hostname_and_expired_certificates_are_rejected_before_http() {
    for (subject, expired, trust) in [
        ("127.0.0.1", false, false),
        ("wrong.example", false, true),
        ("127.0.0.1", true, true),
    ] {
        let server = TlsServer::start(subject, expired, false, Behavior::Success).await;
        let (cap, invoker) = client(&server, trust);
        let Err(InvocationError::Failed(error)) = invoke(&server, cap, invoker).await else {
            panic!("TLS verification must fail")
        };
        assert_eq!(error.code(), "HTTP003");
        assert!(!format!("{error:?}").contains("fixture-private"));
        assert_eq!(server.requests.load(Ordering::SeqCst), 0);
        server.finish().await;
    }
}

#[tokio::test]
async fn verified_tls_supports_http1_and_alpn_negotiated_http2_with_authentication() {
    for h2 in [false, true] {
        let mut server = TlsServer::start("127.0.0.1", false, h2, Behavior::Success).await;
        let (cap, invoker) = client(&server, true);
        assert!(invoke(&server, cap, invoker).await.is_ok());
        let request = server.seen.recv().await.unwrap();
        assert!(request.contains("/items"));
        assert!(request.contains("fixture-private"));
        assert_eq!(server.requests.load(Ordering::SeqCst), 1);
        server.finish().await;
    }
}

#[tokio::test]
async fn http2_refused_stream_is_not_retried_even_for_a_safe_get() {
    let mut server = TlsServer::start("127.0.0.1", false, true, Behavior::RefuseStream).await;
    let (cap, invoker) = client(&server, true);
    let Err(InvocationError::Failed(error)) = invoke(&server, cap, invoker).await else {
        panic!("refused stream must fail")
    };
    assert_eq!(error.code(), "HTTP003");
    assert!(!format!("{error:?}").contains("fixture-private"));
    assert_eq!(server.requests.load(Ordering::SeqCst), 1);
    assert_eq!(server.resets.load(Ordering::SeqCst), 1);
    let request = server
        .seen
        .try_recv()
        .expect("the rejected request was observed");
    assert!(request.starts_with("GET /items "));
    assert!(request.contains("fixture-private"));
    server.finish().await;
}

#[tokio::test]
async fn https_redirect_cannot_downgrade_to_plain_http() {
    let target = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let server = TlsServer::start(
        "127.0.0.1",
        false,
        true,
        Behavior::Redirect(format!("http://{}/stolen", target.local_addr().unwrap())),
    )
    .await;
    let (cap, invoker) = client(&server, true);
    let Err(InvocationError::Failed(error)) = invoke(&server, cap, invoker).await else {
        panic!("downgrade must fail")
    };
    assert_eq!(error.code(), "HTTP005");
    assert!(
        tokio::time::timeout(Duration::from_millis(25), target.accept())
            .await
            .is_err()
    );
    server.finish().await;
}
