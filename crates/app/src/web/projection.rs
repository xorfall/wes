//! Bounded current UI projection. Values remain on the data plane; wakeups are not a journal.
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, BTreeSet, VecDeque},
    io::{self, Write},
    sync::Arc,
};
use wes_adapters::credentials::MemoryCredentials;
use wes_core::{
    ErrorValue,
    capability::{DeclaredRule, EnumChoices, EnumDomain, Parameter, Rule, Safety, Sort},
};
use wes_engine::{
    credentials::Credentials,
    graph::{DependencyGraph, NodeId, NodeState, OutputPort},
    history::{JournalEntry, NoticeContext},
    log::LogStatus,
    session::{SessionObservation, ValuePublication},
};
use wes_language::{Diagnostic, Severity, vocabulary::ANNOTATIONS};

fn display_type(value: &wes_core::Value) -> String {
    if let wes_core::Data::Iter(iter) = value.data()
        && let Some(contract) = iter.item_contract()
    {
        format!("Iter<{}>", contract.name())
    } else {
        value.shape().to_string()
    }
}
fn current_definition<'a>(
    call: &wes_engine::providers::BoundCall,
    names: impl IntoIterator<Item = (&'a String, &'a wes_engine::graph::OutputRef)>,
) -> String {
    let names: Vec<_> = names.into_iter().collect();
    let reference = |output: &wes_engine::graph::OutputRef| {
        if let Some((name, _)) = names.iter().find(|(_, selected)| *selected == output) {
            format!("${name}")
        } else if output.port == OutputPort::Data {
            format!("${}", output.node)
        } else {
            format!("${}::{}", output.node, output.port.selector())
        }
    };
    let invocation = call.invocation();
    let mut text = format!(
        "{} {}",
        invocation.provider.name(),
        invocation.capability.path.join(" ")
    );
    for (key, input) in &invocation.inputs {
        let mut value = String::new();
        if !input_preview(input, &reference, &mut value, &mut 256) {
            value = "[value exceeds source preview]".into();
        }
        if text.len() + key.len() + value.len() > 64 * 1024 {
            text.push_str(" …");
            break;
        }
        text.push_str(&format!(" {key}:{value}"));
    }
    text
}

fn input_preview(
    input: &wes_engine::plan::Input,
    reference: &impl Fn(&wes_engine::graph::OutputRef) -> String,
    text: &mut String,
    nodes: &mut usize,
) -> bool {
    use wes_engine::plan::Input;
    if *nodes == 0 || text.len() > 8192 {
        return false;
    }
    *nodes -= 1;
    match input {
        Input::FromNode(output) => text.push_str(&reference(output)),
        Input::FieldPath { output, fields } => {
            text.push_str(&format!("{}.{}", reference(output), fields.join(".")))
        }
        Input::Literal(value)
            if value.provenance().policy().is_private()
                || value.provenance().policy().is_unknown() =>
        {
            text.push_str("[private value]")
        }
        Input::Literal(value) => {
            let Some(encoded) = wes_adapters::codec::encode_json(
                value.data(),
                wes_adapters::codec::Limits {
                    bytes: 8192 - text.len(),
                    nodes: *nodes + 1,
                },
            )
            .ok()
            .and_then(|bytes| String::from_utf8(bytes).ok()) else {
                return false;
            };
            text.push_str(&encoded);
        }
        Input::Record(fields) => {
            text.push('{');
            for (index, (key, input)) in fields.iter().enumerate() {
                if index > 0 {
                    text.push_str(", ");
                }
                text.push_str(&wes_language::quote_text(key));
                text.push(':');
                if !input_preview(input, reference, text, nodes) {
                    return false;
                }
            }
            text.push('}');
        }
        Input::List { items, .. } => {
            text.push('[');
            for (index, input) in items.iter().enumerate() {
                if index > 0 {
                    text.push_str(", ");
                }
                if !input_preview(input, reference, text, nodes) {
                    return false;
                }
            }
            text.push(']');
        }
    }
    text.len() <= 8192
}

/// A presentation property of the dependency graph, not the current run state.
/// Walk shared descendants once, including branches and independently declared calculations.
fn stream_outputs<T>(graph: &DependencyGraph<T>, source: impl Fn(&T) -> bool) -> BTreeSet<NodeId> {
    let mut queue: VecDeque<_> = graph
        .nodes()
        .filter(|n| source(n.payload()))
        .map(|n| n.id().clone())
        .collect();
    let mut reached = BTreeSet::new();
    while let Some(id) = queue.pop_front() {
        if reached.insert(id.clone()) {
            queue.extend(graph.dependents_of(&id).cloned());
        }
    }
    reached
}

fn max_bytes() -> usize {
    wes_budgets::get("transport.projection.bytes") as usize
}
#[derive(Clone)]
pub(super) struct Projection {
    capacity: Option<Arc<str>>,
    pub generation: String,
    pub workspace: Option<String>,
    pub frames: BTreeMap<String, Arc<str>>,
    pub order: Vec<String>,
    pub nodes: BTreeSet<String>,
    pub runs: BTreeMap<wes_engine::graph::NodeId, wes_engine::runtime::RunId>,
    pub has_conversations: bool,
    complete: bool,
    bytes: usize,
}
impl Projection {
    pub(super) fn closed(
        name: Option<&str>,
        names: &[wes_engine::workspace::WorkspaceName],
        warning: Option<String>,
    ) -> Self {
        let mut p = Self::empty(&format!("closed:{}", name.unwrap_or("")));
        p.push("workspace-context".into(),json!({"event":"workspace-context","name":"","saved":names.iter().map(|n|n.as_str()).collect::<Vec<_>>()})).expect("bounded names");
        p.push(
            "workspace-closed".into(),
            json!({"event":"workspace-closed","workspace":name}),
        )
        .expect("bounded name");
        if let Some(warning) = warning {
            p.storage_warning(warning).expect("bounded warning");
        }
        p
    }

    pub(super) fn capacity(&mut self, capacity: wes_engine::driver::CapacitySnapshot) {
        self.capacity = Some(
            json!({"event":"execution-capacity",
            "operations":{"used":capacity.operations.used,"limit":capacity.operations.limit},
            "streams":{"used":capacity.streams.used,"limit":capacity.streams.limit}})
            .to_string()
            .into(),
        );
    }

