//! Native capture of an original, joined source receipt; copied values cannot construct it.
use super::*;
use crate::storage::{StoreError, StoreWorker};

#[derive(Clone, Debug)]
pub(super) struct CapturedRecording {
    recorded: Arc<crate::streams::RecordedOutput>,
    readers: crate::provider_reads::Readers,
    epoch: String,
}
impl BoundQuery {
    pub(crate) fn capture_recorded(
        &mut self,
        result: Result<Arc<crate::streams::RecordedOutput>, &'static str>,
        readers: &crate::provider_reads::Readers,
    ) {
        self.captured = Some(result.map_err(Failure::Selection).and_then(|recorded| {
            let snapshot = recorded
                .output
                .snapshot
                .as_ref()
                .ok_or(Failure::Uncaptured)?;
            self.read_policy = snapshot.provenance.policy().clone();
            if readers.blocks_policy(&self.read_policy) {
                return Err(Failure::Selection(
                    "Source recording read authority is unavailable",
                ));
            }
            Ok(Capture::Recorded(CapturedRecording {
                recorded,
                readers: readers.clone(),
                epoch: readers.epoch(),
            }))
        }));
    }
}
impl CapturedRecording {
    pub(super) fn policy(&self) -> wes_core::flow::FlowPolicy {
        self.recorded
            .output
            .snapshot
            .as_ref()
            .expect("captured snapshot")
            .provenance
            .policy()
            .clone()
    }
    fn check(&self) -> Result<(), StoreError> {
        let policy = self.policy();
        if !policy.provider_reads().is_empty() && self.epoch != self.readers.epoch() {
            return Err(StoreError::ProviderReadChanged);
        }
        if self.readers.blocks_policy(&policy) {
            return Err(StoreError::ProviderReadUnavailable);
        }
        Ok(())
    }
    pub(super) fn execute(
        self,
        worker: Option<StoreWorker>,
        token: CancellationToken,
    ) -> crate::driver::ExecutionFuture {
        Box::pin(async move {
            let cancelled = || {
                Outcome::Cancelled(
                    RuntimeCode::Cancelled.error("Source recording read was cancelled", None),
                )
            };
            if token.is_cancelled() {
                return cancelled().into();
            }
            if let Err(error) = self.check() {
                return Outcome::Failed(error.read_error("Source recording read refused")).into();
            }
            let output = match worker {
                Some(worker) => {
                    worker
                        .acknowledge_acquired(self.recorded.output.clone())
                        .await
                }
                None => Err(StoreError::Closed),
            };
            if token.is_cancelled() {
                return cancelled().into();
            }
            match self.check().and(output) {
                Ok(output) => {
                    let mut report: crate::driver::ExecutionReport = Outcome::Stored(output).into();
                    report
                        .holds
                        .push(crate::driver::ExecutionHold::retain(self.recorded));
                    report
                }
                Err(error) => {
                    Outcome::Failed(error.read_error("Source recording read refused")).into()
                }
            }
        })
    }
}
