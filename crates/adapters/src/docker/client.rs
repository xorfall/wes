//! Shared inert transport construction. Negotiation happens only inside an admitted invocation.
use serde_json::Value;
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};
use std::time::Duration;
use tokio::sync::Mutex;

pub(super) const METADATA: usize = 1024 * 1024;
#[derive(Clone)]
pub(super) struct DockerEngineClient {
    pub http: reqwest::Client,
    negotiated: Arc<AtomicBool>,
    negotiation: Arc<Mutex<()>>,
}
#[derive(Debug, thiserror::Error)]
pub(super) enum ClientError {
    #[error(
        "Cannot reach the selected Docker socket; check that Docker is running and the socket path and access permissions are correct. No other connection was tried"
    )]
    Transport,
    #[error("Docker daemon returned HTTP {0}")]
    Status(u16),
    #[error("Docker metadata is malformed or exceeds 1 MiB")]
    Metadata,
    #[error("Docker API ranges do not overlap: client 1.45..1.45, daemon {0}..{1}")]
    Version(String, String),
}
impl DockerEngineClient {
    pub fn new(socket: &str) -> Result<Self, &'static str> {
        let endpoint = super::LocalEndpoint::from_socket(socket)?;
        #[cfg(not(any(unix, windows)))]
        {
            let _ = endpoint;
            Err("Docker local transport is unavailable on this platform")
        }
        #[cfg(any(unix, windows))]
        {
            // Each host reaches the daemon through its own kind of local endpoint only.
            #[cfg(unix)]
            let builder = match endpoint {
                super::LocalEndpoint::Socket(path) => reqwest::Client::builder().unix_socket(path),
                super::LocalEndpoint::Pipe(_) => {
                    return Err("Docker named pipes are a Windows transport");
                }
            };
            #[cfg(windows)]
            let builder = match endpoint {
                super::LocalEndpoint::Pipe(name) => {
                    reqwest::Client::builder().windows_named_pipe(format!(r"\\.\pipe\{name}"))
                }
                super::LocalEndpoint::Socket(_) => {
                    return Err(
                        "Docker on Windows requires a named pipe such as //./pipe/docker_engine",
                    );
                }
            };
            let http = builder
                .no_proxy()
                .redirect(reqwest::redirect::Policy::none())
                .retry(reqwest::retry::never())
                .connect_timeout(Duration::from_secs(5))
                .pool_max_idle_per_host(0)
                .build()
                .map_err(|_| "Docker transport configuration failed")?;
            Ok(Self {
                http,
                negotiated: Arc::new(AtomicBool::new(false)),
                negotiation: Arc::new(Mutex::new(())),
            })
        }
    }
    pub fn invalidate(&self) {
        self.negotiated.store(false, Ordering::Release);
    }
    pub async fn endpoint(&self) -> Result<&'static str, ClientError> {
        if !self.negotiated.load(Ordering::Acquire) {
            let _guard = self.negotiation.lock().await;
            if !self.negotiated.load(Ordering::Acquire) {
                let version = self.json(self.http.get("http://localhost/version")).await?;
                let min = version
                    .get("MinAPIVersion")
                    .and_then(Value::as_str)
                    .ok_or(ClientError::Metadata)?;
                let max = version
                    .get("ApiVersion")
                    .and_then(Value::as_str)
                    .ok_or(ClientError::Metadata)?;
                let lower = parse_version(min).ok_or(ClientError::Metadata)?;
                let upper = parse_version(max).ok_or(ClientError::Metadata)?;
                if lower > upper {
                    return Err(ClientError::Metadata);
                }
                if lower > (1, 45) || upper < (1, 45) {
                    return Err(ClientError::Version(min.into(), max.into()));
                }
                self.negotiated.store(true, Ordering::Release);
            }
        }
        Ok("http://localhost/v1.45")
    }
    pub async fn response(
        &self,
        request: reqwest::RequestBuilder,
    ) -> Result<reqwest::Response, ClientError> {
        let response = request.send().await.map_err(|_| {
            self.invalidate();
            ClientError::Transport
        })?;
        if !response.status().is_success() {
            if response.status().as_u16() == 400 {
                self.invalidate();
            }
            return Err(ClientError::Status(response.status().as_u16()));
        }
        Ok(response)
    }
    pub async fn json(&self, request: reqwest::RequestBuilder) -> Result<Value, ClientError> {
        let response = self.response(request).await?;
        let body = super::bounded(response, METADATA).await.map_err(|_| {
            self.invalidate();
            ClientError::Metadata
        })?;
        serde_json::from_slice(&body).map_err(|_| {
            self.invalidate();
            ClientError::Metadata
        })
    }
}
fn parse_version(text: &str) -> Option<(u16, u16)> {
    if text.len() > 11 {
        return None;
    }
    let (major, minor) = text.split_once('.')?;
    if major.is_empty()
        || minor.is_empty()
        || !major
            .bytes()
            .chain(minor.bytes())
            .all(|b| b.is_ascii_digit())
    {
        return None;
    }
    Some((major.parse().ok()?, minor.parse().ok()?))
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn version_is_an_integer_pair_not_decimal_or_lexical() {
        assert!(parse_version("1.100") > parse_version("1.45"));
        for invalid in ["1", "1.45.0", "v1.45", "1.-1", "", "99999999999.1"] {
            assert!(parse_version(invalid).is_none());
        }
    }
}

