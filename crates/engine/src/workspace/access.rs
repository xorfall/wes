//! Bound write checks: the source parser cannot know which existing objects will change.
use super::{Change, PreparedControl, Workspace, WorkspaceError, draft::DeclarationDraft};
use crate::access::{WriteScope, denied};
impl DeclarationDraft {
    pub(super) fn check_pipeline_control_access(
        &self,
        control: &super::actions::Control,
    ) -> Result<(), WorkspaceError> {
        use super::actions::Control;
        let (Control::DropNode(node) | Control::Change { node, .. }) = control else {
            return Ok(());
        };
        let affected = self
            .graph
            .downstream(node)
            .map_err(|e| WorkspaceError::Runtime(e.into()))?;
        for (root, stages) in &self.pipeline_groups {
            if affected.contains(root) || stages.iter().any(|id| affected.contains(id)) {
                for id in std::iter::once(root).chain(stages) {
                    if self.graph.node(id).is_some() {
                        self.access.check_effect(id, &self.graph, &self.bindings)?;
                    }
                }
            }
        }
        Ok(())
    }
    pub(super) fn check_change_access(&self, change: &Change) -> Result<(), WorkspaceError> {
        if !self.access.restricted_mode() {
            return Ok(());
        }
        match change {
            Change::Node { names, task, .. } => {
                if task.call().is_some_and(|c| c.interactive()) {
                    return Err(denied("Interactive conversations require user authority."));
                }
                for (name, _) in names {
                    self.access.check_name(name, &self.graph, &self.bindings)?;
                }
            }
            Change::Aliases(aliases) => {
                for (name, _) in aliases {
                    self.access.check_name(name, &self.graph, &self.bindings)?;
                }
            }
            Change::Provider(product) | Change::DefaultProvider { product, .. } => {
                if self
                    .providers
                    .catalogue()
                    .provider(product.description().name())
                    .is_some()
                {
                    return Err(denied(
                        "Replacing an existing provider requires user authority. Add a new provider name.",
                    ));
                }
            }
            Change::EnvironmentProvider(publication) => {
                if self
                    .environments
                    .inspect(&publication.environment)
                    .is_some_and(|e| e.imports().contains_key(&publication.alias))
                {
                    return Err(denied(
                        "Replacing an existing provider requires user authority. Add a new provider name.",
                    ));
                }
                for name in self.environments.names() {
                    let old = self.environments.inspect(name).expect("registered");
                    let new = publication.registry.inspect(name).ok_or_else(|| {
                        denied("Removing an environment requires user authority.")
                    })?;
                    for (alias, import) in old.imports() {
                        if new.imports().get(alias).is_none_or(|next| {
                            import.declaration() != next.declaration()
                                || import.source() != next.source()
                                || import.target() != next.target()
                                || import.endpoint() != next.endpoint()
                                || import.credential_refs() != next.credential_refs()
                        }) {
                            return Err(denied(
                                "Replacing an existing provider or its shared source requires user authority. Add a new provider name and distinct source.",
                            ));
                        }
                    }
                }
            }
            // Shared template/type/iterator/view registries already reject duplicate names.
            Change::Define { .. } | Change::Types { .. } => (),
        }
        Ok(())
    }
}
impl Workspace {
    pub(crate) fn check_view_access(
        &self,
        handle: &crate::views::Handle,
        scope: &WriteScope,
    ) -> Result<(), WorkspaceError> {
        let affected = self
            .views
            .affected(handle)
            .map_err(|_| denied("View reference is unavailable."))?;
        for node in affected {
            scope.check_effect(&node, self.runtime.graph(), &self.bindings)?;
        }
        Ok(())
    }
    pub(crate) fn check_control_access(
        &self,
        control: &PreparedControl,
        scope: &WriteScope,
    ) -> Result<(), WorkspaceError> {
        use super::actions::Control;
        scope.check_control(&control.operation, self.runtime.graph(), &self.bindings)?;
        let affected = match &control.operation {
            Control::Cancel(node) | Control::Timeout(node, _) => {
                self.runtime.cancellation_scope(node)
            }
            Control::DropNode(node) | Control::Change { node, .. } => {
                self.runtime.invalidation_scope(node)
            }
            _ => vec![],
        };
        for affected in affected {
            scope.check_effect(&affected, self.runtime.graph(), &self.bindings)?;
            for parent in self.views.affected_node(&affected) {
                scope.check_effect(&parent, self.runtime.graph(), &self.bindings)?;
            }
        }
        Ok(())
    }
}
