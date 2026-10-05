//! A desktop owns one writable data home. Switch requests are owned by this task, not HTTP clients.
use super::*;
use crate::{
    runtime::{LaunchedRuntime, RuntimeOptions, launch},
    web,
};
use serde::Serialize;
use std::{
    net::SocketAddr,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};
use tokio::sync::{RwLock, mpsc, oneshot, watch};
use tokio_util::sync::CancellationToken;

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Location {
    pub path: PathBuf,
    pub identity: String,
    pub url: String,
}
#[derive(Clone)]
pub struct Connection {
    token: String,
    pub location: Location,
    pub(crate) gate: Arc<RwLock<()>>,
    pub(crate) retired: Arc<AtomicBool>,
    pub(crate) ready: Arc<AtomicBool>,
    warning: Arc<Mutex<Option<String>>>,
    requests: mpsc::Sender<Switch>,
}
struct Switch {
    token: String,
    path: String,
    reply: oneshot::Sender<Result<Location, String>>,
}
impl Connection {
    pub async fn switch(&self, path: String) -> Result<Location, String> {
        let (reply, receive) = oneshot::channel();
        self.requests
            .try_send(Switch {
                token: self.token.clone(),
                path,
                reply,
            })
            .map_err(|_| "Another data-folder switch is in progress.".to_owned())?;
        receive
            .await
            .map_err(|_| "The desktop is shutting down.".to_owned())?
    }
    pub fn warning(&self) -> Option<String> {
        self.warning.lock().expect("data-home warning").clone()
    }
}
struct Running {
    runtime: Option<LaunchedRuntime>,
    server: web::Server,
    connection: Connection,
}
impl Running {
    async fn stop(self) -> Result<(), Error> {
        // Always join both owners, including when the transport reports an error.
        let server = self.server.shutdown().await;
        let runtime = match self.runtime {
            Some(runtime) => runtime.shutdown().await,
            None => Ok(()),
        };
        server?;
        runtime
    }
}

pub struct DesktopHost {
    location: watch::Receiver<Location>,
    stopped: CancellationToken,
    task: tokio::task::JoinHandle<Result<(), Error>>,
}
impl DesktopHost {
    /// All filesystem and Keychain fixtures can be isolated by supplying a synthetic user home.
    pub async fn start(user_home: PathBuf, site: Option<PathBuf>) -> Result<Self, Error> {
        let selected_user = user_home.clone();
        let selected = tokio::task::spawn_blocking(move || selected_home(&selected_user)).await?;
        let (requests, mut incoming) = mpsc::channel(1);
        let (path, opened) = match selected {
            Ok(path) => {
                let opened = start(&path, &user_home, site.clone(), requests.clone()).await;
                (path, opened)
            }
            Err(error) => (default_home(&user_home), Err(error)),
        };
        let mut active = match opened {
            Ok(running) => running,
            Err(error) => {
                let mut connection = connection(path, String::new(), requests.clone());
                *connection.warning.lock().expect("warning") = Some(error.to_string());
                let server = web::listen_data_home_recovery(connection.clone()).await?;
                connection.location.url = address_url(server.address());
                Running {
                    runtime: None,
                    server,
                    connection,
                }
            }
        };
        active.connection.ready.store(true, Ordering::Release);
        let (locations, location) = watch::channel(active.connection.location.clone());
        let stopped = CancellationToken::new();
        let stop = stopped.clone();
        let task = tokio::spawn(async move {
            loop {
                let request = tokio::select! {
                    biased;
                    _ = stop.cancelled() => break,
                    request = incoming.recv() => match request { Some(r) => r, None => break },
                };
                if request.token != active.connection.token {
                    let _ = request.reply.send(Err(
                        "The data folder has changed. Reload the desktop.".into(),
                    ));
                    continue;
                }
                match prepare(
                    &active,
                    &request.path,
                    &user_home,
                    site.clone(),
                    requests.clone(),
                )
                .await
                {
                    Ok(Some((candidate, pause, gate))) => {
                        active.connection.retired.store(true, Ordering::Release);
                        let next = candidate.connection.location.clone();
                        let old = std::mem::replace(&mut active, candidate);
                        // Let the initiating route return before draining its server. The native
                        // shell navigates only on the watch publication below, after old writers exit.
                        let _ = request.reply.send(Ok(next.clone()));
                        let retired = old.stop().await;
                        drop(pause);
                        drop(gate);
                        if let Err(error) = retired {
                            *active.connection.warning.lock().expect("warning") = Some(format!(
                                "The previous data folder reported a shutdown error: {error}"
                            ));
                        }
                        active.connection.ready.store(true, Ordering::Release);
                        locations.send_replace(next);
                    }
                    Ok(None) => {
                        let _ = request.reply.send(Ok(active.connection.location.clone()));
                    }
                    Err(error) => {
                        let _ = request.reply.send(Err(error.to_string()));
                    }
                }
            }
            active.stop().await
        });
        Ok(Self {
            location,
            stopped,
            task,
        })
    }
    pub fn location(&self) -> Location {
        self.location.borrow().clone()
    }
    pub fn subscribe(&self) -> watch::Receiver<Location> {
        let mut receiver = self.location.clone();
        receiver.borrow_and_update();
        receiver
    }
    pub async fn shutdown(self) -> Result<(), Error> {
        self.stopped.cancel();
        self.task.await.map_err(io::Error::other)?
    }
}

