//! Session-scoped terminals over transport-independent execution endpoints. Closing a view never closes a terminal.
pub mod assistant;
mod bridge;
mod commands;
mod history;
#[cfg(unix)]
mod locale;
#[cfg(unix)]
mod path;
#[cfg(windows)]
mod powershell;
#[cfg(any(unix, windows))]
mod prompt;
#[cfg(windows)]
mod published;
pub(crate) mod ui;
use crate::{ApplicationHandle, CurrentSession};
pub use bridge::{BridgeReply, BridgeRequest, client};
use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeMap, VecDeque},
    io,
    path::PathBuf,
    sync::{Arc, Mutex, mpsc},
    time::Duration,
};
use tokio_util::{sync::CancellationToken, task::TaskTracker};
use wes_adapters::execution_targets::{HostLaunch, TerminalPlan};
use wes_engine::execution::{TargetLease, TerminalIo, TerminalSize};

/// Missing and differently owned IDs deliberately have the same public response.
#[derive(Debug)]
pub(crate) struct Unavailable;
impl std::fmt::Display for Unavailable {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Terminal session is no longer available. Start a new terminal to continue.")
    }
}
impl std::error::Error for Unavailable {}

#[derive(Clone)]
pub struct Config {
    pub api_library: Option<crate::api_library::ApiLibrary>,
    /// Selected data home; never accepted as a filesystem path from terminal requests.
    pub history_home: Option<PathBuf>,
    pub history: Option<String>,
    pub cwd: PathBuf,
    pub executable: PathBuf,
    /// Browser clients expire when polling stops; the native host owns desktop lifetime.
    pub idle_timeout: Option<Duration>,
}
#[derive(Clone)]
pub struct Manager {
    sessions: Arc<Mutex<BTreeMap<String, Arc<TerminalSession>>>>,
    capacity: Arc<tokio::sync::Semaphore>,
    tasks: TaskTracker,
    histories: Arc<tokio::sync::Mutex<history::Registry>>,
}
impl Default for Manager {
    fn default() -> Self {
        Self {
            sessions: Arc::default(),
            capacity: Arc::new(tokio::sync::Semaphore::new(
                wes_budgets::get("ui.terminal.tabs") as usize,
            )),
            tasks: TaskTracker::new(),
            histories: Arc::default(),
        }
    }
}
struct Output {
    bytes: VecDeque<u8>,
    start: u64,
    exit: Option<u32>,
    problem: Option<String>,
    destination: Option<String>,
}
enum Input {
    Write(Vec<u8>),
    Resize(u16, u16),
}
pub(super) struct TerminalSession {
    api_library: Option<crate::api_library::ApiLibrary>,
    library_tasks: TaskTracker,
    library_capacity: Arc<tokio::sync::Semaphore>,
    assistant: assistant::State,
    ui: ui::Broker,
    commands: commands::Commands,
    current: CurrentSession,
    client: String,
    environment: Option<String>,
    token: String,
    workspace_tools: bool,
    stopped: CancellationToken,
    finished: CancellationToken,
    input: mpsc::SyncSender<Input>,
    output: Mutex<Output>,
    changed: tokio::sync::Notify,
    reader: tokio::sync::Semaphore,
    directory: tempfile::TempDir,
    _permit: tokio::sync::OwnedSemaphorePermit,
    heartbeat: Mutex<std::time::Instant>,
}
impl TerminalSession {
    fn check(&self, application: &ApplicationHandle) -> io::Result<()> {
        if self.stopped.is_cancelled()
            || !application
                .session_for_generation(&self.current.generation)
                .is_ok()
        {
            return Err(io::Error::other("Terminal session has ended."));
        }
        Ok(())
    }
    fn append(&self, bytes: &[u8]) {
        let mut out = self.output.lock().expect("terminal output");
        out.bytes.extend(bytes);
        let extra = out.bytes.len().saturating_sub(1024 * 1024);
        out.bytes.drain(..extra);
        out.start += extra as u64;
        drop(out);
        self.changed.notify_waiters();
    }
}
#[derive(Serialize)]
pub struct Frame {
    pub start: u64,
    pub next: u64,
    pub data: String,
    pub exit: Option<u32>,
    pub problem: Option<String>,
    pub destination: Option<String>,
    pub closed: bool,
    pub ui: Option<ui::Request>,
    pub command: Option<commands::CommandRequest>,
}
/// Holds terminal-start admission through workspace retirement; independent pane
/// history remains app-owned and is not guessed from workspace names.
pub(crate) struct WorkspaceTerminals {
    _admission: tokio::sync::OwnedMutexGuard<history::Registry>,
    sessions: Vec<(String, Arc<TerminalSession>)>,
    registry: Arc<Mutex<BTreeMap<String, Arc<TerminalSession>>>>,
}
impl WorkspaceTerminals {
    pub(crate) fn ids(&self) -> Vec<String> {
        self.sessions.iter().map(|(id, _)| id.clone()).collect()
    }
    pub(crate) async fn stop(&self) -> io::Result<()> {
        for (_, s) in &self.sessions {
            s.stopped.cancel();
        }
        for (_, s) in &self.sessions {
            s.finished.cancelled().await;
        }
        let mut registry = self
            .registry
            .lock()
            .map_err(|_| io::Error::other("Terminal registry unavailable"))?;
        let mut failed = false;
        for (id, s) in &self.sessions {
            failed |= s
                .output
                .lock()
                .map_err(|_| io::Error::other("Terminal status unavailable"))?
                .problem
                .is_some();
            registry.remove(id);
        }
        if failed {
            Err(io::Error::other(
                "A terminal's external completion is uncertain",
            ))
        } else {
            Ok(())
        }
    }
}
impl Manager {
    pub(crate) async fn workspace_terminals(&self, generation: &str) -> WorkspaceTerminals {
        let admission = self.histories.clone().lock_owned().await;
        let sessions = self
            .sessions
            .lock()
            .expect("terminal registry")
            .iter()
            .filter(|(_, s)| s.current.generation == generation)
            .map(|(id, s)| (id.clone(), s.clone()))
            .collect();
        WorkspaceTerminals {
            _admission: admission,
            sessions,
            registry: self.sessions.clone(),
        }
    }
    pub fn has_active_sessions(&self) -> bool {
        self.sessions
            .lock()
            .expect("terminal registry")
            .values()
            .any(|s| {
                !s.stopped.is_cancelled()
                    && s.output.lock().expect("terminal output").exit.is_none()
            })
    }