    pub(super) fn storage_warning(&mut self, message: String) -> io::Result<()> {
        self.push(
            "storage-warning".into(),
            json!({"event":"storage-warning", "message":message}),
        )
    }
    pub(super) fn empty(generation: &str) -> Self {
        Self {
            capacity: None,
            generation: generation.into(),
            workspace: None,
            frames: BTreeMap::new(),
            order: vec![],
            nodes: BTreeSet::new(),
            runs: BTreeMap::new(),
            has_conversations: false,
            complete: true,
            bytes: 0,
        }
    }
    pub(super) fn unavailable(generation: &str) -> Self {
        let mut projection = Self::empty(generation);
        projection.complete = false;
        projection.push("unavailable".into(), json!({"event":"projection-unavailable", "message":"Workspace state could not be displayed. The engine and stored results remain available. Load a smaller saved workspace to continue here, or use native commands."})).expect("constant bounded projection error");
        projection
    }
    fn push(&mut self, key: String, event: Value) -> io::Result<()> {
        let mut output = Limited {
            bytes: Vec::new(),
            remaining: max_bytes().saturating_sub(self.bytes),
        };
        serde_json::to_writer(&mut output, &event)?;
        let text = String::from_utf8(output.bytes).map_err(io::Error::other)?;
        self.bytes += text.len() + key.len() + 128;
        if self.bytes > max_bytes() {
            return Err(io::Error::other("client state exceeds its byte budget"));
        }
        self.order.push(key.clone());
        self.frames.insert(key, text.into());
        Ok(())
    }
    pub fn changes(&self, previous: Option<&Self>) -> Vec<Arc<str>> {
        let previous = previous
            .filter(|old| old.generation == self.generation && old.complete == self.complete);
        let mut output = vec![];
        if previous.is_none() {
            output.push(
                json!({"event":"session", "generation":self.generation,"workspace":self.workspace,
                    "cells": self.frames.keys().filter_map(|key| key.strip_prefix("cell:")).collect::<Vec<_>>()})
                    .to_string()
                    .into(),
            );
        }
        if let Some(old) = previous {
            let dropped: Vec<_> = old.nodes.difference(&self.nodes).collect();
            if !dropped.is_empty() {
                output.push(
                    json!({"event":"dropped", "nodes":dropped})
                        .to_string()
                        .into(),
                );
            }
        }
        if let Some(old) = previous {
            let retired: Vec<_> = old
                .frames
                .keys()
                .filter(|key| !self.frames.contains_key(*key))
                .filter_map(|key| key.strip_prefix("cell:"))
                .collect();
            if !retired.is_empty() {
                output.push(
                    json!({"event":"work-retired", "cells":retired})
                        .to_string()
                        .into(),
                );
            }
        }
        // A rolling history is a keyed window. Eviction removes identities; it never
        // requires replaying unchanged entries. One delta is applied atomically by the client.
        let removed: Vec<_> = previous
            .into_iter()
            .flat_map(|old| old.frames.keys())
            .filter(|key| key.starts_with("log:") && !self.frames.contains_key(*key))
            .filter_map(|key| key.strip_prefix("log:"))
            .collect();
        let entries: Vec<_> = self
            .order
            .iter()
            .filter(|key| key.starts_with("log:"))
            .filter(|key| previous.is_none_or(|old| old.frames.get(*key) != self.frames.get(*key)))
            .map(|key| self.frames[key].as_ref())
            .collect();
        for key in &self.order {
            if key.starts_with("log:") {
                continue;
            }
            let frame = &self.frames[key];
            if previous.is_none_or(|old| old.frames.get(key) != Some(frame)) {
                output.push(frame.clone());
            }
        }
        if previous.is_none() || !removed.is_empty() || !entries.is_empty() {
            let removed = serde_json::to_string(&removed).expect("string IDs");
            output.push(
                format!(
                    r#"{{"event":"log-delta","reset":{},"removed":{},"entries":[{}]}}"#,
                    previous.is_none(),
                    removed,
                    entries.join(",")
                )
                .into(),
            );
        }
        if let Some(capacity) = &self.capacity
            && previous.and_then(|old| old.capacity.as_ref()) != Some(capacity)
        {
            output.push(capacity.clone());
        }
        output
    }
}
struct Limited {
    bytes: Vec<u8>,
    remaining: usize,
}
impl Write for Limited {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        if bytes.len() > self.remaining {
            return Err(io::Error::other("client state exceeds its byte budget"));
        }
        self.remaining -= bytes.len();
        self.bytes.extend_from_slice(bytes);
        Ok(bytes.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

pub(super) fn build(
    generation: String,
    workspace: String,
    observation: SessionObservation,
    credentials: &MemoryCredentials,
    storage: Option<&super::services::StorageReport>,
) -> io::Result<Projection> {
    let mut out = Projection {
        capacity: None,
        generation,
        workspace: Some(workspace.clone()),
        frames: BTreeMap::new(),
        order: vec![],
        nodes: BTreeSet::new(),
        runs: BTreeMap::new(),
        has_conversations: !observation.state.conversations.is_empty(),
        complete: true,
        bytes: 0,
    };
    out.push(
        "workspace-context".into(),
        json!({"event":"workspace-context", "name":workspace,
        "saved":observation.saved_workspaces.iter().map(|name| name.as_str()).collect::<Vec<_>>()}),
    )?;
    if let Some(storage) = storage.filter(|report| report.generation == out.generation) {
        out.push("storage".into(), storage.event())?;
    }
    let resources = wes_engine::completion::retained_resources(&observation);
    out.push(
        "vocabulary".into(),
        vocabulary(&observation, credentials, &resources),
    )?;
    let environments: BTreeMap<_, _> = observation
        .environment_catalogues
        .iter()
        .map(|(name, catalogue)| {
            let mut providers = provider_vocabulary(catalogue, None, &resources, Some(name));
            for provider in &mut providers {
                if let Some((kind, target, endpoint)) = provider["name"]
                    .as_str()
                    .and_then(|alias| observation.environment_providers.get(name)?.get(alias))
                {
                    provider["kind"] = json!(if kind == "configured" {
                        "builtin"
                    } else {
                        kind
                    });
                    provider["target"] = json!(target);
                    // Endpoints are configuration, but URL userinfo/query can contain secrets.
                    provider["endpoint"] = json!(endpoint.as_deref().and_then(public_endpoint));
                }
            }
            (name, providers)
        })
        .collect();
    let revisions: BTreeMap<_, _> = observation
        .environment_revisions
        .iter()
        .map(|(n, r)| (n, r.to_string()))
        .collect();
    let clients: BTreeMap<_, _> = observation.environment_clients.iter().map(|(client, context)| (client, json!({"selected":context.selected,"revisions":context.revisions.iter().map(|(n,r)|(n,r.to_string())).collect::<BTreeMap<_,_>>()}))).collect();
    out.push("environments".into(), json!({"event":"environments","managed":observation.environment_managed,"default":observation.default_environment,"enabled":observation.environment_enabled,"credentials":observation.environment_credentials,"revisions":revisions,"providers":environments,"clients":clients}))?;
    out.push("log-status".into(), log_status(&observation.log))?;
    if let Some(values) = &observation.values {
        out.push("keeping".into(), json!({"event":"keeping", "automatic":values.policy.automatic, "under":values.policy.under}))?;
    }
    let execution = &observation.state.execution;
    let started: BTreeMap<_, _> = observation
        .log
        .entries
        .iter()
        .filter_map(|entry| {
            let Some(record) = entry.entry().observation() else {
                return None;
            };
            recorded_start(record, execution.runs.get(record.node()))
                .map(|at| (record.node().as_str(), at))
        })
        .fold(BTreeMap::new(), |mut starts, (node, at)| {
            starts.entry(node).or_insert(at);
            starts
        });
    let mut sources = BTreeMap::new();
    let work_roots = observation.work_roots();
    for cell in &observation.cells {
        let Some(reply) = &cell.reply else {
            continue;
        };
        let nodes: Vec<_> = reply
            .as_ref()
            .map(|reply| {
                reply
                    .nodes
                    .iter()
                    .filter(|node| execution.graph.node(node).is_some())
                    .map(|node| node.as_str())
                    .collect()
            })
            .unwrap_or_default();
        for node in &nodes {
            sources.insert(*node, cell.input.text());
        }
        out.push(format!("cell:{}", cell.input.cell()), json!({"event":"planned", "cell":cell.input.cell(), "text":cell.input.text(), "document":cell.input.document().map(|source| json!({"source":source})), "nodes":nodes,
            "receipts":reply.as_ref().ok().map(|r|r.receipts.iter().map(crate::execution_status::operation_receipt).collect::<Vec<_>>()),
            "diagnostics":reply.as_ref().ok().map(|r| r.diagnostics.diagnostics.iter().map(|d| diagnostic(d, cell.input.text())).collect::<io::Result<Vec<_>>>()).transpose()?,
            "restored":reply.as_ref().is_ok_and(|r|r.restored),
            "workOf":work_roots.get(cell.input.cell()),
            "revisionOf":cell.input.revision_of(),
            "revisionAccepted":cell.input.revision_of().is_some() && reply.as_ref().is_ok_and(|r| !r.nodes.is_empty() && !r.diagnostics.diagnostics.iter().any(|d|d.severity == wes_language::Severity::Error)),
            "repeatOf":cell.input.repeat().map(|r|r.origin.as_str()),
            "acknowledgeEffects":cell.input.repeat().is_some_and(|r|r.acknowledge_effects),
            "repeatFrom":cell.input.repeat().and_then(|r|r.from.as_ref()).map(|n|n.as_str()),
            "repeatedRun":reply.as_ref().ok().and_then(|r|r.repeated_run.as_ref()).map(|r|r.as_str()),
            "failure":reply.as_ref().err().map(|e|e.to_string())}))?;
        let diagnostics: Vec<_> = match reply {
            Ok(reply) => reply
                .diagnostics
                .diagnostics
                .iter()
                .map(|d| diagnostic(d, cell.input.text()))
                .collect::<io::Result<_>>()?,
            Err(error) => vec![
                json!({"code":"ENG007", "severity":"error", "message":error.to_string(), "start":0,"end":0,"hints":[]}),
            ],
        };
        out.push(format!("reported:{}", cell.input.cell()), json!({"event":"reported", "cell":cell.input.cell(), "source":cell.input.text(), "diagnostics":diagnostics}))?;
    }
    let stream_outputs = stream_outputs(&execution.graph, |task| {
        task.call()
            .is_some_and(wes_engine::providers::BoundCall::streaming)
    });
    for node in execution.graph.nodes() {
        let id = node.id().as_str();
        out.nodes.insert(id.to_owned());
        let name = observation
            .state
            .names
            .iter()
            .find(|(_, output)| output.node == *node.id() && output.port == OutputPort::Data)
            .map_or("", |(name, _)| name.as_str());
        let interactive = node
            .payload()
            .call()
            .is_some_and(wes_engine::providers::BoundCall::interactive);
        if let Some(run) = execution.runs.get(node.id()) {
            out.runs.insert(node.id().clone(), run.clone());
            out.bytes += 64;
        }
        out.push(format!("created:{id}"), json!({"event":"created", "node":id, "errorNames":observation.state.names.iter().filter(|(_,output)| output.node==*node.id() && output.port==OutputPort::Error).take(8).map(|(name,_)|name).collect::<Vec<_>>(), "dependencyLifetime":if execution.creation_inputs.contains_key(node.id()) {"creation"} else {"continuous"}, "dependsOn":node.dependencies().keys().map(|id| id.as_str()).collect::<Vec<_>>(), "name":name, "command":sources.get(id).copied().unwrap_or(""), "currentDefinition":node.payload().call().filter(|call|call.definition_changed()).map(|call|current_definition(call, observation.state.names.iter())), "run":execution.runs.get(node.id()).map(|run|run.as_str()), "interactive":interactive, "streamOutput":stream_outputs.contains(node.id()), "streamSource":node.payload().call().is_some_and(wes_engine::providers::BoundCall::streaming), "repeatable":node.payload().traits().repeatable, "traced":node.payload().call().is_some_and(|call|call.invocation().trace_profile.is_some()), "startedAt":started.get(id)}))?;
        if let Some(binding) = node.payload().environments().next() {
            out.push(format!("environment:{id}"), json!({"event":"node-environment","node":id,"environment":binding.environment().name(),"revision":binding.environment().revision().to_string(),"target":binding.import().target().name(),"endpoint":binding.import().endpoint(),"origin":binding.import().origin().environment}))?;
        }
        let mut state = json!({"event":"node", "node":id, "state":state(node.state())});
        if let Some(progress) = execution.progress.get(node.id()) {
            out.push(format!("progress:{id}"),json!({"event":"node-progress","node":id,"run":execution.runs.get(node.id()).map(|run|run.as_str()),"progress":progress}))?;
        }
        if let Some(error) = execution.errors.get(node.id()) {
            state = match node.state() {
                NodeState::Failed => {
                    json!({"event":"failed","node":id,"reason":error.message(),"error":self::error(error)})
                }
                NodeState::Cancelled => {
                    json!({"event":"cancelled","node":id,"code":error.code(),"reason":error.message()})
                }
                _ => state,
            };
        }
        state["updatePending"] = json!(execution.input_updates.contains(node.id()));
        if let Some(reason) = execution.stale_reasons.get(node.id()) {
            state["staleReason"] = json!({"code":reason.code(),"message":reason.message()});
        }
        if let Some(waits) = execution.waiting_inputs.get(node.id()) {
            state["waiting"] =
                crate::execution_status::waiting_inputs(waits, observation.state.names.iter());
        }
        // State and ready share a key: refresh replaces the ready announcement immediately.
        let stopped = execution.evidence_values.get(node.id());
        if node.state() == NodeState::Ready || stopped.is_some() {
            let published = observation
                .values
                .as_ref()
                .and_then(|values| values.outputs.get(node.id()));
            let stored = match published {
                Some(ValuePublication::Complete(value)) => value.stored.as_ref().map(|stored| {
                    (
                        stored.handle.as_str(),
                        stored.bytes,
                        value.durably_retained(),
                        stored.retention.as_str(),
                    )
                }),
                Some(ValuePublication::Recovered(value)) => Some((
                    value.handle.as_str(),
                    value.bytes,
                    true,
                    value.retention.as_str(),
                )),
                _ => None,
            };
            if let Some((handle, bytes, kept, retention)) = stored
                && let Some(value) = stopped
                    .map(|last| &last.value)
                    .or_else(|| execution.values.get(node.id()))
            {
                state = if value.provenance().policy().is_private() {
                    json!({"event":"ready", "node":id, "type":display_type(value), "handle":handle, "bytes":bytes, "provenance":{}, "cautions":["Private · memory only · unavailable after restart"], "kept":false,"private":true})
                } else {
                    json!({"event":"ready", "node":id, "type":display_type(value), "handle":handle, "bytes":bytes, "provenance":value.provenance().facts(), "cautions":value.provenance().cautions(), "kept":kept,"retention":retention,"private":false})
                };
            }
            if let Some(last) = stopped {
                // Historical visibility never overwrites the engine's terminal graph state.
                if state["event"] == "ready" {
                    state["event"] = json!("evidence");
                    state["state"] = json!(self::state(node.state()));
                    state["source"] = json!(last.source.as_str());
                    state["run"] = json!(last.run.as_str());
                    state["kind"] = json!(last.kind.name());
                    if let Some(error) = execution.errors.get(node.id()) {
                        state["error"] = self::error(error);
                        state["reason"] = json!(error.message());
                    }
                }
            }
            // One frame carries both facts: execution completion is not publication success.
            state["publication"] = publication(published);
        }
        state["constructionComplete"] = json!(
            execution
                .creation_inputs
                .get(node.id())
                .copied()
                .unwrap_or(false)
        );
        out.push(format!("state:{id}"), state)?;
        if interactive {
            out.push(format!("conversation:{id}"), json!({"event":"conversation", "node":id, "run":execution.runs.get(node.id()).map_or("", |run| run.as_str()), "active":observation.state.conversations.contains_key(node.id())}))?;
        }
    }
    if let Some(restored) = &observation.state.restoration {
        for warning in crate::startup::warnings(restored) {
            out.push(format!("startup:{}", warning.id), json!({"event":"startup-warning", "id":warning.id, "message":warning.message, "node":warning.node.as_ref().map(|node| node.as_str()), "handle":warning.handle.as_ref().map(|handle| handle.as_str())}))?;
        }
        for call in &restored.interrupted {
            if !out.nodes.contains(call.node.as_str()) {
                continue;
            }
            // A fresh execution resolves the startup display marker; history retains its evidence.
            if execution
                .runs
                .get(&call.node)
                .is_some_and(|run| run != &call.run)
            {
                continue;
            }
            out.push(format!("interrupted:{}", call.node.as_str()), json!({"event":"interrupted", "node":call.node.as_str(), "cell":call.cell,"capability":call.capability,"safe":call.safe,"when":call.at.to_string()}))?;
        }
    }
    for entry in &observation.log.entries {
        let problem = match entry.status() {
            LogStatus::Unconfirmed { error, .. } => error.message(),
            _ => "",
        };
        let Some(event) = history_event(entry.entry(), entry.durable(), problem)? else {
            continue;
        };
        out.push(format!("log:{}", entry.id()), event)?;
    }
    Ok(out)
}
/// Read-only encoding of engine publication evidence; no client-specific lifecycle policy.
fn publication(published: Option<&ValuePublication>) -> Value {
    let (state, handle, problem, uncertain, message) = match published {
        Some(ValuePublication::Pending { .. }) => (
            "pending",
            None,
            None,
            None,
            "The result is being published.",
        ),
        Some(ValuePublication::Complete(value)) => (
            if value.stored.is_some() {
                "available"
            } else {
                "unavailable"
            },
            value.stored.as_ref().map(|stored| &stored.handle),
            value.problem.as_ref(),
            value.uncertain_handle.as_ref(),
            if value.stored.is_some() {
                "The result has been published."
            } else {
                "Result publication completed without an acknowledged readable result. See workspace diagnostics; the command was not rerun."
            },
        ),
        Some(ValuePublication::Recovered(value)) => (
            "available",
            Some(&value.handle),
            None,
            None,
            "The stored result was recovered.",
        ),
        None => (
            "unavailable",
            None,
            None,
            None,
            "No publication metadata is available for this output. See workspace diagnostics; this is not evidence of a pending publication or a failed command.",
        ),
    };
    json!({"state":state, "run":published.map(ValuePublication::run).map(|run| run.as_str()),
    "handle":handle.map(|handle| handle.as_str()),
    "uncertainHandle":uncertain.map(|handle| handle.as_str()),
    "problem":problem.map(error), "message":message,
    "pinBinding":match published {
        Some(ValuePublication::Complete(value)) => value.pin.as_ref().map(|binding|match binding {
            wes_engine::session::PinBinding::Pending => json!({"state":"pending"}),
            wes_engine::session::PinBinding::Bound => json!({"state":"bound"}),
            wes_engine::session::PinBinding::Refused(problem) => json!({"state":"refused","problem":problem}),
        }), _ => None,
    }})
}

/// Display evidence for the exact run; neither a completion nor an older run is a start.
fn recorded_start(
    record: &wes_engine::history::ExecutionRecord,
    current: Option<&wes_engine::runtime::RunId>,
) -> Option<String> {
    (record.state() == NodeState::Running && current.is_some() && record.run() == current)
        .then(|| record.at().to_string())
}

pub(super) fn history_event(
    entry: &JournalEntry,
    durable: bool,
    problem: &str,
) -> io::Result<Option<Value>> {
    let event = match entry {
        JournalEntry::Observed(record)
        | JournalEntry::Snapshot(wes_engine::history::LiveSnapshot {
            observation: record,
            ..
        }) => {
            json!({"event":"log", "record":{"id":record.id(),"node":record.node().as_str(),"run":record.run().map_or("", |run| run.as_str()),"at":record.at().to_string(),"state":state(record.state()).to_uppercase(),"error":record.error().map(error).unwrap_or_else(|| json!([]))},"durable":durable,"persistenceProblem":problem})
        }
        JournalEntry::Diagnosed(record) => {
            json!({"event":"log-diagnostic", "record":{"id":record.id(),"at":record.at().to_string(),"cell":record.cell(),"source":record.source(),"diagnostic":diagnostic(record.diagnostic(),record.source())?},"durable":durable,"persistenceProblem":problem})
        }
        JournalEntry::Noticed(record) => {
            json!({"event":"log-notice", "record":{"id":record.id(),"at":record.at().to_string(),"context":notice_context(record.context()),"error":error(record.error())},"durable":durable,"persistenceProblem":problem})
        }
        _ => return Ok(None),
    };
    Ok(Some(event))
}
fn notice_context(context: &NoticeContext) -> Value {
    let mut value = json!({
        "kind": match context { NoticeContext::Execution {..} => "execution", NoticeContext::Publication {..} => "publication", NoticeContext::Keep {..} => "keep", NoticeContext::Release {..} => "release", NoticeContext::Eviction => "eviction", NoticeContext::StorageWorker => "storage-worker", NoticeContext::WorkspaceShutdown => "workspace-shutdown" },
        "node": context.node().map(|node| node.as_str()),
        "run": context.run().map(|run| run.as_str()),
        "handle": context.handle().map(|handle| handle.as_str()),
    });
    if let NoticeContext::Keep {
        may_have_applied, ..
    }
    | NoticeContext::Release {
        may_have_applied, ..
    } = context
    {
        value["mayHaveApplied"] = json!(may_have_applied);
    }
    value
}
fn log_status(log: &wes_engine::session::LogSnapshot) -> Value {
    json!({"event":"log-status", "pending":log.pending, "omitted":log.omitted.to_string(), "omittedNotDurable":log.omitted_not_durable.to_string(), "unconfirmed":log.unconfirmed.to_string(), "captureFailures":log.capture_failures.to_string()})
}
fn diagnostic(d: &Diagnostic, source: &str) -> io::Result<Value> {
    let offset = |byte| {
        source
            .get(..byte)
            .map(|s| s.encode_utf16().count())
            .ok_or_else(|| io::Error::other("invalid diagnostic source span"))
    };
    Ok(
        json!({"code":d.code,"severity":match d.severity {Severity::Error=>"error", Severity::Warning=>"warning", Severity::Info=>"info"}, "message":d.message,"start":offset(d.span.start())?,"end":offset(d.span.end())?,"hints":d.hints}),
    )
}
fn error(e: &ErrorValue) -> Value {
    json!({"id":e.id().as_str(), "code":e.code(), "message":e.message(), "causeId":e.cause().map_or("", |id| id.as_str()), "locations":e.locations().iter().map(|p| json!({"source":p.source,"start":p.start,"end":p.end,"line":p.line,"column":p.column,"endLine":p.end_line,"endColumn":p.end_column})).collect::<Vec<_>>(), "issues":e.issues().iter().map(|i| json!({"path":i.path,"code":i.code,"message":i.message})).collect::<Vec<_>>()})
}
fn state(state: NodeState) -> &'static str {
    match state {
        NodeState::Pending => "pending",
        NodeState::Running => "running",
        NodeState::Ready => "ready",
        NodeState::Stale => "stale",
        NodeState::Failed => "failed",
        NodeState::Cancelled => "cancelled",
        NodeState::Skipped => "skipped",
    }
}
fn vocabulary(
    o: &SessionObservation,
    credentials: &MemoryCredentials,
    resources: &[wes_engine::completion::ResourceSuggestions],
) -> Value {
    let providers = provider_vocabulary(&o.catalogue, Some(credentials), resources, None);
    use wes_language::vocabulary::{MetaCommand, commands};
    let mut names = commands::roots();
    names.extend(commands::COMMAND_PATHS.iter().filter_map(|p| p.short));
    let commands: Vec<_> = names.into_iter().map(|name| {
        let entry = commands::COMMAND_PATHS.iter().find(|p| p.path == [name]).or_else(||commands::COMMAND_PATHS.iter().find(|p| p.path[0] == name || p.short == Some(name))).expect("command root");
        let short = entry.short == Some(name);
        let path: Vec<String> = if short {entry.path.iter().map(|s|(*s).into()).collect()} else {vec![name.into()]};
        let spec = commands::signature(&path);
        let takes: Vec<String> = if short {vec![]} else if name == "import" {o.importers.iter().cloned().chain(["plan".into(),"apply".into()]).collect()}
            else if let Some(spec) = &spec {spec.tail_words.iter().map(|s|(*s).to_owned()).collect()}
            else {commands::COMMAND_PATHS.iter().filter(|p|p.path[0] == name && p.path.len()>1).map(|p|p.path[1].into()).collect()};
        let project = |p: &Parameter| parameter(p, match &p.sort {
            Sort::Selector(registry) if registry == "type" => o.types.clone(),
            Sort::Selector(registry) if registry == "view" => o.views.keys().cloned().collect(),
            Sort::Selector(registry) if registry == "refresh-scope" => wes_language::vocabulary::RefreshScope::ALL.iter().map(|scope|scope.name().to_owned()).collect(),
            _ => vec![],
        }, &[]);
        let variants: Vec<_> = takes.iter().filter_map(|word| {
            let mut selected = commands::signature(&[name.into(),word.clone()])?;
            if name == "import" {selected.parameters.extend(o.importer_parameters.get(word).into_iter().flatten().cloned());}
            let child_takes=if selected.command==MetaCommand::ImportPlan {o.importers.clone()} else {vec![]};
            let children=child_takes.iter().map(|kind|{
                let mut child=commands::signature(&[name.into(),word.clone(),kind.clone()]).expect("published importer plan signature");
                child.parameters.extend(o.importer_parameters.get(kind).into_iter().flatten().cloned());
                json!({"word":kind,"parameters":child.parameters.iter().map(&project).collect::<Vec<_>>(),"takes":[],"variants":[]})
            }).collect::<Vec<_>>();
            Some(json!({"word":word,"parameters":selected.parameters.iter().map(&project).collect::<Vec<_>>(),"takes":child_takes,"variants":children}))
        }).collect();
        json!({"name":name,"implemented":true,"summary":entry.summary,"takes":takes,
            "open":spec.as_ref().is_some_and(|s|s.open_arguments),"variants":variants,
            "parameters":spec.as_ref().map(|s|s.parameters.iter().map(project).collect::<Vec<_>>()).unwrap_or_default()})
    }).collect();
    let templates = template_vocabulary(&o.templates);
    let package = wes_language::calc::Package::standard();
    json!({"calculation":{"keywords":package.keywords().collect::<Vec<_>>(),"operations":package.operations().map(|(name,_)|name).collect::<Vec<_>>(),"methods":package.operations().filter(|(_,spec)|spec.operation.supports_method()).map(|(name,_)|name).collect::<Vec<_>>()},"event":"vocabulary","commands":commands,"annotations":ANNOTATIONS.iter().map(|(n,_)|n).collect::<Vec<_>>(),"providers":providers,"templates":templates,"types":o.types.clone()})
}
fn public_endpoint(endpoint: &str) -> Option<String> {
    let mut url = url::Url::parse(endpoint).ok()?;
    url.set_username("").ok()?;
    url.set_password(None).ok()?;
    url.set_query(None);
    url.set_fragment(None);
    Some(url.to_string())
}
fn provider_vocabulary(
    catalogue: &wes_core::capability::Catalogue,
    credentials: Option<&MemoryCredentials>,
    resources: &[wes_engine::completion::ResourceSuggestions],
    environment: Option<&str>,
) -> Vec<Value> {
    catalogue.provider_names().filter_map(|name| catalogue.provider(name)).map(|provider| {
        let supplied: Vec<_> = provider.secrets().iter().map(|name| (name, credentials.is_some_and(|c| c.lookup(name).ok().flatten().is_some()))).collect();
        let capabilities: Vec<_> = provider.capabilities().map(|capability| {
            let parameters: Vec<_> = capability.parameters.iter().map(|p| {
                let allowed = capability.rules.iter().filter(|rule| rule.is_binding()).filter_map(|rule| match &rule.rule {
                    Rule::OneOf {key, values} if key == &p.name => Some(values),
                    _ => None,
                }).flatten().cloned().collect();
                let mut projected = parameter(p, allowed, &capability.rules);
                if let Sort::Resource(registry) = &p.sort
                    && let Some(producer) = provider.capabilities().find(|c| c.resources.as_ref().is_some_and(|r| &r.registry == registry)) {
                        projected["resourceHint"] = json!(format!("Run {} {} to load suggestions; refresh its result to update them", provider.name(), producer.path.join(" ")));
                        if let Some(found) = resources.iter().find(|r| r.environment.as_deref() == environment && r.provider == provider.name() && &r.registry == registry) {
                            projected["resources"] = json!({"node":found.node,"observedAtNs":found.observed_at_ns.to_string(),"items":found.items.iter().map(|item|json!({"value":item.value,"label":item.label,"detail":item.detail})).collect::<Vec<_>>()});
                        }
                }
                projected
            }).collect();
            json!({"path":capability.path,"summary":capability.summary,"result":capability.result.to_string(),"safe":capability.safety==Safety::Safe,"parameters":parameters})
        }).collect();
        json!({"name":provider.name(),"ready":supplied.iter().all(|(_,yes)|*yes),"credentials":supplied.iter().map(|(name,yes)|json!({"name":name,"supplied":yes})).collect::<Vec<_>>(),"capabilities":capabilities})
    }).collect()
}

fn parameter(p: &Parameter, allowed: Vec<String>, rules: &[DeclaredRule]) -> Value {
    let mut projected = json!({"name":p.name,"type":p.shape.to_string(),"required":p.required,"allowed":allowed,"content":p.content.as_deref().unwrap_or("")});
    if let Some(domain) = &p.enum_domain {
        projected["choices"] = choices(domain.choices(&p.name, rules));
    }
    projected
}

fn template_parameter(name: &str, contract: Option<&wes_core::contracts::Contract>) -> Value {
    let mut projected = json!({"name":name,"type":contract.map_or("Unknown", |c|c.name()),"required":true,"allowed":[],"content":""});
    if let Some(domain) = contract.and_then(EnumDomain::from_contract) {
        projected["choices"] = choices(domain.choices(name, &[]));
    }
    projected
}

fn template_vocabulary(templates: &wes_language::templates::Templates) -> Vec<Value> {
    templates.snapshot().iter().map(|(name,d)|json!({"name":name,"body":if d.calculation.is_some() { ":calc".to_owned() } else { d.syntax.body.to_string() },"parameters":d.parameters.iter().map(|name|template_parameter(name, d.contracts.get(name).map(Arc::as_ref))).collect::<Vec<_>>()})).collect()
}

fn choices(choices: EnumChoices) -> Value {
    json!({"kind":choices.kind.name(),"members":choices.members,"total":choices.total,"complete":choices.complete})
}

#[cfg(test)]
mod tests {
    fn choice_registry() -> wes_core::contracts::ContractRegistry {
        let mut registry = wes_core::contracts::ContractRegistry::new();
        registry.load("types:\n Status: {base: Text, enum: [queued, done, 'true', '123']}\n Run: {base: Int, enum: [9007199254740993, -9223372036854775808]}\n Amount: {base: Decimal, enum: [12.50, 0.123456789012345678901234567890]}\n Flag: {base: Bool, enum: [true, false]}\n").unwrap();
        registry
    }

