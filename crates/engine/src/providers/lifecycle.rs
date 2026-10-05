//! Shared run-specific recovery receipts for finite and interactive calls. Streams only authorize.
use super::*;
use crate::calls::Calling;

pub(super) async fn begin(
    journal: &Option<CallJournal>,
    bound: &BoundCall,
    run: &Run,
    cancellation: &CancellationToken,
) -> Result<Option<Calling>, Box<ExecutionReport>> {
    if let Some(journal) = journal {
        let Some(admission) = &bound.context.admission else {
            return Err(Box::new(recording_denied(cancellation)));
        };
        let Some(at) = wall_time() else {
            return Err(Box::new(recording_denied(cancellation)));
        };
        let capability = format!(
            "{} {}",
            bound.invocation.provider.name(),
            bound.invocation.capability.path.join(" ")
        );
        journal
            .calling(admission, run, capability, bound.traits().repeatable, at)
            .await
            .map(Some)
            .map_err(|_| Box::new(recording_denied(cancellation)))
    } else if bound.context.admission.is_some() {
        Err(Box::new(recording_denied(cancellation)))
    } else {
        Ok(None)
    }
}
pub(super) async fn finish(
    journal: Option<CallJournal>,
    calling: Option<Calling>,
    mut report: ExecutionReport,
) -> ExecutionReport {
    if let (Some(journal), Some(calling)) = (journal, calling)
        && journal
            .called(calling, matches!(report.outcome, Outcome::Produced(_)))
            .await
            .is_err()
    {
        report.notices.push(RuntimeCode::RecordingFailed.error("The local call finished, but its recovery completion could not be acknowledged. Recovery may still list this attempt as interrupted.", None));
    }
    report
}
