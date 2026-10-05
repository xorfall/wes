//! Docker raw TTY over an upgraded Engine socket. No host shell or Docker CLI.
use super::{client::DockerEngineClient, destination};
use crate::execution_targets::{TerminalPlan, TerminalSupport};
use serde_json::json;
use std::{
    io::{self, Read, Write},
    pin::Pin,
    task::Poll,
    time::Duration,
};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, ReadBuf};
use wes_core::environments::{Target, TargetKind};
use wes_engine::{
    driver::CancellationToken,
    execution::{TerminalExit, TerminalIo, TerminalSize},
};

const UNKNOWN: &str = "ENV036: Docker terminal outcome is unknown; remote work may still be running. No automatic retry or cleanup is claimed.";
const CONTROL_TIMEOUT: Duration = Duration::from_secs(5);

pub(crate) fn plan(target: &Target) -> Result<TerminalPlan, &'static str> {
    if !cfg!(unix) {
        return Err("Docker Unix-socket transport is unavailable on this platform");
    }
    let TargetKind::Docker { socket, .. } = target.kind() else {
        return Err("not a Docker target");
    };
    // Inert construction; no socket is opened by discovery or environment admission.
    let client = DockerEngineClient::new(socket)?;
    let target = target.clone();
    Ok(TerminalPlan::new(
        TerminalSupport {
            label: "Docker",
            workspace_tools: false,
        },
        move |host, size, authority| {
            if host.is_some() {
                return Err(io::Error::other(
                    "Docker terminals cannot receive host bootstrap material",
                ));
            }
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()?;
            let started = std::sync::atomic::AtomicBool::new(false);
            let opened = runtime.block_on(async {
            tokio::select! {
                biased;
                () = authority.cancelled() => Err(io::Error::other("ENV020: Docker terminal authority ended during setup")),
                result = tokio::time::timeout(Duration::from_secs(15), open(&client, &target, size, authority, &started)) => result.unwrap_or_else(|_| Err(io::Error::other("ENV031: Docker terminal setup timed out"))),
            }
        });
            let (stream, api, exec, resolved) = opened.map_err(|error| {
                if started.load(std::sync::atomic::Ordering::Acquire) {
                    io::Error::other(UNKNOWN)
                } else {
                    error
                }
            })?;
            Ok(Box::new(Endpoint {
                stream: Some(stream),
                client,
                runtime,
                api,
                exec,
                resolved,
                authority: authority.clone(),
                buffered: vec![],
                cursor: 0,
                eof: false,
                exit: None,
            }))
        },
    ))
}

async fn open(
    client: &DockerEngineClient,
    target: &Target,
    size: TerminalSize,
    authority: &CancellationToken,
    started: &std::sync::atomic::AtomicBool,
) -> io::Result<(
    reqwest::Upgraded,
    &'static str,
    String,
    destination::Resolved,
)> {
    let api = client
        .endpoint()
        .await
        .map_err(|error| io::Error::other(destination::before_start(error)))?;
    let resolved = destination::resolve(client, api, target)
        .await
        .map_err(io::Error::other)?;
    let TargetKind::Docker { shell, .. } = target.kind() else {
        unreachable!()
    };
    let mut env = target.variables().clone();
    env.entry("TERM".into())
        .or_insert_with(|| "xterm-256color".into());
    let config = json!({"AttachStdin":true,"AttachStdout":true,"AttachStderr":true,"Tty":true,"Privileged":false,
        "ConsoleSize":[size.rows,size.cols],"Cmd":[shell.as_deref().unwrap_or("/bin/sh"),"-i"],
        "WorkingDir":target.cwd().unwrap_or("/"),"Env":env.iter().map(|(k,v)|format!("{k}={v}")).collect::<Vec<_>>()});
    let created = client
        .json(
            client
                .http
                .post(format!("{api}/containers/{}/exec", resolved.id))
                .header("content-type", "application/json")
                .body(config.to_string()),
        )
        .await
        .map_err(|error| io::Error::other(destination::before_start(error)))?;
    let exec = created
        .get("Id")
        .and_then(serde_json::Value::as_str)
        .filter(|id| super::digest(id))
        .ok_or_else(|| io::Error::other("ENV034: invalid Docker exec identity"))?
        .to_owned();
    if authority.is_cancelled() {
        return Err(io::Error::other(
            "ENV020: Docker terminal authority ended before start",
        ));
    }
    started.store(true, std::sync::atomic::Ordering::Release);
    let response = client
        .http
        .post(format!("{api}/exec/{exec}/start"))
        .header("content-type", "application/json")
        .header("connection", "Upgrade")
        .header("upgrade", "tcp")
        .body(json!({"Detach":false,"Tty":true,"ConsoleSize":[size.rows,size.cols]}).to_string())
        .send()
        .await
        .map_err(|_| io::Error::other(UNKNOWN))?;
    if response.status() != reqwest::StatusCode::SWITCHING_PROTOCOLS {
        return Err(io::Error::other(UNKNOWN));
    }
    let stream = response
        .upgrade()
        .await
        .map_err(|_| io::Error::other(UNKNOWN))?;
    Ok((stream, api, exec, resolved))
}

