//! Destructive application coordination stays outside the session being retired.
//! Plan production, names, values and inspection use the ordinary graph path.
use super::*;
use std::{collections::BTreeMap, sync::Mutex};
use wes_core::Data;
use wes_language::vocabulary::{WorkspaceManagementCommand as Command, workspace_management};
use wes_language::{Expression, Statement};
#[derive(Clone, Debug)]
pub struct WorkspaceDeletePlan {
    pub token: String,
    pub details: Data,
}
pub enum Operation {
    PlanDelete {
        workspace: Option<crate::workspace::WorkspaceName>,
    },
    Delete {
        token: String,
        stop: bool,
        protected: bool,
    },
}
pub struct ManagementRequest {
    pub client: String,
    pub operation: Operation,
    pub reply: oneshot::Sender<Result<Option<WorkspaceDeletePlan>, String>>,
}
#[derive(Default)]
pub(super) struct Requests {
    identities: Mutex<BTreeMap<String, SourceInput>>,
    requests: tokio::sync::Mutex<BTreeMap<String, SubmissionReply>>,
}
impl Requests {
    fn routes(source: &SourcePreparation<ParsedSource>) -> bool {
        fn contains(s: &Statement) -> bool {
            match &s.expression {
                Expression::Call(c) => {
                    c.marker.is_some()
                        && c.path.first().is_some_and(|n| n.text == "workspace")
                        && c.path.get(1).is_some_and(|n| n.text == "delete")
                }
                Expression::Pipeline(stages) | Expression::Sandbox(stages) => {
                    stages.iter().any(contains)
                }
                Expression::Fork(branches) => branches.iter().any(|b| contains(&b.body)),
                _ => false,
            }
        }
        match source {
            SourcePreparation::Immediate { statement, .. } => contains(statement),
            SourcePreparation::Declarations(p) => p.statements().iter().any(contains),
            _ => false,
        }
    }
    pub async fn intercept(
        self: &Arc<Self>,
        handle: &SessionHandle,
        input: &SourceInput,
        source: &SourcePreparation<ParsedSource>,
    ) -> Result<Option<SubmissionReply>, SessionError> {
        if !Self::routes(source) {
            return Ok(None);
        }
        if input.is_cooperative() {
            return Err(SessionError::Authority);
        }
        let statement = match source {
            SourcePreparation::Immediate { statement, .. } => statement.as_ref(),
            SourcePreparation::Declarations(p) if p.statements().len() == 1 => &p.statements()[0],
            _ => {
                return Err(error(
                    "Submit workspace deletion on its own; no mixed script was executed.",
                ));
            }
        };
        let Command::Delete(reference, stop, protected) =
            workspace_management(statement).map_err(|d| error(&d.message))?
        else {
            unreachable!()
        };
        let mut requests = self
            .requests
            .try_lock()
            .map_err(|_| SessionError::AdmissionBusy)?;
        {
            let mut ids = self.identities.lock().expect("management requests");
            if let Some(previous) = ids.get(input.cell()) {
                if !previous.same_request(input) {
                    return Err(SessionError::Conflict);
                }
                return Ok(Some(
                    requests
                        .get(input.cell())
                        .cloned()
                        .unwrap_or(Err(SessionError::AdmissionBusy)),
                ));
            }
            if ids.len() >= 10_000
                || ids.values().map(input_charge).sum::<usize>() + input_charge(input)
                    > 8 * 1024 * 1024
            {
                return Err(SessionError::Capacity);
            }
            ids.insert(input.cell().into(), input.clone());
        }
        requests.insert(input.cell().into(), Err(SessionError::AdmissionBusy));
        drop(requests);
        let owner = self.clone();
        let handle = handle.clone();
        let input = input.clone();
        let span = statement.span;
        let task = tokio::spawn(async move {
            let mut admitted = false;
            let result = async {
                let (reply, receive) = oneshot::channel();
                handle
                    .controls
                    .send(Control::ResolveManagement {
                        input: input.clone(),
                        reference,
                        reply,
                    })
                    .await
                    .map_err(|_| SessionError::Stopped)?;
                let (began, token) = receive.await.map_err(|_| SessionError::Stopped)?;
                admitted = began;
                let token = token?;
                let actions = handle
                    .workspace_management
                    .as_ref()
                    .ok_or_else(|| error("This session has no workspace manager."))?;
                let (reply, receive) = oneshot::channel();
                actions
                    .sender
                    .try_send(super::workspaces::WorkspaceRequest::Management(
                        ManagementRequest {
                            client: input.client().into(),
                            operation: Operation::Delete {
                                token,
                                stop,
                                protected,
                            },
                            reply,
                        },
                    ))
                    .map_err(|_| SessionError::AdmissionBusy)?;
                receive
                    .await
                    .map_err(|_| SessionError::Stopped)?
                    .map_err(SessionError::Management)?;
                Ok(Arc::new(SubmissionResult {
                    receipts: vec![],
                    sandbox: None,
                    cell: input.cell().into(),
                    nodes: vec![],
                    refreshed: vec![],
                    accepted: vec![span],
                    removed: vec![],
                    unbound: vec![],
                    diagnostics: SourceDiagnostics {
                        diagnostics: vec![
                            Diagnostic::error("MET010", span, "Workspace deletion completed.")
                                .with_severity(wes_language::Severity::Info),
                        ],
                        issues: vec![],
                    },
                    recorded: false,
                    restored: false,
                    repeated_run: None,
                }))
            }
            .await;
            // A self-deleted session is gone. A surviving issuer publishes its ordinary command cell.
            let (reply, receive) = oneshot::channel();
            if admitted
                && handle
                    .controls
                    .send(Control::CompleteManagement {
                        cell: input.cell().into(),
                        result: result.clone(),
                        reply,
                    })
                    .await
                    .is_ok()
            {
                let _ = receive.await;
            }
            owner
                .requests
                .lock()
                .await
                .insert(input.cell().into(), result.clone());
            result
        });
        Ok(Some(task.await.map_err(|_| SessionError::Stopped)?))
    }
}
fn input_charge(input: &SourceInput) -> usize {
    input
        .text()
        .len()
        .saturating_mul(6)
        .saturating_add(input.context_charge())
        .saturating_add(1024)
}
fn error(message: &str) -> SessionError {
    SessionError::Management(message.into())
}
