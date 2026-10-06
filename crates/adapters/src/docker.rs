//! Docker Engine API 1.45: finite exec, observation and scoped lifecycle. No CLI or build-time probes.
//! Cancellation owns the local connection, not the remote process. Never imply remote rollback.
use serde_json::{Value as Json, json};
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};
use wes_core::{
    Data, Provenance, Value,
    capability::ProviderDescription,
    environments::{Binding, TargetKind},
};
use wes_engine::{
    driver::CancellationToken,
    imports::ImportProduct,
    providers::{Call, InvocationError, InvocationFuture, Invoker},
    runtime::RuntimeCode,
};

pub(crate) mod automatic;
mod client;
pub use automatic::{local as automatic_local, with_candidates as automatic_with_candidates};
mod endpoint;
pub(crate) use endpoint::LocalEndpoint;
mod importer;
pub use importer::{DockerImporter, unconnected};
mod destination;
mod terminal;
pub(crate) use terminal::plan as terminal;
pub(crate) mod observation;
use client::DockerEngineClient;
const OUTPUT: usize = 32 * 1024 * 1024;
const METADATA: usize = 1024 * 1024;

#[derive(Clone)]
struct DockerExec {
    client: DockerEngineClient,
    binding: Binding,
    authority: wes_engine::environments::Authority,
}

pub(crate) fn build(
    alias: &str,
    binding: &Binding,
    authority: wes_engine::environments::Authority,
) -> Result<ImportProduct, &'static str> {
    if binding
        .import()
        .timeout_ms()
        .is_some_and(|ms| ms > 3_600_000)
    {
        return Err("Docker timeout must not exceed one hour");
    }
    let TargetKind::Docker { socket, .. } = binding.import().target().kind() else {
        return Err("not a Docker target");
    };
    if binding.import().endpoint().is_some() || !binding.import().credential_refs().is_empty() {
        return Err(
            "Docker exec endpoint override and secret delivery are unsupported; credentials are never placed in exec metadata",
        );
    }
    let program = crate::execution_targets::command::Program::capture(binding)?;
    if binding
        .import()
        .target()
        .cwd()
        .is_some_and(|s| !s.starts_with('/'))
    {
        return Err("Docker executable and cwd must be absolute container paths");
    }
    let client = DockerEngineClient::new(socket)?;
    let run = program.capability();
    let description =
        ProviderDescription::new(alias, [run], vec![]).map_err(|_| "invalid Docker alias")?;
    ImportProduct::new(description, Arc::new(DockerExec { client, binding: binding.clone(), authority }), vec![
        "Docker exec inherits the container's environment plus declared overrides, never the host environment. No TTY/stdin/secret delivery. Disconnect may leave remote work running; image identity is not a filesystem snapshot.".into()
    ]).map_err(|_| "Docker metadata exceeds budget")
}