    #[test]
    fn imported_parameter_choices_intersect_documented_rules_and_keep_allowed() {
        use wes_core::{
            Shape,
            capability::{Capability, Catalogue, ProviderDescription, RuleBasis},
        };
        let registry = choice_registry();
        let mut capability = Capability::new(["list"], Shape::Unknown, Safety::Safe);
        for (name, kind) in [
            ("status", "Status"),
            ("run", "Run"),
            ("amount", "Amount"),
            ("flag", "Flag"),
            ("plain", "Text"),
        ] {
            let contract = registry.resolve(kind).unwrap();
            capability
                .parameters
                .push(Parameter::new(name, contract.shape(), true).constrained_by(&contract));
        }
        let rule = |values: &[&str], binding| DeclaredRule {
            rule: Rule::OneOf {
                key: "status".into(),
                values: values.iter().map(|s| (*s).into()).collect(),
            },
            basis: if binding {
                RuleBasis::Documented { note: None }
            } else {
                RuleBasis::Inferred {
                    reason: "fixture".into(),
                }
            },
        };
        let mut catalogue = Catalogue::new();
        let project = |capability: Capability, catalogue: &mut Catalogue| {
            catalogue.register(ProviderDescription::new("fixture", [capability], vec![]).unwrap());
            provider_vocabulary(catalogue, None, &[], None)[0]["capabilities"][0]["parameters"]
                .clone()
        };
        let parameters = project(capability.clone(), &mut catalogue);
        assert_eq!(
            parameters[0],
            json!({"name":"status","type":"Text","required":true,"allowed":[],"content":"","choices":{"kind":"text","members":["queued","done","true","123"],"total":4,"complete":true}})
        );
        assert_eq!(
            parameters[1]["choices"],
            json!({"kind":"int","members":["9007199254740993","-9223372036854775808"],"total":2,"complete":true})
        );
        assert_eq!(
            parameters[2]["choices"],
            json!({"kind":"decimal","members":["12.50","0.123456789012345678901234567890"],"total":2,"complete":true})
        );
        assert_eq!(
            parameters[3]["choices"],
            json!({"kind":"bool","members":["true","false"],"total":2,"complete":true})
        );
        assert!(parameters[4].get("choices").is_none());
        capability.rules = vec![
            rule(&["queued", "done", "outside"], true),
            rule(&["done", "123"], true),
            rule(&["queued"], false),
        ];
        let parameters = project(capability.clone(), &mut catalogue);
        assert_eq!(
            parameters[0]["choices"],
            json!({"kind":"text","members":["done"],"total":1,"complete":true})
        );
        assert_eq!(
            parameters[0]["allowed"],
            json!(["done", "outside", "queued", "123", "done"])
        );
        capability.rules.push(rule(&["queued"], true));
        let parameters = project(capability, &mut catalogue);
        assert_eq!(
            parameters[0]["choices"],
            json!({"kind":"text","members":[],"total":0,"complete":true})
        );
    }