    pub async fn start(
        &self,
        config: Config,
        current: CurrentSession,
        client: String,
        port: u16,
        application: ApplicationHandle,
    ) -> io::Result<String> {
        self.start_target(config, current, client, port, application, None)
            .await
    }
    pub async fn start_target(
        &self,
        config: Config,
        current: CurrentSession,
        client: String,
        port: u16,
        application: ApplicationHandle,
        target: Option<TargetLease>,
    ) -> io::Result<String> {
        let plan = match &target {
            Some(lease) => TerminalPlan::for_target(&lease.target).map_err(io::Error::other)?,
            None => TerminalPlan::workspace(),
        };
        let workspace_tools = plan.support().workspace_tools;
        let authority = target
            .as_ref()
            .map(|lease| lease.cancelled.clone())
            .unwrap_or_default();
        if authority.is_cancelled() {
            return Err(io::Error::other("Terminal target authority has ended"));
        }
        // Serialize history ownership through process startup and forget. Visual/client IDs
        // are ephemeral; only the explicitly saved opaque history key survives a restart.
        let mut histories = self.histories.lock().await;
        application
            .session_for_generation(&current.generation)
            .map_err(|_| io::Error::other("Workspace is no longer open"))?;
        if histories.closing {
            return Err(io::Error::other("Terminal host is shutting down."));
        }
        let history_key = config.history.as_deref().map(history::key).transpose()?;
        if let Some(key) = &history_key {
            if histories.forgotten.contains(key) {
                return Err(io::Error::other(
                    "This terminal pane history was forgotten. Open a new pane.",
                ));
            }
            if let Some(previous) = histories.active.get(key).and_then(std::sync::Weak::upgrade) {
                if !previous.stopped.is_cancelled() {
                    return Err(io::Error::other("This terminal pane is already running."));
                }
                previous.finished.cancelled().await;
            }
        }
        self.sessions
            .lock()
            .expect("terminal registry")
            .retain(|_, s| !s.stopped.is_cancelled());
        let permit = self
            .capacity
            .clone()
            .try_acquire_owned()
            .map_err(|_| io::Error::other("Four terminal sessions are already open."))?;
        if client.is_empty() || client.len() > 128 || client.chars().any(char::is_control) {
            return Err(io::Error::other("Invalid terminal client."));
        }
        let id = uuid::Uuid::new_v4().to_string();
        let token = format!("{}{}", uuid::Uuid::new_v4(), uuid::Uuid::new_v4());
        let (send, receive) = mpsc::sync_channel(64);
        let directory = tempfile::Builder::new().prefix("wes-terminal-").tempdir()?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(directory.path(), std::fs::Permissions::from_mode(0o700))?;
        }
        let terminal = Arc::new(TerminalSession {
            api_library: config.api_library.clone(),
            library_tasks: self.tasks.clone(),
            library_capacity: Arc::new(tokio::sync::Semaphore::new(2)),
            assistant: assistant::State::new(history_key.as_deref()),
            ui: ui::Broker::default(),
            commands: commands::Commands::default(),
            current,
            client,
            environment: target.as_ref().map(|lease| lease.environment.clone()),
            token,
            workspace_tools,
            stopped: CancellationToken::new(),
            finished: CancellationToken::new(),
            input: send,
            changed: tokio::sync::Notify::new(),
            reader: tokio::sync::Semaphore::new(1),
            output: Mutex::new(Output {
                bytes: VecDeque::new(),
                start: 0,
                exit: None,
                problem: None,
                destination: None,
            }),
            directory,
            _permit: permit,
            heartbeat: Mutex::new(std::time::Instant::now()),
        });
        let observation = terminal
            .current
            .session
            .observe()
            .await
            .map_err(io::Error::other)?;
        let names = bridge::terminal_catalogue(&observation, &terminal)
            .map(|c| c.provider_names().map(str::to_owned).collect::<Vec<_>>())
            .unwrap_or_default();
        let environment = bridge::terminal_context(&observation, &terminal)
            .and_then(|context| context.selected)
            .unwrap_or_else(|| "no-env".into());
        let initial_environment = environment.clone();
        let idle_timeout = config.idle_timeout;
        let worker = terminal.clone();
        let (started, ready) = tokio::sync::oneshot::channel();
        if let Some(key) = history_key {
            histories.active.insert(key, Arc::downgrade(&terminal));
        }
        self.tasks.spawn_blocking(move || {
            let _captured_target = target;
            let result = run(
                worker.clone(),
                config,
                names,
                initial_environment,
                port,
                receive,
                started,
                plan,
                authority,
            );
            if let Err(error) = result {
                let mut output = worker.output.lock().expect("terminal output");
                if output.problem.is_none() {
                    output.problem = Some(format!("Terminal transport failed: {error}"));
                }
            }
            worker.stopped.cancel();
            worker.finished.cancel();
        });
        ready
            .await
            .map_err(|_| io::Error::other("Could not start terminal process."))??;
        self.sessions
            .lock()
            .expect("terminal registry")
            .insert(id.clone(), terminal.clone());
        let updates = terminal.clone();
        let registry = self.sessions.clone();
        let watch_id = id.clone();
        self.tasks.spawn(async move {
            let mut displayed_environment = environment;
            loop {
                tokio::select! { _ = updates.stopped.cancelled() => break, _ = tokio::time::sleep(Duration::from_millis(400)) => {} }
                if updates.check(&application).is_err() || idle_timeout.is_some_and(|timeout| updates.heartbeat.lock().expect("terminal heartbeat").elapsed() > timeout) { updates.stopped.cancel(); registry.lock().expect("terminal registry").remove(&watch_id); break; }
                // New aliases become real executables. Removed aliases remain harmless stubs whose
                // call-time resolution fails; we never reuse an old provider binding.
                if updates.workspace_tools && let Ok(observation) = updates.current.session.observe().await {
                    let names = bridge::terminal_catalogue(&observation, &updates).map(|c| c.provider_names().map(str::to_owned).collect::<Vec<_>>()).unwrap_or_default();
                    let environment = bridge::terminal_context(&observation, &updates)
                        .and_then(|context| context.selected).unwrap_or_else(|| "no-env".into());
                    let changed = environment != displayed_environment;
                    let next = environment.clone();
                    let path = updates.directory.path().to_path_buf();
                    let published = tokio::task::spawn_blocking(move || {
                        #[cfg(any(unix, windows))]
                        if changed { prompt::publish_environment(&path, &next)?; }
                        let _ = aliases(&path, names);
                        Ok::<_, io::Error>(())
                    }).await;
                    if matches!(published, Ok(Ok(()))) { displayed_environment = environment; }
                }
            }
        });
        Ok(id)
    }
    fn owned(&self, id: &str, generation: &str, client: &str) -> io::Result<Arc<TerminalSession>> {
        self.sessions
            .lock()
            .expect("terminal registry")
            .get(id)
            .filter(|s| s.current.generation == generation && s.client == client)
            .cloned()
            .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, Unavailable))
    }
    pub fn poll(&self, id: &str, generation: &str, client: &str, cursor: u64) -> io::Result<Frame> {
        let terminal = self.owned(id, generation, client)?;
        *terminal.heartbeat.lock().expect("terminal heartbeat") = std::time::Instant::now();
        Ok(Self::frame(&terminal, cursor))
    }
    /// Existing readers can still poll immediately. The UI waits for actual changes instead
    /// of imposing a fixed delay on each chunk. Only one waiting reader owns this terminal.
    pub async fn poll_wait(
        &self,
        id: &str,
        generation: &str,
        client: &str,
        cursor: u64,
        wait_ms: u64,
    ) -> io::Result<Frame> {
        if wait_ms > 1000 {
            return Err(io::Error::other("Terminal wait exceeds 1000 ms."));
        }
        if wait_ms == 0 {
            return self.poll(id, generation, client, cursor);
        }
        let terminal = self.owned(id, generation, client)?;
        let _reader = terminal
            .reader
            .try_acquire()
            .map_err(|_| io::Error::other("A terminal output reader is already waiting."))?;
        *terminal.heartbeat.lock().expect("terminal heartbeat") = std::time::Instant::now();
        let changed = terminal.changed.notified();
        tokio::pin!(changed);
        // Register before inspecting state: output/editor changes cannot fall between the
        // empty check and subscription. notify_waiters also avoids stale queued permits.
        changed.as_mut().enable();
        let frame = Self::frame(&terminal, cursor);
        if frame.start != cursor
            || frame.next != cursor
            || frame.closed
            || frame.problem.is_some()
            || frame.ui.is_some()
            || frame.command.is_some()
        {
            return Ok(frame);
        }
        tokio::select! {
            _ = changed => {},
            _ = terminal.finished.cancelled() => {},
            _ = tokio::time::sleep(Duration::from_millis(wait_ms)) => {},
        }
        Ok(Self::frame(&terminal, cursor))
    }
    fn frame(terminal: &TerminalSession, cursor: u64) -> Frame {
        use base64::Engine;
        let out = terminal.output.lock().expect("terminal output");
        let start = cursor
            .max(out.start)
            .min(out.start + out.bytes.len() as u64);
        let bytes: Vec<_> = out
            .bytes
            .iter()
            .skip((start - out.start) as usize)
            .take(64 * 1024)
            .copied()
            .collect();
        Frame {
            start,
            next: start + bytes.len() as u64,
            data: base64::engine::general_purpose::STANDARD.encode(bytes),
            exit: out.exit,
            problem: out.problem.clone(),
            destination: out.destination.clone(),
            closed: terminal.finished.is_cancelled(),
            ui: terminal.ui.peek(),
            command: terminal.commands.peek(),
        }
    }
    pub fn command_claim(
        &self,
        id: &str,
        generation: &str,
        client: &str,
        request: &str,
    ) -> io::Result<bool> {
        let terminal = self.owned(id, generation, client)?;
        if terminal.stopped.is_cancelled() {
            return Err(io::Error::other("Terminal ended."));
        }
        Ok(terminal.commands.claim(request))
    }
    pub fn command_reply(
        &self,
        id: &str,
        generation: &str,
        client: &str,
        request: &str,
        error: Option<String>,
    ) -> io::Result<()> {
        if error.as_ref().is_some_and(|s| s.len() > 4096) {
            return Err(io::Error::other("Command error too long."));
        }
        self.owned(id, generation, client)?
            .commands
            .finish(request, error);
        Ok(())
    }
    pub fn ui_reply(
        &self,
        id: &str,
        generation: &str,
        client: &str,
        request: &str,
        result: serde_json::Value,
    ) -> io::Result<()> {
        self.owned(id, generation, client)?
            .ui
            .finish(request, result);
        Ok(())
    }
    pub fn write(&self, id: &str, generation: &str, client: &str, text: String) -> io::Result<()> {
        if text.len() > 16 * 1024 {
            return Err(io::Error::other("Terminal input exceeds 16 KiB."));
        }
        let terminal = self.owned(id, generation, client)?;
        if terminal.stopped.is_cancelled() {
            return Err(io::Error::other("Terminal has ended."));
        }
        terminal
            .input
            .try_send(Input::Write(text.into_bytes()))
            .map_err(|_| {
                io::Error::other("Terminal input queue is full or closed; input was not queued.")
            })
    }
    pub fn resize(
        &self,
        id: &str,
        generation: &str,
        client: &str,
        cols: u16,
        rows: u16,
    ) -> io::Result<()> {
        if !(2..=500).contains(&cols) || !(2..=300).contains(&rows) {
            return Err(io::Error::other("Invalid terminal dimensions."));
        }
        self.owned(id, generation, client)?
            .input
            .try_send(Input::Resize(cols, rows))
            .map_err(io::Error::other)
    }
    pub fn close(&self, id: &str, generation: &str, client: &str) -> io::Result<()> {
        let terminal = self.owned(id, generation, client)?;
        terminal.stopped.cancel();
        self.sessions.lock().expect("terminal registry").remove(id);
        Ok(())
    }
    pub async fn close_joined(
        &self,
        id: &str,
        generation: &str,
        client: &str,
    ) -> io::Result<Option<String>> {
        let terminal = self.owned(id, generation, client)?;
        terminal.stopped.cancel();
        terminal.finished.cancelled().await;
        self.sessions.lock().expect("terminal registry").remove(id);
        let problem = terminal
            .output
            .lock()
            .expect("terminal output")
            .problem
            .clone();
        Ok(problem)
    }
    /// Explicit pane deletion only. Ordinary process close/shutdown retains its history.
    pub async fn forget(&self, home: &std::path::Path, key: &str, client: &str) -> io::Result<()> {
        let key = history::key(key)?;
        let mut histories = self.histories.lock().await;
        let active = histories
            .active
            .get(&key)
            .and_then(std::sync::Weak::upgrade);
        if let Some(terminal) = &active {
            if !terminal.stopped.is_cancelled() && terminal.client != client {
                return Err(io::Error::other(
                    "This terminal pane is owned by another client.",
                ));
            }
        }
        // Tombstone before waiting: delayed starts can never resurrect a forgotten pane.
        histories.forgotten.insert(key.clone());
        if let Some(terminal) = active {
            terminal.stopped.cancel();
            terminal.finished.cancelled().await;
        }
        histories.active.remove(&key);
        let home = home.to_owned();
        tokio::task::spawn_blocking(move || history::forget(&home, &key))
            .await
            .map_err(io::Error::other)?
    }
    pub async fn bridge(
        &self,
        token: &str,
        request: BridgeRequest,
        application: ApplicationHandle,
    ) -> BridgeReply {
        let terminal = self
            .sessions
            .lock()
            .expect("terminal registry")
            .values()
            .find(|s| s.workspace_tools && s.token == token)
            .cloned();
        let Some(terminal) = terminal else {
            return BridgeReply::error(3, "Terminal authority is unavailable.");
        };
        match tokio::time::timeout(Duration::from_secs(180), async {
            if request.tool == "wes-agent" {
                assistant::dispatch(terminal, application, &request.args).await
            } else {
                bridge::dispatch(terminal, application, request).await
            }
        })
        .await
        {
            Ok(reply) => reply,
            Err(_) => BridgeReply::error(
                3,
                "Timed out awaiting the engine; the call may still run. No retry was made. Inspect the workspace before repeating it.",
            ),
        }
    }
    pub async fn shutdown(&self) {
        let mut histories = self.histories.lock().await;
        histories.closing = true;
        for terminal in self.sessions.lock().expect("terminal registry").values() {
            terminal.stopped.cancel();
        }
        self.tasks.close();
        drop(histories);
        self.tasks.wait().await;
        self.sessions.lock().expect("terminal registry").clear();
    }
}
fn alias_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 128
        && !name.starts_with('.')
        && name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
}
fn shell_quote(text: &str) -> String {
    format!("'{}'", text.replace('\'', "'\\''"))
}
#[cfg(unix)]
fn aliases(directory: &std::path::Path, names: Vec<String>) -> io::Result<()> {
    use std::os::unix::fs::symlink;
    for name in names.into_iter().take(1024) {
        if alias_name(&name)
            && name != "wes-provider"
            && name != "wes-value"
            && name != "wes-mcp"
            && name != "wesx"
        {
            match symlink("wes-provider", directory.join(name)) {
                Ok(()) => {}
                Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {}
                Err(e) => return Err(e),
            }
        }
    }
    Ok(())
}
/// What a platform contributes to a pane: the shell to start, with its own arguments,
/// startup files, inherited environment and search path, and the name its history is kept
/// under. Everything a pane has on every platform is decided once, in `prepare_host`.
#[cfg(any(unix, windows))]
struct Shell {
    history: &'static str,
    command: HostLaunch,
}