impl Invoker for DockerExec {
    fn invoke(&self, call: Call, token: CancellationToken) -> InvocationFuture {
        let this = self.clone();
        Box::pin(async move {
            let (argv, timeout) =
                crate::execution_targets::command::Program::capture(&this.binding)
                    .and_then(|program| program.prepare(&this.binding, &call))
                    .map_err(failed)?;
            let started = AtomicBool::new(false);
            let authority = this
                .authority
                .availability_lease(this.binding.environment().identity())
                .map_err(|_| failed("ENV020: environment execution authority is disabled"))?;
            tokio::select! {
                biased;
                () = token.cancelled() => if started.load(Ordering::Acquire) { Err(unknown()) } else { Err(InvocationError::Cancelled) },
                () = authority.cancelled() => if started.load(Ordering::Acquire) { Err(unknown()) } else { Err(failed("ENV020: environment execution authority ended before start")) },
                result = tokio::time::timeout(timeout, this.run(argv, None, OUTPUT, &started, &authority)) => result.unwrap_or_else(|_| Err(if started.load(Ordering::Acquire) { unknown() } else { failed("ENV031: Docker transport timed out before start") })),
            }
        })
    }
}
impl DockerExec {
    async fn metadata(
        &self,
        request: reqwest::RequestBuilder,
        uncertain: bool,
    ) -> Result<Json, InvocationError> {
        let response = request.send().await.map_err(|_| {
            self.client.invalidate();
            if uncertain {
                unknown()
            } else {
                failed("ENV031: Docker transport unavailable before start")
            }
        })?;
        if !response.status().is_success() {
            if response.status() == reqwest::StatusCode::BAD_REQUEST {
                self.client.invalidate();
            }
            // Daemon error bodies may contain argv, filesystem paths or private service details.
            return Err(if uncertain {
                unknown()
            } else if response.status() == reqwest::StatusCode::NOT_FOUND {
                failed("ENV032: Docker container or exec instance is unavailable")
            } else {
                failed("ENV033: Docker daemon rejected the request before exec start")
            });
        }
        let bytes = bounded(response, METADATA).await.map_err(|_| {
            self.client.invalidate();
            if uncertain {
                unknown()
            } else {
                failed("ENV034: invalid Docker metadata response")
            }
        })?;
        serde_json::from_slice(&bytes).map_err(|_| {
            if uncertain {
                unknown()
            } else {
                failed("ENV034: invalid Docker metadata response")
            }
        })
    }
    async fn run(
        &self,
        argv: Vec<String>,
        input: Option<Vec<u8>>,
        output_limit: usize,
        started: &AtomicBool,
        authority: &CancellationToken,
    ) -> Result<Value, InvocationError> {
        let api = self
            .client
            .endpoint()
            .await
            .map_err(|error| failed(&error.to_string()))?;
        let import = self.binding.import();
        let target = import.target();
        let resolved = destination::resolve(&self.client, api, target)
            .await
            .map_err(|e| failed(&e))?;
        let id = resolved.id.as_str();
        let actual_image = resolved.image.as_str();
        let config = json!({"AttachStdin":input.is_some(),"AttachStdout":true,"AttachStderr":true,"Tty":false,"Privileged":false,
            "Cmd":argv,"WorkingDir":target.cwd().unwrap_or("/"),"Env":target.variables().iter().map(|(k,v)| format!("{k}={v}")).collect::<Vec<_>>()});
        let created = self
            .metadata(
                self.client
                    .http
                    .post(format!("{api}/containers/{id}/exec"))
                    .header("content-type", "application/json")
                    .body(config.to_string()),
                false,
            )
            .await?;
        let exec = created
            .get("Id")
            .and_then(Json::as_str)
            .filter(|s| digest(s))
            .ok_or_else(|| failed("ENV034: invalid Docker exec identity"))?;
        // From this point onward, a disconnect cannot prove whether the daemon started the child.
        if authority.is_cancelled() {
            return Err(failed(
                "ENV020: environment execution authority is disabled",
            ));
        }
        started.store(true, Ordering::Release);
        let mut request = self
            .client
            .http
            .post(format!("{api}/exec/{exec}/start"))
            .header("content-type", "application/json")
            .body(r#"{"Detach":false,"Tty":false}"#);
        if input.is_some() {
            request = request
                .header("connection", "Upgrade")
                .header("upgrade", "tcp");
        }
        let response = request.send().await.map_err(|_| {
            self.client.invalidate();
            unknown()
        })?;
        let (stdout, stderr) = if let Some(input) = input {
            if response.status() != reqwest::StatusCode::SWITCHING_PROTOCOLS {
                return Err(unknown());
            }
            let stream = response.upgrade().await.map_err(|_| unknown())?;
            attached(stream, input, output_limit).await?
        } else {
            if response.status() != reqwest::StatusCode::OK {
                return Err(unknown());
            }
            let bytes = bounded(response, output_limit).await.map_err(|_| {
                self.client.invalidate();
                unknown()
            })?;
            demultiplex(&bytes).ok_or_else(unknown)?
        };
        let status = self
            .metadata(
                self.client.http.get(format!("{api}/exec/{exec}/json")),
                true,
            )
            .await?;
        if status.get("Running").and_then(Json::as_bool) != Some(false)
            || status.get("ID").and_then(Json::as_str) != Some(exec)
            || status.get("ContainerID").and_then(Json::as_str) != Some(id)
        {
            return Err(unknown());
        }
        let code = status
            .get("ExitCode")
            .and_then(Json::as_i64)
            .ok_or_else(unknown)?;
        let mut provenance = Provenance::default()
            .with_fact("docker.container", id)
            .with_fact("docker.image", actual_image);
        if let Some(selector) = resolved.selector.as_deref() {
            provenance = provenance.with_fact("docker.compose", selector);
        }
        Value::new(
            crate::process::output_shape(),
            Data::Record(indexmap::IndexMap::from_iter([
                ("exitCode".into(), Data::Int(code)),
                ("stdout".into(), Data::Bytes(stdout.into())),
                ("stderr".into(), Data::Bytes(stderr.into())),
            ])),
            provenance.cautioned([
                "Observed image identity excludes writable layers, mounts and external services."
                    .into(),
            ]),
        )
        .map_err(|_| failed("ENV034: invalid Docker output"))
    }
}
pub(crate) async fn execute(
    binding: Binding,
    authority: wes_engine::environments::Authority,
    argv: Vec<String>,
    input: Option<Vec<u8>>,
    timeout: std::time::Duration,
    output_limit: usize,
    token: CancellationToken,
) -> Result<Value, InvocationError> {
    let TargetKind::Docker { socket, .. } = binding.import().target().kind() else {
        return Err(failed("Expected Docker target"));
    };
    let this = DockerExec {
        client: DockerEngineClient::new(socket).map_err(failed)?,
        binding: binding.clone(),
        authority: authority.clone(),
    };
    let lease = authority
        .availability_lease(binding.environment().identity())
        .map_err(|_| failed("ENV020: environment execution authority is disabled"))?;
    let started = AtomicBool::new(false);
    tokio::select! { biased;
        () = token.cancelled() => if started.load(Ordering::Acquire) { Err(unknown()) } else { Err(InvocationError::Cancelled) },
        () = lease.cancelled() => if started.load(Ordering::Acquire) { Err(unknown()) } else { Err(failed("ENV020: environment execution authority ended before start")) },
        result = tokio::time::timeout(timeout, this.run(argv, input, output_limit, &started, &lease)) => result.unwrap_or_else(|_| Err(if started.load(Ordering::Acquire) { unknown() } else { failed("ENV031: Docker transport timed out before start") })),
    }
}

async fn attached(
    stream: reqwest::Upgraded,
    input: Vec<u8>,
    limit: usize,
) -> Result<(Vec<u8>, Vec<u8>), InvocationError> {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let (mut reader, mut writer) = tokio::io::split(stream);
    let send = async {
        writer.write_all(&input).await.map_err(|_| unknown())?;
        writer.shutdown().await.map_err(|_| unknown())
    };
    let receive = async {
        let (mut stdout, mut stderr) = (vec![], vec![]);
        loop {
            let mut header = [0u8; 8];
            let n = reader.read(&mut header[..1]).await.map_err(|_| unknown())?;
            if n == 0 {
                return Ok((stdout, stderr));
            }
            reader
                .read_exact(&mut header[1..])
                .await
                .map_err(|_| unknown())?;
            if header[1..4] != [0, 0, 0] || !matches!(header[0], 1 | 2) {
                return Err(unknown());
            }
            let len = u32::from_be_bytes(header[4..8].try_into().expect("four")) as usize;
            if len
                > limit
                    .saturating_sub(stdout.len())
                    .saturating_sub(stderr.len())
            {
                return Err(unknown());
            }
            let output = if header[0] == 1 {
                &mut stdout
            } else {
                &mut stderr
            };
            let start = output.len();
            output.resize(start + len, 0);
            reader
                .read_exact(&mut output[start..])
                .await
                .map_err(|_| unknown())?;
        }
    };
    let (_, output) = tokio::try_join!(send, receive)?;
    Ok(output)
}

fn digest(s: &str) -> bool {
    s.len() == 64 && s.bytes().all(|b| b.is_ascii_hexdigit())
}
async fn bounded(mut response: reqwest::Response, limit: usize) -> Result<Vec<u8>, ()> {
    if response.content_length().is_some_and(|n| n > limit as u64) {
        return Err(());
    }
    let mut bytes = vec![];
    while let Some(chunk) = response.chunk().await.map_err(|_| ())? {
        if chunk.len() > limit.saturating_sub(bytes.len()) {
            return Err(());
        }
        bytes.extend_from_slice(&chunk);
    }
    Ok(bytes)
}
fn demultiplex(mut bytes: &[u8]) -> Option<(Vec<u8>, Vec<u8>)> {
    let (mut stdout, mut stderr) = (vec![], vec![]);
    while !bytes.is_empty() {
        let header = bytes.get(..8)?;
        if header[1..4] != [0, 0, 0] {
            return None;
        }
        let length = u32::from_be_bytes(header[4..8].try_into().ok()?) as usize;
        let body = bytes.get(8..8usize.checked_add(length)?)?;
        match header[0] {
            1 => stdout.extend_from_slice(body),
            2 => stderr.extend_from_slice(body),
            _ => return None,
        }
        bytes = &bytes[8 + length..];
    }
    Some((stdout, stderr))
}
fn failed(message: &str) -> InvocationError {
    InvocationError::Failed(RuntimeCode::ExecutionFailed.error(message, None))
}
fn unknown() -> InvocationError {
    let error = RuntimeCode::ExecutionFailed.error(
        "ENV036: Docker exec outcome is unknown; remote work may still be running. No automatic retry or cleanup is claimed.",
        None,
    );
    InvocationError::Failed(
        wes_core::ErrorValue::new(
            error.id().clone(),
            wes_core::ErrorValue::REMOTE_OUTCOME_UNKNOWN,
            error.message(),
            vec![],
            None,
        )
        .expect("constant uncertainty code"),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;
    #[test]
    fn framing_requires_complete_non_tty_streams() {
        assert_eq!(
            demultiplex(&[1, 0, 0, 0, 0, 0, 0, 1, b'a', 2, 0, 0, 0, 0, 0, 0, 1, b'b']),
            Some((b"a".to_vec(), b"b".to_vec()))
        );
        for bad in [
            &[1, 0, 0][..],
            &[1, 0, 0, 0, 0, 0, 0, 2, b'a'],
            &[3, 0, 0, 0, 0, 0, 0, 0],
            &[1, 1, 0, 0, 0, 0, 0, 0],
        ] {
            assert!(demultiplex(bad).is_none());
        }
    }
    #[cfg(unix)]
    async fn fake(mode: u8) -> (Result<Value, InvocationError>, Vec<String>) {
        use tokio::{
            io::{AsyncReadExt, AsyncWriteExt},
            net::UnixListener,
        };
        use wes_core::environments::{CapturedSource, CapturedSources, Package};
        use wes_engine::{
            environments::Registry,
            runtime::{Effect, ExecutionTraits, Runtime},
        };
        let root = tempfile::Builder::new()
            .prefix("wes-docker-")
            .tempdir_in("/tmp")
            .unwrap();
        let socket = root.path().join("api.sock");
        let listener = UnixListener::bind(&socket).unwrap();
        let id = "a".repeat(64);
        let image = format!("sha256:{}", "b".repeat(64));
        let exec = "c".repeat(64);
        let constraint = if mode == 2 {
            format!(", image: 'sha256:{}'", "d".repeat(64))
        } else {
            String::new()
        };
        let yaml = format!(
            "version: 1\ntargets: {{remote: {{kind: docker, socket: '{}', container: synthetic, inherit: container, cwd: /remote-only, env: {{QA_ONLY: remote}}{constraint}}}}}\nenvironments: {{dev: {{imports: {{tool: {{source: {{kind: process, bin: /not/on/host/tool}}, bind: {{target: remote}}}}}}}}}}",
            socket.display()
        );
        let yaml = if mode == 7 {
            yaml.replace(
                "kind: process, bin: /not/on/host/tool",
                "kind: builtin, name: sh",
            )
        } else {
            yaml
        };
        let package = Package::parse(&yaml).unwrap();
        let mut sources = CapturedSources::default();
        for key in package.required_sources() {
            sources
                .insert(
                    key,
                    CapturedSource::new(
                        if mode == 7 {
                            "builtin/v1"
                        } else {
                            "process/target/v1"
                        },
                        if mode == 7 { "sh" } else { "/not/on/host/tool" },
                    )
                    .unwrap(),
                )
                .unwrap();
        }
        let mut registry = Registry::default();
        let plan = registry.plan(&package, &sources).unwrap();
        registry.apply(plan).unwrap();
        let authority = wes_engine::environments::Authority::default();
        let token = CancellationToken::new();
        let cancelling = token.clone();
        let product = build(
            "tool",
            &registry.inspect("dev").unwrap().bind("tool").unwrap(),
            authority.clone(),
        )
        .unwrap();
        let serving = tokio::spawn(async move {
            let (mut negotiation, _) = listener.accept().await.unwrap();
            let mut request = vec![];
            while !request.ends_with(b"\r\n\r\n") {
                let mut byte = [0];
                negotiation.read_exact(&mut byte).await.unwrap();
                request.push(byte[0]);
            }
            assert!(request.starts_with(b"GET /version "));
            let body = r#"{"ApiVersion":"1.47","MinAPIVersion":"1.24"}"#;
            negotiation
                .write_all(
                    format!(
                        "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                        body.len()
                    )
                    .as_bytes(),
                )
                .await
                .unwrap();
            drop(negotiation);
            let mut requests = vec![];
            let steps = if mode == 2 || mode == 4 {
                1
            } else if mode == 6 {
                2
            } else if matches!(mode, 1 | 3 | 5) {
                3
            } else {
                4
            };
            for step in 0..steps {
                let (mut stream, _) = listener.accept().await.unwrap();
                let mut request = vec![];
                let end = loop {
                    let mut byte = [0];
                    stream.read_exact(&mut byte).await.unwrap();
                    request.push(byte[0]);
                    assert!(request.len() < 64 * 1024);
                    if request.ends_with(b"\r\n\r\n") {
                        break request.len();
                    }
                };
                let header = String::from_utf8(request.clone()).unwrap();
                let length = header
                    .lines()
                    .find_map(|l| {
                        l.to_ascii_lowercase()
                            .strip_prefix("content-length:")
                            .map(str::trim)
                            .map(str::to_owned)
                    })
                    .map(|s| s.parse::<usize>().unwrap())
                    .unwrap_or(0);
                assert!(length < 1024 * 1024);
                request.resize(end + length, 0);
                stream.read_exact(&mut request[end..]).await.unwrap();
                requests.push(String::from_utf8(request).unwrap());
                if step == 2 && mode == 1 {
                    break;
                }
                if step == 2 && mode == 5 {
                    cancelling.cancel();
                    break;
                }
                if step == 1 && mode == 6 {
                    authority.disable("dev").unwrap();
                }
                let body = match step {
                    0 => json!({"Id":id,"Image":image,"State":{"Running":mode != 4}})
                        .to_string()
                        .into_bytes(),
                    1 => json!({"Id":exec}).to_string().into_bytes(),
                    2 => vec![
                        1, 0, 0, 0, 0, 0, 0, 2, b'o', b'k', 2, 0, 0, 0, 0, 0, 0, 1, b'e',
                    ],
                    _ => json!({"ID":exec,"ContainerID":id,"Running":false,"ExitCode":17})
                        .to_string()
                        .into_bytes(),
                };
                if step == 2 && mode == 8 {
                    stream.write_all(b"HTTP/1.1 101 Switching Protocols\r\nConnection: Upgrade\r\nUpgrade: tcp\r\n\r\n").await.unwrap();
                    stream.write_all(&body).await.unwrap();
                    let mut input = vec![];
                    stream.read_to_end(&mut input).await.unwrap();
                    assert_eq!(input, [0, 255, 1, 2].repeat(65536));
                    continue;
                }
                let status = if step == 2 && mode == 3 {
                    "500 Internal Server Error"
                } else {
                    "200 OK"
                };
                stream
                    .write_all(
                        format!(
                            "HTTP/1.1 {status}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                            body.len()
                        )
                        .as_bytes(),
                    )
                    .await
                    .unwrap();
                stream.write_all(&body).await.unwrap();
            }
            requests
        });
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
            .find_map(|e| match e {
                Effect::Spawn(ticket) => Some(ticket.run),
                _ => None,
            })
            .unwrap();
        let result = if mode == 8 {
            execute(
                registry.inspect("dev").unwrap().bind("tool").unwrap(),
                wes_engine::environments::Authority::default(),
                vec!["/bin/cat".into()],
                Some([0, 255, 1, 2].repeat(65536)),
                Duration::from_secs(5),
                OUTPUT,
                token,
            )
            .await
        } else {
            tokio::time::timeout(
                Duration::from_secs(10),
                product.invoker().invoke(
                    Call {
                        authority: Default::default(),
                        run,
                        capability: product.description().capabilities().next().unwrap().clone(),
                        arguments: if mode == 7 {
                            indexmap::IndexMap::from([(
                                "cmd".into(),
                                Value::new(
                                    wes_core::Shape::Primitive(wes_core::Primitive::Text),
                                    Data::Text("printf '%s' \"$QA_ONLY\"".into()),
                                    Provenance::default(),
                                )
                                .unwrap(),
                            )])
                        } else {
                            Default::default()
                        },
                    },
                    token,
                ),
            )
            .await
            .unwrap()
        };
        let requests = tokio::time::timeout(Duration::from_secs(10), serving)
            .await
            .unwrap()
            .unwrap();
        (result, requests)
    }
    #[cfg(unix)]
    #[tokio::test]
    async fn finite_stdin_uses_hijacked_byte_channel_without_exec_metadata_payload() {
        let (result, requests) = fake(8).await;
        let value = result.unwrap();
        let Data::Record(fields) = value.data() else {
            panic!("record")
        };
        assert_eq!(fields["stdout"], Data::Bytes(b"ok".to_vec().into()));
        assert!(requests[1].contains("\"AttachStdin\":true"));
        assert!(requests[2].to_ascii_lowercase().contains("upgrade: tcp"));
    }
    #[cfg(unix)]
    #[tokio::test]
    async fn engine_api_preserves_remote_namespace_observed_identity_and_child_exit() {
        let (result, requests) = fake(0).await;
        let value = result.unwrap();
        let Data::Record(fields) = value.data() else {
            panic!("record")
        };
        assert_eq!(fields["exitCode"], Data::Int(17));
        assert_eq!(fields["stdout"], Data::Bytes(b"ok".to_vec().into()));
        assert_eq!(fields["stderr"], Data::Bytes(b"e".to_vec().into()));
        assert!(requests[0].starts_with("GET /v1.45/containers/synthetic/json "));
        assert!(requests[1].contains("/not/on/host/tool"));
        assert!(requests[1].contains("/remote-only"));
        assert!(requests[1].contains("QA_ONLY=remote"));
        assert!(!requests[1].contains("PATH="));
        assert!(requests.iter().all(|r| !r.contains("/containers/create")));
        assert!(value.provenance().fact("docker.container").is_some());
    }
    #[cfg(unix)]
    #[tokio::test]
    async fn builtin_shell_uses_container_argv_without_host_interpolation() {
        let (result, requests) = fake(7).await;
        assert!(result.is_ok());
        let body: Json =
            serde_json::from_str(requests[1].split("\r\n\r\n").nth(1).unwrap()).unwrap();
        assert_eq!(
            body["Cmd"],
            json!(["/bin/sh", "-c", "printf '%s' \"$QA_ONLY\""])
        );
        assert_eq!(body["WorkingDir"], "/remote-only");
        assert_eq!(body["Env"], json!(["QA_ONLY=remote"]));
    }
    #[cfg(unix)]
    #[tokio::test]
    async fn disconnect_and_start_rejection_retain_uncertainty_without_retries() {
        for mode in [1, 3] {
            let (result, requests) = fake(mode).await;
            assert!(result.unwrap_err().to_string().contains("ENV036"));
            assert_eq!(requests.len(), 3);
        }
    }
    #[cfg(unix)]
    #[tokio::test]
    async fn identity_mismatch_and_stopped_container_refuse_before_exec_creation() {
        for (mode, code) in [(2, "ENV035"), (4, "ENV032")] {
            let (result, requests) = fake(mode).await;
            assert!(result.unwrap_err().to_string().contains(code));
            assert_eq!(requests.len(), 1);
        }
    }
    #[cfg(unix)]
    #[tokio::test]
    async fn cancellation_after_start_is_unknown_and_disable_during_inspection_blocks_start() {
        let (result, requests) = fake(5).await;
        assert!(result.unwrap_err().to_string().contains("ENV036"));
        assert_eq!(requests.len(), 3);
        let (result, requests) = fake(6).await;
        assert!(result.unwrap_err().to_string().contains("ENV020"));
        assert_eq!(requests.len(), 2);
    }
}