    #[test]
    fn def_vocabulary_uses_captured_parameter_contracts_and_exact_numeric_text() {
        let registry = choice_registry();
        let mut templates = wes_language::templates::Templates::new();
        let parsed = wes_language::parse(&wes_language::SourceText::new(
            "fixture",
            ":def inspect(run: Run, status: Status, amount: Amount, flag: Flag) as fixture list run:?run status:?status amount:?amount flag:?flag plain:?plain",
        ));
        assert!(parsed.diagnostics.is_empty(), "{:?}", parsed.diagnostics);
        let wes_language::Expression::Definition(syntax) = parsed
            .script
            .statements
            .into_iter()
            .next()
            .unwrap()
            .expression
        else {
            panic!("definition")
        };
        templates.define(syntax, &registry).unwrap();
        let projected = template_vocabulary(&templates);
        let parameters = projected[0]["parameters"].as_array().unwrap();
        let parameter = |name: &str| parameters.iter().find(|p| p["name"] == name).unwrap();
        assert_eq!(
            parameter("run"),
            &json!({"name":"run","type":"Run","required":true,"allowed":[],"content":"","choices":{"kind":"int","members":["9007199254740993","-9223372036854775808"],"total":2,"complete":true}})
        );
        assert_eq!(
            parameter("status")["choices"]["members"],
            json!(["queued", "done", "true", "123"])
        );
        assert_eq!(
            parameter("amount")["choices"]["members"],
            json!(["12.50", "0.123456789012345678901234567890"])
        );
        assert_eq!(parameter("flag")["choices"]["kind"], "bool");
        assert!(parameter("plain").get("choices").is_none());
    }

