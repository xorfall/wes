//! Explicit work deletion coordinated by the application, not by presentation code.
use crate::backend::{BackendError, StorageCensus};
use crate::{
    ApplicationHandle, Owner,
    retention::{ManagementError, Request},
};
use serde::Serialize;
use std::{
    collections::{BTreeMap, BTreeSet},
    time::{Duration, Instant},
};
use tokio::sync::oneshot;
use wes_engine::{
    history::HistoryImage,
    session::{SessionCheckpoint, retirement::RetirementPlan},
    storage::{Retention, ValueHandle},
};

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkPreview {
    pub token: String,
    pub cells: Vec<String>,
    pub nodes: Vec<String>,
    pub dependents: Vec<String>,
    pub labels: BTreeMap<String, String>,
    pub payloads: Vec<String>,
    pub protected: Vec<String>,
    pub shared_workspaces: Vec<String>,
    pub expires_in_seconds: u64,
}
#[derive(PartialEq, Eq)]
struct Stamp {
    history: (u64, u64),
    plan: RetirementPlan,
    census: StorageCensus,
    reasons: BTreeMap<ValueHandle, Retention>,
}
pub(crate) struct WorkPlan {
    client: String,
    generation: String,
    cell: String,
    at: Instant,
    stamp: Stamp,
}
impl ApplicationHandle {
    pub async fn preview_delete_work(
        &self,
        generation: String,
        client: String,
        cell: String,
    ) -> Result<WorkPreview, ManagementError> {
        self.require_generation(&generation)?;
        let (reply, receive) = oneshot::channel();
        self.management
            .try_send(Request::WorkPreview {
                generation,
                client,
                cell,
                reply,
            })
            .map_err(|_| ManagementError::Busy)?;
        receive.await.map_err(|_| ManagementError::Unavailable)?
    }
    pub async fn delete_work(
        &self,
        generation: String,
        client: String,
        token: String,
        dependents: bool,
        protected: bool,
    ) -> Result<(), ManagementError> {
        self.require_generation(&generation)?;
        let (reply, receive) = oneshot::channel();
        self.management
            .try_send(Request::WorkConfirm {
                generation,
                client,
                token,
                dependents,
                protected,
                reply,
            })
            .map_err(|_| ManagementError::Busy)?;
        receive.await.map_err(|_| ManagementError::Unavailable)?
    }
}
impl Owner {
    async fn inspect_work(
        &self,
        checkpoint: &SessionCheckpoint,
        cell: &str,
        validate: bool,
    ) -> Result<(Stamp, WorkPreview), ManagementError> {
        let observed = self
            .active
            .as_ref()
            .ok_or(ManagementError::Stale)?
            .running
            .handle
            .observe()
            .await
            .map_err(|_| ManagementError::Unavailable)?;
        let image = checkpoint.history();
        let plan = RetirementPlan::prepare(image, &observed, cell)?;
        if validate {
            let factory = self.config.workspace.clone();
            let (original, candidate) = tokio::task::spawn_blocking(move || {
                Ok::<_, wes_engine::workspace::WorkspaceError>((factory()?, factory()?))
            })
            .await
            .map_err(|_| ManagementError::Unavailable)?
            .map_err(|_| ManagementError::Unavailable)?;
            plan.validate(image, original, candidate).await?;
        }
        let compacted = plan.compact(image, vec![])?;
        let files = self.files.clone();
        let name = self
            .active
            .as_ref()
            .ok_or(ManagementError::Stale)?
            .name
            .clone();
        let peers = self.management_peers.clone();
        let mut census = tokio::task::spawn_blocking(move || {
            files
                .lock()
                .map_err(|_| ManagementError::References)?
                .storage_census_live(&name, &compacted, &peers)
                .map_err(|_| ManagementError::References)
        })
        .await
        .map_err(|_| ManagementError::References)??;
        for (handle, names) in &self.management_live {
            census
                .references
                .entry(handle.clone())
                .or_default()
                .extend(names.iter().cloned());
        }
        // Temporary/private current outputs need no Result record but are still owners.
        if let Some(values) = &observed.values {
            for (node, output) in &values.outputs {
                if !plan.nodes.contains(node)
                    && let Some(handle) = output.handle()
                {
                    census.references.entry(handle.clone()).or_default().insert(
                        self.active
                            .as_ref()
                            .ok_or(ManagementError::Stale)?
                            .name
                            .as_str()
                            .into(),
                    );
                }
            }
        }
        let mut reasons = BTreeMap::new();
        if let Some(storage) = &self.config.storage {
            for handle in &plan.payloads {
                reasons.insert(
                    handle.clone(),
                    storage
                        .worker
                        .retention(handle.clone())
                        .await
                        .map_err(|_| ManagementError::Unavailable)?,
                );
            }
        } else if !plan.payloads.is_empty() {
            return Err(ManagementError::Unavailable);
        }
        let exclusive: Vec<_> = plan
            .payloads
            .iter()
            .filter(|h| !census.references.contains_key(*h))
            .collect();
        let preview = WorkPreview {
            token: String::new(),
            cells: plan.cells.iter().cloned().collect(),
            nodes: plan.nodes.iter().map(ToString::to_string).collect(),
            dependents: plan.dependents.iter().cloned().collect(),
            labels: observed
                .cells
                .iter()
                .filter(|c| plan.cells.contains(c.input.cell()))
                .map(|c| {
                    (
                        c.input.cell().to_owned(),
                        c.input.text().chars().take(160).collect(),
                    )
                })
                .collect(),
            payloads: exclusive.iter().map(ToString::to_string).collect(),
            protected: exclusive
                .iter()
                .filter(|h| reasons.get(*h) == Some(&Retention::Protected))
                .map(ToString::to_string)
                .collect(),
            shared_workspaces: plan
                .payloads
                .iter()
                .filter_map(|h| census.references.get(h))
                .flatten()
                .cloned()
                .collect::<BTreeSet<_>>()
                .into_iter()
                .collect(),
            expires_in_seconds: 120,
        };
        let checkpoint = image.checkpoint();
        Ok((
            Stamp {
                history: (
                    checkpoint.journal.end_offset,
                    checkpoint.recovery.end_offset,
                ),
                plan,
                census,
                reasons,
            },
            preview,
        ))
    }
    async fn work_checkpoint(&self, cell: &str) -> Result<SessionCheckpoint, ManagementError> {
        self.require_backend_retention()?;
        let observed = self
            .active
            .as_ref()
            .ok_or(ManagementError::Stale)?
            .running
            .handle
            .observe()
            .await
            .map_err(|_| ManagementError::Unavailable)?;
        RetirementPlan::preflight(&observed, cell)?;
        self.management_checkpoint().await
    }
    pub(crate) async fn preview_work(
        &mut self,
        generation: String,
        client: String,
        cell: String,
    ) -> Result<WorkPreview, ManagementError> {
        self.check_management(&generation, &client)?;
        self.work_plans
            .retain(|_, p| p.at.elapsed() < Duration::from_secs(120) && p.client != client);
        if self.work_plans.len() >= 16 || cell.len() > 256 {
            return Err(ManagementError::Busy);
        }
        let checkpoint = self.work_checkpoint(&cell).await?;
        let inspected = self.inspect_work(&checkpoint, &cell, true).await;
        checkpoint.resume().await;
        let (stamp, mut preview) = inspected?;
        preview.token = uuid::Uuid::new_v4().to_string();
        self.work_plans.insert(
            preview.token.clone(),
            WorkPlan {
                client,
                generation,
                cell,
                at: Instant::now(),
                stamp,
            },
        );
        Ok(preview)
    }
    pub(crate) async fn confirm_work(
        &mut self,
        generation: String,
        client: String,
        token: String,
        dependents: bool,
        protected: bool,
    ) -> Result<(), ManagementError> {
        self.check_management(&generation, &client)?;
        if !self
            .work_plans
            .get(&token)
            .is_some_and(|p| p.client == client && p.generation == generation)
        {
            return Err(ManagementError::Stale);
        }
        let previous = self
            .work_plans
            .remove(&token)
            .ok_or(ManagementError::Stale)?;
        let checkpoint = self.work_checkpoint(&previous.cell).await?;
        let result = self
            .commit_work(&checkpoint, previous, dependents, protected)
            .await;
        checkpoint.resume().await;
        result
    }
    async fn commit_work(
        &mut self,
        checkpoint: &SessionCheckpoint,
        previous: WorkPlan,
        dependents: bool,
        protected: bool,
    ) -> Result<(), ManagementError> {
        self.active
            .as_ref()
            .ok_or(ManagementError::Stale)?
            .running
            .handle
            .fence_retirement_at_checkpoint(checkpoint)
            .await
            .map_err(|_| ManagementError::Busy)?;
        let (stamp, preview) = self.inspect_work(checkpoint, &previous.cell, false).await?;
        if stamp != previous.stamp || previous.at.elapsed() >= Duration::from_secs(120) {
            return Err(ManagementError::Stale);
        }
        if (preview.cells.iter().any(|cell| cell != &previous.cell) && !dependents)
            || (!preview.protected.is_empty() && !protected)
        {
            return Err(ManagementError::Approval);
        }
        let approved = stamp
            .plan
            .payloads
            .iter()
            .filter(|h| {
                !stamp.census.references.contains_key(*h)
                    && stamp.reasons.get(*h) == Some(&Retention::Protected)
            })
            .cloned()
            .collect();
        let image = stamp.plan.compact(checkpoint.history(), approved)?;
        let files = self.files.clone();
        let name = self
            .active
            .as_ref()
            .ok_or(ManagementError::Stale)?
            .name
            .clone();
        // No live state is reconstructed/replaced: private values and authority stay owned.
        // Publication precedes in-memory retirement; any post-publication failure closes
        // admission. Restart reads the complete published generation and cleanup intent.
        let published = tokio::task::spawn_blocking(move || {
            let mut files = files
                .lock()
                .map_err(|_| (false, ManagementError::References))?;
            files.save(&name, &image).map_err(|e| {
                let published = matches!(e, BackendError::Published(_));
                (
                    published,
                    if published {
                        ManagementError::WorkUnconfirmed
                    } else {
                        ManagementError::WorkNotPublished
                    },
                )
            })?;
            files
                .load(&name)
                .map_err(|_| (true, ManagementError::WorkUnconfirmed))
        })
        .await;
        let (history, image) = match published {
            Ok(Ok(pair)) => pair,
            Ok(Err((false, error))) => return Err(error),
            _ => {
                self.stop_after_publication().await;
                return Err(ManagementError::WorkUnconfirmed);
            }
        };
        if self
            .active
            .as_ref()
            .ok_or(ManagementError::Stale)?
            .recorder
            .replace_sink(history)
            .await
            .is_err()
        {
            self.stop_after_publication().await;
            return Err(ManagementError::WorkUnconfirmed);
        }
        let image = std::sync::Arc::new(image);
        if self
            .active
            .as_ref()
            .ok_or(ManagementError::Stale)?
            .running
            .handle
            .retire_at_checkpoint(stamp.plan, image.clone(), checkpoint)
            .await
            .is_err()
        {
            self.stop_after_publication().await;
            return Err(ManagementError::WorkUnconfirmed);
        }
        self.release_plans.clear();
        self.work_plans.clear();
        // The new history generation's retirement intent is the durable retry source. This
        // does not sweep arbitrary orphan values or reinterpret retention as consent.
        let cleanup = self.cleanup_retired(&image).await;
        // History publication is not a new live session. Keep terminal/agent ownership
        // and in-flight client context attached to the engine that survived retirement.
        let mut current = self
            .current
            .borrow()
            .clone()
            .expect("live retirement session");
        current.storage_warning = None;
        if let Err(error) = &cleanup {
            current.storage_warning = Some(format!("{}: {}", error.code(), error));
        }
        self.current.send_replace(Some(current));
        self.publish_sessions();
        cleanup
    }
    async fn stop_after_publication(&self) {
        self.shutdown.cancel();
        if let Some(active) = &self.active {
            let _ = active.running.handle.shutdown().await;
        }
    }
    pub(crate) async fn cleanup_retired(
        &self,
        image: &std::sync::Arc<HistoryImage>,
    ) -> Result<(), ManagementError> {
        self.require_backend_retention()?;
        let image = image.clone();
        let files = self.files.clone();
        let name = self
            .active
            .as_ref()
            .ok_or(ManagementError::Stale)?
            .name
            .clone();
        let peers = self.management_peers.clone();
        let mut census = tokio::task::spawn_blocking(move || {
            let mut files = files.lock().map_err(|_| ManagementError::CleanupPending)?;
            let census = files
                .storage_census_live(&name, &image, &peers)
                .map_err(|_| ManagementError::CleanupPending)?;
            let collection = files
                .collect_unused()
                .map_err(|_| ManagementError::CleanupPending)?;
            if collection.active != 0 || collection.preserved != 0 {
                return Err(ManagementError::CleanupPending);
            }
            Ok::<_, ManagementError>(census)
        })
        .await
        .map_err(|_| ManagementError::CleanupPending)??;
        for (handle, names) in &self.management_live {
            census
                .references
                .entry(handle.clone())
                .or_default()
                .extend(names.iter().cloned());
        }
        let Some(storage) = &self.config.storage else {
            return if census.cleanup.is_empty() {
                Ok(())
            } else {
                Err(ManagementError::CleanupPending)
            };
        };
        let observed = self
            .active
            .as_ref()
            .ok_or(ManagementError::Stale)?
            .running
            .handle
            .observe()
            .await
            .map_err(|_| ManagementError::CleanupPending)?;
        let live: BTreeSet<_> = observed
            .values
            .iter()
            .flat_map(|v| v.outputs.values())
            .filter_map(|v| v.handle().cloned())
            .collect();
        for (handle, approved) in census.cleanup {
            if census.references.contains_key(&handle) || live.contains(&handle) {
                continue;
            }
            let reason = storage
                .worker
                .retention(handle.clone())
                .await
                .map_err(|_| ManagementError::CleanupPending)?;
            if reason == Retention::Protected && !approved {
                continue;
            }
            storage
                .worker
                .release(handle)
                .await
                .map_err(|_| ManagementError::CleanupPending)?;
        }
        Ok(())
    }
}
