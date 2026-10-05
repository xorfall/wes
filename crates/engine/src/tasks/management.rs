//! Application-owned operations with normal graph results. Executors never own a workspace lock.
use crate::{
    driver::{CancellationToken, ExecutionFuture},
    runtime::{Outcome, RuntimeCode},
    session::{
        WorkspaceActions, WorkspaceRequest,
        management::{ManagementRequest, Operation},
    },
    workspace::WorkspaceName,
};
use tokio::sync::oneshot;
use wes_core::{MetaType, Value};

#[derive(Clone)]
pub struct BoundManagement {
    pub workspace: Option<WorkspaceName>,
    pub(crate) context: Option<(WorkspaceActions, String)>,
}
impl std::fmt::Debug for BoundManagement {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("BoundManagement")
            .field("workspace", &self.workspace)
            .finish_non_exhaustive()
    }
}
impl BoundManagement {
    pub fn execute(self, cancellation: CancellationToken) -> ExecutionFuture {
        Box::pin(async move {
            let failure =
                |message| Outcome::Failed(RuntimeCode::ExecutionFailed.error(message, None)).into();
            let Some((actions, client)) = self.context else {
                return failure("Workspace planning requires an authorized user session.");
            };
            if cancellation.is_cancelled() {
                return failure("Workspace planning was cancelled.");
            }
            let (reply, receive) = oneshot::channel();
            if actions
                .sender
                .try_send(WorkspaceRequest::Management(ManagementRequest {
                    client,
                    operation: Operation::PlanDelete {
                        workspace: self.workspace,
                    },
                    reply,
                }))
                .is_err()
            {
                return failure("Workspace manager is busy or unavailable.");
            }
            match receive.await {
                Ok(Ok(Some(plan))) => Outcome::Produced(Value::management(
                    MetaType::WorkspaceDeletePlan,
                    plan.details,
                    plan.token,
                ))
                .into(),
                Ok(Err(message)) => {
                    Outcome::Failed(RuntimeCode::ExecutionFailed.error(message, None)).into()
                }
                _ => failure("Workspace manager did not return a plan."),
            }
        })
    }
}