    #[test]
    fn projected_large_enum_is_bounded_but_member_199_can_be_suggested() {
        use wes_core::{
            Shape,
            capability::{Capability, Catalogue, ProviderDescription, RuleBasis},
        };
        let mut registry = wes_core::contracts::ContractRegistry::new();
        let members = (0..200)
            .map(|i| i.to_string())
            .collect::<Vec<_>>()
            .join(",");
        registry
            .load(&format!(
                "types: {{Many: {{base: Int, enum: [{members}]}}}}"
            ))
            .unwrap();
        let contract = registry.resolve("Many").unwrap();
        let parameter = Parameter::new("item", contract.shape(), true).constrained_by(&contract);
        let mut capability = Capability::new(["select"], Shape::Unknown, Safety::Safe);
        capability.parameters.push(parameter);
        let mut catalogue = Catalogue::new();
        catalogue
            .register(ProviderDescription::new("fixture", [capability.clone()], vec![]).unwrap());
        let provider = provider_vocabulary(&catalogue, None, &[], None);
        let preview = &provider[0]["capabilities"][0]["parameters"][0]["choices"];
        assert_eq!(
            preview["members"],
            json!((0..64).map(|i| i.to_string()).collect::<Vec<_>>())
        );
        assert_eq!(preview["total"], 200);
        assert_eq!(preview["complete"], false);
        assert_eq!(
            template_parameter("item", Some(&contract))["choices"],
            *preview
        );
        capability.rules.push(DeclaredRule {
            rule: Rule::OneOf {
                key: "item".into(),
                values: ["199".into()].into(),
            },
            basis: RuleBasis::Documented { note: None },
        });
        catalogue.register(ProviderDescription::new("fixture", [capability], vec![]).unwrap());
        let provider = provider_vocabulary(&catalogue, None, &[], None);
        assert_eq!(
            provider[0]["capabilities"][0]["parameters"][0]["choices"],
            json!({"kind":"int","members":["199"],"total":1,"complete":true})
        );
    }

