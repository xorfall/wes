//! Readiness is an adapter acknowledgement, not a first event or a recording setup.
use super::*;
use std::sync::atomic::{AtomicUsize, Ordering};

pub const MAX_READY_WAIT: Duration = Duration::from_secs(60);
static WAITERS: AtomicUsize = AtomicUsize::new(0);
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
pub enum ReadyError {
    #[error("readiness wait exceeds its supported budget")]
    Invalid,
    #[error("readiness wait capacity exhausted")]
    Capacity,
    #[error("source run or connection changed")]
    Changed,
    #[error("source is no longer ready")]
    Closed,
    #[error("source did not acknowledge readiness before the deadline")]
    Timeout,
    #[error("readiness wait was cancelled")]
    Cancelled,
}
struct WaitSlot;
impl WaitSlot {
    fn acquire() -> Result<Self, ReadyError> {
        WAITERS
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |n| {
                (n < wes_budgets::get("execution.waiters") as usize).then_some(n + 1)
            })
            .map(|_| Self)
            .map_err(|_| ReadyError::Capacity)
    }
}
impl Drop for WaitSlot {
    fn drop(&mut self) {
        WAITERS.fetch_sub(1, Ordering::AcqRel);
    }
}
/// Weak, process-local proof for one original run and connection. Keep and join StreamTask separately.
#[derive(Clone)]
pub struct ReadyReceipt {
    owner: Weak<Shared>,
    run: Run,
    epoch: uuid::Uuid,
}
impl std::fmt::Debug for ReadyReceipt {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ReadyReceipt")
            .field("run", &self.run)
            .finish_non_exhaustive()
    }
}
impl ReadyReceipt {
    pub fn run(&self) -> &Run {
        &self.run
    }
    /// Serialize local admission with source closure. The callback must only admit local work:
    /// no I/O, blocking, await or callbacks into this source. This never starts or retains a source.
    pub fn admit<R>(&self, expected: &Run, admission: impl FnOnce() -> R) -> Result<R, ReadyError> {
        if expected != &self.run {
            return Err(ReadyError::Changed);
        }
        let owner = self.owner.upgrade().ok_or(ReadyError::Closed)?;
        let state = owner.state();
        if state.connection_epoch != self.epoch {
            return Err(ReadyError::Changed);
        }
        if state.finished || state.phase != Phase::Open || owner.cancellation.is_cancelled() {
            return Err(ReadyError::Closed);
        }
        Ok(admission())
    }
}
impl StreamHandle {
    /// Wait for the adapter's real handshake/listener acknowledgement, even with an empty window.
    /// Timeouts cancel only this wait. No source start, retry or recording preparation occurs.
    pub async fn wait_ready(
        &self,
        expected: &Run,
        budget: Duration,
        caller: CancellationToken,
    ) -> Result<ReadyReceipt, ReadyError> {
        if budget.is_zero() || budget > MAX_READY_WAIT {
            return Err(ReadyError::Invalid);
        }
        if &self.snapshot().run != expected {
            return Err(ReadyError::Changed);
        }
        let _slot = WaitSlot::acquire()?;
        let owner = self.owner.upgrade().ok_or(ReadyError::Closed)?;
        let epoch = owner.state().connection_epoch;
        drop(owner);
        let until = tokio::time::Instant::now() + budget;
        let mut changes = self.subscribe();
        loop {
            if caller.is_cancelled() {
                return Err(ReadyError::Cancelled);
            }
            let owner = self.owner.upgrade().ok_or(ReadyError::Closed)?;
            let open = {
                let state = owner.state();
                if state.connection_epoch != epoch {
                    return Err(ReadyError::Changed);
                }
                if state.finished || owner.cancellation.is_cancelled() {
                    return Err(ReadyError::Closed);
                }
                state.phase == Phase::Open
            };
            drop(owner);
            if open {
                return Ok(ReadyReceipt {
                    owner: self.owner.clone(),
                    run: expected.clone(),
                    epoch,
                });
            }
            tokio::select! { biased;
                () = caller.cancelled() => return Err(ReadyError::Cancelled),
                () = self.cancellation.cancelled() => return Err(ReadyError::Closed),
                _ = tokio::time::sleep_until(until) => return Err(ReadyError::Timeout),
                change = changes.changed() => if change.is_err() { return Err(ReadyError::Closed); },
            }
        }
    }
}
