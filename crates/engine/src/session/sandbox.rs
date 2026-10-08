//! Authored definitions and joined volatile runtimes. There is no execution-storage port here.
use super::*;
use std::collections::BTreeMap;
use std::sync::Mutex;
use wes_core::Data;
use wes_language::{Expression, Statement, Value};
#[derive(Default)]
struct Names {
    definitions: Vec<String>,
    reserved: std::collections::BTreeSet<String>,
    requests: std::collections::BTreeSet<String>,
}
struct NameReservation<'a> {
    names: &'a Mutex<Names>,
    name: String,
}
impl Drop for NameReservation<'_> {
    fn drop(&mut self) {
        self.names
            .lock()
            .expect("sandbox names")
            .reserved
            .remove(&self.name);
    }
}

#[derive(Clone)]
pub struct Definition {
    pub name: String,
    pub source: String,
    pub owner: String,
    pub environment: Option<wes_core::environments::EnvironmentContext>,
}
/// Only authored configuration crosses this port. Implementations must replace atomically.
pub trait DefinitionStore: Send + Sync {
    fn load(&self) -> Result<Vec<Definition>, String>;
    fn save(&self, definitions: &[Definition]) -> Result<(), String>;
}
#[derive(Clone, Debug)]
pub struct Reply {
    pub name: String,
    pub data: Data,
    pub state: ObservationState,
    pub view: Option<(String, bool)>,
}
/// Control metadata, independent of arbitrary member values.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ObservationState {
    Active,
    Stopped,
    NotRun,
    Removed,
}
impl ObservationState {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Active => "active",
            Self::Stopped => "stopped",
            Self::NotRun => "not run",
            Self::Removed => "removed",
        }
    }
}
struct Running {
    handle: SessionHandle,
    task: SessionTask,
    notification: JoinHandle<()>,
}
impl Running {
    async fn stop(self, uncertainty: &std::sync::atomic::AtomicBool) -> Result<(), SessionError> {
        let _ = self.handle.shutdown().await;
        let joined = self.task.join().await;
        if joined.is_err() || self.handle.remote_outcome_uncertain() {
            uncertainty.store(true, std::sync::atomic::Ordering::Release);
        }
        self.notification.abort();
        let _ = self.notification.await;
        joined.map_err(|_| SessionError::Stopped)
    }
}
struct State {
    revision: u64,
    loaded: bool,
    definitions: BTreeMap<String, Definition>,
    running: BTreeMap<String, Running>,
    stopped: std::collections::BTreeSet<String>,
    requests: BTreeMap<String, (SourceInput, SubmissionReply)>,
}
pub(super) struct Sandboxes {
    uncertainty: Arc<std::sync::atomic::AtomicBool>,
    enabled: bool,
    loaded: std::sync::atomic::AtomicBool,
    store: Option<Arc<dyn DefinitionStore>>,
    state: tokio::sync::Mutex<State>,
    names: Mutex<Names>,
    closed: std::sync::atomic::AtomicBool,
    controls: mpsc::WeakSender<Control>,
    reader: Arc<dyn TypeSourceReader>,
    capacity: crate::driver::ExecutionCapacity,
    concurrency: NonZeroUsize,
    updates: broadcast::WeakSender<()>,
}
impl Sandboxes {
    pub(super) fn new(
        uncertainty: Arc<std::sync::atomic::AtomicBool>,
        enabled: bool,
        store: Option<Arc<dyn DefinitionStore>>,
        controls: mpsc::Sender<Control>,
        reader: Arc<dyn TypeSourceReader>,
        capacity: crate::driver::ExecutionCapacity,
        concurrency: NonZeroUsize,
        updates: broadcast::Sender<()>,
    ) -> Self {
        Self {
            uncertainty,
            enabled,
            store,
            controls: controls.downgrade(),
            reader,
            capacity,
            concurrency,
            updates: updates.downgrade(),
            loaded: false.into(),
            closed: false.into(),
            names: Mutex::new(Names::default()),
            state: tokio::sync::Mutex::new(State {
                revision: 0,
                loaded: false,
                definitions: BTreeMap::new(),
                running: BTreeMap::new(),
                stopped: Default::default(),
                requests: BTreeMap::new(),
            }),
        }
    }
    pub(super) fn names(&self) -> Vec<String> {
        let names = self.names.lock().expect("sandbox names");
        names
            .definitions
            .iter()
            .chain(names.reserved.iter())
            .cloned()
            .collect()
    }
    pub(super) fn definition_names(&self) -> Vec<String> {
        self.names
            .lock()
            .expect("sandbox names")
            .definitions
            .clone()
    }
    async fn load(&self, state: &mut State) -> Result<(), SessionError> {
        if state.loaded {
            return Ok(());
        }
        if let Some(store) = self.store.clone() {
            let definitions = tokio::task::spawn_blocking(move || store.load())
                .await
                .map_err(|_| SessionError::Preparation)?
                .map_err(|_| error("Sandbox definitions could not be loaded."))?;
            if definitions.len() > 64
                || definitions.iter().map(|d| d.source.len()).sum::<usize>() > 4 * 1024 * 1024
            {
                return Err(SessionError::Capacity);
            }
            for definition in definitions {
                validate_definition(&definition)?;
                if state
                    .definitions
                    .insert(definition.name.clone(), definition)
                    .is_some()
                {
                    return Err(error("Duplicate sandbox definition."));
                }
            }
        }
        self.names.lock().expect("sandbox names").definitions =
            state.definitions.keys().cloned().collect();
        state.loaded = true;
        self.loaded
            .store(true, std::sync::atomic::Ordering::Release);
        Ok(())
    }
    pub(super) async fn routes(&self, input: &SourceInput) -> Result<bool, SessionError> {
        if !self.enabled {
            return Ok(false);
        }
        if !self.loaded.load(std::sync::atomic::Ordering::Acquire) {
            let mut state = self.state.lock().await;
            self.load(&mut state).await?;
        }
        if self
            .names
            .lock()
            .expect("sandbox names")
            .requests
            .contains(input.cell())
        {
            return Ok(true);
        }
        let parsed = wes_language::parse(&wes_language::SourceText::new(
            "sandbox-route",
            input.text(),
        ));
        Ok(parsed
            .script
            .statements
            .iter()
            .any(|s| match &s.expression {
                Expression::Sandbox(_) => true,
                Expression::Call(call) if call.marker.is_some() => call
                    .operands
                    .iter()
                    .chain(call.arguments.iter().map(|a| &a.value))
                    .flat_map(Value::references)
                    .any(|n| {
                        self.names()
                            .iter()
                            .any(|name| n.text == *name || n.text.starts_with(&format!("{name}.")))
                    }),
                _ => false,
            }))
    }
    pub(super) fn intercept<'a>(
        self: &'a Arc<Self>,
        input: &'a SourceInput,
        source: &'a SourcePreparation<ParsedSource>,
    ) -> std::pin::Pin<
        Box<
            dyn std::future::Future<Output = Result<Option<SubmissionReply>, SessionError>>
                + Send
                + 'a,
        >,
    > {
        Box::pin(async move {
            if !self.enabled {
                return Ok(None);
            }
            if !self.routes(input).await? {
                return Ok(None);
            }
            let statement = match source {
                SourcePreparation::Declarations(parsed) if parsed.statements().len() == 1 => {
                    Some(&parsed.statements()[0])
                }
                SourcePreparation::Immediate { statement, .. } => Some(statement.as_ref()),
                _ => None,
            };
            let mut state = self
                .state
                .try_lock()
                .map_err(|_| SessionError::AdmissionBusy)?;
            self.load(&mut state).await?;
            if let Some((previous, reply)) = state.requests.get(input.cell()) {
                return if previous.same_request(input) {
                    Ok(Some(reply.clone()))
                } else {
                    Err(SessionError::Conflict)
                };
            }
            let Some(statement) = statement else {
                return Err(error(
                    "Submit a sandbox definition or object operation on its own; no mixed script was executed.",
                ));
            };
            let operation = match &statement.expression {
                Expression::Sandbox(body) => {
                    if statement.error_binding.is_some() || !statement.annotations.is_empty() {
                        return Err(error(
                            "Sandbox accepts a name, not error bindings or execution annotations.",
                        ));
                    }
                    let name = statement
                        .binding
                        .as_ref()
                        .ok_or(SessionError::CommandArgument(
                            "Name the sandbox with > name.",
                        ))?
                        .name
                        .text
                        .clone();
                    Operation::Define(
                        name,
                        body.iter()
                            .map(ToString::to_string)
                            .collect::<Vec<_>>()
                            .join("\n"),
                    )
                }
                Expression::Call(call) if call.marker.is_some() => {
                    let invocation = wes_language::vocabulary::commands::invocation(call)
                        .map_err(|d| preparation_error(&d, input.text()))?;
                    let Some(reference) = call
                        .operands
                        .iter()
                        .chain(call.arguments.iter().map(|a| &a.value))
                        .flat_map(Value::references)
                        .find(|name| {
                            state
                                .definitions
                                .contains_key(name.text.split('.').next().unwrap_or(&name.text))
                        })
                        .map(|name| name.text.as_str())
                    else {
                        return Ok(None);
                    };
                    let root = reference.split('.').next().unwrap_or(reference);
                    if !state.definitions.contains_key(root) {
                        return Ok(None);
                    }
                    if statement.binding.is_some()
                        || statement.error_binding.is_some()
                        || !statement.annotations.is_empty()
                    {
                        return Err(error(
                            "Sandbox observations are transient; binding or persistence requires an explicit export, which is not supported.",
                        ));
                    }
                    if call.arguments.iter().any(|a| a.key.text == "node") {
                        return Err(error(
                            "A sandbox is an object, not a node. Use :inspect $name or :read $name.member.",
                        ));
                    }
                    let removing = invocation.spec.command == MetaCommand::Drop;
                    let scope = call.arguments.iter().find(|a| a.key.text == "scope");
                    let valid = if removing {
                        reference == root && call.operands.len() == 1 && call.arguments.len() == 1 && scope.is_some_and(|a| matches!(&a.value, Value::Word(n) | Value::Text(n) if n.text == "downstream"))
                    } else {
                        call.operands.len() + call.arguments.len() == 1
                    };
                    if !valid {
                        return Err(error(if removing {
                            "Remove a whole sandbox with :remove $name scope:downstream; this joins its work and deletes only its definition."
                        } else {
                            "Sandbox operations require exactly one object reference."
                        }));
                    }
                    match invocation.spec.command {
                        MetaCommand::Drop => Operation::Remove(root.into()),
                        MetaCommand::Read => Operation::Read(reference.into(), false),
                        MetaCommand::Inspect => Operation::Read(reference.into(), true),
                        MetaCommand::Refresh if reference == root => {
                            Operation::Refresh(root.into())
                        }
                        MetaCommand::Cancel if reference == root => Operation::Stop(root.into()),
                        _ => {
                            return Err(error(
                                "Use read/inspect for sandbox members, or refresh/cancel/remove on the whole sandbox.",
                            ));
                        }
                    }
                }
                _ => return Ok(None),
            };
            if self.closed.load(std::sync::atomic::Ordering::Acquire) {
                return Err(SessionError::Stopped);
            }
            if state.requests.len() >= 10_000
                || state
                    .requests
                    .values()
                    .map(|(i, _)| i.text().len())
                    .sum::<usize>()
                    + input.text().len()
                    > 8 * 1024 * 1024
            {
                return Err(SessionError::Capacity);
            }
            // Reserve before leaving the caller: a lost reply is never permission to run again.
            state.requests.insert(
                input.cell().into(),
                (input.clone(), Err(SessionError::AdmissionBusy)),
            );
            self.names
                .lock()
                .expect("sandbox names")
                .requests
                .insert(input.cell().into());
            drop(state);
            let owner = self.clone();
            let input = input.clone();
            let span = statement.span;
            let task = tokio::spawn(async move {
                let mut state = owner.state.lock().await;
                if !matches!(&operation, Operation::Read(..)) {
                    state.revision = state.revision.saturating_add(1);
                }
                let result = owner
                    .perform(&mut state, &input, operation)
                    .await
                    .map(|reply| {
                        Arc::new(SubmissionResult {
                            receipts: vec![],
                            cell: input.cell().into(),
                            nodes: vec![],
                            refreshed: vec![],
                            accepted: vec![span],
                            removed: vec![],
                            unbound: vec![],
                            diagnostics: SourceDiagnostics::default(),
                            recorded: false,
                            restored: false,
                            repeated_run: None,
                            sandbox: Some(reply),
                        })
                    });
                // A request receipt must not retain snapshots of every read.
                let retained = match &result {
                    Ok(reply) if reply.sandbox.as_ref().is_some_and(|s| s.view.is_some()) => {
                        Err(SessionError::HistoricalReplyUnavailable)
                    }
                    _ => result.clone(),
                };
                owner.names.lock().expect("sandbox names").definitions =
                    state.definitions.keys().cloned().collect();
                state
                    .requests
                    .insert(input.cell().into(), (input, retained));
                if let Some(updates) = owner.updates.upgrade() {
                    let _ = updates.send(());
                }
                result
            });
            Ok(Some(task.await.map_err(|_| SessionError::Stopped)?))
        })
    }
    async fn capture(
        &self,
        input: &SourceInput,
        name: Option<String>,
    ) -> Result<Workspace, SessionError> {
        let (reply, receive) = oneshot::channel();
        self.controls
            .upgrade()
            .ok_or(SessionError::Stopped)?
            .send(Control::SandboxWorkspace {
                input: input.clone(),
                name,
                reply,
            })
            .await
            .map_err(|_| SessionError::Stopped)?;
        receive.await.map_err(|_| SessionError::Stopped)?
    }
    async fn perform(
        &self,
        state: &mut State,
        input: &SourceInput,
        operation: Operation,
    ) -> Result<Reply, SessionError> {
        let name = operation.name().to_owned();
        if input.is_cooperative()
            && state
                .definitions
                .get(&name)
                .is_some_and(|d| d.owner != input.client())
            && !matches!(operation, Operation::Read(_, _))
        {
            return Err(SessionError::Authority);
        }
        match operation {
            Operation::Define(_, source) => {
                if !state.definitions.contains_key(&name) && state.definitions.len() >= 64 {
                    return Err(SessionError::Capacity);
                }
                self.names
                    .lock()
                    .expect("sandbox names")
                    .reserved
                    .insert(name.clone());
                let _reservation = NameReservation {
                    names: &self.names,
                    name: name.clone(),
                };
                let mut workspace = self.capture(input, Some(name.clone())).await?;
                let environment = input
                    .environments()
                    .cloned()
                    .or_else(|| workspace.default_environment_context());
                let definition = Definition {
                    name: name.clone(),
                    source,
                    owner: input.client().into(),
                    environment,
                };
                validate_definition(&definition)
                    .map_err(|error| SessionError::AdmissionRefused(error.to_string()))?;
                // Semantic preparation is joined but not committed or executed before save.
                let prepare = run_input(&definition, input, "sandbox-check")?;
                let draft = workspace.draft().map_err(|_| SessionError::Preparation)?;
                let prepared = source::prepare_declarations(
                    prepare,
                    draft,
                    TypeSourceCapture::live(self.reader.clone()),
                    CancellationToken::new(),
                )
                .await
                .map_err(|_| SessionError::Preparation)?;
                if let SourcePreparation::Declarations(prepared) = &prepared {
                    if let Some(diagnostic) = prepared
                        .diagnostics()
                        .diagnostics
                        .iter()
                        .find(|d| d.severity == wes_language::Severity::Error)
                    {
                        return Err(preparation_error(diagnostic, &definition.source));
                    }
                } else {
                    return Err(SessionError::AdmissionRefused(
                        "Sandbox program cannot contain host controls or durable definition operations."
                            .into(),
                    ));
                }
                let mut definitions: Vec<_> = state
                    .definitions
                    .values()
                    .filter(|d| d.name != name)
                    .cloned()
                    .collect();
                definitions.push(definition.clone());
                if definitions.iter().map(|d| d.source.len()).sum::<usize>() > 4 * 1024 * 1024 {
                    return Err(SessionError::Capacity);
                }
                if let Some(store) = self.store.clone() {
                    tokio::task::spawn_blocking(move || store.save(&definitions))
                        .await
                        .map_err(|_| SessionError::Preparation)?
                        .map_err(|_| {
                            error(
                                "Sandbox definition could not be saved; no execution was started.",
                            )
                        })?;
                }
                state.definitions.insert(name.clone(), definition.clone());
                self.names.lock().expect("sandbox names").definitions =
                    state.definitions.keys().cloned().collect();
                if let Some(running) = state.running.remove(&name) {
                    running.stop(&self.uncertainty).await?;
                }
                self.start(state, &definition, input, &mut workspace)
                    .await?;
            }
            Operation::Refresh(_) => {
                let mut workspace = self.capture(input, None).await?;
                if let Some(running) = state.running.remove(&name) {
                    running.stop(&self.uncertainty).await?;
                }
                let definition = state.definitions[&name].clone();
                self.start(state, &definition, input, &mut workspace)
                    .await?;
            }
            Operation::Stop(_) => {
                if let Some(running) = state.running.remove(&name) {
                    running.stop(&self.uncertainty).await?;
                }
                state.stopped.insert(name.clone());
            }
            Operation::Remove(_) => {
                if let Some(running) = state.running.remove(&name) {
                    running.stop(&self.uncertainty).await?;
                }
                state.stopped.insert(name.clone());
                if self.uncertainty.load(std::sync::atomic::Ordering::Acquire) {
                    return Err(error(
                        "Sandbox work was stopped, but this workspace has an uncertain external outcome; the definition was retained. Review the workspace Logs before retrying or removing it.",
                    ));
                }
                let definitions: Vec<_> = state
                    .definitions
                    .values()
                    .filter(|d| d.name != name)
                    .cloned()
                    .collect();
                if let Some(store) = self.store.clone() {
                    tokio::task::spawn_blocking(move || store.save(&definitions))
                        .await
                        .map_err(|_| SessionError::Preparation)?
                        .map_err(|_| {
                            error("Sandbox was stopped but its definition could not be removed.")
                        })?;
                }
                state.definitions.remove(&name);
                state.stopped.remove(&name);
                return Ok(Reply {
                    name,
                    data: record([("kind", text("Sandbox")), ("state", text("removed"))]),
                    state: ObservationState::Removed,
                    view: None,
                });
            }
            Operation::Read(reference, inspect) => {
                return self.read(state, &reference, inspect, input).await;
            }
        }
        let observation_state = if state.running.contains_key(&name) {
            ObservationState::Active
        } else {
            ObservationState::Stopped
        };
        Ok(Reply {
            name,
            data: record([("kind", text("Sandbox")), ("state", text("accepted"))]),
            state: observation_state,
            view: None,
        })
    }
    async fn start(
        &self,
        state: &mut State,
        definition: &Definition,
        caller: &SourceInput,
        workspace: &mut Workspace,
    ) -> Result<(), SessionError> {
        if self.closed.load(std::sync::atomic::Ordering::Acquire) {
            return Err(SessionError::Stopped);
        }
        let child = std::mem::take(workspace);
        let recording = RecordingMode::Ephemeral;
        let (handle, task) = spawn_owned(
            child,
            recording.clone(),
            self.reader.clone(),
            self.concurrency,
            SessionSeed {
                capacity: Some(self.capacity.clone()),
                actions: None,
                cells: Cells::default(),
                log: SessionLog::new(&recording),
                values: None,
                restoration: None,
            },
        )?;
        let mut events = handle.subscribe_updates()?;
        let updates = self.updates.clone();
        let notification = tokio::spawn(async move {
            while !matches!(
                events.recv().await,
                Err(broadcast::error::RecvError::Closed)
            ) {
                if let Some(updates) = updates.upgrade() {
                    let _ = updates.send(());
                }
            }
        });
        let run = run_input(definition, caller, &uuid::Uuid::new_v4().to_string())?;
        let result = handle.submit(run).await;
        state.stopped.remove(&definition.name);
        state.running.insert(
            definition.name.clone(),
            Running {
                handle,
                task,
                notification,
            },
        );
        let result = result?;
        if let Some(diagnostic) = result
            .diagnostics
            .diagnostics
            .iter()
            .find(|d| d.severity == wes_language::Severity::Error)
        {
            return Err(preparation_error(diagnostic, &definition.source));
        }
        Ok(())
    }
    async fn read(
        &self,
        state: &State,
        reference: &str,
        inspect: bool,
        caller: &SourceInput,
    ) -> Result<Reply, SessionError> {
        let (name, member) = reference
            .split_once('.')
            .map_or((reference, None), |(a, b)| (a, Some(b)));
        if let Some(running) = state.running.get(name) {
            let (reply, receive) = oneshot::channel();
            running
                .handle
                .controls
                .send(Control::SandboxRead {
                    member: member.map(str::to_owned),
                    inspect,
                    cooperative: caller.is_cooperative(),
                    reply,
                })
                .await
                .map_err(|_| SessionError::Stopped)?;
            return Ok(Reply {
                name: name.into(),
                data: receive.await.map_err(|_| SessionError::Stopped)??,
                state: ObservationState::Active,
                view: Some((reference.into(), inspect)),
            });
        }
        let mut workspace = self.capture(caller, None).await?;
        let definition = &state.definitions[name];
        let input = run_input(definition, caller, "sandbox-inspect")?;
        let draft = workspace.draft().map_err(|_| SessionError::Preparation)?;
        let prepared = source::prepare_declarations(
            input,
            draft,
            TypeSourceCapture::live(self.reader.clone()),
            CancellationToken::new(),
        )
        .await
        .map_err(|_| SessionError::Preparation)?;
        let SourcePreparation::Declarations(prepared) = prepared else {
            return Err(error(
                "Sandbox definition is unavailable in the current environment.",
            ));
        };
        if prepared
            .diagnostics()
            .diagnostics
            .iter()
            .any(|d| d.severity == wes_language::Severity::Error)
        {
            return Err(error(
                "Sandbox definition has unavailable dependencies. No work was executed.",
            ));
        }
        // Installing an inert graph describes its contracts; no returned effects are dispatched.
        prepared
            .commit(&mut workspace, std::time::Duration::ZERO)
            .map_err(|_| SessionError::Preparation)?;
        Ok(Reply {
            name: name.into(),
            state: if state.stopped.contains(name) {
                ObservationState::Stopped
            } else {
                ObservationState::NotRun
            },
            data: observe(
                &workspace,
                member,
                inspect,
                caller.is_cooperative(),
                Some(if state.stopped.contains(name) {
                    "stopped"
                } else {
                    "not run"
                }),
            )?,
            view: Some((reference.into(), inspect)),
        })
    }
    pub(super) fn close(&self) {
        self.closed
            .store(true, std::sync::atomic::Ordering::Release);
    }
    pub(super) async fn observation(
        &self,
        reference: &str,
        inspect: bool,
    ) -> Result<Reply, SessionError> {
        if self.closed.load(std::sync::atomic::Ordering::Acquire) {
            return Err(SessionError::Stopped);
        }
        let mut state = self
            .state
            .try_lock()
            .map_err(|_| SessionError::AdmissionBusy)?;
        self.load(&mut state).await?;
        let name = reference.split('.').next().unwrap_or(reference);
        if !state.definitions.contains_key(name) {
            return Err(error("Sandbox does not exist."));
        }
        let caller = SourceInput::new("sandbox-view".into(), String::new())
            .map_err(|_| SessionError::Preparation)?;
        self.read(&state, reference, inspect, &caller).await
    }
    pub(super) async fn shutdown(&self) {
        self.close();
        let mut state = self.state.lock().await;
        for running in state.running.values() {
            let _ = running.handle.shutdown().await;
        }
        for (_, running) in std::mem::take(&mut state.running) {
            let _ = running.stop(&self.uncertainty).await;
        }
    }
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Lifecycle {
    pub revision: u64,
    pub definitions: Vec<String>,
    pub running: Vec<String>,
}
impl SessionHandle {
    pub async fn sandbox_lifecycle(&self) -> Result<Lifecycle, SessionError> {
        let mut state = self.sandboxes.state.lock().await;
        self.sandboxes.load(&mut state).await?;
        Ok(Lifecycle {
            revision: state.revision,
            definitions: state.definitions.keys().cloned().collect(),
            running: state.running.keys().cloned().collect(),
        })
    }
    /// Hold sandbox admission while the actor validates ordinary cells and seals
    /// its own source queue. On refusal neither runtime has been stopped or sealed.
    pub async fn stop_workspace(
        &self,
        environments: std::collections::BTreeMap<String, wes_core::environments::Revision>,
        cells: Vec<String>,
        revision: u64,
        stop: bool,
    ) -> Result<(), SessionError> {
        let state = self.sandboxes.state.lock().await;
        if state.revision != revision || (!stop && !state.running.is_empty()) {
            return Err(SessionError::CheckpointBusy);
        }
        let (reply, receive) = oneshot::channel();
        self.controls
            .send(Control::StopWorkspace {
                cells,
                environments,
                stop,
                reply,
            })
            .await
            .map_err(|_| SessionError::Stopped)?;
        receive.await.map_err(|_| SessionError::Stopped)?
    }

