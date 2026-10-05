//! Shared, serialized cooperative operation boundary. Trusted callers grant exact work;
//! neither source text nor an assistant tool can mint or widen a grant.
use super::*;
use crate::access::WriteScope;
use std::collections::BTreeSet;
#[derive(Default)]
pub(super) struct WorkGrant {
    nodes: BTreeSet<NodeId>,
    names: BTreeSet<String>,
    sources: BTreeSet<String>,
}
impl SessionHandle {
    /// Initialize once from the originating UI context, then observe the actor's independent context.
    pub async fn observe_actor(
        &self,
        actor: String,
        inherit: String,
    ) -> Result<SessionObservation, SessionError> {
        self.observe_actor_in(actor, inherit, None).await
    }
    /// Initialize an independent actor in an explicitly selected environment. Existing
    /// actor selections are preserved. This is context, never execution authority.
    pub async fn observe_actor_in(
        &self,
        actor: String,
        inherit: String,
        environment: Option<String>,
    ) -> Result<SessionObservation, SessionError> {
        let (reply, receive) = oneshot::channel();
        self.controls
            .send(Control::ObserveActor {
                actor,
                inherit,
                environment,
                reply,
            })
            .await
            .map_err(|_| SessionError::Stopped)?;
        receive.await.map_err(|_| SessionError::Stopped)?
    }
    /// Replace a live grant with the exact nodes currently belonging to the supplied cells.
    /// Empty cells revoke it. This trusted host port is deliberately absent from agent tools.
    pub async fn grant_work(&self, actor: String, cells: Vec<String>) -> Result<(), SessionError> {
        let (reply, receive) = oneshot::channel();
        self.controls
            .send(Control::GrantWork {
                actor,
                cells,
                reply,
            })
            .await
            .map_err(|_| SessionError::Stopped)?;
        receive.await.map_err(|_| SessionError::Stopped)?
    }
    /// Source disclosure is independent of mutation and value export permissions.
    /// Exact live cell identities; an empty set revokes disclosure.
    pub async fn grant_sources(
        &self,
        actor: String,
        cells: Vec<String>,
    ) -> Result<(), SessionError> {
        let (reply, receive) = oneshot::channel();
        self.controls
            .send(Control::GrantSources {
                actor,
                cells,
                reply,
            })
            .await
            .map_err(|_| SessionError::Stopped)?;
        receive.await.map_err(|_| SessionError::Stopped)?
    }
    /// Returns no source when it is not shared; never derives permission from a result.
    pub async fn read_source(
        &self,
        actor: String,
        cell: String,
    ) -> Result<Option<String>, SessionError> {
        let (reply, receive) = oneshot::channel();
        self.controls
            .send(Control::ReadSource { actor, cell, reply })
            .await
            .map_err(|_| SessionError::Stopped)?;
        receive.await.map_err(|_| SessionError::Stopped)?
    }
    pub async fn cancel_actor(
        &self,
        actor: String,
        nodes: Vec<NodeId>,
    ) -> Result<(), SessionError> {
        let (reply, receive) = oneshot::channel();
        self.controls
            .send(Control::CancelActor {
                actor,
                nodes,
                reply,
            })
            .await
            .map_err(|_| SessionError::Stopped)?;
        receive.await.map_err(|_| SessionError::Stopped)?
    }
    /// Resolve current cell membership and check affected-work authority on the actor.
    /// No whole-workspace observation or journal read is needed to cancel known work.
    pub async fn cancel_actor_work(&self, actor: String, cell: String) -> Result<(), SessionError> {
        let (reply, receive) = oneshot::channel();
        self.controls
            .send(Control::CancelActorWork { actor, cell, reply })
            .await
            .map_err(|_| SessionError::Stopped)?;
        receive.await.map_err(|_| SessionError::Stopped)?
    }
}
impl Actor {
    pub(super) fn attach_actor(
        &mut self,
        actor: &str,
        inherit: &str,
        environment: Option<String>,
    ) -> Result<(), SessionError> {
        if actor.is_empty() || actor.len() > 128 || actor.chars().any(char::is_control) {
            return Err(SessionError::Authority);
        }
        if !self.work_grants.contains_key(actor) {
            if self.work_grants.len() >= 128 {
                return Err(SessionError::Capacity);
            }
            let context = self
                .environment_clients
                .get(inherit)
                .cloned()
                .or_else(|| self.workspace.default_environment_context());
            let context = if let Some(environment) = environment {
                let context = wes_core::environments::EnvironmentContext {
                    selected: Some(environment),
                    revisions: self.workspace.environments().revisions(),
                };
                context.validate()?;
                Some(context)
            } else {
                context
            };
            if let Some(context) = context {
                self.environment_clients.insert(actor.into(), context);
            }
            self.work_grants.insert(actor.into(), WorkGrant::default());
        }
        Ok(())
    }
    pub(super) fn source_scope(&self, input: &SourceInput) -> WriteScope {
        if input.is_cooperative() {
            self.actor_scope(input.client())
        } else {
            WriteScope::default()
        }
    }
    pub(super) fn actor_scope(&self, actor: &str) -> WriteScope {
        // Creation ownership is independent of later repeats, grants and reply presentation.
        // Restored identities have no live owner; a new actor needs an explicit scoped grant.
        let granted = self.work_grants.get(actor);
        let nodes = self
            .node_owners
            .iter()
            .filter(|(_, owner)| *owner == actor)
            .map(|(node, _)| node.clone());
        let names = self
            .name_owners
            .iter()
            .filter(|(_, (owner, _))| owner == actor)
            .map(|(name, _)| name.clone());
        WriteScope::restricted(
            nodes.chain(granted.into_iter().flat_map(|g| g.nodes.iter().cloned())),
            names.chain(granted.into_iter().flat_map(|g| g.names.iter().cloned())),
        )
    }