#[cfg(all(test, unix))]
mod cancellation_tests {
    use super::*;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    #[tokio::test]
    async fn abandoned_negotiation_releases_waiter_without_caching_success() {
        let root = tempfile::Builder::new()
            .prefix("dver-")
            .tempdir_in("/tmp")
            .unwrap();
        let socket = root.path().join("d.sock");
        let listener = tokio::net::UnixListener::bind(&socket).unwrap();
        let client = DockerEngineClient::new(socket.to_str().unwrap()).unwrap();
        let first = {
            let client = client.clone();
            tokio::spawn(async move { client.endpoint().await })
        };
        let (mut stalled, _) = listener.accept().await.unwrap();
        let mut buf = [0; 1024];
        assert!(stalled.read(&mut buf).await.unwrap() > 0);
        first.abort();
        assert!(first.await.unwrap_err().is_cancelled());
        drop(stalled);
        let server = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut header = vec![];
            while !header.ends_with(b"\r\n\r\n") {
                let mut b = [0];
                stream.read_exact(&mut b).await.unwrap();
                header.push(b[0]);
            }
            let body = r#"{"ApiVersion":"1.47","MinAPIVersion":"1.24"}"#;
            stream
                .write_all(
                    format!(
                        "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                        body.len()
                    )
                    .as_bytes(),
                )
                .await
                .unwrap();
        });
        assert_eq!(
            tokio::time::timeout(Duration::from_secs(2), client.endpoint())
                .await
                .unwrap()
                .unwrap(),
            "http://localhost/v1.45"
        );
        server.await.unwrap();
        // Cached success needs no surviving socket listener.
        assert!(client.endpoint().await.is_ok());
    }
}

#[cfg(all(test, windows))]
mod pipe_tests {
    use super::*;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::windows::named_pipe::ServerOptions;
    #[tokio::test]
    async fn negotiation_reaches_the_engine_over_a_named_pipe() {
        let name = format!("wes-test-{}", uuid::Uuid::new_v4());
        let mut server = ServerOptions::new()
            .first_pipe_instance(true)
            .create(format!(r"\\.\pipe\{name}"))
            .unwrap();
        let serving = tokio::spawn(async move {
            server.connect().await.unwrap();
            let mut header = vec![];
            while !header.ends_with(b"\r\n\r\n") {
                let mut b = [0];
                server.read_exact(&mut b).await.unwrap();
                header.push(b[0]);
            }
            assert!(header.starts_with(b"GET /version "));
            let body = r#"{"ApiVersion":"1.47","MinAPIVersion":"1.24"}"#;
            server
                .write_all(
                    format!(
                        "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                        body.len()
                    )
                    .as_bytes(),
                )
                .await
                .unwrap();
            // Closing a pipe discards unread bytes; wait for the client to finish and hang up.
            let _ = server.read(&mut [0]).await;
        });
        let client = DockerEngineClient::new(&format!("//./pipe/{name}")).unwrap();
        assert_eq!(
            tokio::time::timeout(Duration::from_secs(5), client.endpoint())
                .await
                .unwrap()
                .unwrap(),
            "http://localhost/v1.45"
        );
        serving.await.unwrap();
        for unsupported in ["/var/run/docker.sock", "//./pipe/", "//./pipe/a/b"] {
            assert!(DockerEngineClient::new(unsupported).is_err());
        }
    }
}