    pub async fn sandbox_receipt(&self, cell: &str) -> Option<SubmissionReply> {
        self.sandboxes
            .state
            .lock()
            .await
            .requests
            .get(cell)
            .map(|(_, reply)| reply.clone())
    }
    /// Finite export projection: private/unknown provenance is withheld before stdout receives it.
    pub async fn read_sandbox_export(
        &self,
        reference: &str,
        inspect: bool,
    ) -> Result<Reply, SessionError> {
        let mut state = self.sandboxes.state.lock().await;
        self.sandboxes.load(&mut state).await?;
        let name = reference.split('.').next().unwrap_or(reference);
        if !state.definitions.contains_key(name) {
            return Err(error("Sandbox does not exist."));
        }
        let revision = state.revision;
        let running = state
            .running
            .get(name)
            .map(|running| running.handle.clone());
        drop(state);
        if let Some(running) = running {
            running.wait_idle().await?;
        }
        let state = self.sandboxes.state.lock().await;
        if state.revision != revision {
            return Err(SessionError::HistoricalReplyUnavailable);
        }
        let input = SourceInput::new("sandbox-export".into(), String::new())
            .map_err(|_| SessionError::Preparation)?
            .cooperative();
        self.sandboxes
            .read(&state, reference, inspect, &input)
            .await
    }
    /// Trusted UI read port. No submission, request receipt, cell or provider execution.
    pub async fn read_sandbox(
        &self,
        reference: &str,
        inspect: bool,
    ) -> Result<Reply, SessionError> {
        self.sandboxes.observation(reference, inspect).await
    }
}
enum Operation {
    Define(String, String),
    Refresh(String),
    Stop(String),
    Remove(String),
    Read(String, bool),
}
impl Operation {
    fn name(&self) -> &str {
        match self {
            Self::Define(n, _) | Self::Refresh(n) | Self::Stop(n) | Self::Remove(n) => n,
            Self::Read(r, _) => r.split('.').next().unwrap_or(r),
        }
    }
}
fn error(message: &str) -> SessionError {
    SessionError::Sandbox(message.into())
}
pub(super) fn observe(
    workspace: &Workspace,
    member: Option<&str>,
    inspect: bool,
    cooperative: bool,
    dormant: Option<&str>,
) -> Result<Data, SessionError> {
    let mut members = vec![];
    let mut budget = 4 * 1024 * 1024;
    for (label, output) in workspace.bindings().names() {
        if member.is_some_and(|m| m.split('.').next() != Some(label.as_str())) {
            continue;
        }
        let Some(node) = workspace.runtime().graph().node(&output.node) else {
            continue;
        };
        let mut typing = workspace
            .typing(output)
            .unwrap_or_else(|| wes_core::capability::Typing::new(wes_core::Shape::Unknown));
        let value = match workspace.runtime().output(output) {
            crate::runtime::OutputState::Available(v) if dormant.is_none() => Some(v),
            _ => None,
        };
        let withheld = cooperative
            && (typing.provenance.policy().is_confidential()
                || typing.provenance.policy().is_unknown()
                || value.as_ref().is_some_and(|v| {
                    v.provenance().policy().is_confidential()
                        || v.provenance().policy().is_unknown()
                }));
        if withheld {
            if member.is_some() && !inspect {
                return Err(SessionError::Authority);
            }
            members.push(record([("name", text(label)), ("state", text("withheld"))]));
            continue;
        }
        let fields: Vec<_> = member
            .into_iter()
            .flat_map(|m| m.split('.').skip(1))
            .map(str::to_owned)
            .collect();
        if !fields.is_empty() {
            typing.shape = crate::plan::field_shape(&typing.shape, &fields).ok_or_else(|| {
                error("Sandbox member field is not described by its current type.")
            })?;
        }
        if !inspect && member.is_some() {
            let value = value.ok_or_else(|| {
                error("Sandbox member has no current value. Inspect it for its state.")
            })?;
            if !value.data().is_inline() {
                return Err(error(
                    "Sandbox member requires a bounded materialized observation.",
                ));
            }
            let mut data = value.data();
            for field in &fields {
                let Data::Record(record) = data else {
                    return Err(error("Sandbox member field is not a record."));
                };
                data = record
                    .get(field)
                    .ok_or_else(|| error("Sandbox member field is absent."))?;
            }
            crate::value_size::data_charge(data, budget).ok_or(SessionError::Capacity)?;
            return Ok(data.clone());
        }
        let mut row = indexmap::IndexMap::from([
            ("name".into(), text(label)),
            ("type".into(), text(typing.shape.to_string())),
            (
                "state".into(),
                text(if let Some(state) = dormant {
                    state.into()
                } else if workspace.runtime().is_streaming(&output.node) {
                    "streaming".into()
                } else {
                    format!("{:?}", node.state()).to_lowercase()
                }),
            ),
        ]);
        if dormant.is_none()
            && let Some(failure) = workspace.runtime().error_of(&output.node)
        {
            let error_value = failure.to_value();
            if !cooperative
                || (!error_value.provenance().policy().is_confidential()
                    && !error_value.provenance().policy().is_unknown())
            {
                row.insert("error".into(), error_value.data().clone());
            }
        }
        if !inspect && let Some(value) = value {
            let charge = crate::value_size::data_charge(value.data(), budget)
                .ok_or(SessionError::Capacity)?;
            budget = budget.checked_sub(charge).ok_or(SessionError::Capacity)?;
            row.insert("value".into(), value.data().clone());
        }
        members.push(Data::Record(row));
    }
    if member.is_some() && members.is_empty() {
        return Err(error("Sandbox member does not exist."));
    }
    let data = record([
        ("kind", text("Sandbox")),
        ("persistence", text("definition only")),
        ("state", text(dormant.unwrap_or("active"))),
        ("members", Data::List(members)),
    ]);
    crate::value_size::data_charge(&data, 4 * 1024 * 1024).ok_or(SessionError::Capacity)?;
    Ok(data)
}
fn text(value: impl Into<String>) -> Data {
    Data::Text(value.into().into())
}
fn record<const N: usize>(fields: [(&str, Data); N]) -> Data {
    Data::Record(fields.into_iter().map(|(k, v)| (k.into(), v)).collect())
}
fn run_input(
    definition: &Definition,
    caller: &SourceInput,
    id: &str,
) -> Result<SourceInput, SessionError> {
    let mut input = SourceInput::new(id.into(), definition.source.clone())
        .map_err(|_| SessionError::Preparation)?
        .with_source_name(format!("sandbox ${}", definition.name))
        .map_err(|_| SessionError::Preparation)?
        .with_client(caller.client().into())
        .map_err(|_| SessionError::Preparation)?;
    if caller.is_cooperative() {
        input = input.cooperative();
    }
    if let Some(environment) = &definition.environment {
        input = input
            .with_environments(environment.clone())
            .map_err(|_| SessionError::Preparation)?;
    }
    Ok(input)
}
fn validate_definition(definition: &Definition) -> Result<(), SessionError> {
    if definition.name.is_empty()
        || definition.name.len() > 128
        || !definition
            .name
            .chars()
            .all(|c| c.is_alphanumeric() || c == '_')
        || definition.source.len() > 64 * 1024
    {
        return Err(SessionError::Capacity);
    }
    let parsed = wes_language::parse(&wes_language::SourceText::new(
        "sandbox",
        &definition.source,
    ));
    if parsed
        .diagnostics
        .iter()
        .any(|d| d.severity == wes_language::Severity::Error)
    {
        return Err(error("Sandbox program has syntax errors."));
    }
    fn valid(statement: &Statement) -> bool {
        match &statement.expression {
            Expression::Sandbox(_) => false,
            Expression::Pipeline(stages) => stages.iter().all(valid),
            Expression::Fork(branches) => branches.iter().all(|b| valid(&b.body)),
            Expression::Call(call) if call.marker.is_some() => {
                wes_language::vocabulary::commands::invocation(call).is_ok_and(|i| {
                    matches!(
                        i.spec.command,
                        MetaCommand::Read
                            | MetaCommand::Inspect
                            | MetaCommand::List
                            | MetaCommand::Help
                            | MetaCommand::Info
                            | MetaCommand::Accumulate
                            | MetaCommand::Scan
                            | MetaCommand::Stream
                    )
                })
            }
            _ => true,
        }
    }
    if !parsed.script.statements.iter().all(valid) {
        return Err(error(
            "Sandbox supports calculations, definitions, providers and pipelines; host controls, nested sandboxes and durable configuration changes are outside its scope.",
        ));
    }
    Ok(())
}

fn preparation_error(diagnostic: &wes_language::Diagnostic, source: &str) -> SessionError {
    let authored = source
        .get(diagnostic.span.start()..diagnostic.span.end())
        .unwrap_or("");
    let scope = if diagnostic.code == "CAL010" && authored.starts_with('$') {
        let label: String = authored
            .chars()
            .take(128)
            .flat_map(char::escape_default)
            .collect();
        format!(
            " Reference {label}. Sandbox calculations can only reference their own declared members; parent workspace results are not visible."
        )
    } else {
        String::new()
    };
    SessionError::AdmissionRefused(format!(
        "{}: {}{}",
        diagnostic.code,
        diagnostic.public_summary(),
        scope
    ))
}