    pub(super) fn check_cell_change(&self, actor: &str, cell: &str) -> Result<(), SessionError> {
        let observed = self.cells.observed();
        let reply = observed
            .iter()
            .find(|c| c.input.cell() == cell)
            .and_then(|c| c.reply.as_ref())
            .and_then(|r| r.as_ref().ok())
            .filter(|r| !r.nodes.is_empty())
            .ok_or(SessionError::Authority)?;
        for node in &reply.nodes {
            self.actor_scope(actor)
                .check_effect(
                    node,
                    self.workspace.runtime().graph(),
                    self.workspace.bindings(),
                )
                .map_err(SessionError::from_access)?;
        }
        Ok(())
    }

    pub(super) fn grant_work(
        &mut self,
        actor: String,
        cells: Vec<String>,
    ) -> Result<(), SessionError> {
        if self.active.is_some() || !self.environment_work.is_empty() {
            return Err(SessionError::AdmissionBusy);
        }
        if !self.work_grants.contains_key(&actor) || cells.len() > 256 {
            return Err(SessionError::Authority);
        }
        let observed = self.cells.observed();
        let mut grant = WorkGrant::default();
        for id in cells {
            let reply = observed
                .iter()
                .find(|c| c.input.cell() == id)
                .and_then(|c| c.reply.as_ref())
                .and_then(|r| r.as_ref().ok())
                .ok_or(SessionError::Authority)?;
            grant.nodes.extend(reply.nodes.iter().cloned());
            grant.names.extend(
                self.workspace
                    .bindings()
                    .names()
                    .iter()
                    .filter(|(name, output)| {
                        !self.name_owners.contains_key(*name) && reply.nodes.contains(&output.node)
                    })
                    .map(|(name, _)| name.clone()),
            );
            grant.names.extend(
                self.name_owners
                    .iter()
                    .filter(|(_, (_, cell))| *cell == id)
                    .map(|(name, _)| name.clone()),
            );
        }
        if grant.nodes.len() > 4096 || grant.names.len() > 4096 {
            return Err(SessionError::Capacity);
        }
        let current = self.work_grants.get_mut(&actor).expect("registered actor");
        current.nodes = grant.nodes;
        current.names = grant.names;
        Ok(())
    }
    pub(super) fn grant_sources(
        &mut self,
        actor: &str,
        cells: Vec<String>,
    ) -> Result<(), SessionError> {
        if cells.len() > 256 {
            return Err(SessionError::Capacity);
        }
        if !self.work_grants.contains_key(actor)
            || cells.iter().any(|id| self.cells.input(id).is_none())
        {
            return Err(SessionError::Authority);
        }
        self.work_grants
            .get_mut(actor)
            .expect("registered actor")
            .sources = cells.into_iter().collect();
        Ok(())
    }
    pub(super) fn read_source(
        &self,
        actor: &str,
        cell: &str,
    ) -> Result<Option<String>, SessionError> {
        let grant = self.work_grants.get(actor).ok_or(SessionError::Authority)?;
        let input = self.cells.input(cell).ok_or(SessionError::Authority)?;
        let own = input.is_cooperative() && input.client() == actor;
        Ok((own || grant.sources.contains(cell)).then(|| input.text().to_owned()))
    }
    pub(super) fn cancel_actor(
        &mut self,
        actor: &str,
        nodes: &[NodeId],
    ) -> Result<(), SessionError> {
        if self.workspace.runtime().is_closed() {
            return Err(SessionError::Stopped);
        }
        if self.checkpoints.busy() && !self.checkpoints.pending() {
            return Err(SessionError::CheckpointBusy);
        }
        let scope = self.actor_scope(actor);
        for node in nodes {
            for affected in self.workspace.runtime().cancellation_scope(node) {
                scope
                    .check_effect(
                        &affected,
                        self.workspace.runtime().graph(),
                        self.workspace.bindings(),
                    )
                    .map_err(SessionError::from_access)?;
            }
        }
        let effects = self
            .workspace
            .cancel_nodes(nodes, self.io.now())
            .map_err(|_| SessionError::Commit)?;
        self.effects(effects);
        Ok(())
    }
}
