//! Attempt-wide accounting. Releasing record scratch never replenishes work.
use std::sync::{Arc, Mutex};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Dimension {
    Work,
    InputBytes,
    InputRecords,
    HeldMemory,
    OutputBytes,
    OutputRecords,
    AggregateMemory,
    RecordWork,
    RecordMemory,
    RecordOutputs,
}
impl Dimension {
    pub const fn name(self) -> &'static str {
        match self {
            Self::Work => "work",
            Self::InputBytes => "input_charge",
            Self::InputRecords => "input_records",
            Self::HeldMemory => "held_charge",
            Self::RecordWork => "record_work",
            Self::RecordMemory => "record_charge",
            Self::RecordOutputs => "record_outputs",
            Self::OutputBytes => "output_charge",
            Self::OutputRecords => "output_records",
            Self::AggregateMemory => "aggregate_charge",
        }
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Refusal {
    pub dimension: Dimension,
    pub limit: u64,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Limits {
    pub work: u64,
    pub input_bytes: u64,
    pub input_records: u64,
    pub memory_bytes: u64,
    pub output_bytes: u64,
    pub output_records: u64,
}
impl Limits {
    pub fn valid(self) -> bool {
        [
            self.work,
            self.input_bytes,
            self.input_records,
            self.memory_bytes,
            self.output_bytes,
            self.output_records,
        ]
        .into_iter()
        .all(|n| n > 0)
    }
}
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Usage {
    pub work: u64,
    pub input_bytes: u64,
    pub input_records: u64,
    /// Conservative logical charge, not process RSS or allocator measurements.
    pub held_bytes: u64,
    pub high_water_bytes: u64,
    pub output_bytes: u64,
    pub output_records: u64,
}

#[derive(Debug)]
struct PoolState {
    limit: u64,
    reserved: u64,
}
/// Application-owned aggregate admission. No process-global private-state owner.
#[derive(Clone, Debug)]
pub struct MemoryPool(Arc<Mutex<PoolState>>);
impl MemoryPool {
    pub fn new(limit: u64) -> Option<Self> {
        (limit > 0).then(|| Self(Arc::new(Mutex::new(PoolState { limit, reserved: 0 }))))
    }
    pub fn reserved(&self) -> Option<u64> {
        self.0.lock().ok().map(|state| state.reserved)
    }
    /// Reserve the whole captured run ceiling before acquisition/callback allocations.
    /// Unused credit belongs to the run until its joined execution owner drops.
    fn reserve(&self, bytes: u64) -> Result<MemoryLease, Refusal> {
        let mut state = self.0.lock().map_err(|_| Refusal {
            dimension: Dimension::AggregateMemory,
            limit: 0,
        })?;
        let reserved = state
            .reserved
            .checked_add(bytes)
            .filter(|n| *n <= state.limit)
            .ok_or(Refusal {
                dimension: Dimension::AggregateMemory,
                limit: state.limit,
            })?;
        state.reserved = reserved;
        Ok(MemoryLease {
            pool: self.clone(),
            bytes,
        })
    }
}
struct MemoryLease {
    pool: MemoryPool,
    bytes: u64,
}
impl Drop for MemoryLease {
    fn drop(&mut self) {
        if let Ok(mut state) = self.pool.0.lock() {
            // Each lease has exactly one owner; a poisoned pool refuses new admission.
            state.reserved = state
                .reserved
                .checked_sub(self.bytes)
                .expect("aggregate memory lease ownership");
        }
    }
}

pub struct Ledger {
    limits: Limits,
    usage: Usage,
    work: crate::work_budget::WorkCounter,
    _lease: MemoryLease,
}
impl Ledger {
    pub fn new(limits: Limits, pool: &MemoryPool) -> Result<Self, Refusal> {
        Self::with_startup(limits, pool, limits.work)
    }
    pub fn with_startup(limits: Limits, pool: &MemoryPool, startup: u64) -> Result<Self, Refusal> {
        if !limits.valid() {
            return Err(Refusal {
                dimension: Dimension::Work,
                limit: limits.work,
            });
        }
        let lease = pool.reserve(limits.memory_bytes)?;
        Ok(Self {
            limits,
            usage: Usage::default(),
            work: crate::work_budget::WorkCounter::earned(limits.work, startup),
            _lease: lease,
        })
    }
    pub fn usage(&self) -> Usage {
        Usage {
            work: self.work.used(),
            ..self.usage
        }
    }
    pub fn limits(&self) -> Limits {
        self.limits
    }
    pub fn remaining_work(&self) -> u64 {
        self.work.remaining()
    }
    pub(crate) fn work_counter(&self) -> crate::work_budget::WorkCounter {
        self.work.clone()
    }
    pub(crate) fn earn(&self, amount: u64) {
        self.work.grant(amount)
    }
    pub fn work_allowance(&self) -> u64 {
        self.work.allowance()
    }
    /// Admission of the next input precedes its callback. Consumption is committed
    /// with the candidate; this check alone grants neither input nor work credit.
    pub fn admit_input(&self, bytes: u64) -> Result<(), Refusal> {
        add(
            self.usage.input_bytes,
            bytes,
            self.limits.input_bytes,
            Dimension::InputBytes,
        )?;
        add(
            self.usage.input_records,
            1,
            self.limits.input_records,
            Dimension::InputRecords,
        )?;
        Ok(())
    }
    pub fn work(&mut self, amount: u64) -> Result<(), Refusal> {
        self.work.charge(amount).map_err(|_| Refusal {
            dimension: Dimension::Work,
            limit: self.limits.work,
        })?;
        Ok(())
    }
    /// Framing/read work is charged before reading. Consumed position and this
    /// byte counter advance together only after the candidate record is accepted.
    pub fn consume(&mut self, bytes: u64) -> Result<(), Refusal> {
        let input_bytes = add(
            self.usage.input_bytes,
            bytes,
            self.limits.input_bytes,
            Dimension::InputBytes,
        )?;
        let input_records = add(
            self.usage.input_records,
            1,
            self.limits.input_records,
            Dimension::InputRecords,
        )?;
        self.usage.input_bytes = input_bytes;
        self.usage.input_records = input_records;
        Ok(())
    }
    /// Admit old state + candidate + scratch + input + cache + pending outputs.
    /// This is held charge; replacing/releasing owners affects only this counter.
    pub fn held(&mut self, bytes: u64) -> Result<(), Refusal> {
        if bytes > self.limits.memory_bytes {
            return Err(Refusal {
                dimension: Dimension::HeldMemory,
                limit: self.limits.memory_bytes,
            });
        }
        self.usage.held_bytes = bytes;
        self.usage.high_water_bytes = self.usage.high_water_bytes.max(bytes);
        Ok(())
    }
    /// Memory-sink retained charges are cumulative. No implicit rolling archive.
    pub fn output(&mut self, records: u64, bytes: u64) -> Result<(), Refusal> {
        let output_bytes = add(
            self.usage.output_bytes,
            bytes,
            self.limits.output_bytes,
            Dimension::OutputBytes,
        )?;
        let output_records = add(
            self.usage.output_records,
            records,
            self.limits.output_records,
            Dimension::OutputRecords,
        )?;
        self.usage.output_bytes = output_bytes;
        self.usage.output_records = output_records;
        Ok(())
    }
    /// Accept one complete transition candidate. State, cursor and sink handoff
    /// use this one boundary; a refusal advances no consumption/output counter.
    /// Finish consumes no additional source record, but obeys the same sink limits.
    pub fn commit(
        &mut self,
        consumed_bytes: Option<u64>,
        output_records: u64,
        output_bytes: u64,
        held_after: u64,
    ) -> Result<(), Refusal> {
        let mut candidate = self.usage;
        if let Some(bytes) = consumed_bytes {
            candidate.input_bytes = add(
                candidate.input_bytes,
                bytes,
                self.limits.input_bytes,
                Dimension::InputBytes,
            )?;
            candidate.input_records = add(
                candidate.input_records,
                1,
                self.limits.input_records,
                Dimension::InputRecords,
            )?;
        }
        candidate.output_bytes = add(
            candidate.output_bytes,
            output_bytes,
            self.limits.output_bytes,
            Dimension::OutputBytes,
        )?;
        candidate.output_records = add(
            candidate.output_records,
            output_records,
            self.limits.output_records,
            Dimension::OutputRecords,
        )?;
        if held_after > self.limits.memory_bytes {
            return Err(Refusal {
                dimension: Dimension::HeldMemory,
                limit: self.limits.memory_bytes,
            });
        }
        candidate.held_bytes = held_after;
        candidate.high_water_bytes = candidate.high_water_bytes.max(held_after);
        self.usage = candidate;
        Ok(())
    }
}
fn add(current: u64, amount: u64, limit: u64, dimension: Dimension) -> Result<u64, Refusal> {
    current
        .checked_add(amount)
        .filter(|n| *n <= limit)
        .ok_or(Refusal { dimension, limit })
}

#[cfg(test)]
mod tests {
    use super::*;
    fn limits() -> Limits {
        Limits {
            work: 100,
            input_bytes: 100,
            input_records: 2,
            memory_bytes: 100,
            output_bytes: 10,
            output_records: 2,
        }
    }
    #[test]
    fn scratch_release_does_not_replenish_work_or_retained_output() {
        let pool = MemoryPool::new(200).unwrap();
        let mut run = Ledger::new(limits(), &pool).unwrap();
        run.work(90).unwrap();
        run.held(100).unwrap();
        run.output(1, 8).unwrap();
        run.held(8).unwrap();
        assert_eq!(run.remaining_work(), 10);
        assert_eq!(run.usage().high_water_bytes, 100);
        assert_eq!(run.work(11).unwrap_err().dimension, Dimension::Work);
        assert_eq!(
            run.output(1, 3).unwrap_err().dimension,
            Dimension::OutputBytes
        );
        assert_eq!(run.usage().output_records, 1);
        assert_eq!(run.usage().output_bytes, 8);
    }
    #[test]
    fn cumulative_input_admission_is_atomic_and_checked_without_overflow() {
        let pool = MemoryPool::new(100).unwrap();
        let mut run = Ledger::new(limits(), &pool).unwrap();
        run.consume(50).unwrap();
        let before = run.usage();
        assert_eq!(
            run.consume(51).unwrap_err().dimension,
            Dimension::InputBytes
        );
        assert_eq!(run.usage(), before);
        run.consume(0).unwrap();
        assert_eq!(
            run.consume(0).unwrap_err().dimension,
            Dimension::InputRecords
        );
        assert_eq!(run.work(u64::MAX).unwrap_err().dimension, Dimension::Work);
        assert_eq!(run.usage().work, 0);
    }
    #[test]
    fn aggregate_credit_is_owned_until_run_drop_and_never_granted_twice() {
        let pool = MemoryPool::new(150).unwrap();
        let run = Ledger::new(limits(), &pool).unwrap();
        assert_eq!(pool.reserved(), Some(100));
        assert!(matches!(
            Ledger::new(limits(), &pool),
            Err(Refusal {
                dimension: Dimension::AggregateMemory,
                ..
            })
        ));
        assert_eq!(pool.reserved(), Some(100));
        drop(run);
        assert_eq!(pool.reserved(), Some(0));
        assert!(Ledger::new(limits(), &pool).is_ok());
        assert_eq!(pool.reserved(), Some(0));
    }
}
