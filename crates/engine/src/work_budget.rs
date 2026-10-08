//! Shared monotonic execution work. Dropping scratch or a VM cannot refund it.
use std::sync::{
    Arc,
    atomic::{AtomicU64, Ordering},
};

#[derive(Clone, Debug)]
pub(crate) struct WorkCounter {
    limit: u64,
    counters: Arc<Counters>,
}
#[derive(Debug)]
struct Counters {
    allowance: AtomicU64,
    used: AtomicU64,
}
impl WorkCounter {
    pub(crate) fn earned(limit: u64, startup: u64) -> Self {
        Self {
            limit,
            counters: Arc::new(Counters {
                allowance: AtomicU64::new(startup.min(limit)),
                used: AtomicU64::new(0),
            }),
        }
    }
    pub(crate) fn charge(&self, amount: u64) -> Result<(), ()> {
        let allowance = self.counters.allowance.load(Ordering::Acquire);
        self.counters
            .used
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |used| {
                used.checked_add(amount).filter(|n| *n <= allowance)
            })
            .map(|_| ())
            .map_err(|_| ())
    }
    /// Only committed input earns allowance. Releasing memory and re-reading
    /// speculative input never call this operation.
    pub(crate) fn grant(&self, amount: u64) {
        let _ = self
            .counters
            .allowance
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |old| {
                Some(old.saturating_add(amount).min(self.limit))
            });
    }
    pub(crate) fn remaining(&self) -> u64 {
        let used = self.used();
        self.allowance() - used
    }
    pub(crate) fn allowance(&self) -> u64 {
        self.counters.allowance.load(Ordering::Acquire)
    }
    pub(crate) fn used(&self) -> u64 {
        self.counters.used.load(Ordering::Acquire)
    }
    pub(crate) fn limit(&self) -> u64 {
        self.limit
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn concurrent_owners_share_monotonic_work_and_cannot_overdraw_or_refund() {
        let work = WorkCounter::earned(1000, 100);
        std::thread::scope(|scope| {
            for _ in 0..8 {
                let owner = work.clone();
                scope.spawn(move || while owner.charge(1).is_ok() {});
            }
        });
        assert_eq!(work.used(), 100);
        assert_eq!(work.remaining(), 0);
        work.grant(u64::MAX);
        assert_eq!(work.allowance(), 1000);
        assert!(work.charge(u64::MAX).is_err());
        assert_eq!(work.used(), 100);
        work.charge(900).unwrap();
        assert_eq!(work.remaining(), 0);
        drop(work.clone());
        assert_eq!(work.used(), 1000);
    }
}
