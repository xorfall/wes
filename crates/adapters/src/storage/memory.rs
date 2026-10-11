//! Encoded payload cache. A lease protects availability, not an unbounded cache preference.
use super::*;
use std::{cell::Cell, sync::Arc};
use wes_engine::storage::ValueLease;

struct Entry {
    bytes: Arc<[u8]>,
    used: Cell<u64>,
}
pub(super) struct MemoryValues {
    entries: HashMap<ValueHandle, Entry>,
    bytes: u64,
    budget: NonZeroU64,
    maximum: usize,
    clock: Cell<u64>,
    evicted: VecDeque<ValueHandle>,
}
impl MemoryValues {
    pub(super) fn new(budget: NonZeroU64, maximum: usize) -> Self {
        Self {
            entries: HashMap::new(),
            bytes: 0,
            budget,
            maximum,
            clock: Cell::new(0),
            evicted: VecDeque::new(),
        }
    }
    fn touch(&self, entry: &Entry) {
        let next = self.clock.get().saturating_add(1);
        self.clock.set(next);
        entry.used.set(next);
    }
    pub(super) fn store(
        &mut self,
        value: &Value,
        limits: Limits,
    ) -> Result<ValueHandle, StoreError> {
        if value.shape().contains_meta() || !value.data().is_storable_snapshot() {
            return Err(StoreError::NonMaterialized);
        }
        let limit = Limits {
            bytes: limits
                .bytes
                .min(usize::try_from(self.budget.get()).unwrap_or(usize::MAX)),
            ..limits
        };
        let bytes = codec::encode_value(value, limit).map_err(|error| match error {
            codec::CodecError::Bytes => StoreError::Limit("encoded memory result"),
            codec::CodecError::Work => StoreError::Limit("encoded memory result work"),
            error => StoreError::backend("encoding a memory result", error),
        })?;
        let size = bytes.len() as u64;
        if size > self.budget.get() || self.maximum == 0 {
            return Err(StoreError::Limit("memory results"));
        }
        // Check feasibility before evicting anything: protected allocations cannot be reclaimed.
        let reclaimable = self
            .entries
            .values()
            .filter(|e| Arc::strong_count(&e.bytes) == 1);
        let (count, reclaimable_bytes) = reclaimable.fold((0usize, 0u64), |(n, sum), entry| {
            (
                n.saturating_add(1),
                sum.saturating_add(entry.bytes.len() as u64),
            )
        });
        if self
            .bytes
            .saturating_sub(reclaimable_bytes)
            .saturating_add(size)
            > self.budget.get()
            || self.entries.len().saturating_sub(count) >= self.maximum
        {
            return Err(StoreError::Limit("protected memory results"));
        }
        let mut candidates = self
            .entries
            .iter()
            .filter(|(_, e)| Arc::strong_count(&e.bytes) == 1)
            .map(|(h, e)| (h.clone(), e.bytes.len() as u64, e.used.get()))
            .collect::<Vec<_>>();
        candidates.sort_by(|(a, _, x), (b, _, y)| x.cmp(y).then_with(|| a.cmp(b)));
        let mut used = self.bytes;
        let mut retained = self.entries.len();
        let mut needed = 0;
        for (_, bytes, _) in &candidates {
            if used.saturating_add(size) <= self.budget.get() && retained < self.maximum {
                break;
            }
            used -= bytes;
            retained -= 1;
            needed += 1;
        }
        if self.evicted.len().saturating_add(needed) > self.maximum {
            return Err(StoreError::Limit("pending memory evictions"));
        }
        for (candidate, _, _) in candidates.into_iter().take(needed) {
            self.remove(&candidate);
            self.evicted.push_back(candidate);
        }
        let handle = ValueHandle::fresh();
        let entry = Entry {
            bytes: bytes.into(),
            used: Cell::new(0),
        };
        self.touch(&entry);
        self.bytes += size;
        self.entries.insert(handle.clone(), entry);
        Ok(handle)
    }
    pub(super) fn encoded(&self, handle: &ValueHandle) -> Option<Vec<u8>> {
        let entry = self.entries.get(handle)?;
        self.touch(entry);
        Some(entry.bytes.to_vec())
    }
    pub(super) fn size(&self, handle: &ValueHandle) -> Option<u64> {
        self.entries.get(handle).map(|e| e.bytes.len() as u64)
    }
    pub(super) fn hold(&self, handle: &ValueHandle) -> Option<ValueLease> {
        let entry = self.entries.get(handle)?;
        Some(ValueLease::retain(entry.bytes.clone()))
    }
    pub(super) fn held(&self, handle: &ValueHandle) -> bool {
        self.entries
            .get(handle)
            .is_some_and(|e| Arc::strong_count(&e.bytes) > 1)
    }
    pub(super) fn remove(&mut self, handle: &ValueHandle) -> bool {
        if let Some(entry) = self.entries.remove(handle) {
            self.bytes -= entry.bytes.len() as u64;
            true
        } else {
            false
        }
    }
    pub(super) fn handles(&self) -> impl Iterator<Item = (&ValueHandle, u64)> {
        self.entries.iter().map(|(h, e)| (h, e.bytes.len() as u64))
    }
    pub(super) fn pending_evictions(&self, maximum: usize) -> impl Iterator<Item = &ValueHandle> {
        self.evicted.iter().take(maximum)
    }
    pub(super) fn drain_evictions(&mut self, count: usize) {
        self.evicted.drain(..count);
    }
    pub(super) fn has_evictions(&self) -> bool {
        !self.evicted.is_empty()
    }
}
