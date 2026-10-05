//! Cooperative work authority. Independent of transport and model vendors.
use crate::{
    bindings::Bindings,
    graph::{DependencyGraph, NodeId},
    tasks::BoundTask,
    workspace::{WorkspaceError, actions::Control},
};
use std::collections::BTreeSet;
use wes_language::{Expression, Statement, vocabulary::MetaCommand};

/// Only trusted session controls issue scopes. None denotes the local user's authority.
#[derive(Clone, Debug, Default)]
pub(crate) struct WriteScope(Option<(BTreeSet<NodeId>, BTreeSet<String>)>);
impl WriteScope {
    pub(crate) fn restricted(
        nodes: impl IntoIterator<Item = NodeId>,
        names: impl IntoIterator<Item = String>,
    ) -> Self {
        Self(Some((
            nodes.into_iter().collect(),
            names.into_iter().collect(),
        )))
    }
    pub(crate) fn restricted_mode(&self) -> bool {
        self.0.is_some()
    }
    pub(crate) fn include(&mut self, node: NodeId) {
        if let Some((nodes, _)) = &mut self.0 {
            nodes.insert(node);
        }
    }
    pub(crate) fn include_name(&mut self, name: String) {
        if let Some((_, names)) = &mut self.0 {
            names.insert(name);
        }
    }
    pub(crate) fn check_effect(
        &self,
        node: &NodeId,
        graph: &DependencyGraph<BoundTask>,
        bindings: &Bindings,
    ) -> Result<(), WorkspaceError> {
        if let Some((allowed_nodes, allowed_names)) = &self.0 {
            let affected = graph
                .downstream(node)
                .map_err(|e| WorkspaceError::Runtime(e.into()))?;
            if affected.iter().any(|node| !allowed_nodes.contains(node)) {
                return Err(denied(if allowed_nodes.contains(node) {
                    "This operation affects protected work downstream of the target. Create a new branch or ask the user to grant change scope for the affected work through wes's host controls."
                } else {
                    "The target is protected work outside this actor's change scope. Create a new branch or ask the user to grant change scope through wes's host controls."
                }));
            }
            if bindings.names().iter().any(|(name, output)| {
                affected.contains(&output.node) && !allowed_names.contains(name)
            }) {
                return Err(denied(
                    "This operation affects a protected result name or alias. Use a new branch with a new name, or ask the user to grant change scope for the affected work through wes's host controls.",
                ));
            }
        }
        Ok(())
    }
    pub(crate) fn check_name(
        &self,
        name: &str,
        graph: &DependencyGraph<BoundTask>,
        bindings: &Bindings,
    ) -> Result<(), WorkspaceError> {
        if let Some(output) = bindings.resolve(name, graph) {
            if self
                .0
                .as_ref()
                .is_some_and(|(_, names)| !names.contains(name))
            {
                return Err(denied(
                    "This result name belongs to protected work. Use a new name or ask the user to grant change scope through wes's host controls.",
                ));
            }
            self.check_effect(&output.node, graph, bindings)?;
        }
        Ok(())
    }
    pub(crate) fn check_control(
        &self,
        operation: &Control,
        graph: &DependencyGraph<BoundTask>,
        bindings: &Bindings,
    ) -> Result<(), WorkspaceError> {
        if !self.restricted_mode() {
            return Ok(());
        }
        match operation {
            Control::Policy(None, _) => Err(denied(
                "Workspace defaults are shared. Select a node or use execute reactive:true for newly created work.",
            )),
            Control::DropName(name) => self.check_name(name, graph, bindings),
            Control::Refresh(node)
            | Control::RefreshDownstream(node, _)
            | Control::Cancel(node)
            | Control::Timeout(node, _)
            | Control::Policy(Some(node), _)
            | Control::DropNode(node)
            | Control::Change { node, .. } => self.check_effect(node, graph, bindings),
        }
    }
}
pub(crate) fn denied(message: &'static str) -> WorkspaceError {
    WorkspaceError::Rejected {
        diagnostics: vec![
            wes_language::Diagnostic::error("AUT001", wes_language::Span::at(0), message)
                .with_public_message(message),
        ],
        issues: vec![],
    }
}
/// Discoverability and admission share this classification. Allowed mutations still require
/// target checks after binding, under the session owner; this is not an authority grant.
pub const ENVIRONMENT_ACTIONS: &[&str] = &["plan", "apply", "discard", "use", "clear"];
pub fn cooperative_command(command: MetaCommand) -> bool {
    !matches!(
        command,
        MetaCommand::Save
            | MetaCommand::Load
            | MetaCommand::WorkspacePlan
            | MetaCommand::WorkspaceDelete
    )
}
pub fn admit(parsed: &wes_language::Parsed) -> Result<(), &'static str> {
    if parsed.script.statements.is_empty()
        || parsed
            .script
            .statements
            .iter()
            .map(Statement::stage_count)
            .sum::<usize>()
            > 64
    {
        return Err("Submit 1..64 statements; immediate controls must be submitted separately.");
    }
    for statement in &parsed.script.statements {
        admit_statement(statement)?;
    }
    Ok(())
}
fn admit_statement(statement: &Statement) -> Result<(), &'static str> {
    if statement
        .annotations
        .iter()
        .any(|a| !matches!(a.name.text.as_str(), "trace" | "timeout" | "env"))
    {
        return Err("Conversation annotations require user authority.");
    }
    match &statement.expression {
        Expression::Sandbox(statements) => {
            for statement in statements {
                admit_statement(statement)?;
            }
        }
        Expression::Fork(branches) => {
            for branch in branches {
                admit_statement(&branch.body)?;
            }
        }
        Expression::Pipeline(stages) => {
            for stage in stages {
                admit_statement(stage)?;
            }
        }
        Expression::Call(call) => {
            let head = call.path.first().map(|n| n.text.as_str()).unwrap_or("");
            if head.starts_with('/') {
                return Err("UI commands use the UI control channel.");
            }
            if let Ok(invocation) = wes_language::vocabulary::commands::invocation(call) {
                let command = invocation.spec.command;
                if !cooperative_command(command) {
                    return Err(
                        "Workspace management requires user authority. Use workspace_open to join an existing workspace.",
                    );
                }
                if command == MetaCommand::Env
                    && wes_language::vocabulary::EnvironmentCommand::validate(statement).is_ok()
                    && !call
                        .path
                        .get(1)
                        .is_some_and(|n| ENVIRONMENT_ACTIONS.contains(&n.text.as_str()))
                {
                    return Err(
                        "Environment authority and destructive definition controls require user authority.",
                    );
                }
            }
        }
        _ => (),
    }
    Ok(())
}