/// The pane's shell and the workspace around it. One owner for what does not depend on the
/// platform: the control directory, the assistant configuration, the selected environment's
/// label, the pane's private history and its budget, the bridge and its token, and the
/// workspace identity.
#[cfg(any(unix, windows))]
fn prepare_host(
    terminal: &TerminalSession,
    config: Config,
    names: Vec<String>,
    environment: String,
    port: u16,
) -> io::Result<HostLaunch> {
    let directory = terminal.directory.path();
    let bridge = format!("http://127.0.0.1:{port}/terminal-bridge");
    // The control directory wins search-path collisions; provider names come after the
    // host's own programs.
    std::fs::create_dir(directory.join("assistants"))?;
    publish(directory, &config.executable, names)?;
    assistant::prepare(directory, &config.executable, &bridge, &terminal.token)?;
    prompt::publish_environment(directory, &environment)?;
    let Shell {
        history,
        mut command,
    } = shell(directory)?;
    command.cwd = Some(config.cwd);
    if let Some(key) = &config.history {
        let home = config
            .history_home
            .as_deref()
            .ok_or_else(|| io::Error::other("Terminal history is unavailable in this server."))?;
        command.env("WES_HISTORY_FILE", history::prepare(home, key, history)?);
        // The same budget the pane's history is validated against when it is opened again.
        command.env(
            "WES_HISTORY_BYTES",
            wes_budgets::get("terminal.history.bytes").to_string(),
        );
    }
    // Resolved from the host's own search path: a provider named git is not a prompt helper.
    command.env("WES_PROMPT_GIT", prompt::git().unwrap_or_default());
    command.env(
        "WES_PROMPT_ENVIRONMENT",
        prompt::environment_file(directory),
    );
    command.env("TERM", "xterm-256color");
    command.env("WES_BRIDGE_URL", &bridge);
    command.env("WES_BRIDGE_TOKEN", &terminal.token);
    command.env("WES_WORKSPACE", terminal.current.name.as_str());
    command.env("WES_ASSISTANT_DIRECTORY", directory);
    command.env("WES_MCP_CONFIG", directory.join("mcp.json"));
    Ok(command)
}

