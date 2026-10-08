//! Captured interactive providers share finite call admission and run-specific recovery receipts.
use super::lifecycle::{begin, finish};
use super::*;
use crate::conversations;

pub(super) fn execute(
    journal: Option<CallJournal>,
    ticket: RunTicket<BoundCall>,
    cancellation: CancellationToken,
) -> InteractiveExecutionFuture {
    let policy = ticket.payload.failure_policy(&ticket.inputs);
    let work = async move {
        let validation_cancel = cancellation.clone();
        let prepared = tokio::task::spawn_blocking(move || {
            let bound = ticket.payload;
            if !bound.interactive() || bound.streaming() || bound.provider.conversations.is_none() {
                return Err(InvocationError::Failed(RuntimeCode::ExecutionFailed.error(
                    "The call has no compatible conversation execution port.",
                    None,
                )));
            }
            prepare(&bound.invocation, &ticket.inputs, &validation_cancel)
                .map(|(arguments, carried)| (bound, ticket.run, arguments, carried))
        })
        .await;
        let (bound, run, arguments, carried) = match prepared {
            Ok(Ok(prepared)) => prepared,
            Ok(Err(error)) => return Err(error.outcome().into()),
            Err(_) => {
                return Err(Outcome::Failed(
                    RuntimeCode::ExecutionFailed
                        .error("Input validation terminated unexpectedly.", None),
                )
                .into());
            }
        };
        if cancellation.is_cancelled() {
            return Err(InvocationError::Cancelled.outcome().into());
        }
        let calling = begin(&journal, &bound, &run, &cancellation)
            .await
            .map_err(|report| *report)?;
        if cancellation.is_cancelled() {
            return Err(finish(
                journal,
                calling,
                InvocationError::Cancelled.outcome().into(),
            )
            .await);
        }
        if let Err(reason) = bound.require_dispatch(&carried) {
            return Err(finish(journal, calling, environment_denied(reason).into()).await);
        }
        let carried = bound.output_provenance(&carried);
        // Conversation bytes precede value publication and cannot yet carry a policy label.
        // Refuse this unsupported channel before invoking a confidential-output provider.
        if carried.policy().is_confidential() {
            return Err(finish(journal, calling, Outcome::Failed(RuntimeCode::ExecutionFailed.error(
                "ENV021: confidential conversations are unsupported; use a finite call with an appropriate output policy.", None)).into()).await);
        }
        let spawned = conversations::spawn(
            Call {
                authority: bound.context.authority.clone(),
                run,
                capability: bound.invocation.capability,
                arguments,
            },
            bound
                .provider
                .conversations
                .as_ref()
                .expect("validated conversation port")
                .clone(),
            conversations::output_bytes(),
            cancellation,
        );
        let (handle, task) = match spawned {
            Ok(started) => started,
            Err(_) => {
                return Err(finish(
                    journal,
                    calling,
                    Outcome::Failed(RuntimeCode::ExecutionFailed.error(
                        "The conversation metadata or output limits are invalid.",
                        None,
                    ))
                    .into(),
                )
                .await);
            }
        };
        let completion = Box::pin(async move {
            let outcome = match task.join().await {
                Ok(Ok(value)) => {
                    let provenance = value
                        .provenance()
                        .inheriting(&carried)
                        .cautioned(bound.invocation.cautions);
                    Outcome::Produced(value.with_provenance(provenance))
                }
                Ok(Err(error)) => error.outcome(),
                Err(_) => Outcome::Failed(RuntimeCode::ExecutionFailed.error(
                    "The conversation lifetime owner terminated unexpectedly.",
                    None,
                )),
            };
            finish(
                journal,
                calling,
                outcome.with_policy(carried.policy()).into(),
            )
            .await
        });
        Ok(InteractiveExecution { handle, completion })
    };
    Box::pin(async move {
        work.await.map_err(|mut report: ExecutionReport| {
            report.outcome = report.outcome.with_policy(&policy);
            report.notices = report
                .notices
                .into_iter()
                .map(|e| e.with_policy(&policy))
                .collect();
            report
        })
    })
}
