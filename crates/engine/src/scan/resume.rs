//! Explicit captured continuation. Worker-entry selection never invokes the original producer.
use super::{
    Runner,
    ledger::MemoryPool,
    owned::{OwnedAnalysis, Selection},
};
use crate::{
    calc::{Failure, LocalServices},
    driver::{CancellationToken, ExecutionFuture, progress::Reporter},
    plan::{Input, MetaTask},
    storage::StoreWorker,
    workspace::Workspace,
};
use std::sync::Arc;
use wes_core::{Data, flow::FlowPolicy};
use wes_language::{Diagnostic, Span};
#[derive(Clone)]
pub struct BoundAttempt {
    selection: OwnedAnalysis,
    services: Option<Arc<dyn LocalServices>>,
    span: Span,
    live: bool,
    raise: Option<(super::bounds::RequestedBounds, String)>,
}
impl std::fmt::Debug for BoundAttempt {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("BoundAttempt")
            .field("selection", &self.selection)
            .finish_non_exhaustive()
    }
}
impl BoundAttempt {
    pub(crate) fn bind(
        task: MetaTask,
        services: Option<Arc<dyn LocalServices>>,
        span: Span,
    ) -> Result<Self, Diagnostic> {
        let selection = OwnedAnalysis::bind(&task, span)?;
        let raised = task.spec.command == wes_language::vocabulary::MetaCommand::ScanContinue;
        if task.inputs.keys().any(|key| {
            key != "follow"
                && !(raised
                    && (key == "basis"
                        || super::bounds::RequestedBounds::NAMES.contains(&key.as_str())))
        }) {
            return Err(Diagnostic::error(
                "CAL009",
                span,
                "Resume preserves captured inputs and granted limits; use :scan continuation and :scan continue to review an explicit increase",
            ));
        }
        let live = match task.inputs.get("follow") {
            None => false,
            Some(Input::Literal(value)) => match value.data() {
                Data::Bool(value) => *value,
                _ => {
                    return Err(Diagnostic::error(
                        "CAL009",
                        span,
                        "Resume follow: must be a literal boolean",
                    ));
                }
            },
            _ => {
                return Err(Diagnostic::error(
                    "CAL009",
                    span,
                    "Resume follow: must be a literal boolean",
                ));
            }
        };
        let raise = if raised {
            let Some(Input::Literal(value)) = task.inputs.get("basis") else {
                return Err(Diagnostic::error(
                    "CAL009",
                    span,
                    "Continue requires literal basis: from the current continuation preview",
                ));
            };
            let Data::Text(basis) = value.data() else {
                return Err(Diagnostic::error(
                    "CAL009",
                    span,
                    "Continue basis: must be a literal digest",
                ));
            };
            if basis.len() != 71
                || !basis.starts_with("sha256:")
                || !basis[7..]
                    .bytes()
                    .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
            {
                return Err(Diagnostic::error(
                    "CAL009",
                    span,
                    "Continue basis: must be a canonical sha256 digest",
                ));
            }
            Some((
                super::bounds::RequestedBounds::parse(&task, span)?,
                basis.to_string(),
            ))
        } else {
            None
        };
        Ok(Self {
            raise,
            selection,
            services,
            span,
            live,
        })
    }
    pub(crate) fn command(&self) -> wes_language::vocabulary::MetaCommand {
        if self.raise.is_some() {
            wes_language::vocabulary::MetaCommand::ScanContinue
        } else {
            wes_language::vocabulary::MetaCommand::ScanResume
        }
    }
    pub(crate) fn live(&self) -> bool {
        self.live
    }
    pub(crate) fn policy(&self) -> FlowPolicy {
        self.selection.policy()
    }
    pub(crate) fn capture(&mut self, workspace: &Workspace) {
        self.selection.capture(workspace, Selection::Continue);
    }
    pub(crate) fn execute(
        self,
        run: String,
        pool: MemoryPool,
        storage: Option<StoreWorker>,
        token: CancellationToken,
        progress: Reporter,
        values: crate::driver::lifetime::Reporter,
    ) -> ExecutionFuture {
        Box::pin(async move {
            let fail =
                |message: String| super::bound::failed(Failure::new("CAL004", self.span, message));
            let issued = match self.selection.issued() {
                Ok(run) => run,
                Err(message) => return fail(message).into(),
            };
            let Some(worker) = storage else {
                return fail("Resume requires an owned durable store".into()).into();
            };
            if token.is_cancelled() {
                return super::bound::failed(Failure::cancelled(self.span)).into();
            }
            let captured = match if self.raise.is_some() {
                worker.dataset_analysis_read(issued).await
            } else {
                worker.dataset_continuation(issued).await
            } {
                Ok(c) => c,
                Err(e) => return fail(e.to_string()).into(),
            };
            let span = self.span;
            let cancel = token.clone();
            let prepared = tokio::task::spawn_blocking(move || {
                if let Some((requested, basis)) = self.raise {
                    let totals = requested.requested(captured.checkpoint.budget.totals);
                    return Runner::prepare_continue(
                        captured,
                        totals,
                        &basis,
                        self.live,
                        run,
                        &pool,
                        self.services,
                        cancel,
                        span,
                    );
                }
                Runner::prepare_resume(
                    captured.source,
                    self.live,
                    captured.reference,
                    captured.checkpoint,
                    run,
                    &pool,
                    self.services,
                    cancel,
                    span,
                )
            })
            .await;
            let prepared = match prepared {
                Ok(Ok(p)) => p,
                Ok(Err(e)) => return super::bound::failed(e).into(),
                Err(_) => return fail("Resume preparation failed".into()).into(),
            };
            if token.is_cancelled() {
                return super::bound::failed(Failure::cancelled(span)).into();
            }
            // Always join the admitted store operation; a lost receipt never causes retry.
            let admission = match worker.dataset_resume(prepared.request()).await {
                Ok(admission) => admission,
                Err(e) => return fail(e.to_string()).into(),
            };
            let mut runner = match prepared.acknowledge(admission.reference) {
                Ok(runner) => runner,
                Err(e) => return super::bound::failed(e).into(),
            };
            runner.retain_writer(admission.lease);
            if self.live {
                let initial = runner.current_prefix();
                let admitted = match initial {
                    Ok(value) => values.publish(value).await,
                    Err(_) => false,
                };
                if !admitted {
                    runner.refuse_scan(Failure::new(
                        "CAL004",
                        span,
                        "initial continued analysis prefix was not acknowledged",
                    ));
                }
            }
            super::bound::run_admitted(runner, Some(worker), token, progress, span).await
        })
    }
}