async fn start(
    path: &Path,
    user_home: &Path,
    site: Option<PathBuf>,
    requests: mpsc::Sender<Switch>,
) -> Result<Running, Error> {
    let runtime = launch(RuntimeOptions::new(path.to_owned(), user_home.to_owned())).await?;
    let mut connection = connection(
        runtime.home().to_owned(),
        runtime.identity().id.clone(),
        requests,
    );
    let server = match runtime.serve_managed(site, connection.clone()).await {
        Ok(server) => server,
        Err(error) => {
            let _ = runtime.shutdown().await;
            return Err(error);
        }
    };
    connection.location.url = address_url(server.address());
    Ok(Running {
        runtime: Some(runtime),
        server,
        connection,
    })
}
fn connection(path: PathBuf, identity: String, requests: mpsc::Sender<Switch>) -> Connection {
    Connection {
        token: uuid::Uuid::new_v4().to_string(),
        location: Location {
            path,
            identity,
            url: String::new(),
        },
        gate: Arc::default(),
        retired: Arc::new(AtomicBool::new(false)),
        ready: Arc::new(AtomicBool::new(false)),
        warning: Arc::default(),
        requests,
    }
}
pub(crate) fn address_url(address: SocketAddr) -> String {
    format!("http://{address}/")
}

type Prepared = (
    Running,
    Vec<wes_engine::session::SessionCheckpoint>,
    tokio::sync::OwnedRwLockWriteGuard<()>,
);
async fn prepare(
    active: &Running,
    written: &str,
    user_home: &Path,
    site: Option<PathBuf>,
    requests: mpsc::Sender<Switch>,
) -> Result<Option<Prepared>, Error> {
    let path = expand(written, user_home)?;
    let current = &active.connection.location.path;
    if path == *current && active.runtime.is_some() {
        return Ok(None);
    }
    if path != *current && (path.starts_with(current) || current.starts_with(&path)) {
        return Err(io::Error::other("Data folders must not contain one another.").into());
    }
    let gate = active.connection.gate.clone().try_write_owned()
        .map_err(|_| io::Error::other("An operation is in progress. Wait for it to finish before opening another data folder."))?;
    if active.server.has_pending_io() {
        return Err(io::Error::other(
            "A file operation is still finishing. Wait before opening another data folder.",
        )
        .into());
    }
    if active.server.has_active_terminals() {
        return Err(io::Error::other(
            "Close Shell terminal sessions before opening another data folder.",
        )
        .into());
    }
    let mut checkpoints = Vec::new();
    if let Some(runtime) = &active.runtime {
        let sessions = runtime.handle.subscribe_sessions().borrow().clone();
        for current in sessions {
            let session = current.session;
            let observed = session.observe().await?;
            if !observed.state.execution.idle
                || !observed.state.execution.streaming.is_empty()
                || !observed.state.conversations.is_empty()
                || observed.cells.iter().any(|c| c.reply.is_none())
            {
                return Err(io::Error::other(
                    "Finish or cancel running work and streams before opening another data folder.",
                )
                .into());
            }
            let checkpoint = tokio::time::timeout(Duration::from_secs(10), session.checkpoint())
                .await
                .map_err(|_| io::Error::other(
                    "Pending work or writes have not settled. The current data folder remains open.",
                ))??;
            checkpoints.push(checkpoint);
        }
    }
    let candidate = match start(&path, user_home, site, requests).await {
        Ok(candidate) => candidate,
        Err(error) => {
            for checkpoint in checkpoints {
                checkpoint.resume().await;
            }
            return Err(error);
        }
    };
    let user = user_home.to_owned();
    let selected = candidate.connection.location.path.clone();
    let old = current.to_owned();
    let remembered = tokio::task::spawn_blocking(move || {
        if let Err(error) = remember_home(&user, &selected) {
            // Atomic publication can precede a late sync error; restore the previous pointer.
            remember_home(&user, &old)?;
            return Err(error);
        }
        Ok::<_, Error>(())
    })
    .await;
    match remembered {
        Ok(Ok(())) => Ok(Some((candidate, checkpoints, gate))),
        result => {
            let _ = candidate.stop().await;
            for checkpoint in checkpoints {
                checkpoint.resume().await;
            }
            match result {
                Ok(Err(e)) => Err(e),
                Err(e) => Err(e.into()),
                _ => unreachable!(),
            }
        }
    }
}
