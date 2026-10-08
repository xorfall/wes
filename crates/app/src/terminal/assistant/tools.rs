use super::super::{BridgeReply, BridgeRequest, TerminalSession, bridge, commands};
use crate::{ApplicationHandle, CurrentSession};
use serde::Deserialize;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    sync::{Arc, Mutex},
    time::Duration,
};
use wes_engine::{graph::NodeState, session::SessionObservation, source::SourceInput};

pub(in crate::terminal) struct State {
    actor: String,
    namespace: String,
    workspaces: Mutex<BTreeMap<String, CurrentSession>>,
}
impl Default for State {
    fn default() -> Self {
        Self::new(None)
    }
}
impl State {
    pub(in crate::terminal) fn new(history: Option<&str>) -> Self {
        Self {
            actor: format!("actor-{}", uuid::Uuid::new_v4()),
            namespace: history.map_or_else(
                || format!("session-{}", uuid::Uuid::new_v4()),
                |id| format!("terminal-{id}"),
            ),
            workspaces: Default::default(),
        }
    }
}
#[derive(Clone)]
struct Execution {
    cell: String,
    actor: String,
    steps: Vec<String>,
}
#[derive(Deserialize)]
#[serde(
    tag = "name",
    content = "arguments",
    rename_all = "snake_case",
    deny_unknown_fields
)]
enum Tool {
    ViewAuthoring(super::view_authoring::Read),
    ViewToolchain(Empty),
    ViewRenderStatus(RenderStatus),
    WorkspaceContext(Empty),
    WorkspaceOpen(WorkspaceOpen),
    WorkspaceSnapshot(Page),
    CellRead(CellRead),
    LayoutRead(Empty),
    TabOpen(TabOpen),
    Help(Help),
    ValuesList(Empty),
    ValueRead(ValueRead),
    DatasetInspect(DatasetRead),
    DatasetPage(DatasetRead),
    Validate(Source),
    Execute(Execute),
    ExecutionRead(ExecutionRead),
    Cancel(Owned),
    PaneCommand(PaneCommand),
    DraftRead(Empty),
    DraftUpdate(DraftUpdate),
    SpecList(Page),
    SpecRead(super::spec::Read),
    SpecSave(super::spec::Save),
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RenderStatus {
    name: String,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct WorkspaceOpen {
    name: String,
    #[serde(default)]
    create: bool,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Page {
    #[serde(default)]
    offset: usize,
    #[serde(default = "page_limit")]
    limit: usize,
}
fn page_limit() -> usize {
    50
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CellRead {
    cell: String,
    #[serde(default)]
    source: bool,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct TabOpen {
    pane: String,
    #[serde(default)]
    activate: bool,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Empty {}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Help {
    #[serde(default)]
    depth: u8,
    #[serde(default)]
    provider: Option<String>,
    #[serde(default)]
    command: Option<String>,
    #[serde(default)]
    tail: Vec<String>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ValueRead {
    name: String,
    #[serde(flatten)]
    selection: wes_adapters::codec::ValueSelection,
    #[serde(default)]
    typed: bool,
}
#[derive(Deserialize)]
struct DatasetRead {
    name: String,
    #[serde(flatten)]
    read: crate::dataset_reads::Request,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Source {
    source: String,
}
#[derive(Clone, Copy, Default, Deserialize)]
#[serde(rename_all = "snake_case")]
enum Results {
    #[default]
    Summary,
    Full,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Execute {
    #[serde(default)]
    sequential: bool,
    #[serde(default)]
    reactive: bool,
    #[serde(default)]
    results: Results,
    source: String,
    context: String,
    request_id: String,
    #[serde(default)]
    wait_ms: u64,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ExecutionRead {
    #[serde(default)]
    results: Results,
    request_id: String,
    #[serde(default)]
    wait_ms: u64,
    #[serde(default)]
    trace: bool,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Owned {
    request_id: String,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct DraftUpdate {
    text: String,
    revision: String,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct PaneCommand {
    command: String,
}
fn fail(message: &str) -> BridgeReply {
    BridgeReply::error(2, message)
}
fn rejoin_required(name: &str, reason: &str) -> BridgeReply {
    let join = json!({"name":"workspace_open","arguments":{"name":name,"create":false}});
    fail(&format!(
        "{reason} Join this workspace with {join}, then use its returned workspace and fresh context. Membership belongs to the live terminal session; joining does not restore grants or replay work. If a previous execution reply was lost, read execution_read with its original request_id before considering another execution."
    ))
}
fn context(
    observation: &SessionObservation,
    terminal: &TerminalSession,
    current: &CurrentSession,
) -> Value {
    let selected = bridge::context(observation, &terminal.assistant.actor).and_then(|c| c.selected);
    let revisions: BTreeMap<_, _> = observation
        .environment_revisions
        .iter()
        .map(|(n, r)| (n, r.to_string()))
        .collect();
    json!({"workspace":current.name.as_str(),"generation":current.generation,"selected":selected,"revisions":revisions})
}
/// A compact equality token, not a capability: normal authority checks still apply.
fn context_token(context: &Value) -> String {
    format!("ctx:{:x}", Sha256::digest(context.to_string().as_bytes()))
}
fn context_reply(
    observation: &SessionObservation,
    terminal: &TerminalSession,
    current: &CurrentSession,
) -> Value {
    let environment = context(observation, terminal, current);
    let catalogue = bridge::catalogue(observation, &terminal.assistant.actor);
    json!({"workspace":current.name.as_str(),"generation":current.generation,
        "context":context_token(&environment),"actor":terminal.assistant.actor,"request_scope":terminal.assistant.namespace,"environment":environment,
        "executeArguments":{"workspace":current.name.as_str(),"context":context_token(&environment)},
        "routing":"workspace_open joins a workspace; it does not redirect subsequent calls. Pass workspace explicitly on execute, cancel and tab_open.",
        "enabled":observation.environment_enabled,
        "providers":catalogue.map(|c|c.provider_names().take(1024).collect::<Vec<_>>()).unwrap_or_default()})
}
fn check_current(
    terminal: &TerminalSession,
    application: &ApplicationHandle,
    current: &CurrentSession,
) -> Result<(), BridgeReply> {
    terminal
        .check(application)
        .map_err(|_| fail("Terminal authority has ended."))?;
    application
        .session_for_generation(&current.generation)
        .map_err(|_| rejoin_required(current.name.as_str(), "Workspace was reloaded."))?;
    Ok(())
}
async fn observe_current(
    terminal: &TerminalSession,
    application: &ApplicationHandle,
    current: &CurrentSession,
) -> Result<SessionObservation, BridgeReply> {
    let observation = current
        .session
        .observe_actor_in(
            terminal.assistant.actor.clone(),
            terminal.client.clone(),
            (current.generation == terminal.current.generation)
                .then(|| terminal.environment.clone())
                .flatten(),
        )
        .await
        .map_err(|_| fail("Workspace unavailable."))?;
    check_current(terminal, application, current)?;
    Ok(observation)
}
fn validate_wait(wait_ms: u64) -> Result<(), BridgeReply> {
    if wait_ms > 1000 {
        return Err(fail("wait_ms must be between 0 and 1000."));
    }
    Ok(())
}
fn settled(observation: &SessionObservation, execution: &Execution) -> bool {
    let Some(cell) = observation
        .cells
        .iter()
        .find(|cell| cell.input.cell() == execution.cell)
    else {
        return false;
    };
    match &cell.reply {
        None => false,
        Some(Err(_)) => true,
        Some(Ok(reply)) => reply.nodes.iter().chain(&reply.refreshed).all(|id| {
            !observation.state.execution.graph.node(id).is_some_and(|n| {
                matches!(n.state(), NodeState::Pending | NodeState::Running)
                    || (reply.refreshed.contains(id) && n.state() == NodeState::Stale)
            })
        }),
    }
}
/// Subscribe before observing, so completion between observation and waiting cannot be lost.
async fn observe_execution(
    terminal: &TerminalSession,
    application: &ApplicationHandle,
    current: &CurrentSession,
    execution: &Execution,
    wait_ms: u64,
) -> Result<SessionObservation, BridgeReply> {
    validate_wait(wait_ms)?;
    let deadline = tokio::time::Instant::now() + Duration::from_millis(wait_ms);
    let mut updates = current
        .session
        .subscribe_updates()
        .map_err(|_| fail("Workspace unavailable."))?;
    let mut sessions = application.subscribe_sessions();
    loop {
        let workflow_before = current.session.sequential_state(&execution.cell);
        let observation = observe_current(terminal, application, current).await?;
        if !execution.steps.is_empty()
            && workflow_before != current.session.sequential_state(&execution.cell)
        {
            continue;
        }
        if (if execution.steps.is_empty() {
            settled(&observation, execution)
        } else {
            workflow_state(&observation, execution, current)
                != wes_engine::session::SequentialState::Running
        }) || tokio::time::Instant::now() >= deadline
        {
            return Ok(observation);
        }
        tokio::select! {
            _ = terminal.stopped.cancelled() => return Err(fail("Terminal authority has ended.")),
            result = sessions.changed() => {
                result.map_err(|_| fail("Workspace unavailable."))?;
                check_current(terminal, application, current)?;
            }
            result = updates.recv() => {
                if matches!(result, Err(tokio::sync::broadcast::error::RecvError::Closed)) {
                    return Err(fail("Workspace unavailable."));
                }
            }
            _ = tokio::time::sleep_until(deadline) => {
                check_current(terminal, application, current)?;
                return Ok(observation);
            }
        }
    }
}
fn parsed(source: &str) -> Result<wes_language::Parsed, BridgeReply> {
    if source.len() > 64 * 1024 {
        return Err(fail("Source exceeds 64 KiB."));
    }
    Ok(wes_language::parse(&wes_language::SourceText::new(
        "assistant",
        source,
    )))
}
fn permitted(parsed: &wes_language::Parsed) -> Result<(), BridgeReply> {
    wes_engine::access::admit(parsed).map_err(fail)
}

fn diagnostics(items: &[wes_language::Diagnostic]) -> Value {
    json!(items.iter().take(128).map(|d|json!({"code":d.code,"severity":format!("{:?}",d.severity).to_lowercase(),"message":d.message})).collect::<Vec<_>>())
}
fn sandbox_report(reply: &wes_engine::session::SubmissionReply) -> Value {
    match reply {
        Ok(reply) => match &reply.sandbox {
            Some(sandbox) => {
                let data = wes_adapters::codec::encode_json(
                    &sandbox.data,
                    wes_adapters::codec::Limits::default(),
                )
                .ok()
                .filter(|bytes| bytes.len() <= 64 * 1024)
                .and_then(|bytes| serde_json::from_slice::<Value>(&bytes).ok());
                json!({"kind":"sandbox","name":sandbox.name,"settled":true,"persistence":"definition only","observation":data,"observationAvailable":data.is_some(),"hasExecutionCell":false})
            }
            None => json!({"settled":false}),
        },
        Err(error) => {
            json!({"kind":"sandbox","settled":true,"error":error.to_string(),"hasExecutionCell":false})
        }
    }
}

async fn execution(
    state: &State,
    id: &str,
    current: &CurrentSession,
) -> Result<Execution, BridgeReply> {
    current
        .session
        .find_request(&state.namespace, id)
        .await
        .map_err(|e| fail(&e.to_string()))?
        .map(|record| Execution {
            cell: record.cell,
            steps: record.steps,
            actor: state.actor.clone(),
        })
        .ok_or_else(|| fail("Unknown execution request_id for this session and workspace."))
}
fn report(
    observation: &SessionObservation,
    execution: &Execution,
    trace: bool,
    results: Results,
) -> Value {
    report_bounded(
        observation,
        execution,
        trace,
        results,
        &mut (8 * 1024 * 1024 - 64 * 1024),
    )
}
fn report_bounded(
    observation: &SessionObservation,
    execution: &Execution,
    trace: bool,
    results: Results,
    body_budget: &mut usize,
) -> Value {
    let Some(cell) = observation
        .cells
        .iter()
        .find(|cell| cell.input.cell() == execution.cell)
    else {
        return json!({"cell":execution.cell,"settled":false,"status":"admission_pending_or_unavailable","retry":"Identity is reserved; details may be pending, retired or unavailable. Never repeat effects under a new ID without resolving their outcome."});
    };
    let Some(Ok(reply)) = &cell.reply else {
        let mut result = json!({"cell":execution.cell});
        super::inspection::admission(&mut result, cell);
        return result;
    };
    let mut bounded = |value: &wes_core::Value, limit: usize| match bridge::exported_bounded(
        value,
        false,
        (*body_budget).min(limit),
    ) {
        Ok(text) => {
            *body_budget = body_budget.saturating_sub(text.len() + 128);
            json!({"available":true,"value":serde_json::from_str::<Value>(&text).expect("encoded value")})
        }
        Err(_) => {
            json!({"available":false,"reason":"Value is private, unknown, lazy or exceeds the response budget; reduce/read it separately."})
        }
    };
    let nodes: Vec<_> = reply.nodes.iter().chain(&reply.refreshed).map(|id| {
        let graph = &observation.state.execution;
        let state = graph.graph.node(id).map(|n|n.state());
        let mut result = json!({"node":id.to_string(),"state":state.map(|s|format!("{s:?}").to_lowercase()),"run":graph.runs.get(id).map(ToString::to_string)});
        result["updatePending"] = json!(graph.input_updates.contains(id));
        if let Some(completion)=crate::execution_status::public_completion(observation,id,cell.input.is_cooperative()&&cell.input.client()==execution.actor) { result["completion"]=completion; }
        if let Some(reason) = graph.stale_reasons.get(id) {
            result["staleReason"] = json!({"code":reason.code(),"message":reason.message()});
        }
        if matches!(results, Results::Full) && state == Some(NodeState::Ready) && let Some(value) = graph.values.get(id) { result["result"] = bounded(value, 8 * 1024 * 1024); }
        if state == Some(NodeState::Ready) && let Some(value) = graph.values.get(id)
            && let Some(outcome) = public_outcome(value) { result["outcome"] = outcome; }
        if let Some(waits)=graph.waiting_inputs.get(id){result["waiting"]=crate::execution_status::waiting_inputs(waits, observation.state.names.iter());}
        result["streaming"] = json!(graph.streaming.contains(id));
        if reply.refreshed.contains(id) { result["refreshed"] = json!(true); }
        result["names"] = json!(observation.state.names.iter().filter(|(_, output)| output.node == *id).map(|(name,_)| name).collect::<Vec<_>>());
        if let Some(error) = graph.errors.get(id) { result["error"] = bounded(&error.to_value(), 4096); }
        if trace && let Some(value) = observation.traces.get(id, None) { result["trace"] = bounded(&value, 8 * 1024 * 1024); }
        result
    }).collect();
    let codes = super::inspection::diagnostics(
        &reply.diagnostics.diagnostics,
        (cell.input.is_cooperative() && cell.input.client() == execution.actor)
            .then_some(cell.input.text()),
    );
    json!({"cell":execution.cell,"restored":reply.restored,"receipts":reply.receipts.iter().map(crate::execution_status::operation_receipt).collect::<Vec<_>>(),"settled":settled(observation, execution),"nodes":nodes,"diagnostics":codes,"accepted":reply.accepted.iter().map(|span|json!({"start":span.start(),"end":span.end()})).collect::<Vec<_>>(),"removed":reply.removed.iter().map(ToString::to_string).collect::<Vec<_>>(),"unbound":reply.unbound})
}
fn workflow_state(
    observation: &SessionObservation,
    execution: &Execution,
    current: &CurrentSession,
) -> wes_engine::session::SequentialState {
    use wes_engine::session::{SequentialState, SessionHandle};
    if let Some(state) = current.session.sequential_state(&execution.cell) {
        return state;
    }
    // Restored identities never resume. Missing steps mean an interrupted outcome.
    for cell in &execution.steps {
        match SessionHandle::sequential_step_state(observation, cell) {
            Some(SequentialState::Completed) => (),
            Some(state) => return state,
            None => return SequentialState::Interrupted,
        }
    }
    SequentialState::Completed
}
fn request_report(
    observation: &SessionObservation,
    execution: &Execution,
    current: &CurrentSession,
    trace: bool,
    results: Results,
) -> Value {
    if execution.steps.is_empty() {
        return report(observation, execution, trace, results);
    }
    let state = workflow_state(observation, execution, current);
    let mut budget = 8 * 1024 * 1024 - 256 * 1024;
    let steps = execution
        .steps
        .iter()
        .enumerate()
        .map(|(index, cell)| {
            let step = Execution {
                cell: cell.clone(),
                actor: execution.actor.clone(),
                steps: Vec::new(),
            };
            let mut result = report_bounded(observation, &step, trace, results, &mut budget);
            result["index"] = json!(index);
            if !observation.cells.iter().any(|c| c.input.cell() == cell) {
                result["status"] = json!("not_admitted_or_unavailable");
            }
            result
        })
        .collect::<Vec<_>>();
    json!({"cell":execution.cell,"sequential":true,"status":state.as_str(),"settled":state!=wes_engine::session::SequentialState::Running,"steps":steps})
}
/// Contract metadata is only inspected after the same root egress policy as value_read.
fn public_outcome(value: &wes_core::Value) -> Option<Value> {
    use wes_core::{Data, Shape};
    let policy = value.provenance().policy();
    if policy.is_private() || policy.is_unknown() {
        return None;
    }
    let (Shape::Record(shape), Data::Record(data)) = (value.shape(), value.data()) else {
        return None;
    };
    let (key, label) = match shape.name() {
        "ProcessOutput" => ("exitCode", "exit_code"),
        "HttpResponse" => ("status", "http_status"),
        _ => return None,
    };
    let Data::Int(code) = data.get(key)? else {
        return None;
    };
    Some(json!({label:code, "success": if key == "exitCode" { *code == 0 } else { *code < 400 }}))
}

fn annotate_help(reply: &mut Value) {
    if let Some(path) = reply["path"]
        .as_str()
        .filter(|_| reply.get("provider").is_none())
    {
        let words: Vec<String> = path.split_whitespace().map(str::to_owned).collect();
        if let Some(spec) = wes_language::vocabulary::commands::signature(&words) {
            let allowed = wes_engine::access::cooperative_command(spec.command)
                && (spec.command != wes_language::vocabulary::MetaCommand::Env
                    || words.get(1).is_none_or(|action| {
                        wes_engine::access::ENVIRONMENT_ACTIONS.contains(&action.as_str())
                    }));
            reply["assistant_allowed"] = json!(allowed);
        }
    }
    if let Some(children) = reply["children"].as_array_mut() {
        for child in children {
            annotate_help(child);
        }
    }
}
pub(in crate::terminal) async fn dispatch(
    terminal: Arc<TerminalSession>,
    application: ApplicationHandle,
    arguments: &[String],
) -> BridgeReply {
    let result = perform(&terminal, &application, arguments).await;
    match result {
        Ok(value) => {
            let text = value.to_string();
            if text.len() > 8 * 1024 * 1024 {
                fail("Assistant response exceeds 8 MiB; request a smaller value or operation.")
            } else {
                BridgeReply {
                    code: 0,
                    stdout: text,
                    stderr: String::new(),
                }
            }
        }
        Err(error) => error,
    }
}
async fn perform(
    terminal: &Arc<TerminalSession>,
    application: &ApplicationHandle,
    arguments: &[String],
) -> Result<Value, BridgeReply> {
    terminal
        .check(application)
        .map_err(|_| fail("Terminal authority has ended."))?;
    if arguments.len() != 1 {
        return Err(fail("Expected one structured assistant request."));
    }
    let mut request: Value =
        serde_json::from_str(&arguments[0]).map_err(|_| fail("Invalid tool request."))?;
    super::protocol::validate_call(&request).map_err(|message| fail(&message))?;
    let workspace = request
        .get_mut("arguments")
        .and_then(Value::as_object_mut)
        .and_then(|args| args.remove("workspace"));
    let current = match workspace.as_ref() {
        None => terminal.current.clone(),
        Some(Value::String(name)) if name == terminal.current.name.as_str() => {
            terminal.current.clone()
        }
        Some(Value::String(name)) => terminal
            .assistant
            .workspaces
            .lock()
            .expect("joined workspaces")
            .get(name)
            .cloned()
            .ok_or_else(|| {
                rejoin_required(
                    name,
                    "This connection has not joined the requested workspace.",
                )
            })?,
        _ => return Err(fail("workspace must be a name.")),
    };
    application
        .session_for_generation(&current.generation)
        .map_err(|_| rejoin_required(current.name.as_str(), "Workspace was reloaded."))?;
    let tool: Tool = serde_json::from_value(request).map_err(|_| {
        fail("Tool arguments could not be decoded; use this tool's inputSchema from tools/list.")
    })?;
    if workspace.is_some()
        && matches!(
            tool,
            Tool::WorkspaceOpen(_)
                | Tool::PaneCommand(_)
                | Tool::LayoutRead(_)
                | Tool::DraftRead(_)
                | Tool::DraftUpdate(_)
                | Tool::SpecList(_)
                | Tool::SpecRead(_)
                | Tool::SpecSave(_)
                | Tool::ViewAuthoring(_)
                | Tool::ViewToolchain(_)
        )
    {
        return Err(fail(
            "This tool belongs to the originating connection; omit workspace.",
        ));
    }
    check_current(terminal, application, &current)?;
    let dataset_inspect = matches!(&tool, Tool::DatasetInspect(_));
    match tool {
        Tool::ViewAuthoring(input) => Ok(super::view_authoring::read(input)),
        Tool::ViewToolchain(_) => tokio::task::spawn_blocking(crate::view_toolchain::status)
            .await
            .map_err(|_| fail("Toolchain inventory unavailable.")),
        Tool::ViewRenderStatus(input) => {
            render_status(terminal, application, &current, &input.name).await
        }
        Tool::SpecList(page) => {
            super::spec::list(terminal, application, page.offset, page.limit).await
        }
        Tool::SpecRead(input) => super::spec::read(terminal, application, input).await,
        Tool::SpecSave(input) => super::spec::save(terminal, application, input).await,
        Tool::WorkspaceOpen(input) => {
            let name = wes_engine::workspace::WorkspaceName::new(input.name)
                .map_err(|_| fail("Invalid workspace name."))?;
            let joined = application
                .open_workspace(name, input.create)
                .await
                .map_err(|e| fail(&e.to_string()))?;
            terminal
                .check(application)
                .map_err(|_| fail("Terminal authority ended."))?;
            terminal
                .assistant
                .workspaces
                .lock()
                .expect("joined workspaces")
                .insert(joined.name.as_str().to_owned(), joined.clone());
            let observation = observe_current(terminal, application, &joined).await?;
            Ok(context_reply(&observation, terminal, &joined))
        }
        Tool::WorkspaceSnapshot(page) => {
            let observation = observe_current(terminal, application, &current).await?;
            if page.limit == 0 || page.limit > 100 {
                return Err(fail("limit must be between 1 and 100."));
            }
            let cells: Vec<_> = observation
                .cells
                .iter()
                .skip(page.offset)
                .take(page.limit)
                .map(|cell| super::inspection::cell(&observation, cell, false, None))
                .collect();
            Ok(
                json!({"workspace":current.name.as_str(),"generation":current.generation,
                "offset":page.offset,"total":observation.cells.len(),"cells":cells}),
            )
        }
        Tool::CellRead(input) => {
            let observation = observe_current(terminal, application, &current).await?;
            let cell = observation
                .cells
                .iter()
                .find(|cell| cell.input.cell() == input.cell)
                .ok_or_else(|| fail("Cell not found in this workspace."))?;
            let source = if input.source {
                current
                    .session
                    .read_source(terminal.assistant.actor.clone(), input.cell.clone())
                    .await
                    .map_err(|_| fail("Cell source unavailable."))?
            } else {
                None
            };
            check_current(terminal, application, &current)?;
            let mut result = super::inspection::cell(&observation, cell, true, source.as_deref());
            if input.source && source.is_none() {
                result["source"] = json!({"available":false,"reason":"Source is not shared with this actor. An explicit source grant is independent of work editing and public results."});
            }
            Ok(result)
        }
        Tool::LayoutRead(_) => {
            ui_request(terminal, crate::terminal::ui::Operation::LayoutRead).await
        }
        Tool::TabOpen(input) => {
            if input.pane.len() > 64 || input.pane.chars().any(char::is_control) {
                return Err(fail("Invalid pane id."));
            }
            ui_request(
                terminal,
                crate::terminal::ui::Operation::TabOpen {
                    pane: input.pane,
                    workspace: current.name.as_str().into(),
                    activate: input.activate,
                },
            )
            .await
        }
        Tool::PaneCommand(input) => {
            commands::dispatch(terminal, input.command, Some(&terminal.assistant.actor)).await?;
            Ok(json!({"applied":true}))
        }
        Tool::WorkspaceContext(_) => {
            let observation = observe_current(terminal, application, &current).await?;
            Ok(context_reply(&observation, terminal, &current))
        }
        Tool::Help(input) => {
            use wes_language::{Argument, Call, Name, Span, Value as SyntaxValue};
            if input.command.is_some() && input.provider.is_some() {
                return Err(fail("Choose a command or provider help target, not both."));
            }
            let observation = observe_current(terminal, application, &current).await?;
            let empty = wes_core::capability::Catalogue::default();
            let catalogue = bridge::catalogue(&observation, &terminal.assistant.actor);
            let span = Span::at(0);
            let mut call = Call {
                marker: Some(span),
                path: vec![Name {
                    text: "help".into(),
                    span,
                }],
                operands: vec![],
                arguments: vec![],
                span,
            };
            if let Some(provider) = input.provider {
                let mut path = vec![provider];
                path.extend(input.tail);
                call.arguments.push(Argument {
                    key: Name {
                        text: if path.len() == 1 {
                            "provider"
                        } else {
                            "capability"
                        }
                        .into(),
                        span,
                    },
                    value: SyntaxValue::Text(Name {
                        text: path.join(" "),
                        span,
                    }),
                    span,
                });
            } else if let Some(command) = input.command {
                let mut path: Vec<_> = command.split_whitespace().map(str::to_owned).collect();
                path.extend(input.tail);
                call.arguments.push(Argument {
                    key: Name {
                        text: "command".into(),
                        span,
                    },
                    value: SyntaxValue::Text(Name {
                        text: path.join(" "),
                        span,
                    }),
                    span,
                });
            } else if !input.tail.is_empty() {
                return Err(fail("Help tail requires a command or provider target."));
            }
            let value = wes_engine::tasks::help_tree(
                &call,
                catalogue.unwrap_or(&empty),
                &observation.importer_metadata,
                &wes_engine::driver::CancellationToken::new(),
                input.depth,
            )
            .map_err(|d| fail(&d.message))?;
            let encoded = bridge::exported_bounded(&value, false, 64 * 1024).map_err(|_| {
                fail("Help exceeds 64 KiB; choose a narrower command/provider path or lower depth.")
            })?;
            let mut reply: serde_json::Value =
                serde_json::from_str(&encoded).map_err(|_| fail("Help could not be encoded."))?;
            annotate_help(&mut reply);
            Ok(reply)
        }
        Tool::DatasetInspect(mut input) | Tool::DatasetPage(mut input) => {
            input.read.inspect = dataset_inspect;
            let observation = observe_current(terminal, application, &current).await?;
            let (value, _) = bridge::named_observation(&observation, &input.name)?;
            let store = application
                .storage
                .as_ref()
                .ok_or_else(|| fail("Dataset storage is unavailable in this application."))?;
            let reply = crate::dataset_reads::read(store, value, input.read.clone(), 64 * 1024)
                .await
                .map_err(|error| fail(&error.to_string()))?;
            let after = observe_current(terminal, application, &current).await?;
            bridge::revalidate_read(&observation, &after, &input.name)?;
            Ok(reply)
        }
        Tool::ValueRead(input) => {
            let observation = observe_current(terminal, application, &current).await?;
            let (value, stopped) = bridge::named_observation(&observation, &input.name)?;
            let text = if input.selection.is_requested() {
                let bytes = wes_adapters::codec::encode_selection(
                    value,
                    &input.selection,
                    input.typed,
                    wes_adapters::codec::Limits {
                        bytes: 64 * 1024,
                        nodes: 100_000,
                    },
                )
                .map_err(|e| fail(&e.to_string()))?;
                String::from_utf8(bytes).expect("JSON UTF-8")
            } else {
                bridge::exported(value, input.typed)?
            };
            let after = observe_current(terminal, application, &current).await?;
            bridge::revalidate_read(&observation, &after, &input.name)?;
            serde_json::from_str(&text)
                .map(|value| bridge::observed_result(value, stopped))
                .map_err(|_| fail("Value unavailable."))
        }
        Tool::ValuesList(_) => {
            let reply = bridge::dispatch_at(
                terminal.clone(),
                application.clone(),
                &current,
                BridgeRequest {
                    tool: "wes-value".into(),
                    args: vec!["list".into()],
                },
            )
            .await;
            if reply.code != 0 {
                return Err(reply);
            }
            serde_json::from_str(&reply.stdout).map_err(|_| fail("Value unavailable."))
        }
        Tool::Validate(input) => {
            let parsed = parsed(&input.source)?;
            let restriction = permitted(&parsed).err().map(|e| e.stderr);
            Ok(
                json!({"scope":"syntax and assistant admission only; engine performs binding/type checks on execute","valid":parsed.diagnostics.iter().all(|d|d.severity!=wes_language::Severity::Error)&&restriction.is_none(),"diagnostics":diagnostics(&parsed.diagnostics),"restriction":restriction}),
            )
        }
        Tool::Execute(input) => {
            validate_wait(input.wait_ms)?;
            let observation = observe_current(terminal, application, &current).await?;
            if input.request_id.is_empty()
                || input.request_id.len() > 128
                || input.request_id.chars().any(char::is_control)
            {
                return Err(fail(
                    "request_id must be a nonempty, single-line identifier up to 128 bytes.",
                ));
            }
            let parsed = parsed(&input.source)?;
            if parsed
                .diagnostics
                .iter()
                .any(|d| d.severity == wes_language::Severity::Error)
            {
                return Ok(
                    json!({"accepted":false,"diagnostics":diagnostics(&parsed.diagnostics)}),
                );
            }
            permitted(&parsed)?;
            let current_context = context_token(&context(&observation, terminal, &current));
            let mut source =
                SourceInput::new(format!("assistant-{}", uuid::Uuid::new_v4()), input.source)
                    .map_err(|_| fail("Invalid source."))?
                    .with_client(terminal.assistant.actor.clone())
                    .map_err(|_| fail("Invalid client."))?
                    .cooperative()
                    .with_reactive(input.reactive);
            if let Some(context) = bridge::context(&observation, &terminal.assistant.actor) {
                source = source
                    .with_environments(context)
                    .map_err(|_| fail("Invalid environment context."))?;
            }
            check_current(terminal, application, &current)?;
            let admission = if input.sequential {
                current
                    .session
                    .submit_sequential_request(
                        terminal.assistant.namespace.clone(),
                        input.request_id.clone(),
                        input.context.clone(),
                        input.context == current_context,
                        source,
                        terminal.stopped.clone(),
                    )
                    .await
            } else {
                current
                    .session
                    .submit_request(
                        terminal.assistant.namespace.clone(),
                        input.request_id.clone(),
                        input.context.clone(),
                        input.context == current_context,
                        source,
                    )
                    .await
            };
            let admitted = admission.map_err(|e| {
                if matches!(e, wes_engine::history::RecordError::RequestContext) {
                    fail(&format!("Context does not match target workspace '{}'. Read workspace_context with that workspace and use its returned workspace/context together. workspace_open does not redirect later calls.", current.name.as_str()))
                } else { fail(&e.to_string()) }
            })?;
            let fresh = admitted.claim.fresh;
            let execution = Execution {
                cell: admitted.claim.record.cell,
                steps: admitted.claim.record.steps,
                actor: terminal.assistant.actor.clone(),
            };
            let sandbox = match admitted.submission.as_ref() {
                Some(Ok(reply)) if reply.sandbox.is_some() => admitted.submission.clone(),
                _ => current.session.sandbox_receipt(&execution.cell).await,
            };
            if let Some(reply) = sandbox {
                let observation = observe_current(terminal, application, &current).await?;
                return Ok(
                    json!({"request_id":input.request_id,"duplicate":!fresh,"context":context_token(&context(&observation,terminal,&current)),"execution":sandbox_report(&reply)}),
                );
            }
            let observation =
                observe_execution(terminal, application, &current, &execution, input.wait_ms)
                    .await?;
            let mut result = json!({"request_id":input.request_id,"duplicate":!fresh,"context":context_token(&context(&observation,terminal,&current)),"plans":plan_summaries(&observation, terminal),"execution":request_report(&observation,&execution,&current,false,input.results)});
            if let Some(Err(error)) = admitted.submission {
                result["execution"]["admission_error"] = json!(error.to_string());
            }
            Ok(result)
        }
        Tool::ExecutionRead(input) => {
            validate_wait(input.wait_ms)?;
            let execution = execution(&terminal.assistant, &input.request_id, &current).await?;
            if let Some(reply) = current.session.sandbox_receipt(&execution.cell).await {
                return Ok(sandbox_report(&reply));
            }
            let observation =
                observe_execution(terminal, application, &current, &execution, input.wait_ms)
                    .await?;
            Ok(request_report(
                &observation,
                &execution,
                &current,
                input.trace,
                input.results,
            ))
        }
        Tool::Cancel(input) => {
            execution(&terminal.assistant, &input.request_id, &current).await?;
            check_current(terminal, application, &current)?;
            current
                .session
                .cancel_request_work(
                    terminal.assistant.actor.clone(),
                    &current
                        .session
                        .find_request(&terminal.assistant.namespace, &input.request_id)
                        .await
                        .map_err(|e| fail(&e.to_string()))?
                        .ok_or_else(|| fail("Unknown request"))?,
                )
                .await
                .map_err(|e| fail(&e.to_string()))?;
            Ok(json!({"cancellation_requested":true,"remote_effects_may_remain":true}))
        }
        Tool::DraftRead(_) => ui_request(terminal, crate::terminal::ui::Operation::DraftRead).await,
        Tool::DraftUpdate(input) => {
            if input.text.len() > 64 * 1024 || input.revision.len() > 128 {
                return Err(fail("Editor input exceeds its limit."));
            }
            ui_request(
                terminal,
                crate::terminal::ui::Operation::DraftUpdate {
                    text: input.text,
                    revision: input.revision,
                },
            )
            .await
        }
    }
}

async fn ui_request(
    terminal: &TerminalSession,
    operation: crate::terminal::ui::Operation,
) -> Result<Value, BridgeReply> {
    let (id, reply) = terminal.ui.begin(operation).map_err(fail)?;
    terminal.changed.notify_waiters();
    let result = tokio::select! {
        _ = terminal.stopped.cancelled() => Err(fail("Terminal ended before UI acknowledgement.")),
        result = tokio::time::timeout(Duration::from_secs(10), reply) => match result {
            Ok(Ok(value)) => Ok(value),
            _ => Err(fail("UI did not acknowledge. Inspect layout before repeating.")),
        }
    };
    terminal.ui.abandon(&id);
    result
}

fn plan_summaries(observation: &SessionObservation, terminal: &TerminalSession) -> Value {
    json!(observation.environment_plans.iter().filter(|((actor,_),_)| actor == &terminal.assistant.actor).map(|((_,name),changes)| json!({"name":name,"changes":changes.iter().map(|c| json!({"environment":c.name,"added":c.before.is_none(),"before":c.before.map(|r|r.to_string()),"after":c.after.map(|r|r.to_string())})).collect::<Vec<_>>()})).collect::<Vec<_>>())
}

include!("render_status.rs");

#[cfg(test)]
mod tests {
    use super::*;
    #[cfg(unix)]
    include!("describe_tests.rs");
    #[cfg(unix)]
    include!("import_tests.rs");
    #[cfg(unix)]
    include!("render_status_tests.rs");
    #[cfg(unix)]
    include!("spec_tests.rs");
    #[cfg(unix)]
    include!("view_authoring_tests.rs");
    include!("workspace_rejoin_tests.rs");
    use crate::{
        runtime::{RuntimeOptions, launch},
        terminal::Manager,
    };

    #[test]
    fn advertised_argument_shapes_decode_with_and_without_optional_fields() {
        fn sample(schema: &Value, all: bool) -> Value {
            if let Some(choice) = schema["enum"].as_array().and_then(|a| a.first()) {
                return choice.clone();
            }
            match schema["type"].as_str().unwrap() {
                "object" => {
                    let required = schema["required"].as_array().unwrap();
                    Value::Object(
                        schema["properties"]
                            .as_object()
                            .unwrap()
                            .iter()
                            .filter(|(name, _)| {
                                all || required.contains(&Value::String((*name).clone()))
                            })
                            .map(|(name, schema)| (name.clone(), sample(schema, all)))
                            .collect(),
                    )
                }
                "string" => {
                    if schema["pattern"].is_string() {
                        json!("a".repeat(64))
                    } else {
                        json!("sample")
                    }
                }
                "integer" => schema.get("minimum").cloned().unwrap_or(json!(0)),
                "boolean" => json!(false),
                "array" => json!([sample(&schema["items"], all)]),
                other => panic!("unsupported schema {other}"),
            }
        }
        for declaration in super::super::protocol::registry()["tools"]
            .as_array()
            .unwrap()
        {
            for all in [false, true] {
                let mut request = json!({"name":declaration["name"],"arguments":sample(&declaration["inputSchema"],all)});
                if all && declaration["name"] == "dataset_page" {
                    // Independently constructed reference: schema validation cannot
                    // manufacture a real committed capability from these identities.
                    request["arguments"]["extent"] = json!({
                        "store":"10000000-0000-4000-8000-000000000001",
                        "dataset":"10000000-0000-4000-8000-000000000002",
                        "generation":"1", "manifest":"10000000-0000-4000-8000-000000000003",
                        "manifestDigest":format!("sha256:{}", "a".repeat(64)), "manifestBytes":"100",
                        "schemaDigest":format!("sha256:{}", "b".repeat(64)), "records":"0",
                        "authorizationGeneration":"1"
                    });
                }
                super::super::protocol::validate_call(&request).unwrap();
                request["arguments"]
                    .as_object_mut()
                    .unwrap()
                    .remove("workspace");
                assert!(
                    serde_json::from_value::<Tool>(request).is_ok(),
                    "{} all={all}",
                    declaration["name"]
                );
            }
        }
    }

    async fn invoke(
        terminal: &Arc<TerminalSession>,
        app: &ApplicationHandle,
        name: &str,
        mut arguments: Value,
    ) -> Value {
        if matches!(name, "execute" | "cancel" | "tab_open") && arguments.get("workspace").is_none()
        {
            arguments["workspace"] = json!(terminal.current.name.as_str());
        }
        perform(
            terminal,
            app,
            &[json!({"name":name,"arguments":arguments}).to_string()],
        )
        .await
        .unwrap_or_else(|error| panic!("{name}: {}", error.stderr))
    }
    async fn test_terminal(
        manager: &Manager,
        runtime: &crate::runtime::LaunchedRuntime,
        home: &std::path::Path,
        root: &std::path::Path,
        history: &str,
    ) -> Arc<TerminalSession> {
        let mut config = crate::runtime::browser_services(home.into(), root.into())
            .unwrap()
            .terminal
            .unwrap();
        config.history = Some(history.into());
        let current = runtime.handle.current().unwrap();
        let id = manager
            .start(
                config,
                current.clone(),
                "request-test".into(),
                0,
                runtime.handle.clone(),
            )
            .await
            .unwrap();
        manager
            .owned(&id, &current.generation, "request-test")
            .unwrap()
    }
    #[tokio::test(flavor = "multi_thread")]
    async fn captured_terminal_environment_initializes_tools_without_mutating_ui_or_actor_selection()
     {
        let root = tempfile::tempdir().unwrap();
        let home = root.path().join("home");
        let runtime = launch(RuntimeOptions::new(home.clone(), root.path().into()))
            .await
            .unwrap();
        let current = runtime.handle.current().unwrap();
        let ui = SourceInput::new("clear-ui".into(), ":env clear".into())
            .unwrap()
            .with_client("ui".into())
            .unwrap();
        current.session.submit(ui).await.unwrap();
        current.session.wait_idle().await.unwrap();
        let observation = current.session.observe().await.unwrap();
        let lease = current
            .session
            .capture_execution_target(
                "default".into(),
                observation.environment_revisions["default"],
                "local".into(),
            )
            .await
            .unwrap();
        let manager = Manager::default();
        let config = crate::runtime::browser_services(home, root.path().into())
            .unwrap()
            .terminal
            .unwrap();
        let id = manager
            .start_target(
                config,
                current.clone(),
                "ui".into(),
                0,
                runtime.handle.clone(),
                Some(lease),
            )
            .await
            .unwrap();
        let terminal = manager.owned(&id, &current.generation, "ui").unwrap();
        let _stop_on_failure = terminal.stopped.clone().drop_guard();
        assert_eq!(
            bridge::terminal_context(&observation, &terminal)
                .unwrap()
                .selected
                .as_deref(),
            Some("default")
        );
        // The host shell provider carries the platform's own name.
        assert!(
            bridge::terminal_catalogue(&observation, &terminal)
                .unwrap()
                .provider(if cfg!(windows) { "cmd" } else { "sh" })
                .is_some()
        );
        let context = invoke(&terminal, &runtime.handle, "workspace_context", json!({})).await;
        assert_eq!(context["environment"]["selected"], "default");
        let observation = current.session.observe().await.unwrap();
        assert_eq!(observation.environment_clients["ui"].selected, None);
        // Explicit actor selection wins after initialization, including a cleared selection.
        let input = SourceInput::new("clear-actor".into(), ":env clear".into())
            .unwrap()
            .with_client(terminal.assistant.actor.clone())
            .unwrap();
        current.session.submit(input).await.unwrap();
        current.session.wait_idle().await.unwrap();
        assert!(
            invoke(&terminal, &runtime.handle, "workspace_context", json!({})).await["environment"]
                ["selected"]
                .is_null()
        );
        assert!(
            current
                .session
                .observe_actor_in("invalid".into(), "ui".into(), Some("missing".into()))
                .await
                .is_err()
        );
        manager.shutdown().await;
        runtime.shutdown().await.unwrap();
    }
    #[tokio::test(flavor = "multi_thread")]
    async fn more_than_256_requests_resume_after_restart_without_restoring_authority_and_retirement_keeps_identity()
     {
        let root = tempfile::tempdir().unwrap();
        let home = root.path().join("home");
        let history = uuid::Uuid::new_v4().to_string();
        let runtime = launch(RuntimeOptions::new(home.clone(), root.path().into()))
            .await
            .unwrap();
        let manager = Manager::default();
        let terminal = test_terminal(&manager, &runtime, &home, root.path(), &history).await;
        let _stop_on_failure = terminal.stopped.clone().drop_guard();
        let context = invoke(&terminal, &runtime.handle, "workspace_context", json!({})).await;
        let mut first = Value::Null;
        for index in 0..270 {
            let result = invoke(&terminal, &runtime.handle, "execute", json!({"source":":calc { return 42; }", "request_id":format!("r{index}"), "context":context["context"], "wait_ms":1000})).await;
            assert_eq!(result["duplicate"], false);
            assert_eq!(result["execution"]["settled"], true);
            if index == 0 {
                first = result;
            }
        }
        let first_cell = first["execution"]["cell"].as_str().unwrap().to_owned();
        assert!(
            runtime
                .handle
                .current()
                .unwrap()
                .session
                .read_source(terminal.assistant.actor.clone(), first_cell.clone())
                .await
                .unwrap()
                .is_some()
        );
        let old_actor = terminal.assistant.actor.clone();
        assert_eq!(
            invoke(
                &terminal,
                &runtime.handle,
                "cancel",
                json!({"request_id":"r0"})
            )
            .await["cancellation_requested"],
            true
        );
        manager.shutdown().await;
        runtime.shutdown().await.unwrap();
        drop(terminal);
        let runtime = launch(RuntimeOptions::new(home.clone(), root.path().into()))
            .await
            .unwrap();
        let manager = Manager::default();
        let terminal = test_terminal(&manager, &runtime, &home, root.path(), &history).await;
        let _stop_on_failure = terminal.stopped.clone().drop_guard();
        assert_ne!(terminal.assistant.actor, old_actor);
        assert_eq!(
            invoke(&terminal, &runtime.handle, "workspace_context", json!({})).await["request_scope"],
            context["request_scope"]
        );
        let read = invoke(
            &terminal,
            &runtime.handle,
            "execution_read",
            json!({"request_id":"r0"}),
        )
        .await;
        assert_eq!(read["cell"], first_cell);
        let duplicate = invoke(&terminal, &runtime.handle, "execute", json!({"source":":calc { return 42; }", "request_id":"r0", "context":context["context"]})).await;
        assert_eq!(duplicate["duplicate"], true);
        assert_eq!(duplicate["execution"]["cell"], first_cell);
        // A verified persisted identity never restores a new actor's mutation authority.
        let refused = perform(
            &terminal,
            &runtime.handle,
            &[
                json!({"name":"cancel","arguments":{"workspace":"default","request_id":"r0"}})
                    .to_string(),
            ],
        )
        .await
        .unwrap_err();
        assert!(
            refused.stderr.contains("protected work") && refused.stderr.contains("host controls"),
            "{}",
            refused.stderr
        );
        let current = runtime.handle.current().unwrap();
        assert_eq!(current.session.observe().await.unwrap().cells.len(), 270);
        assert!(
            current
                .session
                .read_source(terminal.assistant.actor.clone(), first_cell.clone())
                .await
                .unwrap()
                .is_none()
        );
        let preview = runtime
            .handle
            .preview_delete_work(
                current.generation.clone(),
                "request-test".into(),
                first_cell.clone(),
            )
            .await
            .unwrap();
        runtime
            .handle
            .delete_work(
                current.generation.clone(),
                "request-test".into(),
                preview.token,
                false,
                false,
            )
            .await
            .unwrap();
        // The persisted history changes, but this exact terminal and agent remain live.
        assert_eq!(
            runtime.handle.current().unwrap().generation,
            current.generation
        );
        assert!(!terminal.stopped.is_cancelled());
        terminal.check(&runtime.handle).unwrap();
        let live_context = invoke(&terminal, &runtime.handle, "workspace_context", json!({})).await;
        let continued = invoke(&terminal, &runtime.handle, "execute", json!({"source":":calc { return 99; }", "request_id":"after-retirement", "context":live_context["context"], "wait_ms":1000})).await;
        assert_eq!(continued["execution"]["settled"], true);
        let retired = invoke(&terminal, &runtime.handle, "execute", json!({"source":":calc { return 42; }", "request_id":"r0", "context":context["context"]})).await;
        assert_eq!(retired["duplicate"], true);
        assert_eq!(retired["execution"]["cell"], first_cell);
        assert!(
            perform(
                &terminal,
                &runtime.handle,
                &[
                    json!({"name":"cancel","arguments":{"workspace":"default","request_id":"r0"}})
                        .to_string()
                ]
            )
            .await
            .is_err()
        );
        assert_eq!(current.session.observe().await.unwrap().cells.len(), 270);
        manager.shutdown().await;
        runtime.shutdown().await.unwrap();
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn typed_value_read_carries_captured_contracts_for_full_selected_paged_and_shape_reads() {
        let root = tempfile::tempdir().unwrap();
        let home = root.path().join("home");
        let runtime = launch(RuntimeOptions::new(home.clone(), root.path().into()))
            .await
            .unwrap();
        let manager = Manager::default();
        let history = uuid::Uuid::new_v4().to_string();
        let terminal = test_terminal(&manager, &runtime, &home, root.path(), &history).await;
        let _stop_on_failure = terminal.stopped.clone().drop_guard();
        let current = runtime.handle.current().unwrap();
        let package = "version: 2\ntypes: {Status: {base: Text, enum: [ready, failed], display: {enumTones: {ready: ok, failed: bad}}}, Row: {base: Record, fields: {status: Status}}}";
        current
            .session
            .submit(
                SourceInput::new("meta-package".into(), ":package load source:\"\"".into())
                    .unwrap()
                    .with_document(Some(package.into()))
                    .unwrap(),
            )
            .await
            .unwrap();
        current.session.submit(SourceInput::new("meta-values".into(), ":def rows() -> List<Row> as :calc pure { return [{status:'ready'}]; }\nrows > declared".into()).unwrap()).await.unwrap();
        current.session.wait_idle().await.unwrap();
        let full = invoke(
            &terminal,
            &runtime.handle,
            "value_read",
            json!({"name":"declared","typed":true}),
        )
        .await;
        assert_eq!(
            full["meta"]["fields"]["/e/f:status"]["members"],
            json!(["ready", "failed"])
        );
        let paged = invoke(
            &terminal,
            &runtime.handle,
            "value_read",
            json!({"name":"declared","typed":true,"offset":1,"limit":1}),
        )
        .await;
        assert_eq!(
            full["meta"]["fields"]["/e/f:status"]["tones"]["ready"],
            "ok"
        );
        assert_eq!(paged["value"]["meta"], full["meta"]);
        let selected = invoke(
            &terminal,
            &runtime.handle,
            "value_read",
            json!({"name":"declared","typed":true,"select":"/0/status"}),
        )
        .await;
        assert_eq!(selected["value"]["meta"]["contract"]["name"], "Status");
        let shape = invoke(
            &terminal,
            &runtime.handle,
            "value_read",
            json!({"name":"declared","typed":true,"shape_only":true}),
        )
        .await;
        assert_eq!(shape["meta"], full["meta"]);
        assert_eq!(
            invoke(
                &terminal,
                &runtime.handle,
                "value_read",
                json!({"name":"declared"})
            )
            .await,
            json!([{"status":"ready"}])
        );
        manager.shutdown().await;
        runtime.shutdown().await.unwrap();
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn help_tree_and_refresh_observation_reduce_discovery_and_polling_calls() {
        let root = tempfile::tempdir().unwrap();
        let home = root.path().join("home");
        let runtime = launch(RuntimeOptions::new(home.clone(), root.path().into()))
            .await
            .unwrap();
        let manager = Manager::default();
        let history = uuid::Uuid::new_v4().to_string();
        let terminal = test_terminal(&manager, &runtime, &home, root.path(), &history).await;
        let _stop_on_failure = terminal.stopped.clone().drop_guard();
        let help = invoke(
            &terminal,
            &runtime.handle,
            "help",
            json!({"command":"node", "depth":1}),
        )
        .await;
        let refresh = help["children"]
            .as_array()
            .unwrap()
            .iter()
            .find(|child| child["name"] == "refresh")
            .unwrap();
        assert!(refresh["invocation"].is_object(), "{help}");
        assert_eq!(refresh["assistant_allowed"], true);
        assert!(serde_json::to_vec(&help).unwrap().len() < 64 * 1024);
        let docker = invoke(
            &terminal,
            &runtime.handle,
            "help",
            json!({"provider":"docker", "tail":["container"], "depth":1}),
        )
        .await;
        let create = docker["children"]
            .as_array()
            .unwrap()
            .iter()
            .find(|child| child["name"] == "create")
            .unwrap();
        assert!(create["invocation"].is_object(), "{docker}");
        let excessive = perform(
            &terminal,
            &runtime.handle,
            &[json!({"name":"help", "arguments":{"command":"node", "depth":4}}).to_string()],
        )
        .await
        .unwrap_err();
        assert!(
            excessive
                .stderr
                .contains("arguments.depth must be at most 3")
        );
        let context = invoke(&terminal, &runtime.handle, "workspace_context", json!({})).await;
        let initial = invoke(&terminal, &runtime.handle, "execute", json!({"source":":calc { return 1; } > seed\n:calc { return $seed + 1; } > child", "request_id":"seed", "context":context["context"], "wait_ms":1000})).await;
        let refreshed = invoke(&terminal, &runtime.handle, "execute", json!({"source":":refresh $seed scope:downstream", "request_id":"refresh", "context":initial["context"], "wait_ms":1000})).await;
        assert_eq!(refreshed["execution"]["settled"], true, "{refreshed}");
        let nodes = refreshed["execution"]["nodes"].as_array().unwrap();
        assert_eq!(nodes.len(), 2);
        for (before, after) in initial["execution"]["nodes"]
            .as_array()
            .unwrap()
            .iter()
            .zip(nodes)
        {
            assert_eq!(after["refreshed"], true);
            assert_eq!(after["state"], "ready");
            assert_ne!(before["run"], after["run"]);
        }
        assert_eq!(
            invoke(
                &terminal,
                &runtime.handle,
                "value_read",
                json!({"name":"child"})
            )
            .await,
            json!(2)
        );
        let receipt = invoke(
            &terminal,
            &runtime.handle,
            "execution_read",
            json!({"request_id":"refresh"}),
        )
        .await;
        assert_eq!(receipt["nodes"], refreshed["execution"]["nodes"]);
        let automatic = invoke(&terminal, &runtime.handle, "execute", json!({"source":":refresh $seed", "request_id":"refresh-root", "context":refreshed["context"], "wait_ms":1000})).await;
        // A root-only refresh observes the root, not its automatic dependents.
        // Wait on the original request, which contains both the root and child.
        assert_eq!(automatic["execution"]["nodes"].as_array().unwrap().len(), 1);
        let current = invoke(
            &terminal,
            &runtime.handle,
            "execution_read",
            json!({"request_id":"seed", "wait_ms":1000}),
        )
        .await;
        assert_eq!(current["settled"], true, "{current}");
        assert_eq!(
            current["nodes"][1]["state"], "ready",
            "bounded pure child updates under automatic policy"
        );
        assert_ne!(current["nodes"][1]["run"], nodes[1]["run"]);
        assert!(current["nodes"][1].get("staleReason").is_none());
        let manual = invoke(&terminal, &runtime.handle, "execute", json!({"source":":policy $child mode:manual", "request_id":"manual-child", "context":automatic["context"], "wait_ms":1000})).await;
        invoke(&terminal, &runtime.handle, "execute", json!({"source":":refresh $seed", "request_id":"refresh-manual-root", "context":manual["context"], "wait_ms":1000})).await;
        let stale = invoke(
            &terminal,
            &runtime.handle,
            "execution_read",
            json!({"request_id":"seed"}),
        )
        .await;
        assert_eq!(stale["nodes"][1]["state"], "stale");
        assert_eq!(
            stale["nodes"][1]["staleReason"]["code"],
            "dependency_refreshed"
        );
        let cell = invoke(
            &terminal,
            &runtime.handle,
            "cell_read",
            json!({"cell":initial["execution"]["cell"]}),
        )
        .await;
        assert_eq!(
            cell["nodes"][1]["staleReason"],
            stale["nodes"][1]["staleReason"]
        );
        assert!(cell.get("source").is_none());
        manager.shutdown().await;
        runtime.shutdown().await.unwrap();
        drop(terminal);
        let reopened = launch(RuntimeOptions::new(home.clone(), root.path().into()))
            .await
            .unwrap();
        let manager = Manager::default();
        let terminal = test_terminal(&manager, &reopened, &home, root.path(), &history).await;
        let _stop_on_failure = terminal.stopped.clone().drop_guard();
        let restored = invoke(
            &terminal,
            &reopened.handle,
            "execution_read",
            json!({"request_id":"refresh"}),
        )
        .await;
        assert_eq!(restored["restored"], true);
        assert_eq!(
            restored["nodes"][1]["staleReason"]["code"],
            "dependency_refreshed"
        );
        assert_eq!(restored["nodes"].as_array().unwrap().len(), 2);
        assert!(
            restored["nodes"]
                .as_array()
                .unwrap()
                .iter()
                .all(|node| node["refreshed"] == true)
        );
        manager.shutdown().await;
        reopened.shutdown().await.unwrap();
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn sequential_requests_require_targets_and_never_resume_reserved_steps_after_restart() {
        let root = tempfile::tempdir().unwrap();
        let home = root.path().join("home");
        let history = uuid::Uuid::new_v4().to_string();
        let runtime = launch(RuntimeOptions::new(home.clone(), root.path().into()))
            .await
            .unwrap();
        let manager = Manager::default();
        let terminal = test_terminal(&manager, &runtime, &home, root.path(), &history).await;
        let _stop_on_failure = terminal.stopped.clone().drop_guard();
        let context = invoke(&terminal, &runtime.handle, "workspace_context", json!({})).await;
        let refused=perform(&terminal,&runtime.handle,&[json!({"name":"execute","arguments":{"source":":calc { return 1; }","context":context["context"],"request_id":"missing-target"}}).to_string()]).await.unwrap_err();
        assert!(refused.stderr.contains("workspace"));
        let joined = invoke(
            &terminal,
            &runtime.handle,
            "workspace_open",
            json!({"name":"other","create":true}),
        )
        .await;
        assert_eq!(joined["executeArguments"]["workspace"], "other");
        let mismatch=perform(&terminal,&runtime.handle,&[json!({"name":"execute","arguments":{"workspace":"default","source":":calc { return 1; }","context":joined["context"],"request_id":"wrong-target"}}).to_string()]).await.unwrap_err();
        assert!(
            mismatch.stderr.contains("default") && mismatch.stderr.contains("does not redirect")
        );
        let current = runtime.handle.current().unwrap();
        assert!(current.session.observe().await.unwrap().cells.is_empty());
        let text = ":calc { return 1; } > first\n:calc { return $first + 1; } > second";
        let token = wes_engine::driver::CancellationToken::new();
        token.cancel();
        let original = current
            .session
            .submit_sequential_request(
                terminal.assistant.namespace.clone(),
                "reserved".into(),
                context["context"].as_str().unwrap().into(),
                true,
                SourceInput::new("reserved-root".into(), text.into())
                    .unwrap()
                    .with_client(terminal.assistant.actor.clone())
                    .unwrap()
                    .cooperative(),
                token,
            )
            .await
            .unwrap();
        assert!(original.claim.fresh);
        manager.shutdown().await;
        runtime.shutdown().await.unwrap();
        drop(terminal);
        let runtime = launch(RuntimeOptions::new(home.clone(), root.path().into()))
            .await
            .unwrap();
        let manager = Manager::default();
        let terminal = test_terminal(&manager, &runtime, &home, root.path(), &history).await;
        let _stop_on_failure = terminal.stopped.clone().drop_guard();
        let read = invoke(
            &terminal,
            &runtime.handle,
            "execution_read",
            json!({"request_id":"reserved","wait_ms":1000}),
        )
        .await;
        assert_eq!(read["status"], "interrupted");
        assert_eq!(read["settled"], true);
        let duplicate=invoke(&terminal,&runtime.handle,"execute",json!({"request_id":"reserved","context":context["context"],"source":text,"sequential":true,"wait_ms":1000})).await;
        assert_eq!(duplicate["duplicate"], true);
        assert_eq!(duplicate["execution"]["status"], "interrupted");
        let current = runtime.handle.current().unwrap();
        assert!(current.session.observe().await.unwrap().cells.is_empty());
        let refused=perform(&terminal,&runtime.handle,&[json!({"name":"cancel","arguments":{"workspace":"default","request_id":"reserved"}}).to_string()]).await.unwrap_err();
        assert!(!refused.stderr.is_empty());
        manager.shutdown().await;
        runtime.shutdown().await.unwrap();
    }

    #[test]
    fn outcome_metadata_never_discloses_private_or_unknown_values() {
        use wes_core::{Data, Primitive, Provenance, RecordShape, Shape};
        for (name, field, code, key) in [
            ("ProcessOutput", "exitCode", 1, "exit_code"),
            ("HttpResponse", "status", 404, "http_status"),
        ] {
            let value = wes_core::Value::new(
                Shape::Record(
                    RecordShape::new(name, [(field.into(), Shape::Primitive(Primitive::Int))])
                        .unwrap(),
                ),
                Data::Record([(field.into(), Data::Int(code))].into_iter().collect()),
                Provenance::default(),
            )
            .unwrap();
            assert_eq!(
                public_outcome(&value),
                Some(json!({key:code, "success":false}))
            );
            for policy in [
                wes_core::flow::FlowPolicy::default().private(),
                wes_core::flow::FlowPolicy::default().unknown(),
            ] {
                assert_eq!(
                    public_outcome(
                        &value
                            .clone()
                            .with_provenance(Provenance::default().with_policy(&policy))
                    ),
                    None
                );
            }
        }
    }
    #[test]
    fn compact_context_preserves_every_original_validity_dimension() {
        let original = json!({"workspace":"default","generation":"one","selected":"demo","revisions":{"demo":"v1"}});
        let token = context_token(&original);
        assert_eq!(token.len(), 68);
        assert_eq!(context_token(&original), token);
        for (key, value) in [
            ("workspace", json!("shared")),
            ("generation", json!("two")),
            ("selected", json!(null)),
            ("revisions", json!({"demo":"v2"})),
        ] {
            let mut changed = original.clone();
            changed[key] = value;
            assert_ne!(context_token(&changed), token, "{key}");
        }
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn waits_observe_completion_timeout_admission_failure_and_revocation() {
        let root = tempfile::tempdir().unwrap();
        let runtime = launch(RuntimeOptions::new(
            root.path().join("home"),
            root.path().into(),
        ))
        .await
        .unwrap();
        let manager = Manager::default();
        let current = runtime.handle.current().unwrap();
        let config = crate::runtime::browser_services(root.path().join("home"), root.path().into())
            .unwrap()
            .terminal
            .unwrap();
        let id = manager
            .start(
                config,
                current.clone(),
                "wiring-test".into(),
                0,
                runtime.handle.clone(),
            )
            .await
            .unwrap();
        let terminal = manager
            .owned(&id, &current.generation, "wiring-test")
            .unwrap();
        let _stop_on_failure = terminal.stopped.clone().drop_guard();
        let execution = Execution {
            cell: "waited".into(),
            actor: terminal.assistant.actor.clone(),
            steps: Vec::new(),
        };
        let start = tokio::time::Instant::now();
        let timed = observe_execution(&terminal, &runtime.handle, &current, &execution, 10)
            .await
            .ok()
            .unwrap();
        assert!(!settled(&timed, &execution));
        assert!(start.elapsed() >= Duration::from_millis(10));
        let waiting = observe_execution(&terminal, &runtime.handle, &current, &execution, 1000);
        let submission = async {
            current
                .session
                .submit(
                    SourceInput::new(execution.cell.clone(), ":calc { return 42; }".into())
                        .unwrap(),
                )
                .await
                .unwrap();
        };
        let (observed, ()) = tokio::join!(waiting, submission);
        assert!(settled(&observed.ok().unwrap(), &execution));
        let mut failed = current.session.observe().await.unwrap();
        failed
            .cells
            .iter_mut()
            .find(|c| c.input.cell() == execution.cell)
            .unwrap()
            .reply = Some(Err(wes_engine::session::SessionError::Stopped));
        assert!(settled(&failed, &execution));
        assert_eq!(
            report(&failed, &execution, false, Results::Full)["status"],
            "admission_failed"
        );
        let missing = Execution {
            cell: "never-admitted".into(),
            ..execution
        };
        let wait = observe_execution(&terminal, &runtime.handle, &current, &missing, 1000);
        let revoke = async {
            tokio::task::yield_now().await;
            terminal.stopped.cancel();
        };
        let (reply, ()) = tokio::join!(wait, revoke);
        assert!(reply.is_err());
        manager.shutdown().await;
        runtime.shutdown().await.unwrap();
    }
}
