//! Immutable execution evidence. Reads never submit, hydrate or refresh work.
use crate::{
    ApplicationHandle, Owner,
    retention::{ManagementError as Error, Request},
};
use serde::Serialize;
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, BTreeSet},
    sync::Arc,
};
use tokio::sync::oneshot;
use wes_adapters::codec::{Limits, encode_display_value, history::encode_journal};
use wes_engine::{
    history::{HistoryCaptureLimits, HistoryImage, JournalEntry, Persistence, Record},
    session::SessionObservation,
    storage::{Retention, ValueHandle},
};

#[derive(Serialize)]
struct Run {
    node: String,
    run: String,
    state: String,
    at: String,
    handle: Option<String>,
    protected: bool,
    trace: bool,
}
fn nodes(
    observation: &SessionObservation,
    cell: &str,
) -> Result<(BTreeSet<String>, Vec<Value>), Error> {
    let roots = observation.work_roots();
    let root = roots.get(cell).ok_or(Error::EvidenceMissing)?;
    let mut nodes = BTreeSet::new();
    let attempts = observation.cells.iter().filter(|c| roots.get(c.input.cell()) == Some(root)).map(|c| {
        let reply = c.reply.as_ref().and_then(|r|r.as_ref().ok());
        if let Some(r) = reply { nodes.extend(r.nodes.iter().map(|n|n.as_str().to_owned())); }
        json!({"id":c.input.cell(), "source":c.input.text(), "revisionOf":c.input.revision_of(), "repeatOf":c.input.repeat().map(|r|&r.origin),
            "nodes":reply.map(|r|r.nodes.iter().map(|n|n.as_str()).collect::<Vec<_>>()).unwrap_or_default(),
            "failure":c.reply.as_ref().and_then(|r|r.as_ref().err()).map(ToString::to_string),
            "diagnostics":reply.map(|r|r.diagnostics.diagnostics.iter().map(|d|d.message.as_str()).collect::<Vec<_>>()).unwrap_or_default()})
    }).collect();
    Ok((nodes, attempts))
}
fn runs(image: &HistoryImage, nodes: &BTreeSet<String>) -> BTreeMap<String, Run> {
    let mut runs = BTreeMap::new();
    for e in image.journal() {
        if let Some(o) = e.observation()
            && nodes.contains(o.node().as_str())
            && let Some(run) = o.run()
        {
            let item = runs.entry(run.as_str().to_owned()).or_insert_with(|| Run {
                node: o.node().to_string(),
                run: run.to_string(),
                state: String::new(),
                at: String::new(),
                handle: None,
                protected: false,
                trace: false,
            });
            if !matches!(
                o.state(),
                wes_engine::graph::NodeState::Stale | wes_engine::graph::NodeState::Pending
            ) || item.state.is_empty()
            {
                item.state = format!("{:?}", o.state());
                item.at = o.at().to_string();
            }
        }
    }
    for e in image.journal() {
        let evidence = match e {
            JournalEntry::Payload { node, run, handle }
            | JournalEntry::ProtectedRun { node, run, handle } => Some((node, run, handle)),
            JournalEntry::Snapshot(s) => s.result.as_ref().map(|r| (&r.node, &r.run, &r.handle)),
            JournalEntry::Result(r) => Some((&r.node, &r.run, &r.handle)),
            _ => None,
        };
        if let Some((node, run, handle)) = evidence
            && let Some(item) = runs.get_mut(run.as_str())
            && item.node == node.as_str()
        {
            item.handle = Some(handle.to_string());
            item.protected |= matches!(e, JournalEntry::ProtectedRun { .. });
        }
        if let JournalEntry::Trace(t) = e
            && let Some(item) = runs.get_mut(t.run.as_str())
            && item.node == t.node.as_str()
        {
            item.trace = true;
        }
    }
    runs
}
fn detail(image: &HistoryImage, run: &Run) -> Result<Value, Error> {
    let command = image
        .journal()
        .iter()
        .find_map(|e| match e {
            JournalEntry::Command(c) if c.nodes.iter().any(|n| n.as_str() == run.node) => Some(e),
            _ => None,
        })
        .ok_or(Error::EvidenceMissing)?;
    let definition: Value = serde_json::from_slice(
        &encode_journal(command, Limits::default()).map_err(|_| Error::EvidenceMissing)?,
    )
    .map_err(|_| Error::EvidenceMissing)?;
    // A later argument edit changes the executed definition of its target. The
    // declaration alone is not per-run evidence for that target's subsequent runs.
    let mut declared = false;
    for entry in image.journal() {
        if std::ptr::eq(entry, command) {
            declared = true;
            continue;
        }
        if !declared {
            continue;
        }
        if let Some(o) = entry.observation()
            && o.run().is_some_and(|id| id.as_str() == run.run)
        {
            break;
        }
        if let JournalEntry::Command(c) = entry
            && c.changed_nodes.iter().any(|node| node.as_str() == run.node)
        {
            return Err(Error::EvidenceMissing);
        }
    }
    let trace = image.journal().iter().find_map(|e| match e {
        JournalEntry::Trace(t) if t.run.as_str() == run.run && t.node.as_str() == run.node => {
            Some(&t.value)
        }
        _ => None,
    });
    let trace = trace
        .map(|v| {
            encode_display_value(v, Limits::default())
                .map_err(|_| Error::EvidenceMissing)
                .and_then(|v| {
                    serde_json::from_slice::<Value>(&v).map_err(|_| Error::EvidenceMissing)
                })
        })
        .transpose()?;
    let error = image
        .journal()
        .iter()
        .rev()
        .filter_map(|entry| entry.observation())
        .find(|observation| {
            observation.node().as_str() == run.node
                && observation.run().is_some_and(|id| id.as_str() == run.run)
                && observation.error().is_some()
        })
        .and_then(|observation| observation.error())
        .map(|error| {
            json!({"code":error.code(), "message":error.message(),
            "locations":error.locations().iter().map(|p| json!({"source":p.source,"start":p.start,"end":p.end,"line":p.line,"column":p.column,"endLine":p.end_line,"endColumn":p.end_column})).collect::<Vec<_>>(), "issues":error.issues().iter().map(|issue| json!({"path":issue.path,
                "code":issue.code,"message":issue.message})).collect::<Vec<_>>() })
        });
    Ok(
        json!({"run":run,"definition":definition["entry"],"trace":trace,"error":error,
        "contextNote":"Captured source references and environment revisions. Evaluated historical input payloads and old name resolution are not reconstructed.",
        "traceNote":if trace.is_some() {"Recorded trace for this run."} else {"No recorded trace for this run; it cannot be recovered by protecting the result."}}),
    )
}
impl ApplicationHandle {
    pub async fn work_history(
        &self,
        generation: String,
        client: String,
        cell: String,
        run: Option<String>,
    ) -> Result<Value, Error> {
        self.require_generation(&generation)?;
        let (reply, receive) = oneshot::channel();
        self.management
            .try_send(Request::History {
                generation,
                client,
                cell,
                run,
                reply,
            })
            .map_err(|_| Error::Busy)?;
        receive.await.map_err(|_| Error::Unavailable)?
    }
    pub async fn protect_run(
        &self,
        generation: String,
        client: String,
        cell: String,
        run: String,
    ) -> Result<Value, Error> {
        self.require_generation(&generation)?;
        let (reply, receive) = oneshot::channel();
        self.management
            .try_send(Request::Protect {
                generation,
                client,
                cell,
                run,
                reply,
            })
            .map_err(|_| Error::Busy)?;
        receive.await.map_err(|_| Error::Unavailable)?
    }
}
impl Owner {
    pub(crate) async fn read_work_history(
        &self,
        generation: &str,
        client: &str,
        cell: &str,
        selected: Option<&str>,
    ) -> Result<Value, Error> {
        self.check_management(generation, client)?;
        let observation = self
            .active
            .as_ref()
            .ok_or(Error::Stale)?
            .running
            .handle
            .observe()
            .await
            .map_err(|_| Error::Unavailable)?;
        let (nodes, attempts) = nodes(&observation, cell)?;
        let captured = self
            .active
            .as_ref()
            .ok_or(Error::Stale)?
            .recorder
            .capture(HistoryCaptureLimits::default())
            .await
            .map_err(|_| Error::EvidenceMissing)?;
        let runs = runs(captured.image(), &nodes);
        let value = match selected {
            Some(id) => {
                let run = runs.get(id).ok_or(Error::EvidenceMissing)?;
                let mut value = detail(captured.image(), run)?;
                value["canProtect"] = json!(self.history_protectable(captured.image(), run).await);
                value
            }
            None => {
                json!({"attempts":attempts,"runs":runs.into_values().collect::<Vec<_>>(),"unconfirmedWrites":captured.append_report().failed.to_string()})
            }
        };
        if serde_json::to_vec(&value)
            .map_err(|_| Error::Unavailable)?
            .len()
            > 8 * 1024 * 1024
        {
            return Err(Error::EvidenceMissing);
        }
        Ok(value)
    }
    /// Presentation eligibility only; protection rechecks policy and durability at mutation time.
    async fn history_protectable(&self, image: &HistoryImage, run: &Run) -> bool {
        if image.checkpoint().journal.persistence == Persistence::Volatile {
            return false;
        }
        let Some(storage) = &self.config.storage else {
            return false;
        };
        let Some(handle) = run
            .handle
            .as_deref()
            .and_then(|handle| ValueHandle::new(handle).ok())
        else {
            return false;
        };
        let Ok(Some(value)) = storage.worker.read(handle).await else {
            return false;
        };
        let policy = value.value.provenance().policy();
        !policy.is_private() && !policy.is_unknown()
    }
    pub(crate) async fn protect_run(
        &self,
        generation: &str,
        client: &str,
        cell: &str,
        selected: &str,
    ) -> Result<Value, Error> {
        self.check_management(generation, client)?;
        let checkpoint = self.management_checkpoint().await?;
        let result = async {
            let observation = self
                .active
                .as_ref()
                .ok_or(Error::Stale)?
                .running
                .handle
                .observe()
                .await
                .map_err(|_| Error::Unavailable)?;
            let (nodes, _) = nodes(&observation, cell)?;
            let runs = runs(checkpoint.history(), &nodes);
            let run = runs.get(selected).ok_or(Error::EvidenceMissing)?;
            let mut evidence = detail(checkpoint.history(), run)?;
            let handle = ValueHandle::new(run.handle.as_deref().ok_or(Error::EvidenceMissing)?)
                .map_err(|_| Error::EvidenceMissing)?;
            let worker = &self
                .config
                .storage
                .as_ref()
                .ok_or(Error::Unavailable)?
                .worker;
            let value = worker
                .read(handle.clone())
                .await
                .map_err(|_| Error::EvidenceMissing)?
                .ok_or(Error::EvidenceMissing)?;
            let policy = value.value.provenance().policy();
            if policy.is_private() || policy.is_unknown() {
                return Err(Error::EvidenceMissing);
            }
            drop(value);
            if checkpoint.history().checkpoint().journal.persistence == Persistence::Volatile {
                return Err(Error::EvidenceMissing);
            }
            let kept = worker
                .retain_as(handle.clone(), Retention::Protected)
                .await
                .map_err(|_| Error::ProtectionUnconfirmed)?;
            if !kept.kept
                || kept.retention != Retention::Protected
                || kept.retained_persistence == Persistence::Volatile
            {
                return Err(Error::ProtectionUnconfirmed);
            }
            if !run.protected {
                let entry = JournalEntry::ProtectedRun {
                    node: wes_engine::graph::NodeId::new(&run.node)
                        .map_err(|_| Error::EvidenceMissing)?,
                    run: wes_engine::runtime::RunId::new(&run.run)
                        .map_err(|_| Error::EvidenceMissing)?,
                    handle,
                };
                let receipt = self
                    .active
                    .as_ref()
                    .ok_or(Error::Stale)?
                    .recorder
                    .append(Arc::new(Record::Journal(entry)))
                    .await
                    .map_err(|_| Error::ProtectionUnconfirmed)?;
                if receipt.persistence == Persistence::Volatile {
                    return Err(Error::ProtectionUnconfirmed);
                }
            }
            evidence["run"]["protected"] = json!(true);
            evidence["canProtect"] = json!(true);
            Ok(evidence)
        }
        .await;
        checkpoint.resume().await;
        result
    }
}