    #[test]
    fn selector_and_legacy_parameters_keep_allowed_without_choices() {
        let selector = Parameter::new("type", wes_core::Shape::Unknown, true).selecting("type");
        assert_eq!(
            parameter(&selector, vec!["Text".into(), "Int".into()], &[]),
            json!({"name":"type","type":"Unknown","required":true,"allowed":["Text","Int"],"content":""})
        );
        let legacy = Parameter::new("status", wes_core::Shape::Unknown, false);
        assert_eq!(
            parameter(&legacy, vec!["queued".into()], &[]),
            json!({"name":"status","type":"Unknown","required":false,"allowed":["queued"],"content":""})
        );
    }

    #[test]
    fn structured_source_preview_preserves_references_and_redacts_private_literal_leaves() {
        use wes_engine::{graph::OutputRef, plan::Input};
        let private = wes_core::Value::new(
            wes_core::Shape::Unknown,
            wes_core::Data::Text("sensitive fixture".into()),
            wes_core::Provenance::default()
                .with_policy(&wes_core::flow::FlowPolicy::default().private()),
        )
        .unwrap();
        let input = Input::Record(
            [
                (
                    "nested".into(),
                    Input::List {
                        items: vec![Input::FromNode(OutputRef {
                            node: NodeId::new("producer").unwrap(),
                            port: OutputPort::Error,
                        })],
                        item_shape: wes_core::Shape::Unknown,
                    },
                ),
                ("secret".into(), Input::Literal(private)),
            ]
            .into(),
        );
        let mut text = String::new();
        assert!(super::input_preview(
            &input,
            &|output| format!("${}::{}", output.node, output.port.selector()),
            &mut text,
            &mut 256
        ));
        assert_eq!(
            text,
            "{\"nested\":[$producer::error], \"secret\":[private value]}"
        );
        assert!(!text.contains("sensitive fixture"));
        assert!(!super::input_preview(
            &input,
            &|_| String::new(),
            &mut String::new(),
            &mut 1
        ));
    }

