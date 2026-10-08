//! Bounded memory-only private values. This wrapper never spills to its backing store.
use super::*;
use std::collections::BTreeMap;
pub(super) struct PolicyValues<S> {
    backing: S,
    private: BTreeMap<ValueHandle, (Value, u64)>,
    bytes: u64,
}
impl<S> PolicyValues<S> {
    pub fn new(backing: S) -> Self {
        Self {
            backing,
            private: BTreeMap::new(),
            bytes: 0,
        }
    }
}
impl<S: ValueStore> ValueStore for PolicyValues<S> {
    fn supports_confidential(&self) -> bool {
        self.backing.supports_confidential()
    }
    fn retained_persistence(&self) -> crate::history::Persistence {
        self.backing.retained_persistence()
    }
    fn store(&mut self, value: &Value) -> Result<ValueHandle, StoreError> {
        if !value.provenance().policy().is_private() {
            return self.backing.store(value);
        }
        let charge = crate::value_size::value_charge(
            value,
            (64 * 1024 * 1024u64).saturating_sub(self.bytes),
        )
        .ok_or(StoreError::Limit("private memory"))?;
        if self.private.len() >= 1024 {
            return Err(StoreError::Limit("private values"));
        }
        let handle = ValueHandle::fresh();
        self.private.insert(handle.clone(), (value.clone(), charge));
        self.bytes += charge;
        Ok(handle)
    }
    fn read(&self, handle: &ValueHandle) -> Result<Option<LoadedValue>, StoreError> {
        match self.private.get(handle) {
            Some((value, _)) => Ok(Some(LoadedValue {
                value: value.clone(),
            })),
            None => self.backing.read(handle),
        }
    }
    fn encoded(&self, handle: &ValueHandle) -> Result<Option<Vec<u8>>, StoreError> {
        if self.private.contains_key(handle) {
            return Err(StoreError::Restricted);
        }
        self.backing.encoded(handle)
    }
    fn size(&self, handle: &ValueHandle) -> Result<Option<u64>, StoreError> {
        match self.private.get(handle) {
            Some((_, bytes)) => Ok(Some(*bytes)),
            None => self.backing.size(handle),
        }
    }
    fn keep(&mut self, handle: &ValueHandle) -> Result<bool, StoreError> {
        if self.private.contains_key(handle) {
            return Err(StoreError::Restricted);
        }
        self.backing.keep(handle)
    }
    fn is_kept(&self, handle: &ValueHandle) -> Result<bool, StoreError> {
        if self.private.contains_key(handle) {
            Ok(false)
        } else {
            self.backing.is_kept(handle)
        }
    }
    fn keep_with_reason(
        &mut self,
        handle: &ValueHandle,
        reason: Retention,
    ) -> Result<bool, StoreError> {
        if self.private.contains_key(handle) {
            return Err(StoreError::Restricted);
        }
        self.backing.keep_with_reason(handle, reason)
    }
    fn retention(&self, handle: &ValueHandle) -> Result<Retention, StoreError> {
        if self.private.contains_key(handle) {
            Ok(Retention::Temporary)
        } else {
            self.backing.retention(handle)
        }
    }
    fn retention_usage(&self) -> Result<RetentionUsage, StoreError> {
        let mut usage = self.backing.retention_usage()?;
        usage.private_count = self.private.len() as u64;
        usage.private_bytes = self.bytes;
        Ok(usage)
    }
    fn release(&mut self, handle: &ValueHandle) -> Result<bool, StoreError> {
        if let Some((_, bytes)) = self.private.remove(handle) {
            self.bytes -= bytes;
            return Ok(true);
        }
        self.backing.release(handle)
    }
    fn take_evicted(&mut self, maximum: usize) -> Result<EvictionBatch, StoreError> {
        self.backing.take_evicted(maximum)
    }
}