struct Endpoint {
    // Stream/client drop before their owned reactor. No background collector survives shutdown.
    stream: Option<reqwest::Upgraded>,
    client: DockerEngineClient,
    runtime: tokio::runtime::Runtime,
    api: &'static str,
    exec: String,
    resolved: destination::Resolved,
    authority: CancellationToken,
    buffered: Vec<u8>,
    cursor: usize,
    eof: bool,
    exit: Option<TerminalExit>,
}
impl Read for Endpoint {
    fn read(&mut self, bytes: &mut [u8]) -> io::Result<usize> {
        if bytes.is_empty() {
            return Ok(0);
        }
        if self.cursor < self.buffered.len() {
            let n = bytes.len().min(self.buffered.len() - self.cursor);
            bytes[..n].copy_from_slice(&self.buffered[self.cursor..self.cursor + n]);
            self.cursor += n;
            return Ok(n);
        }
        if self.eof {
            return Ok(0);
        }
        let stream = self
            .stream
            .as_mut()
            .ok_or_else(|| io::Error::other("Docker terminal is closed"))?;
        let result = self.runtime.block_on(std::future::poll_fn(|cx| {
            let mut buf = ReadBuf::new(bytes);
            Poll::Ready(match Pin::new(&mut *stream).poll_read(cx, &mut buf) {
                Poll::Ready(Ok(())) => Ok(buf.filled().len()),
                Poll::Ready(Err(e)) => Err(e),
                Poll::Pending => Err(io::ErrorKind::WouldBlock.into()),
            })
        }));
        if matches!(result, Ok(0)) {
            self.eof = true;
        }
        result
    }
}
impl Write for Endpoint {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        let stream = self
            .stream
            .as_mut()
            .ok_or_else(|| io::Error::other("Docker terminal is closed"))?;
        self.runtime.block_on(std::future::poll_fn(|cx| {
            Poll::Ready(match Pin::new(&mut *stream).poll_write(cx, bytes) {
                Poll::Ready(result) => result,
                Poll::Pending => Err(io::ErrorKind::WouldBlock.into()),
            })
        }))
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}
impl TerminalIo for Endpoint {
    fn identity(&self) -> Option<String> {
        Some(format!("Docker container {}", self.resolved.id))
    }
    fn resize(&mut self, size: TerminalSize) -> io::Result<()> {
        self.runtime.block_on(async {
            tokio::select! {
                biased;
                () = self.authority.cancelled() => Err(io::Error::other(UNKNOWN)),
                result = tokio::time::timeout(CONTROL_TIMEOUT, self.client.response(self.client.http.post(format!("{}/exec/{}/resize?h={}&w={}",self.api,self.exec,size.rows,size.cols)))) => {
                    result.map_err(|_|io::Error::other(UNKNOWN))?.map_err(|_|io::Error::other(UNKNOWN))?; Ok(())
                }
            }
        })
    }
    fn wait_ready(&mut self, _: bool, timeout: Duration) -> io::Result<()> {
        if self.eof || self.cursor < self.buffered.len() {
            return Ok(());
        }
        self.buffered.resize(16384, 0);
        self.cursor = 0;
        let stream = self
            .stream
            .as_mut()
            .ok_or_else(|| io::Error::other("Docker terminal is closed"))?;
        let result = self.runtime.block_on(async {
            tokio::select! {
                biased;
                () = self.authority.cancelled() => Ok(0),
                result = tokio::time::timeout(timeout, stream.read(&mut self.buffered)) => result.unwrap_or(Ok(0)),
            }
        });
        match result {
            Ok(n) => {
                self.buffered.truncate(n);
                Ok(())
            }
            Err(error) => {
                self.buffered.clear();
                Err(error)
            }
        }
    }
    fn try_exit(&mut self) -> io::Result<Option<TerminalExit>> {
        if self.exit.is_some() || !self.eof {
            return Ok(self.exit);
        }
        let exit = self.runtime.block_on(async {
            tokio::select! {
                biased;
                () = self.authority.cancelled() => None,
                result = tokio::time::timeout(CONTROL_TIMEOUT, self.client.json(self.client.http.get(format!("{}/exec/{}/json",self.api,self.exec)))) => {
                    let status = result.ok()?.ok()?;
                    if status.get("Running")?.as_bool()? || status.get("ID")?.as_str()? != self.exec || status.get("ContainerID")?.as_str()? != self.resolved.id { return None; }
                    Some(TerminalExit { code: status.get("ExitCode")?.as_u64()?.try_into().ok()?, uncertain: false })
                }
            }
        });
        self.exit = Some(exit.unwrap_or(TerminalExit {
            code: 255,
            uncertain: true,
        }));
        Ok(self.exit)
    }
    fn shutdown(&mut self) -> io::Result<TerminalExit> {
        // No request is sent to kill/restart a container or retry an exec. Drop owns the socket.
        self.stream.take();
        let exit = self.exit.unwrap_or(TerminalExit {
            code: 255,
            uncertain: true,
        });
        self.exit = Some(exit);
        Ok(exit)
    }
}
impl Drop for Endpoint {
    fn drop(&mut self) {
        let _ = self.shutdown();
    }
}