    #[test]
    fn capacity_delta_is_global_small_and_does_not_replay_unchanged_state() {
        use wes_engine::driver::{CapacitySnapshot, SlotUsage};
        let mut first = super::Projection::empty("session");
        first.capacity(CapacitySnapshot {
            operations: SlotUsage { used: 1, limit: 4 },
            streams: SlotUsage { used: 2, limit: 7 },
        });
        let initial = first.changes(None);
        assert!(initial[0].contains("session"));
        assert!(initial.last().unwrap().contains("execution-capacity"));
        assert!(first.changes(Some(&first)).is_empty());
        let mut next = first.clone();
        next.capacity(CapacitySnapshot {
            operations: SlotUsage { used: 0, limit: 4 },
            streams: SlotUsage { used: 2, limit: 7 },
        });
        let changed = next.changes(Some(&first));
        assert_eq!(changed.len(), 1);
        let event: serde_json::Value = serde_json::from_str(&changed[0]).unwrap();
        assert_eq!(event["operations"]["used"], 0);
        assert_eq!(event["streams"]["limit"], 7);
    }

    #[test]
    fn stream_presentation_follows_dependency_edges_without_marking_finite_ancestors() {
        use wes_engine::graph::OutputRef;
        let mut graph = DependencyGraph::new();
        let finite = graph.add(false, []).unwrap();
        let source = graph.add(true, [OutputRef::data(finite.clone())]).unwrap();
        let branch = graph.add(false, [OutputRef::data(source.clone())]).unwrap();
        let another = graph.add(true, []).unwrap();
        let join = graph
            .add(
                false,
                [
                    OutputRef::data(branch.clone()),
                    OutputRef::data(another.clone()),
                ],
            )
            .unwrap();
        let unrelated = graph.add(false, []).unwrap();
        assert_eq!(
            stream_outputs(&graph, |stream| *stream),
            BTreeSet::from([source, branch, another, join])
        );
        assert!(!stream_outputs(&graph, |stream| *stream).contains(&finite));
        assert!(!stream_outputs(&graph, |stream| *stream).contains(&unrelated));
    }

    #[test]
    fn provider_endpoint_projection_omits_credentials_query_and_fragment() {
        assert_eq!(
            super::public_endpoint("https://synthetic:secret@example.invalid/api?key=hidden#token"),
            Some("https://example.invalid/api".into())
        );
        assert_eq!(super::public_endpoint("not a URL"), None);
    }

