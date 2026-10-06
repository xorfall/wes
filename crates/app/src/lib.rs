//! Concrete named-workspace/session composition. No second parser, graph or execution semantics.
pub mod api_library;
pub mod backend;
pub mod budgets;
pub mod credential_store;
pub mod credential_vault;
pub mod data_home;
mod execution_status;
pub mod retention;
pub mod retirement;
pub mod runtime;
mod sandbox_definitions;
pub mod scenarios;
pub mod startup;
pub mod telemetry;
pub mod view_toolchain;
pub mod web;
pub mod work_history;
pub mod workspace_deletion;
/// Synchronous host process preparation; the caller retains child ownership.
pub use wes_adapters::process::serialized_spawn;

use backend::{BackendError, FileBackend, WorkspaceBackend};
use std::{
    collections::BTreeMap,
    num::NonZeroUsize,
    path::PathBuf,
    sync::{Arc, Mutex},
};
use thiserror::Error;
use tokio::{
    sync::{mpsc, oneshot, watch},
    task::JoinHandle,
};
use wes_adapters::{
    journal::{Durability, ReadLimits},
    workspaces::WorkspaceFileError,
};
use wes_engine::{
    calls::{CallJournal, RequiredPersistence},
    driver::CancellationToken,
    history::{
        AppendReceipt, HistoryCapture, HistoryCaptureLimits, HistoryCheckpoint, HistoryImage,
        Persistence, RecordError,
    },
    recording::{Recorder, RecorderLimits, RecorderTask, spawn_recorder},
    session::{
        self, RecordingMode, SessionError, SessionHandle, SessionStorage, SessionTask,
        SubmissionReply, WorkspaceAction, WorkspaceActionError, WorkspaceActions,
        WorkspaceOperation, WorkspaceRequest,
    },
    source::SourceInput,
    type_sources::TypeSourceReader,
    workspace::{Workspace, WorkspaceError, WorkspaceName},
};