/// The workspace commands as scripts that hand their own name to the bridge.
#[cfg(unix)]
fn publish(
    directory: &std::path::Path,
    executable: &std::path::Path,
    names: Vec<String>,
) -> io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    let script = format!(
        "#!/bin/sh\nexec {} --terminal-bridge \"${{0##*/}}\" \"$@\"\n",
        shell_quote(&executable.to_string_lossy())
    );
    for name in ["wes-provider", "wes-value", "assistants/wesx"] {
        let path = directory.join(name);
        std::fs::write(&path, &script)?;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700))?;
    }
    aliases(directory, names)
}
/// zsh where the system has it, bash otherwise, with a private startup file and none of the
/// user's.
#[cfg(unix)]
fn shell(directory: &std::path::Path) -> io::Result<Shell> {
    let shell = if std::path::Path::new("/bin/zsh").exists() {
        "/bin/zsh"
    } else {
        "/bin/bash"
    };
    let zsh = shell.ends_with("zsh");
    let bootstrap = prompt::prepare(directory, zsh)?;
    let mut command = HostLaunch {
        executable: shell.into(),
        ..Default::default()
    };
    if zsh {
        command.arg("-d");
    } else {
        command.arg("--noprofile");
        command.arg("--rcfile");
        command.arg(&bootstrap);
    }
    command.arg("-i");
    if zsh {
        command.env("ZDOTDIR", &bootstrap);
    }
    for (key, value) in locale::environment(std::env::vars_os()) {
        command.env(key, value);
    }
    // zsh emits an inverse '%' to mark partial lines before its prompt. Initial PTY
    // resizing can leave this synthetic marker visible; ordinary program output is untouched.
    if zsh {
        command.env("PROMPT_EOL_MARK", "");
    }
    command.env(
        "PATH",
        path::terminal(
            std::env::var_os("PATH").as_deref(),
            std::env::var_os("HOME").as_deref(),
            directory,
        )?,
    );
    // Enables colorized `ls` output without loading the user's rc; a display-only flag, no code runs.
    command.env("CLICOLOR", "1");
    command.env("SHELL", shell);
    Ok(Shell {
        history: if zsh { "zsh" } else { "bash" },
        command,
    })
}