    use super::*;
    #[test]
    fn resource_vocabulary_is_environment_scoped_and_keeps_identity_separate_from_label() {
        use wes_core::{
            Shape,
            capability::{Capability, Catalogue, ProviderDescription, ResourceProjection},
        };
        use wes_engine::completion::{ResourceCandidate, ResourceSuggestions};
        let mut inventory = Capability::new(["list"], Shape::Unknown, Safety::Safe);
        inventory.resources = Some(ResourceProjection {
            registry: "resource".into(),
            rows: "rows".into(),
            key: "id".into(),
            label: "name".into(),
            detail: "state".into(),
            observed_at: "at".into(),
        });
        let mut inspect = Capability::new(["read"], Shape::Unknown, Safety::Safe);
        inspect
            .parameters
            .push(Parameter::new("item", Shape::Unknown, true).suggesting("resource"));
        let mut catalogue = Catalogue::new();
        catalogue.register(ProviderDescription::new("qa", [inventory, inspect], vec![]).unwrap());
        let resources = [ResourceSuggestions {
            environment: Some("dev".into()),
            provider: "qa".into(),
            registry: "resource".into(),
            node: "id1000".into(),
            observed_at_ns: 123,
            items: vec![ResourceCandidate {
                value: "exact-id".into(),
                label: "display-name".into(),
                detail: "ready".into(),
            }],
        }];
        let view = provider_vocabulary(&catalogue, None, &resources, Some("dev"));
        let p = &view[0]["capabilities"][1]["parameters"][0];
        assert_eq!(p["resources"]["observedAtNs"], "123");
        assert_eq!(p["resources"]["items"][0]["value"], "exact-id");
        assert_eq!(p["resources"]["items"][0]["label"], "display-name");
        for environment in [None, Some("prod")] {
            let view = provider_vocabulary(&catalogue, None, &resources, environment);
            let p = &view[0]["capabilities"][1]["parameters"][0];
            assert!(p.get("resources").is_none());
            assert!(p["resourceHint"].as_str().unwrap().contains("qa list"));
        }
    }
    #[test]
    fn publication_preserves_authority_uncertainty_and_original_problem_separately() {
        use wes_engine::{
            history::Persistence,
            runtime::{RunId, RuntimeCode},
            session::PublishedValue,
            storage::{StoredOutput, ValueHandle},
        };
        let run = RunId::new("current").unwrap();
        let pending = publication(Some(&ValuePublication::Pending { run: run.clone() }));
        assert_eq!(pending["state"], "pending");
        assert_eq!(pending["run"], "current");
        assert!(pending["handle"].is_null());
        assert!(pending["problem"].is_null());
        let problem =
            RuntimeCode::RecordingFailed.error("Unconfirmed storage; execution unchanged", None);
        let uncertain = ValueHandle::fresh();
        let mut value = PublishedValue {
            pin: None,
            run,
            stored: None,
            journal: None,
            problem: Some(problem.clone()),
            uncertain_handle: Some(uncertain.clone()),
        };
        let failed = publication(Some(&ValuePublication::Complete(value.clone())));
        assert_eq!(failed["state"], "unavailable");
        assert!(failed["handle"].is_null());
        assert_eq!(failed["uncertainHandle"], uncertain.as_str());
        assert_eq!(failed["problem"], error(&problem));
        let acknowledged = ValueHandle::fresh();
        value.stored = Some(StoredOutput {
            handle: acknowledged.clone(),
            bytes: 1,
            kept: true,
            retention: wes_engine::storage::Retention::Protected,
            retained_persistence: Persistence::Volatile,
        });
        value.pin = Some(wes_engine::session::PinBinding::Refused(
            "View changed during retention".into(),
        ));
        let available = publication(Some(&ValuePublication::Complete(value)));
        assert_eq!(available["state"], "available");
        assert_eq!(available["handle"], acknowledged.as_str());
        assert_eq!(available["problem"], failed["problem"]);
        assert_eq!(available["pinBinding"]["state"], "refused");
        assert_eq!(
            available["pinBinding"]["problem"],
            "View changed during retention"
        );
        let absent = publication(None);
        assert_eq!(absent["state"], "unavailable");
        assert!(absent["run"].is_null());
        assert!(absent["problem"].is_null());
        assert!(absent["pinBinding"].is_null());
    }
    #[test]
    fn execution_timestamp_requires_running_evidence_for_the_exact_current_run() {
        use wes_engine::{graph::NodeId, history::ExecutionRecord, runtime::RunId};
        let run = RunId::new("current").unwrap();
        let old = RunId::new("obsolete").unwrap();
        let at = "2026-09-16T10:11:12Z".parse().unwrap();
        let record = |run, state| {
            ExecutionRecord::new(
                "event".into(),
                NodeId::new("node").unwrap(),
                run,
                at,
                state,
                None,
            )
            .unwrap()
        };
        let started = record(Some(run.clone()), NodeState::Running);
        assert_eq!(recorded_start(&started, Some(&run)), Some(at.to_string()));
        assert_eq!(recorded_start(&started, Some(&old)), None);
        assert_eq!(recorded_start(&started, None), None);
        assert_eq!(
            recorded_start(&record(Some(run.clone()), NodeState::Ready), Some(&run)),
            None
        );
        assert_eq!(
            recorded_start(&record(None, NodeState::Running), None),
            None
        );
    }
    #[test]
    fn unavailable_projection_replaces_stale_state_and_recovery_resends_a_complete_greeting() {
        let mut complete = Projection::empty("same-session");
        complete.nodes.insert("n".into());
        complete
            .push("created:n".into(), json!({"event":"created", "node":"n"}))
            .unwrap();
        let unavailable = Projection::unavailable("same-session");
        let failed = unavailable.changes(Some(&complete));
        assert!(failed[0].contains("session"));
        assert!(
            failed
                .iter()
                .any(|frame| frame.contains("projection-unavailable"))
        );
        assert!(unavailable.nodes.is_empty());
        assert!(unavailable.runs.is_empty());
        assert!(unavailable.changes(Some(&unavailable)).is_empty());
        let restored = complete.changes(Some(&unavailable));
        assert!(restored[0].contains("session"));
        assert!(restored.iter().any(|frame| frame.contains("created")));
    }
    #[test]
    fn operational_context_retains_run_handle_and_uncertainty_without_inventing_identities() {
        use wes_engine::{graph::NodeId, runtime::RunId, storage::ValueHandle};
        let node = NodeId::new("node").unwrap();
        let run = RunId::new("run").unwrap();
        let handle = ValueHandle::fresh();
        let publication = notice_context(&NoticeContext::Publication {
            node: node.clone(),
            run: run.clone(),
            handle: Some(handle.clone()),
        });
        assert_eq!(
            publication,
            json!({"kind":"publication", "node":"node", "run":"run", "handle":handle.as_str()})
        );
        assert_eq!(
            notice_context(&NoticeContext::Execution { node, run })["handle"],
            Value::Null
        );
        for context in [
            NoticeContext::Keep {
                handle: handle.clone(),
                may_have_applied: false,
            },
            NoticeContext::Release {
                handle,
                may_have_applied: true,
            },
        ] {
            let value = notice_context(&context);
            assert!(value["node"].is_null());
            assert_eq!(value["mayHaveApplied"], value["kind"] == "release");
        }
        for context in [NoticeContext::Eviction, NoticeContext::StorageWorker] {
            let value = notice_context(&context);
            assert!(value["node"].is_null());
            assert!(value.get("mayHaveApplied").is_none());
        }
    }
    #[test]
    fn diagnostic_offsets_are_utf16_and_invalid_boundaries_are_refused() {
        let text = "a😀çx";
        let diagnostic_value = diagnostic(
            &Diagnostic::error("TEST", wes_language::Span::new(5, 7).unwrap(), "point"),
            text,
        )
        .unwrap();
        assert_eq!(diagnostic_value["start"], 3);
        assert_eq!(diagnostic_value["end"], 4);
        assert!(
            diagnostic(
                &Diagnostic::error("TEST", wes_language::Span::at(2), "invalid"),
                text
            )
            .is_err()
        );
    }
    #[test]
    fn rolling_ten_thousand_entry_history_sends_only_the_changed_records() {
        let mut old = Projection::empty("load");
        for n in 0..10_000 {
            old.push(
                format!("log:{n}"),
                json!({"event":"log", "record":{"id":n.to_string()},"durable":false}),
            )
            .unwrap();
        }
        for n in 10_000..10_100 {
            let mut next = old.clone();
            next.frames.remove(&format!("log:{}", n - 10_000));
            next.order.retain(|key| next.frames.contains_key(key));
            next.push(
                format!("log:{n}"),
                json!({"event":"log", "record":{"id":n.to_string()},"durable":false}),
            )
            .unwrap();
            let delta = next.changes(Some(&old));
            assert_eq!(delta.len(), 1);
            assert!(
                delta[0].len() < 256,
                "one eviction must not replay 10,000 records"
            );
            let delta: Value = serde_json::from_str(&delta[0]).unwrap();
            assert_eq!(delta["removed"], json!([(n - 10_000).to_string()]));
            assert_eq!(delta["entries"].as_array().unwrap().len(), 1);
            old = next;
        }
    }
    #[test]
    fn retirement_diffs_cells_without_nodes_and_resets_removed_history_without_session_change() {
        let mut old = Projection::empty("same-live-session");
        old.push(
            "cell:diagnostic".into(),
            json!({"event":"planned", "cell":"diagnostic", "nodes":[]}),
        )
        .unwrap();
        old.push(
            "cell:kept".into(),
            json!({"event":"planned", "cell":"kept", "nodes":[]}),
        )
        .unwrap();
        old.push(
            "log:removed".into(),
            json!({"event":"log-diagnostic", "record":{"id":"removed"}}),
        )
        .unwrap();
        old.push(
            "log:kept".into(),
            json!({"event":"log", "record":{"id":"kept"}}),
        )
        .unwrap();
        let mut next = old.clone();
        next.frames.remove("cell:diagnostic");
        next.frames.remove("log:removed");
        next.order.retain(|key| next.frames.contains_key(key));
        let events: Vec<Value> = next
            .changes(Some(&old))
            .iter()
            .map(|event| serde_json::from_str(event).unwrap())
            .collect();
        assert_eq!(
            events,
            vec![
                json!({"event":"work-retired", "cells":["diagnostic"]}),
                json!({"event":"log-delta", "reset":false,"removed":["removed"],"entries":[]}),
            ]
        );
        assert!(next.changes(Some(&next)).is_empty());
        let reconnect: Value = serde_json::from_str(&next.changes(None)[0]).unwrap();
        assert_eq!(reconnect["generation"], "same-live-session");
        assert_eq!(reconnect["cells"], json!(["kept"]));
    }

    #[test]
    fn diff_retains_log_receipt_changes_drops_removed_nodes_and_resets_only_on_window_or_session_change()
     {
        let mut old = Projection {
            capacity: None,
            workspace: None,
            generation: "one".into(),
            frames: BTreeMap::new(),
            order: vec![],
            nodes: BTreeSet::from(["a".into()]),
            runs: BTreeMap::new(),
            has_conversations: false,
            complete: true,
            bytes: 0,
        };
        old.push("log:entry".into(), json!({"event":"log","durable":false}))
            .unwrap();
        let mut next = old.clone();
        next.frames.insert(
            "log:entry".into(),
            json!({"event":"log","durable":true}).to_string().into(),
        );
        next.nodes.clear();
        let changed = next.changes(Some(&old));
        assert_eq!(changed.len(), 2);
        assert!(changed[0].contains("dropped"));
        assert!(changed[1].contains("true"));
        assert!(next.changes(Some(&next)).is_empty());
        let mut moved = next.clone();
        assert!(moved.changes(Some(&next)).is_empty());
        moved.generation = "two".into();
        assert!(moved.changes(Some(&next))[0].contains("session"));
        assert!(
            old.push("oversized".into(), json!({"text":"x".repeat(max_bytes())}))
                .is_err()
        );
    }
}
