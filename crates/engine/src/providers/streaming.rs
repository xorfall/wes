//! Streaming validation/admission shares the finite boundary without finite recovery attempts.
use super::*;
use crate::streams;

pub(super) fn execute(
    journal: Option<CallJournal>,
    ticket: RunTicket<BoundCall>,
    cancellation: CancellationToken,
) -> StreamExecutionFuture {
    let policy = ticket.payload.failure_policy(&ticket.inputs);
    let work = async move {
        let validation_cancel = cancellation.clone();
        let prepared = tokio::task::spawn_blocking(move || {
            let bound = ticket.payload;
            if !bound.streaming()
                || bound.invocation.interactive
                || bound.provider.streams.is_none()
            {
                return Err(InvocationError::Failed(RuntimeCode::ExecutionFailed.error(
                    "The call has no compatible streaming execution port.",
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
        if let Some(journal) = journal {
            let Some(admission) = &bound.context.admission else {
                return Err(recording_denied(&cancellation));
            };
            // Join accepted writes even if cancellation arrives during interrupted admission repair.
            if journal.authorize(admission, &run).await.is_err() {
                return Err(recording_denied(&cancellation));
            }
        } else if bound.context.admission.is_some() {
            return Err(recording_denied(&cancellation));
        }
        if cancellation.is_cancelled() {
            return Err(InvocationError::Cancelled.outcome().into());
        }
        if let Err(reason) = bound.require_dispatch(&carried) {
            return Err(environment_denied(reason).into());
        }
        let attribution = bound
            .output_provenance(&carried)
            .cautioned(bound.invocation.cautions.iter().cloned());
        streams::spawn_with_delivery(
            Call {
                authority: bound.context.authority.clone(),
                run,
                capability: bound.invocation.capability,
                arguments,
            },
            bound
                .provider
                .streams
                .as_ref()
                .expect("validated stream port")
                .clone(),
            attribution,
            streams::Limits::default(),
            cancellation,
            bound.context.stream_budget,
        )
        .map_err(|_| {
            Outcome::Failed(
                RuntimeCode::ExecutionFailed
                    .error("The stream metadata or window limits are invalid.", None),
            )
            .into()
        })
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