fn run(
    terminal: Arc<TerminalSession>,
    config: Config,
    names: Vec<String>,
    environment: String,
    port: u16,
    input: mpsc::Receiver<Input>,
    started: tokio::sync::oneshot::Sender<io::Result<()>>,
    plan: TerminalPlan,
    authority: CancellationToken,
) -> io::Result<()> {
    let opening = (|| {
        let host = if plan.support().workspace_tools {
            Some(prepare_host(&terminal, config, names, environment, port)?)
        } else {
            None
        };
        if terminal.stopped.is_cancelled() {
            return Err(io::Error::other("Terminal access ended before launch"));
        }
        plan.open(host, TerminalSize { cols: 80, rows: 24 }, &authority)
    })();
    let mut endpoint = match opening {
        Ok(endpoint) => endpoint,
        Err(error) => {
            let _ = started.send(Err(io::Error::other(error.to_string())));
            return Err(error);
        }
    };
    terminal.output.lock().expect("terminal output").destination = endpoint.identity();
    if started.send(Ok(())).is_err() {
        terminal.stopped.cancel();
    }
    let result = pump(
        &terminal.stopped,
        endpoint.as_mut(),
        &input,
        &authority,
        |bytes| terminal.append(bytes),
    );
    terminal.stopped.cancel();
    let exit = endpoint.shutdown()?;
    let mut output = terminal.output.lock().expect("terminal output");
    output.exit = Some(exit.code);
    if exit.uncertain {
        output.problem=Some("ENV036: Terminal connection ended; remote processes may have run or still be running. No reconnect or remote cleanup is claimed.".into());
    }
    result
}

