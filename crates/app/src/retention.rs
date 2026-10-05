//! Application-owned payload management. The UI receives evidence, not filesystem authority.
use crate::backend::PayloadReferences;
use crate::{ApplicationHandle, Owner};
use serde::Serialize;
use std::{
    collections::BTreeSet,
    time::{Duration, Instant},
};
use tokio::sync::oneshot;
use wes_engine::{
    session::SessionCheckpoint,
    storage::{Retention, RetentionUsage, ValueHandle},
};

#[derive(Debug, thiserror::Error)]
pub enum ManagementError {
    #[error("The target workspace does not exist. Nothing was created or deleted.")]
    WorkspaceMissing,
    #[error(
        "The target workspace could not be opened for inspection (storage, restoration or live-workspace capacity). Nothing was deleted; the issuing workspace remains selected."
    )]
    WorkspaceUnavailable,
    #[error(
        "Work deletion is in progress. This request was refused; wait for the updated workspace before inspecting or changing work."
    )]
    Retiring,
    #[error("This workspace backend does not support retention management. No change was made.")]
    UnsupportedBackend,
    #[error(
        "The requested run evidence is missing, private, incomplete or outside the bounded history reader. No complete protection was confirmed."
    )]
    EvidenceMissing,
    #[error(
        "Run protection could not be confirmed. Its result may have been retained; inspect the same run before retrying. No execution was started."
    )]
    ProtectionUnconfirmed,
    #[error("{0}")]
    Work(#[from] wes_engine::session::retirement::RetirementError),
    #[error(
        "Approve dependent work and final-reference Protected content explicitly. Nothing was deleted."
    )]
    Approval,
    #[error(
        "Work deletion reached publication, but completion could not be confirmed. Reopen wes to recover; do not assume the work remains or retry execution."
    )]
    WorkUnconfirmed,
    #[error(
        "The replacement history could not be published. No work was deleted; resolve the storage problem before requesting another preview."
    )]
    WorkNotPublished,
    #[error(
        "Work was deleted. Some authorized physical cleanup is still pending; reopening wes will retry it. This does not restore deleted work."
    )]
    CleanupPending,
    #[error("Storage management is unavailable.")]
    Unavailable,
    #[error("Pinned views still use this result: {0}. Rebind those views before releasing it.")]
    PinnedViews(String),
    #[error(
        "Wait for execution, streams, recording and workspace operations across this session to finish, then request a new preview. Unrelated active work also blocks deletion in this version."
    )]
    Busy,
    #[error(
        "The deletion preview expired or its workspace, work, retention or saved references changed. Request a new preview; nothing was deleted by this request."
    )]
    Stale,
    #[error(
        "All saved-workspace references could not be verified. Deletion was refused; nothing was deleted."
    )]
    References,
    #[error("The selected result is unavailable or private; deletion was refused.")]
    Missing,
    #[error(
        "Result deletion could not be confirmed. Stored copies may have changed; inspect the result before making another request."
    )]
    Unconfirmed,
}
impl ManagementError {
    pub fn code(&self) -> &'static str {
        match self {
            Self::WorkspaceMissing => "STO016",
            Self::WorkspaceUnavailable => "STO017",
            Self::Retiring => "STO015",
            Self::UnsupportedBackend => "STO014",
            Self::EvidenceMissing => "STO012",
            Self::ProtectionUnconfirmed => "STO013",
            Self::Work(_) => "STO007",
            Self::Approval => "STO008",
            Self::WorkUnconfirmed => "STO009",
            Self::WorkNotPublished => "STO011",
            Self::CleanupPending => "STO010",
            Self::Unavailable => "STO001",
            Self::PinnedViews(_) => "STO018",
            Self::Busy => "STO002",
            Self::Stale => "STO003",
            Self::References => "STO004",
            Self::Missing => "STO005",
            Self::Unconfirmed => "STO006",
        }
    }
}
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ReleasePreview {
    pub token: String,
    pub handle: String,
    pub retention: String,
    pub nodes: Vec<String>,
    pub downstream: Vec<String>,
    pub workspaces: Vec<String>,
    pub expires_in_seconds: u64,
}
#[derive(PartialEq, Eq)]
struct Stamp {
    history: (u64, u64),
    references: PayloadReferences,
    reason: Retention,
    graph: Vec<(String, String, String, Vec<String>)>,
}
pub(crate) struct Plan {
    client: String,
    generation: String,
    handle: ValueHandle,
    at: Instant,
    stamp: Stamp,
}
impl Plan {
    fn fresh(&self) -> bool {
        self.at.elapsed() < Duration::from_secs(120)
    }
}
pub(crate) enum Request {
    History {
        generation: String,
        client: String,
        cell: String,
        run: Option<String>,
        reply: oneshot::Sender<Result<serde_json::Value, ManagementError>>,
    },
    Protect {
        generation: String,
        client: String,
        cell: String,
        run: String,
        reply: oneshot::Sender<Result<serde_json::Value, ManagementError>>,
    },
    WorkPreview {
        generation: String,
        client: String,
        cell: String,
        reply: oneshot::Sender<Result<crate::retirement::WorkPreview, ManagementError>>,
    },
    WorkConfirm {
        generation: String,
        client: String,
        token: String,
        dependents: bool,
        protected: bool,
        reply: oneshot::Sender<Result<(), ManagementError>>,
    },
    Preview {
        generation: String,
        client: String,
        handle: ValueHandle,
        reply: oneshot::Sender<Result<ReleasePreview, ManagementError>>,
    },
    Confirm {
        generation: String,
        client: String,
        token: String,
        reply: oneshot::Sender<Result<(), ManagementError>>,
    },
}
impl Request {
    fn destructive(&self) -> bool {
        !matches!(self, Self::History { .. } | Self::Protect { .. })
    }
    fn refuse(self, error: ManagementError) {
        match self {
            Self::History { reply, .. } | Self::Protect { reply, .. } => {
                let _ = reply.send(Err(error));
            }
            Self::WorkPreview { reply, .. } => {
                let _ = reply.send(Err(error));
            }
            Self::WorkConfirm { reply, .. } | Self::Confirm { reply, .. } => {
                let _ = reply.send(Err(error));
            }
            Self::Preview { reply, .. } => {
                let _ = reply.send(Err(error));
            }
        }
    }
    pub(crate) fn generation(&self) -> &str {
        match self {
            Self::History { generation, .. }
            | Self::Protect { generation, .. }
            | Self::WorkPreview { generation, .. }
            | Self::WorkConfirm { generation, .. }
            | Self::Preview { generation, .. }
            | Self::Confirm { generation, .. } => generation,
        }
    }
}
impl ApplicationHandle {
    pub(crate) fn require_generation(&self, generation: &str) -> Result<(), ManagementError> {
        if self.shutdown.is_cancelled()
            || !self
                .current()
                .is_ok_and(|current| current.generation == generation)
        {
            return Err(ManagementError::Stale);
        }
        Ok(())
    }
    pub async fn retention_usage(&self) -> Result<RetentionUsage, ManagementError> {
        self.storage
            .as_ref()
            .ok_or(ManagementError::Unavailable)?
            .retention_usage()
            .await
            .map_err(|_| ManagementError::Unavailable)
    }
    pub async fn preview_release(
        &self,
        generation: String,
        client: String,
        handle: ValueHandle,
    ) -> Result<ReleasePreview, ManagementError> {
        self.require_generation(&generation)?;
        let (reply, receive) = oneshot::channel();
        self.management
            .try_send(Request::Preview {
                generation,
                client,
                handle,
                reply,
            })
            .map_err(|_| ManagementError::Busy)?;
        receive.await.map_err(|_| ManagementError::Unavailable)?
    }
    pub async fn confirm_release(
        &self,
        generation: String,
        client: String,
        token: String,
    ) -> Result<(), ManagementError> {
        self.require_generation(&generation)?;
        let (reply, receive) = oneshot::channel();
        self.management
            .try_send(Request::Confirm {
                generation,
                client,
                token,
                reply,
            })
            .map_err(|_| ManagementError::Busy)?;
        receive.await.map_err(|_| ManagementError::Unavailable)?
    }
}
impl Owner {
    pub(crate) fn check_management(
        &self,
        generation: &str,
        client: &str,
    ) -> Result<(), ManagementError> {
        if client.is_empty()
            || client.len() > 128
            || !self
                .current
                .borrow()
                .as_ref()
                .is_some_and(|s| s.generation == generation)
            || self.shutdown.is_cancelled()
        {
            return Err(ManagementError::Stale);
        }
        Ok(())
    }
    pub(crate) fn require_backend_retention(&self) -> Result<(), ManagementError> {
        if self
            .files
            .lock()
            .map_err(|_| ManagementError::Unavailable)?
            .capabilities()
            .retention
        {
            Ok(())
        } else {
            Err(ManagementError::UnsupportedBackend)
        }
    }
    pub(crate) async fn management_checkpoint(&self) -> Result<SessionCheckpoint, ManagementError> {
        self.require_backend_retention()?;
        Self::settled_checkpoint(
            &self
                .active
                .as_ref()
                .ok_or(ManagementError::Stale)?
                .running
                .handle,
        )
        .await
    }
    async fn settled_checkpoint(
        handle: &wes_engine::session::SessionHandle,
    ) -> Result<SessionCheckpoint, ManagementError> {
        let observed = handle
            .observe()
            .await
            .map_err(|_| ManagementError::Unavailable)?;
        if !observed.state.execution.idle
            || observed.state.admission_pending
            || observed.state.checkpoint_pending
            || !observed.state.execution.streaming.is_empty()
            || !observed.state.conversations.is_empty()
        {
            return Err(ManagementError::Busy);
        }
        // A just-admitted concurrent submission may still win before this pause. A
        // bounded wait refuses the request; cancellation never undoes entered work.
        tokio::time::timeout(Duration::from_secs(5), handle.checkpoint())
            .await
            .map_err(|_| ManagementError::Busy)?
            .map_err(|_| ManagementError::Busy)
    }
    async fn inspect_release(
        &self,
        checkpoint: &SessionCheckpoint,
        handle: &ValueHandle,
    ) -> Result<(Stamp, ReleasePreview), ManagementError> {
        let worker = &self
            .config
            .storage
            .as_ref()
            .ok_or(ManagementError::Unavailable)?
            .worker;
        let value = worker
            .read(handle.clone())
            .await
            .map_err(|_| ManagementError::Missing)?
            .ok_or(ManagementError::Missing)?;
        if value.value.provenance().policy().is_private() {
            return Err(ManagementError::Missing);
        }
        drop(value);
        let reason = worker
            .retention(handle.clone())
            .await
            .map_err(|_| ManagementError::Unavailable)?;
        let image = checkpoint.history();
        let files = self.files.clone();
        let name = self
            .active
            .as_ref()
            .ok_or(ManagementError::Stale)?
            .name
            .clone();
        let selected = handle.clone();
        let image = image.clone();
        let peers = self.management_peers.clone();
        let mut references = tokio::task::spawn_blocking(move || {
            files
                .lock()
                .map_err(|_| ManagementError::References)?
                .payload_references_live(&selected, &name, &image, &peers)
                .map_err(|_| ManagementError::References)
        })
        .await
        .map_err(|_| ManagementError::References)??;
        if !references.retained_views.is_empty() {
            let mut examples = references
                .retained_views
                .iter()
                .take(8)
                .map(|(workspace, view)| format!("{workspace}/{view}"))
                .collect::<Vec<_>>()
                .join(", ");
            if references.retained_views.len() > 8 {
                examples.push_str(&format!(" (+{} more)", references.retained_views.len() - 8));
            }
            return Err(ManagementError::PinnedViews(examples));
        }
        if let Some(live) = self.management_live.get(handle) {
            for (name, _, used) in &mut references.names {
                *used |= live.contains(name);
            }
        }
        let observed = self
            .active
            .as_ref()
            .ok_or(ManagementError::Stale)?
            .running
            .handle
            .observe()
            .await
            .map_err(|_| ManagementError::Unavailable)?;
        let execution = &observed.state.execution;
        // Streaming can arrive during the race before pause admission. Never inspect a
        // moving window or let confirmation invalidate a conversation underneath its owner.
        if !execution.streaming.is_empty() || !observed.state.conversations.is_empty() {
            return Err(ManagementError::Busy);
        }
        let mut nodes = vec![];
        let mut downstream = BTreeSet::new();
        if let Some(values) = observed.values {
            for (node, output) in values.outputs {
                if output.handle() == Some(handle) {
                    nodes.push(node.as_str().to_owned());
                    for dependent in execution
                        .graph
                        .downstream(&node)
                        .map_err(|_| ManagementError::Unavailable)?
                    {
                        downstream.insert(dependent.as_str().to_owned());
                    }
                }
            }
        }
        nodes.sort();
        for node in &nodes {
            downstream.remove(node);
        }
        let graph: Vec<_> = execution
            .graph
            .nodes()
            .map(|node| {
                (
                    node.id().as_str().to_owned(),
                    format!("{:?}", node.state()),
                    execution
                        .runs
                        .get(node.id())
                        .map_or(String::new(), |r| r.as_str().to_owned()),
                    node.dependencies()
                        .keys()
                        .map(|n| n.as_str().to_owned())
                        .collect::<Vec<_>>(),
                )
            })
            .collect();
        let charge = graph.iter().fold(0usize, |sum, (id, state, run, edges)| {
            edges.iter().fold(
                sum.saturating_add(id.len() + state.len() + run.len() + 128),
                |n, e| n.saturating_add(e.len() + 32),
            )
        });
        if graph.len() > 10_000 || charge > 4 * 1024 * 1024 {
            return Err(ManagementError::References);
        }
        let preview = ReleasePreview {
            token: String::new(),
            handle: handle.to_string(),
            retention: reason.as_str().into(),
            nodes,
            downstream: downstream.into_iter().collect(),
            workspaces: references
                .names
                .iter()
                .filter(|(_, _, used)| *used)
                .map(|(n, _, _)| n.clone())
                .collect(),
            expires_in_seconds: 120,
        };
        let boundary = checkpoint.history().checkpoint();
        Ok((
            Stamp {
                history: (boundary.journal.end_offset, boundary.recovery.end_offset),
                references,
                reason,
                graph,
            },
            preview,
        ))
    }
    /// Hold every other live session steady through reference inspection and physical cleanup.
    pub(crate) async fn pause_management_peers(
        &mut self,
    ) -> Result<std::collections::BTreeMap<String, SessionCheckpoint>, ManagementError> {
        let mut checkpoints = std::collections::BTreeMap::new();
        let mut charged = 0u64;
        for parked in self.parked.values() {
            let handle = &parked.active.running.handle;
            let checkpoint = Self::settled_checkpoint(handle).await?;
            handle
                .fence_retirement_at_checkpoint(&checkpoint)
                .await
                .map_err(|_| ManagementError::Busy)?;
            charged = charged.saturating_add(checkpoint.history().charged_bytes());
            if charged > 128 * 1024 * 1024 {
                return Err(ManagementError::References);
            }
            self.management_peers
                .insert(parked.active.name.clone(), checkpoint.history().clone());
            let observed = handle
                .observe()
                .await
                .map_err(|_| ManagementError::Unavailable)?;
            if !observed.state.execution.streaming.is_empty()
                || !observed.state.conversations.is_empty()
            {
                return Err(ManagementError::Busy);
            }
            if let Some(values) = observed.values {
                for output in values.outputs.values() {
                    if let Some(value) = output.handle() {
                        self.management_live
                            .entry(value.clone())
                            .or_default()
                            .insert(parked.active.name.as_str().into());
                    }
                }
            }
            checkpoints.insert(parked.active.name.as_str().into(), checkpoint);
        }
        Ok(checkpoints)
    }
    pub(crate) async fn manage_storage(&mut self, request: Request) {
        let checkpoints = if request.destructive() {
            match self.pause_management_peers().await {
                Ok(checkpoints) => checkpoints,
                Err(error) => {
                    self.management_peers.clear();
                    self.management_live.clear();
                    request.refuse(error);
                    return;
                }
            }
        } else {
            std::collections::BTreeMap::new()
        };
        self.manage_storage_paused(request, &checkpoints).await;
        self.management_peers.clear();
        self.management_live.clear();
        for checkpoint in checkpoints.into_values() {
            checkpoint.resume().await;
        }
    }
    async fn manage_storage_paused(
        &mut self,
        request: Request,
        peer_checkpoints: &std::collections::BTreeMap<String, SessionCheckpoint>,
    ) {
        self.release_plans.retain(|_, plan| plan.fresh());
        match request {
            Request::History {
                generation,
                client,
                cell,
                run,
                reply,
            } => {
                if !reply.is_closed() {
                    let _ = reply.send(
                        self.read_work_history(&generation, &client, &cell, run.as_deref())
                            .await,
                    );
                }
            }
            Request::Protect {
                generation,
                client,
                cell,
                run,
                reply,
            } => {
                let result = self.protect_run(&generation, &client, &cell, &run).await;
                let _ = reply.send(result);
            }
            Request::WorkPreview {
                generation,
                client,
                cell,
                reply,
            } => {
                if !reply.is_closed() {
                    let result = self.preview_work(generation, client, cell).await;
                    let _ = reply.send(result);
                }
            }
            Request::WorkConfirm {
                generation,
                client,
                token,
                dependents,
                protected,
                reply,
            } => {
                let result = self
                    .confirm_work(generation, client, token, dependents, protected)
                    .await;
                let _ = reply.send(result);
            }
            Request::Preview {
                generation,
                client,
                handle,
                reply,
            } => {
                if reply.is_closed() {
                    return;
                }
                let result = async {
                    self.check_management(&generation, &client)?;
                    // One outstanding preview per client. Never evict another client's authority.
                    self.release_plans.retain(|_, p| p.client != client);
                    if self.release_plans.len() >= 16 {
                        return Err(ManagementError::Busy);
                    }
                    let checkpoint = self.management_checkpoint().await?;
                    let inspected = self.inspect_release(&checkpoint, &handle).await;
                    checkpoint.resume().await;
                    let (stamp, mut preview) = inspected?;
                    let token = uuid::Uuid::new_v4().to_string();
                    preview.token = token.clone();
                    self.release_plans.insert(
                        token,
                        Plan {
                            client,
                            generation,
                            handle,
                            at: Instant::now(),
                            stamp,
                        },
                    );
                    Ok(preview)
                }
                .await;
                let _ = reply.send(result);
            }
            Request::Confirm {
                generation,
                client,
                token,
                reply,
            } => {
                let result = async {
                    self.check_management(&generation, &client)?;
                    if !self
                        .release_plans
                        .get(&token)
                        .is_some_and(|p| p.client == client && p.generation == generation)
                    {
                        return Err(ManagementError::Stale);
                    }
                    let plan = self
                        .release_plans
                        .remove(&token)
                        .ok_or(ManagementError::Stale)?;
                    let checkpoint = self.management_checkpoint().await?;
                    let released = async {
                        let (stamp, _) = self.inspect_release(&checkpoint, &plan.handle).await?;
                        if stamp != plan.stamp || !plan.fresh() {
                            return Err(ManagementError::Stale);
                        }
                        // No Save/Load, new source or Keep can interleave with the pause and
                        // this owner. Reads and physical release share the serial store worker.
                        let receipt = self
                            .active
                            .as_ref()
                            .ok_or(ManagementError::Stale)?
                            .running
                            .handle
                            .release_at_checkpoint(plan.handle.clone(), &checkpoint)
                            .await
                            .map_err(|_| ManagementError::Unconfirmed)?;
                        if receipt.problem.is_some() {
                            return Err(ManagementError::Unconfirmed);
                        }
                        // The shared store has one payload. Invalidate matching live outputs in
                        // every paused owner as well; repeated physical release is idempotent.
                        if let Some(names) = self.management_live.get(&plan.handle) {
                            for name in names {
                                let peer =
                                    self.parked.get(name).ok_or(ManagementError::Unconfirmed)?;
                                let pause = peer_checkpoints
                                    .get(name)
                                    .ok_or(ManagementError::Unconfirmed)?;
                                let receipt = peer
                                    .active
                                    .running
                                    .handle
                                    .release_at_checkpoint(plan.handle.clone(), pause)
                                    .await
                                    .map_err(|_| ManagementError::Unconfirmed)?;
                                if receipt.problem.is_some() {
                                    return Err(ManagementError::Unconfirmed);
                                }
                            }
                        }
                        Ok(())
                    }
                    .await;
                    checkpoint.resume().await;
                    released
                }
                .await;
                // An admitted confirmation finishes even when the requesting client disconnects.
                let _ = reply.send(result);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn deletion_authority_expires_without_a_cleanup_timer() {
        let mut plan = Plan {
            client: "alice".into(),
            generation: "session".into(),
            handle: ValueHandle::fresh(),
            at: Instant::now(),
            stamp: Stamp {
                history: (0, 0),
                references: PayloadReferences {
                    names: vec![],
                    retained_views: vec![],
                },
                reason: Retention::Unknown,
                graph: vec![],
            },
        };
        assert!(plan.fresh());
        plan.at = Instant::now() - Duration::from_secs(121);
        assert!(!plan.fresh());
    }
}
