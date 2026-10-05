//! Consuming, effect-free structural reconstruction. It has no executor or live runtime access.
use super::{BatchApplied, DeclarationDraft, Installation, Workspace, WorkspaceError};
use crate::{
    graph::NodeId,
    runtime::{RestoredState, RunId},
    source::PreparedReplay,
};
use indexmap::IndexSet;
use std::time::Duration;

pub struct ReplayWorkspace {
    workspace: Workspace,
    used: IndexSet<NodeId>,
    hydrating: bool,
    validation_only: bool,
}
impl ReplayWorkspace {
    pub(crate) async fn restore_environments(
        &mut self,
        record: &crate::environments::EnvironmentRecord,
    ) -> Result<(), wes_core::environments::EnvironmentError> {
        let record_id = record.id().to_owned();
        let captured_record = record.clone();
        if self.hydrating {
            return Err(wes_core::environments::EnvironmentError {
                code: "ENV009",
                message: "environment declarations cannot follow value hydration".into(),
            });
        }
        let registry = self.workspace.environments().planning_snapshot();
        let record = record.clone();
        let loader = self.workspace.environment_loader.clone();
        let images = self.workspace.environment_images.clone();
        let (plan, images) = tokio::task::spawn_blocking(move || {
            let plan = record.prepare(&registry)?;
            let images = match loader {
                Some(loader) => images.build(&plan, loader.as_ref())?,
                None => images,
            };
            Ok::<_, wes_core::environments::EnvironmentError>((plan, images))
        })
        .await
        .map_err(|_| wes_core::environments::EnvironmentError {
            code: "ENV009",
            message: "environment reconstruction worker failed".into(),
        })??;
        self.workspace.apply_environment_plan(plan).map_err(|_| {
            wes_core::environments::EnvironmentError {
                code: "ENV009",
                message: "environment reconstruction could not be installed".into(),
            }
        })?;
        self.workspace.environment_images = images;
        if self
            .workspace
            .environment_records
            .iter()
            .map(|r| r.charge())
            .sum::<u64>()
            .saturating_add(captured_record.charge())
            > 64 * 1024 * 1024
        {
            return Err(wes_core::environments::EnvironmentError {
                code: "ENV007",
                message: "environment input history exceeds 64 MiB".into(),
            });
        }
        self.workspace
            .environment_records
            .push(captured_record.clone());
        self.workspace.environment_record = Some(captured_record);
        if !self.validation_only
            && let Some(authority) = self
                .workspace
                .environment_loader
                .as_ref()
                .and_then(|l| l.authority())
        {
            authority.establish_namespace(&record_id);
            authority.restored();
        }
        Ok(())
    }
    /// Reserve recorded identities even on rejected source. Full history coordinators use this
    /// entry point, not a bare draft, so new work cannot reuse IDs from failed reconstruction.
    pub async fn prepare(
        &mut self,
        command: &crate::history::CommandRecord,
        cancellation: crate::driver::CancellationToken,
    ) -> Result<PreparedReplay, crate::source::SourceError> {
        if self.hydrating {
            return Err(WorkspaceError::Obsolete.into());
        }
        let next = self.workspace.next_revision()?;
        self.workspace
            .runtime
            .reserve_historical_ids(&command.nodes);
        self.workspace.revision = next;
        crate::source::prepare_replay(command, self.draft()?, cancellation).await
    }
    /// A configured empty workspace may provide catalogues/types, never live nodes.
    pub fn new(workspace: Workspace) -> Result<Self, WorkspaceError> {
        if !workspace.runtime.graph().is_empty()
            || !workspace.runtime.is_idle()
            || workspace.runtime.is_closed()
        {
            return Err(WorkspaceError::Obsolete);
        }
        Ok(Self {
            workspace,
            used: IndexSet::new(),
            hydrating: false,
            validation_only: false,
        })
    }
    /// Structural validation must not reset the live embedding's credential grants.
    pub(crate) fn validation(workspace: Workspace) -> Result<Self, WorkspaceError> {
        let mut replay = Self::new(workspace)?;
        replay.validation_only = true;
        Ok(replay)
    }
    pub(crate) fn reserve_retired(
        &mut self,
        record: &crate::history::RetiredWork,
    ) -> Result<(), WorkspaceError> {
        if record.validate().is_err() || record.nodes.iter().any(|n| self.used.contains(n)) {
            return Err(WorkspaceError::Obsolete);
        }
        self.workspace.runtime.reserve_historical_ids(&record.nodes);
        self.used.extend(record.nodes.iter().cloned());
        Ok(())
    }
    pub fn workspace(&self) -> &Workspace {
        &self.workspace
    }
    pub fn draft(&self) -> Result<DeclarationDraft, WorkspaceError> {
        if self.hydrating {
            return Err(WorkspaceError::Obsolete);
        }
        self.workspace.draft()
    }
    /// PreparedReplay validates exact identities and rejects semantic partial acceptance first.
    /// Remember removed historical IDs too: a later record cannot silently reuse their identity.
    pub fn apply(&mut self, prepared: PreparedReplay) -> Result<BatchApplied, WorkspaceError> {
        if self.hydrating {
            return Err(WorkspaceError::Obsolete);
        }
        let nodes: Vec<_> = prepared.nodes().cloned().collect();
        if nodes.iter().any(|node| self.used.contains(node)) {
            return Err(super::rejected(
                "ENG008",
                wes_language::Span::at(0),
                "replay reuses an earlier historical node identity",
            ));
        }
        let result = self.workspace.commit_batch_installation(
            prepared.batch,
            Duration::ZERO,
            Installation::Held,
        )?;
        debug_assert!(
            result.effects.is_empty(),
            "reconstruction creates no execution effects"
        );
        self.used.extend(nodes);
        Ok(result)
    }
    /// The restore coordinator supplies only the selected run's retained bytes/observation state.
    /// Hydration is effect-free and retains policy/timeout configuration, including held handlers.
    pub fn hydrate(
        &mut self,
        node: &NodeId,
        state: RestoredState,
        run: Option<RunId>,
    ) -> Result<(), WorkspaceError> {
        self.workspace
            .runtime
            .restore_held_state(node, state, run)?;
        self.hydrating = true;
        Ok(())
    }
    /// No work starts here. Required-call admission reconstruction and session identity/Log/store
    /// snapshots are separate coordinator responsibilities, not implied by these structural nodes.
    pub fn finish(self) -> Workspace {
        self.workspace
    }
}

impl Workspace {
    pub(crate) fn restore_view_reference(
        &mut self,
        node: &NodeId,
        state: RestoredState,
        run: Option<RunId>,
    ) -> Result<(), crate::runtime::RuntimeError> {
        self.runtime.restore_held_state(node, state, run)
    }
}
