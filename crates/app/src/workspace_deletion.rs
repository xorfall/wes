//! Whole-workspace retirement belongs to the application owner. Preview tokens
//! carry no filesystem authority and all admitted physical work is joined.
use crate::{
    backend::{StorageCensus, WorkspaceDeletion, WorkspaceIdentity},
    retention::ManagementError,
    *,
};
use serde::Serialize;
use std::{collections::BTreeSet, time::Duration};
use tokio::time::Instant;
use wes_engine::{
    history::{JournalEntry, unresolved_calls},
    storage::{Retention, ValueHandle},
};

const PLAN_TTL: Duration = Duration::from_secs(120);

fn expired(at: Instant) -> bool {
    at.elapsed() >= PLAN_TTL
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Preview {
    pub token: String,
    pub workspace: String,
    pub identity: String,
    pub cells: usize,
    pub nodes: usize,
    pub running: Vec<String>,
    pub streams: Vec<String>,
    pub sandboxes: Vec<String>,
    pub running_sandboxes: Vec<String>,
    pub terminals: Vec<String>,
    pub exclusive_payloads: usize,
    pub protected_payloads: usize,
    pub shared_workspaces: Vec<String>,
    pub blockers: Vec<String>,
    pub preserved: Vec<String>,
    pub expires_in_seconds: u64,
}
#[derive(Clone, Debug)]
struct Stamp {
    environments: BTreeMap<String, wes_core::environments::Revision>,
    identity: WorkspaceIdentity,
    cells: Vec<String>,
    history: wes_engine::history::deletion::DeletionEvidence,
    all_cells: Vec<String>,
    relevant_payloads: BTreeSet<ValueHandle>,
    sandboxes: wes_engine::session::sandbox::Lifecycle,
    census: StorageCensus,
    reasons: BTreeMap<ValueHandle, Retention>,
    terminals: Vec<String>,
}
impl PartialEq for Stamp {
    fn eq(&self, other: &Self) -> bool {
        self.environments == other.environments
            && self.identity == other.identity
            && self.cells == other.cells
            && self.history == other.history
            && self.sandboxes == other.sandboxes
            && self.reasons == other.reasons
            && self.terminals == other.terminals
            && self.relevant_payloads == other.relevant_payloads
            && self.census.identities == other.census.identities
            && self.relevant_payloads.iter().all(|h| {
                self.census.references.get(h) == other.census.references.get(h)
                    && self.census.cleanup.get(h) == other.census.cleanup.get(h)
            })
    }
}
impl Eq for Stamp {}
pub(crate) struct Plan {
    issuer_generation: String,
    client: String,
    generation: String,
    at: Instant,
    stamp: Stamp,
}
pub(crate) enum Request {
    Preview {
        generation: String,
        client: String,
        reply: oneshot::Sender<Result<Preview, ManagementError>>,
    },
    Confirm {
        generation: String,
        client: String,
        token: String,
        stop: bool,
        protected: bool,
        reply: oneshot::Sender<Result<(), ManagementError>>,
    },
}
impl Request {
    pub(crate) fn generation(&self) -> &str {
        match self {
            Self::Preview { generation, .. } | Self::Confirm { generation, .. } => generation,
        }
    }
}
impl ApplicationHandle {
    pub fn subscribe_workspace_names(&self) -> watch::Receiver<Arc<[WorkspaceName]>> {
        self.names.clone()
    }
    pub(crate) fn attach_terminals(
        &self,
        manager: terminal::Manager,
    ) -> Result<(), ApplicationError> {
        let mut managers = self
            .terminals
            .lock()
            .map_err(|_| ApplicationError::Worker)?;
        if managers.len() >= 16 {
            return Err(ApplicationError::Capacity);
        }
        managers.push(manager);
        Ok(())
    }
    pub async fn preview_workspace_deletion(
        &self,
        generation: String,
        client: String,
    ) -> Result<Preview, ManagementError> {
        self.require_generation(&generation)?;
        let (reply, receive) = oneshot::channel();
        self.deletions
            .try_send(Request::Preview {
                generation,
                client,
                reply,
            })
            .map_err(|_| ManagementError::Busy)?;
        receive.await.map_err(|_| ManagementError::Unavailable)?
    }
    pub async fn delete_workspace(
        &self,
        generation: String,
        client: String,
        token: String,
        stop: bool,
        protected: bool,
    ) -> Result<(), ManagementError> {
        self.require_generation(&generation)?;
        let (reply, receive) = oneshot::channel();
        self.deletions
            .try_send(Request::Confirm {
                generation,
                client,
                token,
                stop,
                protected,
                reply,
            })
            .map_err(|_| ManagementError::Busy)?;
        receive.await.map_err(|_| ManagementError::Unavailable)?
    }
}
fn empty_image() -> HistoryImage {
    let receipt = AppendReceipt {
        persistence: Persistence::Volatile,
        end_offset: 0,
    };
    HistoryCapture::new(HistoryCaptureLimits::default()).finish(HistoryCheckpoint {
        journal: receipt,
        recovery: receipt,
    })
}
fn uncertain(image: &HistoryImage) -> bool {
    !unresolved_calls(image.recovery()).is_empty()
        || image.journal().iter().any(|entry| matches!(entry, JournalEntry::Noticed(notice) if notice.error().code() == wes_core::ErrorValue::REMOTE_OUTCOME_UNKNOWN))
        || image
            .journal()
            .iter()
            .filter_map(JournalEntry::observation)
            .filter_map(|e| e.error())
            .any(|e| e.code() == wes_core::ErrorValue::REMOTE_OUTCOME_UNKNOWN)
}
fn payloads(image: &HistoryImage) -> BTreeSet<ValueHandle> {
    image
        .journal()
        .iter()
        .flat_map(|e| {
            let mut handles = Vec::new();
            if let Some(handle) = e.payload_reference() {
                handles.push(handle.clone());
            }
            if let JournalEntry::Retired(r) = e {
                handles.extend(r.payloads.iter().cloned());
            }
            handles
        })
        .collect()
}
impl Owner {
    async fn terminal_barriers(
        &self,
        generation: &str,
    ) -> Result<Vec<terminal::WorkspaceTerminals>, ManagementError> {
        let managers = self
            .terminals
            .lock()
            .map_err(|_| ManagementError::Unavailable)?
            .clone();
        let mut barriers = vec![];
        for manager in managers {
            barriers.push(manager.workspace_terminals(generation).await);
        }
        Ok(barriers)
    }
    async fn deletion_inspect(
        &self,
        generation: &str,
        terminals: Vec<String>,
    ) -> Result<(Stamp, Preview), ManagementError> {
        let active = self.active.as_ref().ok_or(ManagementError::Stale)?;
        let observed = active
            .running
            .handle
            .observe()
            .await
            .map_err(|_| ManagementError::Unavailable)?;
        let sandbox = active
            .running
            .handle
            .sandbox_lifecycle()
            .await
            .map_err(|_| ManagementError::Unavailable)?;
        let captured = active
            .recorder
            .capture(HistoryCaptureLimits::default())
            .await
            .map_err(|_| ManagementError::References)?;
        if captured.append_report().failed != 0 {
            return Err(ManagementError::References);
        }
        let image = captured.into_image();
        let mut peers = BTreeMap::new();
        let mut live = BTreeMap::<ValueHandle, BTreeSet<String>>::new();
        let mut blockers = vec![];
        for p in self.parked.values() {
            let image = if let Some(image) = self.management_peers.get(&p.active.name) {
                image.clone()
            } else {
                let capture = p
                    .active
                    .recorder
                    .capture(HistoryCaptureLimits::default())
                    .await
                    .map_err(|_| ManagementError::References)?;
                if capture.append_report().failed != 0 {
                    return Err(ManagementError::References);
                }
                capture.into_image()
            };
            peers.insert(p.active.name.clone(), image);
            let state = p
                .active
                .running
                .handle
                .observe()
                .await
                .map_err(|_| ManagementError::References)?;
            if state.state.execution.executing.iter().any(|id| {
                state
                    .state
                    .execution
                    .graph
                    .node(id)
                    .is_none_or(|n| !n.payload().observational())
            }) || !state.state.execution.streaming.is_empty()
                || !state.state.conversations.is_empty()
            {
                blockers.push(format!("Workspace {} has active work; stop or finish it before deleting shared storage.",p.active.name.as_str()));
            }
            if let Some(values) = state.values {
                for output in values.outputs.values() {
                    if let Some(h) = output.handle() {
                        live.entry(h.clone())
                            .or_default()
                            .insert(p.active.name.as_str().into());
                    }
                }
            }
        }
        if observed.state.admission_pending || observed.state.checkpoint_pending {
            blockers.push("Workspace admission or storage operation is pending; refresh this preview after it settles.".into());
        }
        if uncertain(&image) && observed.state.execution.idle {
            blockers.push(
                "Unresolved external-call evidence must be reconciled before deletion.".into(),
            );
        }
        let name = active.name.clone();
        let files = self.files.clone();
        let own = image.clone();
        let (identity, mut census) = tokio::task::spawn_blocking(move || {
            let mut files = files.lock().map_err(|_| ManagementError::References)?;
            let identity = files
                .identity(&name)
                .map_err(|_| ManagementError::UnsupportedBackend)?
                .ok_or(ManagementError::Stale)?;
            let census = files
                .storage_census_live(&name, &own, &peers)
                .map_err(|_| ManagementError::References)?;
            Ok::<_, ManagementError>((identity, census))
        })
        .await
        .map_err(|_| ManagementError::References)??;
        for (h, names) in live {
            census.references.entry(h).or_default().extend(names);
        }
        let mut handles = payloads(&image);
        if let Some(values) = &observed.values {
            handles.extend(values.outputs.values().filter_map(|v| v.handle().cloned()));
        }
        let observation_payloads = image.observation_payloads();
        let mut relevant_payloads = BTreeSet::new();
        let mut reasons = BTreeMap::new();
        let mut shared = BTreeSet::new();
        let mut exclusive = 0;
        let mut protected = 0;
        for h in handles {
            let other = census
                .references
                .get(&h)
                .into_iter()
                .flatten()
                .filter(|n| n.as_str() != active.name.as_str())
                .cloned()
                .collect::<Vec<_>>();
            if other.is_empty() {
                exclusive += 1;
                let reason = match &self.config.storage {
                    Some(s) => s
                        .worker
                        .retention(h.clone())
                        .await
                        .map_err(|_| ManagementError::Unavailable)?,
                    None => Retention::Unknown,
                };
                if reason == Retention::Protected {
                    protected += 1;
                }
                if !observation_payloads.contains(&h) || reason == Retention::Protected {
                    relevant_payloads.insert(h.clone());
                    reasons.insert(h, reason);
                }
            } else {
                if !observation_payloads.contains(&h) {
                    relevant_payloads.insert(h.clone());
                }
                shared.extend(other);
            }
        }

        let mut cells = observed
            .cells
            .iter()
            .map(|c| c.input.cell().to_owned())
            .collect::<Vec<_>>();
        cells.sort();
        let all_cells = cells.clone();
        cells.retain(|id| {
            observed
                .cells
                .iter()
                .find(|c| c.input.cell() == id)
                .is_none_or(|c| !wes_engine::history::deletion::observation_source(c.input.text()))
        });
        let preview = Preview {
            token: String::new(),
            workspace: active.name.as_str().into(),
            identity: identity.id.clone(),
            cells: all_cells.len(),
            nodes: observed.state.execution.graph.nodes().count(),
            running: observed
                .state
                .execution
                .executing
                .iter()
                .filter(|id| {
                    observed
                        .state
                        .execution
                        .graph
                        .node(id)
                        .is_none_or(|n| !n.payload().observational())
                })
                .map(ToString::to_string)
                .collect(),
            streams: observed
                .state
                .execution
                .streaming
                .iter()
                .map(ToString::to_string)
                .collect(),
            sandboxes: sandbox.definitions.clone(),
            running_sandboxes: sandbox.running.clone(),
            terminals: terminals.clone(),
            exclusive_payloads: exclusive,
            protected_payloads: protected,
            shared_workspaces: shared.into_iter().collect(),
            blockers,
            preserved: vec![
                "Other workspaces and their shared results".into(),
                "API library, saved credentials, presentation settings and original source files"
                    .into(),
                "Independent terminal pane history and archived source inputs".into(),
                "External API, file, container and database effects (stopping is not rollback)"
                    .into(),
            ],
            expires_in_seconds: PLAN_TTL.as_secs(),
        };
        let _ = generation;
        Ok((
            Stamp {
                environments: observed.environment_revisions.clone(),
                identity,
                cells,
                history: image.deletion_evidence(),
                all_cells,
                relevant_payloads,
                sandboxes: sandbox,
                census,
                reasons,
                terminals,
            },
            preview,
        ))
    }
    async fn prepare_workspace_deletion(
        &mut self,
        issuer_generation: String,
        client: String,
        workspace: Option<WorkspaceName>,
    ) -> Result<Preview, ManagementError> {
        self.check_management(&issuer_generation, &client)?;
        let generation = match workspace {
            None => issuer_generation.clone(),
            Some(name) => {
                self.open_session(name, false)
                    .await
                    .map_err(|error| match error {
                        ApplicationError::Backend(BackendError::Missing) => {
                            ManagementError::WorkspaceMissing
                        }
                        _ => ManagementError::WorkspaceUnavailable,
                    })?
                    .generation
            }
        };
        self.select_generation(&generation);
        let result = self
            .capture_workspace_deletion(generation, issuer_generation.clone(), client)
            .await;
        self.select_generation(&issuer_generation);
        result
    }
    async fn capture_workspace_deletion(
        &mut self,
        generation: String,
        issuer_generation: String,
        client: String,
    ) -> Result<Preview, ManagementError> {
        self.check_management(&generation, &client)?;
        self.deletion_plans
            .retain(|_, p| !expired(p.at) && p.client != client);
        if self.deletion_plans.len() >= 16 {
            return Err(ManagementError::Busy);
        }
        let barriers = self.terminal_barriers(&generation).await?;
        let terminals = barriers.iter().flat_map(|b| b.ids()).collect();
        let (stamp, mut preview) = self.deletion_inspect(&generation, terminals).await?;
        preview.token = uuid::Uuid::new_v4().to_string();
        self.deletion_plans.insert(
            preview.token.clone(),
            Plan {
                issuer_generation,
                client,
                generation,
                at: Instant::now(),
                stamp,
            },
        );
        Ok(preview)
    }
    pub(crate) async fn manage_source_workspace(
        &mut self,
        request: wes_engine::session::management::ManagementRequest,
    ) {
        use wes_engine::session::management::{Operation, WorkspaceDeletePlan};
        let result = async {
            let generation = self
                .current
                .borrow()
                .as_ref()
                .ok_or(ManagementError::Stale)?
                .generation
                .clone();
            match request.operation {
                Operation::PlanDelete { workspace } => {
                    let preview = self
                        .prepare_workspace_deletion(generation, request.client, workspace)
                        .await?;
                    let mut projection = serde_json::to_value(&preview).expect("preview");
                    projection
                        .as_object_mut()
                        .expect("preview object")
                        .remove("token");
                    projection["type"] = serde_json::json!("WorkspaceDeletePlan");
                    projection["lifetime"] = serde_json::json!(
                        "session only; applying revalidates the captured workspace"
                    );
                    let bytes = serde_json::to_vec(&projection).expect("projection");
                    let data = wes_adapters::codec::decode_json_preserving(
                        &bytes,
                        wes_adapters::codec::Limits {
                            bytes: 256 * 1024,
                            nodes: 20_000,
                        },
                    )
                    .map_err(|_| ManagementError::Busy)?;
                    Ok(Some(WorkspaceDeletePlan {
                        token: preview.token,
                        details: data,
                    }))
                }
                Operation::Delete {
                    token,
                    stop,
                    protected,
                } => {
                    self.confirm_workspace_deletion(
                        generation,
                        request.client,
                        token,
                        stop,
                        protected,
                    )
                    .await?;
                    Ok(None)
                }
            }
        }
        .await;
        let _ = request
            .reply
            .send(result.map_err(|e: ManagementError| format!("{}: {}", e.code(), e)));
    }
    pub(crate) async fn manage_deletion(&mut self, request: Request) {
        match request {
            Request::Preview {
                generation,
                client,
                reply,
            } => {
                let result = self
                    .prepare_workspace_deletion(generation, client, None)
                    .await;
                let _ = reply.send(result);
            }
            Request::Confirm {
                generation,
                client,
                token,
                stop,
                protected,
                reply,
            } => {
                let result = self
                    .confirm_workspace_deletion(generation, client, token, stop, protected)
                    .await;
                let _ = reply.send(result);
            }
        }
    }
    async fn confirm_workspace_deletion(
        &mut self,
        issuer_generation: String,
        client: String,
        token: String,
        stop: bool,
        protected: bool,
    ) -> Result<(), ManagementError> {
        self.check_management(&issuer_generation, &client)?;
        let generation = self
            .deletion_plans
            .get(&token)
            .filter(|plan| plan.client == client && plan.issuer_generation == issuer_generation)
            .ok_or(ManagementError::Stale)?
            .generation
            .clone();
        // Resolve the captured generation, never a name which could now denote a replacement.
        self.select_generation(&generation);
        let result = self
            .confirm_selected_workspace_deletion(generation, client, token, stop, protected)
            .await;
        self.select_generation(&issuer_generation);
        result
    }
    async fn confirm_selected_workspace_deletion(
        &mut self,
        generation: String,
        client: String,
        token: String,
        stop: bool,
        protected: bool,
    ) -> Result<(), ManagementError> {
        self.check_management(&generation, &client)?;
        if !self
            .deletion_plans
            .get(&token)
            .is_some_and(|p| p.client == client && p.generation == generation)
        {
            return Err(ManagementError::Stale);
        }
        let plan = self
            .deletion_plans
            .remove(&token)
            .ok_or(ManagementError::Stale)?;
        if expired(plan.at) {
            return Err(ManagementError::Stale);
        }
        let barriers = self.terminal_barriers(&generation).await?;
        let (stamp, preview) = self
            .deletion_inspect(&generation, barriers.iter().flat_map(|b| b.ids()).collect())
            .await?;
        if stamp != plan.stamp {
            return Err(ManagementError::Stale);
        }
        if !preview.blockers.is_empty() {
            return Err(ManagementError::Busy);
        }
        if !protected && preview.protected_payloads != 0 {
            return Err(ManagementError::Approval);
        }
        if !stop
            && (!preview.running.is_empty()
                || !preview.streams.is_empty()
                || !preview.running_sandboxes.is_empty()
                || !preview.terminals.is_empty())
        {
            return Err(ManagementError::Busy);
        }
        let peers = match self.pause_management_peers().await {
            Ok(p) => p,
            Err(e) => {
                self.management_peers.clear();
                self.management_live.clear();
                return Err(e);
            }
        };
        let verified = self
            .deletion_inspect(&generation, barriers.iter().flat_map(|b| b.ids()).collect())
            .await;
        let result = match verified {
            Ok((fresh, _)) if fresh == stamp => {
                self.delete_workspace_paused(stamp, stop, protected, barriers)
                    .await
            }
            Ok(_) => Err(ManagementError::Stale),
            Err(e) => Err(e),
        };
        self.management_peers.clear();
        self.management_live.clear();
        for peer in peers.into_values() {
            peer.resume().await;
        }
        result
    }
    async fn delete_workspace_paused(
        &mut self,
        stamp: Stamp,
        stop: bool,
        protected: bool,
        barriers: Vec<terminal::WorkspaceTerminals>,
    ) -> Result<(), ManagementError> {
        let active = self.active.as_ref().ok_or(ManagementError::Stale)?;
        active
            .running
            .handle
            .stop_workspace(
                stamp.environments.clone(),
                stamp.all_cells.clone(),
                stamp.sandboxes.revision,
                stop,
            )
            .await
            .map_err(|_| ManagementError::Stale)?;
        // Admission has closed before any terminal is stopped. Await all children,
        // including sandbox runtimes, entered providers and terminal processes.
        let active = self.active.take().ok_or(ManagementError::Stale)?;
        let name = active.name.clone();
        let original_current = self.current.clone();
        let joined_handle = active.running.handle.clone();
        let (joined, terminal_results) = tokio::join!(
            stop_running(active.running),
            futures_util::future::join_all(barriers.iter().map(|b| b.stop()))
        );
        let terminal_problem = terminal_results.iter().any(Result::is_err);
        // A sandbox has no durable run history. Preserve only the safety fact learned
        // while joining it, so refusing deletion cannot erase uncertainty on reopen.
        let uncertainty_recorded = if joined_handle.remote_outcome_uncertain() || terminal_problem {
            let at = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .ok()
                .and_then(|d| {
                    wes_core::Timestamp::new(i64::try_from(d.as_secs()).ok()?, d.subsec_nanos())
                        .ok()
                });
            match at {
                Some(at) => {
                    let error = wes_core::ErrorValue::new(wes_core::ErrorId::new(uuid::Uuid::new_v4().to_string()).expect("id"), wes_core::ErrorValue::REMOTE_OUTCOME_UNKNOWN, "A workspace job or sandbox stopped with an uncertain external outcome; reconcile it before deletion.", vec![], None).expect("error");
                    let notice = wes_engine::history::NoticeRecord::new(
                        uuid::Uuid::new_v4().to_string(),
                        at,
                        wes_engine::history::NoticeContext::WorkspaceShutdown,
                        error,
                    )
                    .expect("notice");
                    active
                        .recorder
                        .append(Arc::new(wes_engine::history::Record::Journal(
                            JournalEntry::Noticed(notice),
                        )))
                        .await
                        .is_ok()
                }
                None => false,
            }
        } else {
            true
        };
        let captured = active
            .recorder
            .capture(HistoryCaptureLimits::default())
            .await;
        let writer = stop_writer(active.recorder, active.writer).await;
        let result = async {
            if joined.is_err() || writer.is_err() || !uncertainty_recorded {
                return Err(ManagementError::Unconfirmed);
            }
            let captured = captured.map_err(|_| ManagementError::References)?;
            if captured.append_report().failed != 0 {
                return Err(ManagementError::References);
            }
            let image = captured.into_image();
            if terminal_problem || uncertain(&image) {
                return Err(ManagementError::Work(
                    wes_engine::session::retirement::RetirementError::Uncertain,
                ));
            }
            // Stop may have retained a final value. It belongs to the already
            // approved command set, but newly Protected data still needs consent.
            let handles = payloads(&image)
                .into_iter()
                .chain(stamp.reasons.keys().cloned())
                .collect::<BTreeSet<_>>();
            let mut approved = vec![];
            for h in &handles {
                if stamp
                    .census
                    .references
                    .get(h)
                    .is_some_and(|names| names.iter().any(|n| n != name.as_str()))
                    || self.management_live.contains_key(h)
                {
                    continue;
                }
                if let Some(storage) = &self.config.storage {
                    if storage
                        .worker
                        .retention(h.clone())
                        .await
                        .map_err(|_| ManagementError::Unavailable)?
                        == Retention::Protected
                    {
                        if !protected {
                            return Err(ManagementError::Approval);
                        }
                        approved.push(h.to_string());
                    }
                }
            }
            let intent = WorkspaceDeletion {
                version: 1,
                name: name.as_str().into(),
                identity: stamp.identity,
                payloads: handles.iter().map(ToString::to_string).collect(),
                protected: approved,
            };
            let files = self.files.clone();
            let deletion = intent.clone();
            let commit = tokio::task::spawn_blocking(move || {
                files
                    .lock()
                    .map_err(|_| BackendError::Invalid)?
                    .delete_workspace(&deletion)
            })
            .await
            .map_err(|_| ManagementError::WorkUnconfirmed)?;
            match commit {
                Ok(()) => {}
                Err(BackendError::Published(_)) => return Err(ManagementError::WorkUnconfirmed),
                Err(_) => return Err(ManagementError::WorkNotPublished),
            }
            self.cleanup_deleted().await
        }
        .await;
        if let Err(error) = &result {
            if matches!(
                error,
                ManagementError::CleanupPending | ManagementError::WorkUnconfirmed
            ) {
                *self
                    .cleanup_warning
                    .lock()
                    .map_err(|_| ManagementError::Unavailable)? =
                    Some(format!("{}: {}", error.code(), error));
            }
        }
        // Durable identity decides whether the old workspace remains. Never infer
        // presence from a failed cleanup or uncertain final directory sync.
        let files = self.files.clone();
        let selected = name.clone();
        let exists = tokio::task::spawn_blocking(move || {
            let files = files.lock().map_err(|_| BackendError::Invalid)?;
            Ok::<_, BackendError>((files.identity(&selected)?.is_some(), files.names()?))
        })
        .await;
        match exists {
            Ok(Ok((present, names))) => {
                self.names.send_replace(names.into());
                if present {
                    match self.open_session(name.clone(), false).await {
                        Ok(current) => {
                            original_current.send_replace(Some(current));
                            self.current = original_current;
                            self.bindings
                                .lock()
                                .map_err(|_| ManagementError::Unavailable)?
                                .insert(name.as_str().into(), self.current.subscribe());
                        }
                        Err(_) => {
                            original_current.send_replace(None);
                            return Err(ManagementError::WorkUnconfirmed);
                        }
                    }
                } else {
                    original_current.send_replace(None);
                    self.bindings
                        .lock()
                        .map_err(|_| ManagementError::Unavailable)?
                        .remove(name.as_str());
                    if self.active.is_none() {
                        if let Some(next) = self.parked.keys().next().cloned() {
                            self.select_context(&next);
                        }
                    }
                }
            }
            _ => {
                self.default_current.send_replace(None);
                return Err(ManagementError::WorkUnconfirmed);
            }
        }
        self.publish_sessions();
        result
    }
    pub(crate) async fn cleanup_deleted(&self) -> Result<(), ManagementError> {
        let files = self.files.clone();
        let peers = self.management_peers.clone();
        let (pending, census) = tokio::task::spawn_blocking(move || {
            let mut files = files.lock().map_err(|_| ManagementError::CleanupPending)?;
            let pending = files
                .pending_deletions()
                .map_err(|_| ManagementError::CleanupPending)?;
            if pending.is_empty() {
                return Ok((pending, StorageCensus::default()));
            }
            let empty = empty_image();
            let mut fake = WorkspaceName::new(format!("cleanup-{}", uuid::Uuid::new_v4()))
                .expect("valid name");
            let names = files.names().map_err(|_| ManagementError::CleanupPending)?;
            while names.contains(&fake) {
                fake = WorkspaceName::new(format!("cleanup-{}", uuid::Uuid::new_v4()))
                    .expect("valid name");
            }
            let census = files
                .storage_census_live(&fake, &empty, &peers)
                .map_err(|_| ManagementError::CleanupPending)?;
            let report = files
                .collect_unused()
                .map_err(|_| ManagementError::CleanupPending)?;
            if report.active != 0 || report.preserved != 0 {
                return Err(ManagementError::CleanupPending);
            }
            Ok::<_, ManagementError>((pending, census))
        })
        .await
        .map_err(|_| ManagementError::CleanupPending)??;
        for deletion in pending {
            for h in &deletion.payloads {
                let handle = ValueHandle::new(h).map_err(|_| ManagementError::CleanupPending)?;
                if census.references.contains_key(&handle)
                    || self.management_live.contains_key(&handle)
                {
                    continue;
                }
                let Some(storage) = &self.config.storage else {
                    return Err(ManagementError::CleanupPending);
                };
                if storage
                    .worker
                    .retention(handle.clone())
                    .await
                    .map_err(|_| ManagementError::CleanupPending)?
                    == Retention::Protected
                    && !deletion.protected.contains(h)
                {
                    return Err(ManagementError::CleanupPending);
                }
                storage
                    .worker
                    .release(handle)
                    .await
                    .map_err(|_| ManagementError::CleanupPending)?;
            }
            let files = self.files.clone();
            let directory = self.config.directory.clone();
            tokio::task::spawn_blocking(move || {
                let name = WorkspaceName::new(deletion.name)
                    .map_err(|_| ManagementError::CleanupPending)?;
                sandbox_definitions::FileDefinitions::new(directory, &name)
                    .remove()
                    .map_err(|_| ManagementError::CleanupPending)?;
                files
                    .lock()
                    .map_err(|_| ManagementError::CleanupPending)?
                    .finish_deletion(&deletion.identity.id)
                    .map_err(|_| ManagementError::CleanupPending)
            })
            .await
            .map_err(|_| ManagementError::CleanupPending)??;
        }
        Ok(())
    }
}

#[cfg(test)]
mod clock_tests {
    use super::*;

    #[tokio::test(start_paused = true)]
    async fn plan_expires_at_the_exact_deadline() {
        let at = Instant::now();
        tokio::time::advance(PLAN_TTL - Duration::from_millis(1)).await;
        assert!(!expired(at));
        tokio::time::advance(Duration::from_millis(1)).await;
        assert!(expired(at));
    }
}