/// One bounded input/output/lifetime loop for every transport. No target or OS dispatch.
fn pump(
    stopped: &CancellationToken,
    endpoint: &mut dyn TerminalIo,
    input: &mpsc::Receiver<Input>,
    authority: &CancellationToken,
    mut append: impl FnMut(&[u8]),
) -> io::Result<()> {
    let mut queued = VecDeque::new();
    let mut buffer = [0u8; 16384];
    loop {
        if stopped.is_cancelled() || authority.is_cancelled() {
            break;
        }
        if queued.is_empty() {
            for _ in 0..64 {
                match input.try_recv() {
                    Ok(Input::Write(bytes)) => {
                        queued.extend(bytes);
                        break;
                    }
                    Ok(Input::Resize(cols, rows)) => {
                        endpoint.resize(TerminalSize { cols, rows })?
                    }
                    Err(_) => break,
                }
            }
        }
        if !queued.is_empty() {
            let (bytes, _) = queued.as_slices();
            match endpoint.write(bytes) {
                Ok(n) => {
                    queued.drain(..n);
                }
                Err(e) if e.kind() == io::ErrorKind::WouldBlock => {}
                Err(e) => return Err(e),
            }
        }
        for _ in 0..16 {
            match endpoint.read(&mut buffer) {
                Ok(0) => break,
                Ok(n) => append(&buffer[..n]),
                Err(e) if e.kind() == io::ErrorKind::WouldBlock => break,
                Err(e) => return Err(e),
            }
        }
        if endpoint.try_exit()?.is_some() {
            break;
        }
        endpoint.wait_ready(!queued.is_empty(), Duration::from_millis(10))?;
    }
    Ok(())
}

