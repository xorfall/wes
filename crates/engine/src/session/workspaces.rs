//! Source-level workspace actions retain ordinary session identities and checked meta semantics.
//! The composition root owns filesystem access and candidate-session switching.
use super::{Actor, SessionError, SubmissionResult};
use crate::{
    plan::Plan,
    workspace::{PreparedMeta, WorkspaceName},
};
use thiserror::Error;
use tokio::sync::{mpsc, oneshot};
use wes_language::{Diagnostic, Span, vocabulary::MetaCommand};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WorkspaceOperation {
    Save,
    Load,
}
#[derive(Clone, Copy, Debug, Error)]
pub enum WorkspaceActionError {
    #[error("another live workspace owns this name; save to a different name")]
    LiveName,
    #[error("use a workspace split to open another name from this pane")]
    BoundLoad,
    #[error("the limit of 16 live workspaces was reached; restart to release hidden sessions")]
    Capacity,
    #[error("workspace storage is unavailable")]
    Unavailable,
    #[error("there is no saved workspace with this name")]
    Missing,
    #[error("saved workspace data is invalid or inaccessible")]
    Storage,
    #[error("the name may have been replaced, but its durability was not confirmed")]
    Published,
    #[error("the current workspace could not establish a complete checkpoint")]
    Checkpoint,
    #[error("the saved workspace could not be reconstructed; the current workspace remains open")]
    Restore,
    #[error("the application is shutting down")]
    Stopped,
}
#[derive(Clone)]
pub struct WorkspaceActions {
    pub(crate) sender: mpsc::Sender<WorkspaceRequest>,
}
pub enum WorkspaceRequest {
    Action(WorkspaceAction),
    Management(super::management::ManagementRequest),
}
pub struct WorkspaceAction {
    operation: WorkspaceOperation,
    name: WorkspaceName,
    reply: oneshot::Sender<Result<bool, WorkspaceActionError>>,
}
impl WorkspaceActions {
    pub fn channel() -> (Self, mpsc::Receiver<WorkspaceRequest>) {
        let (sender, receiver) = mpsc::channel(1);
        (Self { sender }, receiver)
    }
}
impl WorkspaceAction {
    pub fn operation(&self) -> WorkspaceOperation {
        self.operation
    }
    pub fn name(&self) -> &WorkspaceName {
        &self.name
    }
    pub fn complete_save(self, result: Result<bool, WorkspaceActionError>) {
        let _ = self.reply.send(result);
    }
    /// Acknowledges the concrete save/switch outcome, never an intention to perform it later.
    pub fn complete(self, result: Result<(), WorkspaceActionError>) {
        let _ = self.reply.send(result.map(|()| false));
    }
}
impl Actor {
    pub(super) fn workspace_action(
        &mut self,
        meta: PreparedMeta,
        mut result: SubmissionResult,
        span: Span,
    ) {
        let Plan::Action { task, .. } = meta.plan() else {
            unreachable!("checked workspace action")
        };
        result.diagnostics.diagnostics = meta.diagnostics().to_vec();
        let name = task
            .tail
            .first()
            .and_then(|name| WorkspaceName::new(name.clone()).ok());
        let Some(name) = name else {
            result.diagnostics.diagnostics.push(Diagnostic::error(
                "MET011",
                span,
                "invalid workspace name",
            ));
            self.finish_immediate(result);
            return;
        };
        let Some(actions) = &self.workspace_actions else {
            result.diagnostics.diagnostics.push(Diagnostic::error(
                "MET011",
                span,
                "this session has no workspace manager",
            ));
            self.finish_immediate(result);
            return;
        };
        if !self.workspace_waiting.is_empty() {
            result.diagnostics.diagnostics.push(Diagnostic::error(
                "MET011",
                span,
                "another workspace action is in progress",
            ));
            self.finish_immediate(result);
            return;
        }
        let operation = match meta.command() {
            MetaCommand::Save => WorkspaceOperation::Save,
            MetaCommand::Load => WorkspaceOperation::Load,
            _ => unreachable!("checked workspace action"),
        };
        if !self
            .cells
            .reserve(&result.cell, super::identity::charge(&result) + 4096)
        {
            self.finish_cell(&result.cell, Err(SessionError::Capacity));
            return;
        }
        let (reply, receive) = oneshot::channel();
        let written_name = name.as_str().to_owned();
        if actions
            .sender
            .try_send(WorkspaceRequest::Action(WorkspaceAction {
                operation,
                name,
                reply,
            }))
            .is_err()
        {
            result.diagnostics.diagnostics.push(Diagnostic::error(
                "MET011",
                span,
                "workspace manager is unavailable or busy",
            ));
            self.finish_immediate(result);
            return;
        }
        self.workspace_waiting.spawn(async move {
            match receive
                .await
                .unwrap_or(Err(WorkspaceActionError::Unavailable))
            {
                Ok(replaced) => {
                    result.accepted.push(span);
                    result.diagnostics.diagnostics.push(
                        Diagnostic::error(
                            "MET010",
                            span,
                            format!(
                                "{} workspace '{written_name}'",
                                match operation {
                                    WorkspaceOperation::Save if replaced => "replaced saved",
                                    WorkspaceOperation::Save => "saved",
                                    WorkspaceOperation::Load => "opened",
                                }
                            ),
                        )
                        .with_severity(wes_language::Severity::Info),
                    );
                }
                Err(error) => result.diagnostics.diagnostics.push(Diagnostic::error(
                    "MET011",
                    span,
                    error.to_string(),
                )),
            }
            (result.cell.clone(), Ok(std::sync::Arc::new(result)))
        });
    }
}
