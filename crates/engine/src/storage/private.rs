//! Bounded memory-only private values. This wrapper never spills to its backing store.
use super::*;
use std::collections::BTreeMap;
pub(super) struct PolicyValues<S> {
    backing: S,
    private: BTreeMap<ValueHandle, (Arc<Value>, u64)>,
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
    fn read_policy(
        &self,
        handle: &ValueHandle,
    ) -> Result<Option<wes_core::flow::FlowPolicy>, StoreError> {
        match self.private.get(handle) {
            Some((v, _)) => Ok(Some(v.provenance().policy().clone())),
            None => self.backing.read_policy(handle),
        }
    }
    fn dependencies(&self, handle: &ValueHandle) -> Result<Vec<ValueHandle>, StoreError> {
        if self.private.contains_key(handle) {
            Ok(vec![])
        } else {
            self.backing.dependencies(handle)
        }
    }
    fn compose(
        &mut self,
        handle: &ValueHandle,
        members: &[super::SnapshotMember],
    ) -> Result<super::StoredSnapshot, StoreError> {
        self.backing.compose(handle, members)
    }
    fn begin_acquisition(
        &mut self,
        maximum: u64,
        owner: std::sync::Weak<()>,
    ) -> Result<ValueHandle, StoreError> {
        self.backing.begin_acquisition(maximum, owner)
    }
    fn append_acquisition(&mut self, h: &ValueHandle, p: &str, b: &[u8]) -> Result<(), StoreError> {
        self.backing.append_acquisition(h, p, b)
    }
    fn seal_acquisition(
        &mut self,
        h: &ValueHandle,
        shell: &Value,
        leaves: &[LeafSelection],
    ) -> Result<StoredSnapshot, StoreError> {
        if shell.provenance().policy().is_private() || shell.provenance().policy().is_unknown() {
            return Err(StoreError::Restricted);
        }
        self.backing.seal_acquisition(h, shell, leaves)
    }
    fn abort_acquisition(&mut self, h: &ValueHandle) -> Result<(), StoreError> {
        self.backing.abort_acquisition(h)
    }
    fn describe(
        &self,
        h: &ValueHandle,
        fields: &[String],
    ) -> Result<Option<StoredSnapshot>, StoreError> {
        if self.private.contains_key(h) {
            return Ok(None);
        }
        self.backing.describe(h, fields)
    }
    fn describe_bounded(
        &self,
        h: &ValueHandle,
        maximum: u64,
    ) -> Result<Option<StoredSnapshot>, StoreError> {
        if self.private.contains_key(h) {
            return Ok(None);
        }
        self.backing.describe_bounded(h, maximum)
    }
    fn read_selected(
        &self,
        h: &ValueHandle,
        fields: &[String],
        limit: u64,
    ) -> Result<Option<LoadedValue>, StoreError> {
        if self.private.contains_key(h) {
            return Err(StoreError::Restricted);
        }
        self.backing.read_selected(h, fields, limit)
    }
    fn source_range(
        &self,
        s: &StoredSnapshot,
        from: u64,
        maximum: usize,
    ) -> Result<SourceRange, StoreError> {
        if self.private.contains_key(&s.handle) {
            return Err(StoreError::Restricted);
        }
        self.backing.source_range(s, from, maximum)
    }
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
        self.private
            .insert(handle.clone(), (Arc::new(value.clone()), charge));
        self.bytes += charge;
        Ok(handle)
    }
    fn store_with_placement(
        &mut self,
        value: &Value,
        placement: ValuePlacement,
    ) -> Result<ValueHandle, StoreError> {
        if value.provenance().policy().is_private() {
            self.store(value)
        } else {
            self.backing.store_with_placement(value, placement)
        }
    }
    fn has_temporary_file(&self, handle: &ValueHandle) -> Result<bool, StoreError> {
        if self.private.contains_key(handle) {
            Ok(false)
        } else {
            self.backing.has_temporary_file(handle)
        }
    }
    fn hold(&mut self, handle: &ValueHandle) -> Result<Option<ValueLease>, StoreError> {
        if let Some((value, _)) = self.private.get(handle) {
            Ok(Some(ValueLease::retain(value.clone())))
        } else {
            self.backing.hold(handle)
        }
    }
    fn requires_ownership_record(&self, handle: &ValueHandle) -> Result<bool, StoreError> {
        if self.private.contains_key(handle) {
            Ok(false)
        } else {
            self.backing.requires_ownership_record(handle)
        }
    }
    fn read(&self, handle: &ValueHandle) -> Result<Option<LoadedValue>, StoreError> {
        match self.private.get(handle) {
            Some((value, _)) => Ok(Some(LoadedValue {
                value: value.as_ref().clone(),
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
    fn digest(&self, handle: &ValueHandle) -> Result<Option<String>, StoreError> {
        if self.private.contains_key(handle) {
            return Err(StoreError::Restricted);
        }
        self.backing.digest(handle)
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
        if self
            .private
            .get(handle)
            .is_some_and(|(value, _)| Arc::strong_count(value) > 1)
        {
            return Err(StoreError::Limit("active private result holds"));
        }
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