#[cfg(windows)]
fn aliases(directory: &std::path::Path, names: Vec<String>) -> io::Result<()> {
    published::aliases(directory, names)
}
/// The workspace commands as programs: this executable under each command's name.
#[cfg(windows)]
fn publish(
    directory: &std::path::Path,
    executable: &std::path::Path,
    names: Vec<String>,
) -> io::Result<()> {
    published::prepare(directory, executable)?;
    published::aliases(directory, names)
}
/// Windows PowerShell with its own bootstrap, an allowlisted environment and the pane's
/// search path.
#[cfg(windows)]
fn shell(directory: &std::path::Path) -> io::Result<Shell> {
    let mut command = powershell::launch(directory)?;
    for (key, value) in powershell::environment(std::env::vars_os()) {
        command.env(key, value);
    }
    // Windows matches names without regard to case; an inherited "Path" must not survive
    // beside the one set here.
    command
        .environment
        .retain(|key, _| !key.eq_ignore_ascii_case("PATH"));
    command.env(
        "PATH",
        published::path(std::env::var_os("PATH").as_deref(), directory)?,
    );
    Ok(Shell {
        history: "powershell",
        command,
    })
}
#[cfg(not(any(unix, windows)))]
fn aliases(_: &std::path::Path, _: Vec<String>) -> io::Result<()> {
    Err(io::Error::other("Terminal requires Unix."))
}
#[cfg(not(any(unix, windows)))]
fn prepare_host(
    _: &TerminalSession,
    _: Config,
    _: Vec<String>,
    _: String,
    _: u16,
) -> io::Result<HostLaunch> {
    Err(io::Error::other(
        "Workspace shell preparation is not supported on this host",
    ))
}

#[cfg(all(test, unix))]
mod lifetime_tests;
#[cfg(all(test, windows))]
mod windows_tests;