pub struct Config {
    /// A private named-workspace directory, separate from the shared value store.
    pub directory: PathBuf,
    pub initial: WorkspaceName,
    pub durability: Durability,
    pub concurrency: NonZeroUsize,
    pub max_streams: NonZeroUsize,
    /// Recreates only configured registries; captured imports and source are restored by engine.
    pub workspace: Arc<dyn Fn() -> Result<Workspace, WorkspaceError> + Send + Sync>,
    pub type_reader: Arc<dyn TypeSourceReader>,
    /// Shared across names. The embedding owner closes this worker after joining the application.
    pub storage: Option<SessionStorage>,
}
#[derive(Debug, Error)]
pub enum ApplicationError {
    #[error("the limit of 16 live workspaces was reached; restart to release hidden sessions")]
    Capacity,
    #[error("the workspace is not open, or its session generation is stale")]
    MissingSession,
    #[error("the application is no longer running")]
    Stopped,
    #[error("an owned application worker failed")]
    Worker,
    #[error("history shutdown reported failed writes")]
    UnconfirmedHistory,
    #[error(transparent)]
    Session(#[from] SessionError),
    #[error(transparent)]
    Files(#[from] WorkspaceFileError),
    #[error(transparent)]
    Backend(#[from] BackendError),
    #[error(transparent)]
    History(#[from] RecordError),
    #[error(transparent)]
    Restore(#[from] session::RestoreError),
    #[error(transparent)]
    Workspace(#[from] WorkspaceError),
}
#[derive(Clone)]
pub struct CurrentSession {
    pub name: WorkspaceName,
    pub identity: Option<String>,
    /// Identifies the live engine session; changes on reconstruction/reload, not
    /// in-place history compaction or work retirement.
    pub generation: String,
    pub session: SessionHandle,
    pub storage_warning: Option<String>,
}
#[derive(Clone)]
pub struct ApplicationHandle {
    capacity: watch::Receiver<wes_engine::driver::CapacitySnapshot>,
    current: watch::Receiver<Option<CurrentSession>>,
    sessions: watch::Receiver<Vec<CurrentSession>>,
    bindings: Arc<Mutex<BTreeMap<String, watch::Receiver<Option<CurrentSession>>>>>,
    openings: mpsc::Sender<OpenRequest>,
    shutdown: CancellationToken,
    management: mpsc::Sender<retention::Request>,
    storage: Option<wes_engine::storage::StoreWorker>,
    names: watch::Receiver<Arc<[WorkspaceName]>>,
    cleanup_warning: Arc<Mutex<Option<String>>>,
    deletions: mpsc::Sender<workspace_deletion::Request>,
    terminals: Arc<Mutex<Vec<terminal::Manager>>>,
}
impl ApplicationHandle {
    pub fn subscribe_capacity(&self) -> watch::Receiver<wes_engine::driver::CapacitySnapshot> {
        self.capacity.clone()
    }
    pub fn cleanup_warning(&self) -> Option<String> {
        self.cleanup_warning.lock().ok().and_then(|w| w.clone())
    }
    /// Observe this handle's selection (the default selection or one fixed workspace).
    pub fn subscribe(&self) -> watch::Receiver<Option<CurrentSession>> {
        self.current.clone()
    }
    pub fn current(&self) -> Result<CurrentSession, ApplicationError> {
        if self.current.has_changed().is_err() {
            return Err(ApplicationError::Stopped);
        }
        self.current
            .borrow()
            .clone()
            .ok_or(ApplicationError::Stopped)
    }
    /// Observe live named sessions without acquiring any journal ownership.
    pub fn subscribe_sessions(&self) -> watch::Receiver<Vec<CurrentSession>> {
        self.sessions.clone()
    }
    /// Bind all subsequent operations to a named live workspace.
    pub fn bound(&self, name: &str) -> Result<Self, ApplicationError> {
        if self.shutdown.is_cancelled() {
            return Err(ApplicationError::Stopped);
        }
        let current = self
            .bindings
            .lock()
            .map_err(|_| ApplicationError::Worker)?
            .get(name)
            .cloned()
            .ok_or(ApplicationError::MissingSession)?;
        Ok(Self {
            current,
            ..self.clone()
        })
    }
    pub fn session_for_generation(
        &self,
        generation: &str,
    ) -> Result<CurrentSession, ApplicationError> {
        if self.shutdown.is_cancelled() {
            return Err(ApplicationError::Stopped);
        }
        self.bindings
            .lock()
            .map_err(|_| ApplicationError::Worker)?
            .values()
            .find_map(|current| {
                current
                    .borrow()
                    .clone()
                    .filter(|s| s.generation == generation)
            })
            .ok_or(ApplicationError::MissingSession)
    }
    /// Open without changing the default selection. Repeated names share one live writer.
    pub async fn open_workspace(
        &self,
        name: WorkspaceName,
        create: bool,
    ) -> Result<CurrentSession, ApplicationError> {
        self.open_workspace_identity(name, create, None).await
    }
    pub async fn open_workspace_identity(
        &self,
        name: WorkspaceName,
        create: bool,
        identity: Option<String>,
    ) -> Result<CurrentSession, ApplicationError> {
        let (reply, receive) = oneshot::channel();
        self.openings
            .send(OpenRequest {
                name,
                create,
                identity,
                reply,
            })
            .await
            .map_err(|_| ApplicationError::Stopped)?;
        receive.await.map_err(|_| ApplicationError::Stopped)?
    }
    /// Bind the request once. A request caught by a switch is never silently retried in another session.
    pub async fn submit(&self, input: SourceInput) -> SubmissionReply {
        let session = self.current().map_err(|_| SessionError::Stopped)?.session;
        session.submit(input).await
    }
    /// Requests cancellation promptly even while named-workspace I/O is outstanding. Join afterward.
    pub async fn shutdown(&self) {
        self.shutdown.cancel();
        if let Ok(current) = self.current() {
            let _ = current.session.shutdown().await;
        }
    }
}
pub struct ApplicationTask(JoinHandle<Result<(), ApplicationError>>);
impl ApplicationTask {
    pub async fn join(self) -> Result<(), ApplicationError> {
        self.0.await.map_err(|_| ApplicationError::Worker)?
    }
}
struct Running {
    handle: SessionHandle,
    task: SessionTask,
    actions: mpsc::Receiver<WorkspaceRequest>,
}
struct Active {
    running: Running,
    name: WorkspaceName,
    recorder: Recorder,
    writer: RecorderTask,
    journal: CallJournal,
}
fn max_live_workspaces() -> usize {
    wes_budgets::get("workspace.live") as usize
}
struct OpenRequest {
    identity: Option<String>,
    name: WorkspaceName,
    create: bool,
    reply: oneshot::Sender<Result<CurrentSession, ApplicationError>>,
}
struct Parked {
    active: Active,
    current: watch::Sender<Option<CurrentSession>>,
}
struct Owner {
    names: watch::Sender<Arc<[WorkspaceName]>>,
    config: Config,
    files: Arc<Mutex<Box<dyn WorkspaceBackend>>>,
    current: watch::Sender<Option<CurrentSession>>,
    shutdown: CancellationToken,
    active: Option<Active>,
    parked: BTreeMap<String, Parked>,
    default_current: watch::Sender<Option<CurrentSession>>,
    sessions: watch::Sender<Vec<CurrentSession>>,
    bindings: Arc<Mutex<BTreeMap<String, watch::Receiver<Option<CurrentSession>>>>>,
    openings: mpsc::Receiver<OpenRequest>,
    capacity: wes_engine::driver::ExecutionCapacity,
    management_peers: BTreeMap<WorkspaceName, HistoryImage>,
    management_live: BTreeMap<wes_engine::storage::ValueHandle, std::collections::BTreeSet<String>>,
    management: mpsc::Receiver<retention::Request>,
    release_plans: std::collections::BTreeMap<String, retention::Plan>,
    work_plans: std::collections::BTreeMap<String, retirement::WorkPlan>,
    deletion_plans: BTreeMap<String, workspace_deletion::Plan>,
    cleanup_warning: Arc<Mutex<Option<String>>>,
    deletions: mpsc::Receiver<workspace_deletion::Request>,
    terminals: Arc<Mutex<Vec<terminal::Manager>>>,
}

/// Open or create the requested initial local name. Existing contents must pass strict restoration;
/// neither startup nor loading invokes providers. Filesystem work runs on joined blocking workers.
pub async fn open(
    config: Config,
) -> Result<(ApplicationHandle, ApplicationTask), ApplicationError> {
    let path = config.directory.clone();
    let durability = config.durability;
    open_backend(config, move || {
        Ok(Box::new(FileBackend::open(
            &path,
            ReadLimits::default(),
            durability,
        )?))
    })
    .await
}
/// Explicit backend selection. Config.directory is not accessed; errors never fall back to files.
/// The caller creates the backend on an appropriate worker and owns its authentication/lifecycle.
pub async fn open_with_backend(
    config: Config,
    backend: impl WorkspaceBackend + 'static,
) -> Result<(ApplicationHandle, ApplicationTask), ApplicationError> {
    open_backend(config, move || Ok(Box::new(backend))).await
}
async fn open_backend(
    config: Config,
    create: impl FnOnce() -> Result<Box<dyn WorkspaceBackend>, BackendError> + Send + 'static,
) -> Result<(ApplicationHandle, ApplicationTask), ApplicationError> {
    let capacity = wes_engine::driver::ExecutionCapacity::with_stream_limit(
        config.concurrency,
        config.max_streams,
    )
    .map_err(SessionError::from)?;
    let initial = config.initial.clone();
    let (files, saved, selected) = tokio::task::spawn_blocking(move || {
        let mut files = create()?;
        if files.capabilities().automatic_cleanup {
            files.collect_unused()?;
        }
        let names = files.names()?;
        // Fresh homes get the explicit configured initial name. A deletion tombstone
        // means absence is intentional, including after deleting the final workspace.
        let selected = if names.contains(&initial) || !files.has_deletions()? {
            Some((initial, true))
        } else {
            names.first().cloned().map(|name| (name, false))
        };
        Ok::<_, BackendError>((files, Arc::<[WorkspaceName]>::from(names), selected))
    })
    .await
    .map_err(|_| ApplicationError::Worker)??;
    let shutdown = CancellationToken::new();
    let (names, observed_names) = watch::channel(saved);
    let (current, _) = watch::channel(None);
    let (default_current, selected_session) = watch::channel(None);
    let (sessions, observed_sessions) = watch::channel(vec![]);
    let bindings = Arc::new(Mutex::new(BTreeMap::new()));
    let (openings, open_requests) = mpsc::channel(16);
    let (management, requests) = mpsc::channel(16);
    let (deletions, delete_requests) = mpsc::channel(16);
    let terminals = Arc::new(Mutex::new(Vec::new()));
    let cleanup_warning = Arc::new(Mutex::new(None));
    let handle = ApplicationHandle {
        capacity: capacity.subscribe(),
        current: selected_session,
        sessions: observed_sessions,
        bindings: bindings.clone(),
        openings,
        shutdown: shutdown.clone(),
        management,
        storage: config.storage.as_ref().map(|s| s.worker.clone()),
        names: observed_names,
        cleanup_warning: cleanup_warning.clone(),
        deletions,
        terminals: terminals.clone(),
    };
    let mut owner = Owner {
        names,
        config,
        files: Arc::new(Mutex::new(files)),
        current,
        shutdown,
        active: None,
        parked: BTreeMap::new(),
        default_current,
        sessions,
        bindings,
        openings: open_requests,
        capacity,
        management_peers: BTreeMap::new(),
        management_live: BTreeMap::new(),
        management: requests,
        release_plans: Default::default(),
        work_plans: Default::default(),
        deletion_plans: BTreeMap::new(),
        cleanup_warning,
        deletions: delete_requests,
        terminals,
    };
    let cleanup = owner.cleanup_deleted().await;
    if let Err(error) = &cleanup {
        *owner
            .cleanup_warning
            .lock()
            .map_err(|_| ApplicationError::Worker)? = Some(format!("{}: {}", error.code(), error));
    }
    if let Some((name, create)) = selected {
        owner.open_session(name, create).await?;
    }
    if let Some(active) = &owner.active {
        if owner
            .files
            .lock()
            .map_err(|_| ApplicationError::Worker)?
            .capabilities()
            .automatic_cleanup
        {
            if let Ok(checkpoint) = active.running.handle.checkpoint().await {
                let result = owner
                    .cleanup_retired(&Arc::new(checkpoint.history().clone()))
                    .await;
                if let Err(error) = cleanup.and(result) {
                    let mut current = owner.current.borrow().clone().expect("open session");
                    current.storage_warning = Some(format!("{}: {}", error.code(), error));
                    owner.current.send_replace(Some(current));
                }
                checkpoint.resume().await;
            }
        }
    }
    owner.publish_sessions();
    Ok((handle, ApplicationTask(tokio::spawn(owner.run()))))
}

fn required(durability: Durability) -> RequiredPersistence {
    match durability {
        Durability::File => RequiredPersistence::FileSynced,
        Durability::FileAndDirectory => RequiredPersistence::FileAndDirectorySynced,
    }
}
fn current_session(
    name: &WorkspaceName,
    running: &Running,
    identity: Option<String>,
) -> CurrentSession {
    CurrentSession {
        name: name.clone(),
        identity,
        generation: uuid::Uuid::new_v4().to_string(),
        session: running.handle.clone(),
        storage_warning: None,
    }
}
async fn prepare(
    config: &Config,
    name: &WorkspaceName,
    journal: CallJournal,
    image: HistoryImage,
    cancelled: CancellationToken,
    names: watch::Receiver<Arc<[WorkspaceName]>>,
) -> Result<session::RestoredSession, ApplicationError> {
    let factory = config.workspace.clone();
    let mut workspace = tokio::task::spawn_blocking(move || factory())
        .await
        .map_err(|_| ApplicationError::Worker)??;
    workspace.set_saved_workspace_names(names);
    workspace = workspace.with_sandbox_store(Arc::new(sandbox_definitions::FileDefinitions::new(
        config.directory.clone(),
        name,
    )));
    Ok(session::restore(
        workspace,
        RecordingMode::Required(journal),
        config.storage.clone(),
        image,
        cancelled,
    )
    .await?)
}
fn start(
    config: &Config,
    restored: session::RestoredSession,
    capacity: wes_engine::driver::ExecutionCapacity,
) -> Result<Running, ApplicationError> {
    let (port, actions) = WorkspaceActions::channel();
    let (handle, task) = restored.spawn_with_capacity(
        config.type_reader.clone(),
        config.concurrency,
        port,
        capacity,
    )?;
    Ok(Running {
        handle,
        task,
        actions,
    })
}
async fn stop_running(running: Running) -> Result<(), ApplicationError> {
    let Running {
        handle,
        task,
        actions,
    } = running;
    drop(actions); // Refuse any still-queued action before joining its session-side reply waiter.
    let _ = handle.shutdown().await;
    task.join().await.map_err(|_| ApplicationError::Worker)
}
async fn stop_writer(recorder: Recorder, writer: RecorderTask) -> Result<(), ApplicationError> {
    let report = recorder.shutdown().await;
    let joined = writer.join().await;
    let report = report?;
    joined?;
    if report.failed != 0 {
        return Err(ApplicationError::UnconfirmedHistory);
    }
    Ok(())
}
impl Owner {
    fn publish_sessions(&self) {
        let mut sessions = self
            .parked
            .values()
            .filter_map(|p| p.current.borrow().clone())
            .collect::<Vec<_>>();
        sessions.extend(self.current.borrow().clone());
        sessions.sort_by(|a, b| a.name.cmp(&b.name));
        let default = self.default_current.borrow().clone();
        if let Some(default) = default {
            if let Some(updated) = sessions.iter().find(|s| s.name == default.name) {
                if updated.generation != default.generation
                    || updated.storage_warning != default.storage_warning
                {
                    self.default_current.send_replace(Some(updated.clone()));
                }
            } else {
                self.default_current.send_replace(sessions.first().cloned());
            }
        }
        self.sessions.send_replace(sessions);
    }
    fn select_context(&mut self, name: &str) -> bool {
        if self
            .active
            .as_ref()
            .is_some_and(|a| a.name.as_str() == name)
        {
            return true;
        }
        let Some(mut next) = self.parked.remove(name) else {
            return false;
        };
        if let Some(old) = self.active.replace(next.active) {
            next.active = old;
        } else {
            self.current = next.current;
            return true;
        }
        std::mem::swap(&mut next.current, &mut self.current);
        self.parked.insert(next.active.name.as_str().into(), next);
        true
    }
    fn select_generation(&mut self, generation: &str) {
        let name = self
            .parked
            .iter()
            .find(|(_, p)| {
                p.current
                    .borrow()
                    .as_ref()
                    .is_some_and(|s| s.generation == generation)
            })
            .map(|(name, _)| name.clone());
        if let Some(name) = name {
            self.select_context(&name);
        }
    }
    async fn run(mut self) -> Result<(), ApplicationError> {
        let failure = loop {
            let action = tokio::select! {
                biased;
                _ = self.shutdown.cancelled() => break None,
                action = std::future::poll_fn(|cx| {
                    if let Some(active) = &mut self.active {
                        if let std::task::Poll::Ready(Some(action)) = active.running.actions.poll_recv(cx) {
                            return std::task::Poll::Ready((active.name.as_str().to_owned(), action));
                        }
                    }
                    for (name, parked) in &mut self.parked {
                        if let std::task::Poll::Ready(Some(action)) = parked.active.running.actions.poll_recv(cx) {
                            return std::task::Poll::Ready((name.clone(), action));
                        }
                    }
                    std::task::Poll::Pending
                }) => action,
                Some(request) = self.openings.recv() => {
                    let matches = match request.identity {
                        None => Ok(true),
                        Some(expected) => {
                            let files = self.files.clone(); let name = request.name.clone();
                            tokio::task::spawn_blocking(move || files.lock().map_err(|_| ApplicationError::Worker)?.identity(&name).map(|i| i.is_some_and(|i| i.id == expected)).map_err(ApplicationError::from)).await.map_err(|_| ApplicationError::Worker).and_then(|r|r)
                        }
                    };
                    let result = match matches { Ok(true) => self.open_session(request.name, request.create).await, Ok(false) => Err(ApplicationError::MissingSession), Err(e) => Err(e) };
                    self.publish_sessions();
                    let _ = request.reply.send(result);
                    continue;
                },
                Some(request) = self.deletions.recv() => {
                    self.select_generation(request.generation());
                    self.manage_deletion(request).await;
                    self.publish_sessions();
                    continue;
                },
                Some(request) = self.management.recv() => {
                    self.select_generation(request.generation());
                    self.manage_storage(request).await;
                    self.publish_sessions();
                    continue;
                },
            };
            self.select_context(&action.0);
            let action = match action.1 {
                WorkspaceRequest::Action(action) => action,
                WorkspaceRequest::Management(request) => {
                    self.manage_source_workspace(request).await;
                    self.publish_sessions();
                    continue;
                }
            };
            match action.operation() {
                WorkspaceOperation::Save => {
                    let result = self.save(action.name().clone()).await;
                    action.complete_save(result);
                }
                WorkspaceOperation::Load => {
                    if let Err(error) = self.load(action).await {
                        break Some(error);
                    }
                }
            }
            self.publish_sessions();
        };
        self.default_current.send_replace(None);
        self.current.send_replace(None);
        self.sessions.send_replace(Vec::new());
        for parked in self.parked.values() {
            parked.current.send_replace(None);
        }
        // Request every shutdown first, so joining one blocked provider does not leave others running.
        let handles = self
            .active
            .iter()
            .map(|a| a.running.handle.clone())
            .chain(
                self.parked
                    .values()
                    .map(|p| p.active.running.handle.clone()),
            )
            .collect::<Vec<_>>();
        futures_util::future::join_all(handles.iter().map(|h| h.shutdown())).await;
        let mut result = failure.map_or(Ok(()), Err);
        let active = self
            .active
            .into_iter()
            .chain(self.parked.into_values().map(|p| p.active));
        for active in active {
            let session = stop_running(active.running).await;
            let writer = stop_writer(active.recorder, active.writer).await;
            result = result.and(session).and(writer);
        }
        result
    }
    async fn open_session(
        &mut self,
        name: WorkspaceName,
        create: bool,
    ) -> Result<CurrentSession, ApplicationError> {
        if self.active.as_ref().is_some_and(|a| a.name == name) {
            return self
                .current
                .borrow()
                .clone()
                .ok_or(ApplicationError::Stopped);
        }
        if let Some(parked) = self.parked.get(name.as_str()) {
            return parked
                .current
                .borrow()
                .clone()
                .ok_or(ApplicationError::Stopped);
        }
        if self.parked.len() + usize::from(self.active.is_some()) >= max_live_workspaces() {
            return Err(ApplicationError::Capacity);
        }
        let files = self.files.clone();
        let target = name.clone();
        let names = self.names.clone();
        let (history, image, identity) = tokio::task::spawn_blocking(move || {
            let mut files = files.lock().map_err(|_| BackendError::Invalid)?;
            let loaded = match files.load(&target) {
                Err(BackendError::Missing) if create => {
                    let receipt = AppendReceipt {
                        persistence: Persistence::Volatile,
                        end_offset: 0,
                    };
                    let empty = HistoryCapture::new(HistoryCaptureLimits::default()).finish(
                        HistoryCheckpoint {
                            journal: receipt,
                            recovery: receipt,
                        },
                    );
                    files.save(&target, &empty)?;
                    names.send_replace(files.names()?.into());
                    files.load(&target)
                }
                result => result,
            }?;
            let identity = files.identity(&target).ok().flatten().map(|i| i.id);
            Ok::<_, BackendError>((loaded.0, loaded.1, identity))
        })
        .await
        .map_err(|_| ApplicationError::Worker)??;
        let (recorder, writer) = spawn_recorder(history, RecorderLimits::default())?;
        let journal = CallJournal::new(recorder.clone(), required(self.config.durability));
        let restored = prepare(
            &self.config,
            &name,
            journal.clone(),
            image,
            self.shutdown.clone(),
            self.names.subscribe(),
        )
        .await;
        let running = match restored.and_then(|restored| {
            if self.shutdown.is_cancelled() {
                Err(ApplicationError::Stopped)
            } else {
                start(&self.config, restored, self.capacity.clone())
            }
        }) {
            Ok(running) => running,
            Err(error) => {
                let _ = stop_writer(recorder, writer).await;
                return Err(error);
            }
        };
        let current_session = current_session(&name, &running, identity);
        let (current, observed) = watch::channel(Some(current_session.clone()));
        self.bindings
            .lock()
            .map_err(|_| ApplicationError::Worker)?
            .insert(name.as_str().into(), observed);
        self.parked.insert(
            name.as_str().into(),
            Parked {
                active: Active {
                    running,
                    name,
                    recorder,
                    writer,
                    journal,
                },
                current,
            },
        );
        if self.active.is_none() {
            self.select_context(current_session.name.as_str());
            self.default_current
                .send_replace(Some(current_session.clone()));
        }
        Ok(current_session)
    }
    async fn checkpoint(&self) -> Result<session::SessionCheckpoint, WorkspaceActionError> {
        let active = self.active.as_ref().ok_or(WorkspaceActionError::Stopped)?;
        tokio::select! {
            _ = self.shutdown.cancelled() => Err(WorkspaceActionError::Stopped),
            checkpoint = active.running.handle.checkpoint() => checkpoint.map_err(|_| WorkspaceActionError::Checkpoint),
        }
    }
    async fn save(&self, name: WorkspaceName) -> Result<bool, WorkspaceActionError> {
        if self.parked.contains_key(name.as_str()) {
            return Err(WorkspaceActionError::LiveName);
        }
        let checkpoint = self.checkpoint().await?;
        let current = self.active.as_ref().is_some_and(|a| name == a.name);
        let files = self.files.clone();
        let names = self.names.clone();
        let result = tokio::task::spawn_blocking(move || {
            // The actor is awaiting this physical job, even if the original client disconnects.
            let mut files = files
                .lock()
                .map_err(|_| WorkspaceActionError::Unavailable)?;
            if !current && files.capabilities().automatic_cleanup {
                files.collect_unused().map_err(file_error)?;
            }
            let replaced = !current && files.names().map_err(file_error)?.contains(&name);
            let saved = if current {
                files.save_current(&name, checkpoint.history())
            } else {
                files.save(&name, checkpoint.history())
            };
            if saved.is_ok() {
                names.send_replace(files.names().map_err(file_error)?.into());
            } else if matches!(saved, Err(BackendError::Published(_))) {
                // Preserve publication uncertainty even when name refresh also fails.
                if let Ok(updated) = files.names() {
                    names.send_replace(updated.into());
                }
            }
            saved.map(|()| replaced).map_err(file_error)
        })
        .await
        .map_err(|_| WorkspaceActionError::Unavailable)?;
        if current && matches!(result, Err(WorkspaceActionError::Published)) {
            // Complete the action waiter before the owner joins the old session/writer.
            self.shutdown.cancel();
        }
        result
    }
    async fn load(&mut self, action: WorkspaceAction) -> Result<(), ApplicationError> {
        let active = self
            .active
            .as_ref()
            .ok_or(ApplicationError::MissingSession)?;
        if !self
            .default_current
            .borrow()
            .as_ref()
            .is_some_and(|s| s.name == active.name)
        {
            action.complete(Err(WorkspaceActionError::BoundLoad));
            return Ok(());
        }
        if action.name() != &active.name {
            let opened = self.open_session(action.name().clone(), false).await;
            match opened {
                Ok(current) => {
                    self.default_current.send_replace(Some(current));
                    action.complete(Ok(()));
                }
                Err(error) => action.complete(Err(match error {
                    ApplicationError::Backend(error) => file_error(error),
                    ApplicationError::Capacity => WorkspaceActionError::Capacity,
                    ApplicationError::Stopped => WorkspaceActionError::Stopped,
                    _ => WorkspaceActionError::Restore,
                })),
            }
            return Ok(());
        }
        let checkpoint = match self.checkpoint().await {
            Ok(checkpoint) => checkpoint,
            Err(error) => {
                action.complete(Err(error));
                return Ok(());
            }
        };
        let name = action.name().clone();
        let (image, pause) = checkpoint.into_parts();
        let journal = self
            .active
            .as_ref()
            .ok_or(ApplicationError::MissingSession)?
            .journal
            .clone();
        let restored = prepare(
            &self.config,
            &name,
            journal,
            image,
            self.shutdown.clone(),
            self.names.subscribe(),
        )
        .await;
        let running = match restored.and_then(|restored| {
            if self.shutdown.is_cancelled() {
                Err(ApplicationError::Stopped)
            } else {
                start(&self.config, restored, self.capacity.clone())
            }
        }) {
            Ok(running) => running,
            Err(_) => {
                action.complete(Err(if self.shutdown.is_cancelled() {
                    WorkspaceActionError::Stopped
                } else {
                    WorkspaceActionError::Restore
                }));
                return Ok(());
            }
        };
        let current = current_session(
            &name,
            &running,
            self.current
                .borrow()
                .as_ref()
                .and_then(|s| s.identity.clone()),
        );
        // Reconstruction and candidate actor creation succeeded before old ownership is changed.
        let active = self
            .active
            .as_mut()
            .ok_or(ApplicationError::MissingSession)?;
        let old_running = std::mem::replace(&mut active.running, running);
        active.name = name;
        self.current.send_replace(Some(current));
        self.publish_sessions();
        self.release_plans.clear();
        self.work_plans.clear();
        action.complete(Ok(())); // Releases the old session's action waiter before joined retirement.
        let retired_session = stop_running(old_running).await;
        drop(pause);
        retired_session
    }
}
fn file_error(error: BackendError) -> WorkspaceActionError {
    match error {
        BackendError::Missing => WorkspaceActionError::Missing,
        BackendError::Published(_) => WorkspaceActionError::Published,
        _ => WorkspaceActionError::Storage,
    }
}

pub mod terminal;

/// Desktop startup timing without coupling the shell to engine internals.
pub fn diagnostics_startup() -> wes_engine::diagnostics::Operation {
    wes_engine::diagnostics::Operation::start("startup")
}
