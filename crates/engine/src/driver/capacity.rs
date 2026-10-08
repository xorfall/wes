//! Shared slot ownership and coalesced observations; counts describe leases, not graph states.
use super::DriverError;
use std::{num::NonZeroUsize, sync::Arc};
use tokio::sync::{AcquireError, OwnedSemaphorePermit, Semaphore, TryAcquireError, watch};

pub const DEFAULT_MAX_STREAMS: NonZeroUsize = NonZeroUsize::new(32).unwrap();
pub const MAX_STREAMS: usize = 1024;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SlotUsage {
    pub used: usize,
    pub limit: usize,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CapacitySnapshot {
    pub operations: SlotUsage,
    pub streams: SlotUsage,
}
#[derive(Clone)]
pub struct ExecutionCapacity {
    pub(crate) calls: Pool,
    pub(crate) streams: Pool,
    pub(crate) conversations: Arc<Semaphore>,
    pub(crate) scan_memory: crate::scan::ledger::MemoryPool,
    changes: watch::Sender<CapacitySnapshot>,
}
impl ExecutionCapacity {
    pub fn new(concurrency: NonZeroUsize) -> Result<Self, DriverError> {
        Self::with_stream_limit(
            concurrency,
            NonZeroUsize::new(wes_budgets::get("execution.streams") as usize).unwrap(),
        )
    }
    pub fn with_stream_limit(
        concurrency: NonZeroUsize,
        max_streams: NonZeroUsize,
    ) -> Result<Self, DriverError> {
        if concurrency.get() > Semaphore::MAX_PERMITS {
            return Err(DriverError::InvalidConcurrency);
        }
        if max_streams.get() > MAX_STREAMS {
            return Err(DriverError::InvalidStreamCapacity);
        }
        let (changes, _) = watch::channel(CapacitySnapshot {
            operations: SlotUsage {
                used: 0,
                limit: concurrency.get(),
            },
            streams: SlotUsage {
                used: 0,
                limit: max_streams.get(),
            },
        });
        Ok(Self {
            calls: Pool::new(concurrency.get(), Kind::Operations, changes.clone()),
            streams: Pool::new(max_streams.get(), Kind::Streams, changes.clone()),
            conversations: Arc::new(Semaphore::new(
                wes_budgets::get("execution.conversations") as usize
            )),
            scan_memory: crate::scan::ledger::MemoryPool::new(wes_budgets::get(
                "scan.aggregate.bytes",
            ))
            .expect("positive scan aggregate budget"),
            changes,
        })
    }
    pub fn subscribe(&self) -> watch::Receiver<CapacitySnapshot> {
        self.changes.subscribe()
    }
}
#[derive(Clone, Copy)]
enum Kind {
    Operations,
    Streams,
}
impl Kind {
    fn usage(self, snapshot: &mut CapacitySnapshot) -> &mut SlotUsage {
        match self {
            Self::Operations => &mut snapshot.operations,
            Self::Streams => &mut snapshot.streams,
        }
    }
}
#[derive(Clone)]
pub(crate) struct Pool {
    semaphore: Arc<Semaphore>,
    kind: Kind,
    changes: watch::Sender<CapacitySnapshot>,
}
impl Pool {
    fn new(limit: usize, kind: Kind, changes: watch::Sender<CapacitySnapshot>) -> Self {
        Self {
            semaphore: Arc::new(Semaphore::new(limit)),
            kind,
            changes,
        }
    }
    pub async fn acquire_owned(self) -> Result<Permit, AcquireError> {
        let permit = self.semaphore.clone().acquire_owned().await?;
        Ok(self.track(permit))
    }
    pub fn try_acquire_owned(self) -> Result<Permit, TryAcquireError> {
        let permit = self.semaphore.clone().try_acquire_owned()?;
        Ok(self.track(permit))
    }
    fn track(self, permit: OwnedSemaphorePermit) -> Permit {
        self.changes.send_modify(|s| self.kind.usage(s).used += 1);
        Permit {
            permit: Some(permit),
            pool: self,
        }
    }
}
pub(crate) struct Permit {
    permit: Option<OwnedSemaphorePermit>,
    pool: Pool,
}
impl Drop for Permit {
    fn drop(&mut self) {
        self.pool.changes.send_modify(|snapshot| {
            self.pool.kind.usage(snapshot).used -= 1;
            // Release under the observation lock: a successor cannot publish its increment
            // before this decrement. Dropping on cancellation/panic follows the same path.
            drop(self.permit.take());
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn shared_leases_publish_latest_occupancy_and_failed_admission_changes_nothing() {
        let capacity = ExecutionCapacity::with_stream_limit(
            NonZeroUsize::new(2).unwrap(),
            NonZeroUsize::new(1).unwrap(),
        )
        .unwrap();
        let mut observed = capacity.subscribe();
        let other_workspace = capacity.clone();
        let stream = capacity.streams.clone().try_acquire_owned().unwrap();
        let operation = other_workspace.calls.clone().acquire_owned().await.unwrap();
        assert_eq!(observed.borrow_and_update().streams.used, 1);
        assert_eq!(observed.borrow().operations.used, 1);
        assert!(other_workspace.streams.clone().try_acquire_owned().is_err());
        assert!(!observed.has_changed().unwrap());
        drop(operation); // opened stream releases ordinary concurrency
        assert_eq!(observed.borrow().operations.used, 0);
        assert_eq!(observed.borrow().streams.used, 1);
        drop(stream); // physical cleanup owns the stream lease until here
        assert_eq!(observed.borrow_and_update().streams.used, 0);
        assert!(!observed.has_changed().unwrap());
    }
    #[test]
    fn default_and_upper_bound() {
        let capacity = ExecutionCapacity::new(NonZeroUsize::new(4).unwrap()).unwrap();
        assert_eq!(capacity.subscribe().borrow().streams.limit, 32);
        assert!(
            ExecutionCapacity::with_stream_limit(
                NonZeroUsize::new(1).unwrap(),
                NonZeroUsize::new(MAX_STREAMS + 1).unwrap()
            )
            .is_err()
        );
    }
}
