//! Root-aware value ownership. A kept descriptor alone is never a kept dataset.
use super::super::datasets::{DatasetRootKind, DatasetRootRequest};
use super::*;
use wes_core::{Data, DatasetRef, Shape};

struct Prefix<'a> {
    reference: &'a DatasetRef,
    element: &'a Shape,
}
fn prefixes(value: &Value) -> Result<Vec<Prefix<'_>>, StoreError> {
    let mut pending = vec![(value.data(), value.shape(), 0usize)];
    let mut output: Vec<Prefix<'_>> = Vec::new();
    let mut visited = 0usize;
    while let Some((data, shape, depth)) = pending.pop() {
        visited += 1;
        if depth > 128 || visited > 1_000_000 {
            return Err(StoreError::Limit("dataset reference traversal"));
        }
        match (data, shape) {
            (Data::Dataset(reference), Shape::Dataset(element)) => {
                if let Some(prior) = output.iter().find(|p| p.reference == reference.as_ref()) {
                    if prior.element != element.as_ref() {
                        return Err(StoreError::Conflict);
                    }
                } else {
                    if output.len() == 128 {
                        return Err(StoreError::Limit("dataset references"));
                    }
                    output.push(Prefix { reference, element });
                }
            }
            (Data::List(items), shape) => {
                let element = match shape {
                    Shape::List(element) => element.as_ref(),
                    _ => &Shape::Unknown,
                };
                if items.len() > 1_000_000usize.saturating_sub(visited + pending.len()) {
                    return Err(StoreError::Limit("dataset reference traversal"));
                }
                pending.extend(items.iter().map(|d| (d, element, depth + 1)));
            }
            (Data::Record(fields), shape) => {
                if fields.len() > 1_000_000usize.saturating_sub(visited + pending.len()) {
                    return Err(StoreError::Limit("dataset reference traversal"));
                }
                for (name, data) in fields {
                    let shape = match shape {
                        Shape::Record(record) => record.field(name).unwrap_or(&Shape::Unknown),
                        _ => &Shape::Unknown,
                    };
                    pending.push((data, shape, depth + 1));
                }
            }
            (Data::Option(Some(data)), shape) => {
                let element = match shape {
                    Shape::Option(element) => element.as_ref(),
                    _ => &Shape::Unknown,
                };
                pending.push((data, element, depth + 1));
            }
            // Unknown shape may not hide a dataset reference, even inside an open record.
            (Data::Dataset(_), _) => return Err(StoreError::Conflict),
            _ => {}
        }
    }
    Ok(output)
}
pub(super) fn has_datasets(value: &Value) -> Result<bool, StoreError> {
    Ok(!prefixes(value)?.is_empty())
}

impl<V: ValueStore> Owner<V> {
    fn value_root(&self, handle: &ValueHandle) -> Result<Option<DatasetRootRequest>, StoreError> {
        self.datasets
            .as_ref()
            .map(|port| port.value_root(handle))
            .transpose()
            .map(Option::flatten)
    }
    pub(super) fn validate_prefixes(&self, value: &Value) -> Result<Vec<DatasetRef>, StoreError> {
        if !value.provenance().policy().dataset_reads().is_empty() {
            self.datasets()?
                .check_read_origins(value.provenance().policy().dataset_reads())?;
        }
        let entries = prefixes(value)?;
        let mut refs = Vec::with_capacity(entries.len());
        for entry in entries {
            let info = self.datasets()?.inspect(entry.reference)?;
            if entry.element != &info.schema.root().shape() {
                return Err(StoreError::Conflict);
            }
            if value.provenance().policy().join(&info.policy) != *value.provenance().policy() {
                return Err(StoreError::Restricted);
            }
            if info
                .policy
                .origins()
                .iter()
                .any(|origin| !value.provenance().policy().origins().contains(origin))
                || info
                    .policy
                    .dataset_reads()
                    .iter()
                    .any(|origin| !value.provenance().policy().dataset_reads().contains(origin))
            {
                return Err(StoreError::Restricted);
            }
            refs.push(entry.reference.clone());
        }
        Ok(refs)
    }
    fn root_value(
        &mut self,
        handle: &ValueHandle,
        value: &Value,
        retention: Retention,
    ) -> Result<(), StoreError> {
        let prefixes = self.validate_prefixes(value)?;
        if prefixes.is_empty() {
            return Ok(());
        }
        self.datasets_mut()?.set_root(DatasetRootRequest {
            identity: handle.clone(),
            kind: DatasetRootKind::Value,
            prefixes,
            captures: vec![],
            retention,
            policy: value.provenance().policy().clone(),
        })?;
        Ok(())
    }
}
impl<V: ValueStore> ValueStore for Owner<V> {
    fn supports_confidential(&self) -> bool {
        self.values.supports_confidential()
    }
    fn retained_persistence(&self) -> crate::history::Persistence {
        self.values.retained_persistence()
    }
    fn store(&mut self, value: &Value) -> Result<ValueHandle, StoreError> {
        let refs = self.validate_prefixes(value)?;
        if !refs.is_empty()
            && (value.provenance().policy().is_private()
                || value.provenance().policy().is_unknown())
        {
            return Err(StoreError::Restricted);
        }
        let handle = self.values.store(value)?;
        self.root_value(&handle, value, Retention::Temporary)
            .map_err(|source| StoreError::Published {
                handle: handle.clone(),
                source: Box::new(source),
            })?;
        Ok(handle)
    }
    fn read(&self, handle: &ValueHandle) -> Result<Option<LoadedValue>, StoreError> {
        let value = self.values.read(handle)?;
        if let Some(loaded) = &value {
            self.validate_prefixes(&loaded.value)?;
        }
        Ok(value)
    }
    fn encoded(&self, handle: &ValueHandle) -> Result<Option<Vec<u8>>, StoreError> {
        if let Some(loaded) = self.values.read(handle)? {
            self.validate_prefixes(&loaded.value)?;
        }
        self.values.encoded(handle)
    }
    fn size(&self, handle: &ValueHandle) -> Result<Option<u64>, StoreError> {
        self.values.size(handle)
    }
    fn retention_size(&self, handle: &ValueHandle) -> Result<Option<u64>, StoreError> {
        let Some(mut size) = self.values.size(handle)? else {
            return Ok(None);
        };
        if let Some(root) = self
            .value_root(handle)?
            .filter(|root| !root.prefixes.is_empty())
        {
            size = size
                .checked_add(self.datasets()?.retention_size_many(&root.prefixes)?)
                .ok_or(StoreError::Limit("retention bytes"))?;
        }
        Ok(Some(size))
    }
    fn automatic_retention_allowed(&self, handle: &ValueHandle) -> Result<bool, StoreError> {
        if !self.values.automatic_retention_allowed(handle)? {
            return Ok(false);
        }
        for prefix in self
            .value_root(handle)?
            .into_iter()
            .flat_map(|root| root.prefixes)
        {
            if self.datasets()?.inspect(&prefix)?.policy.is_confidential()
                || self.datasets()?.inspect(&prefix)?.lifecycle
                    != crate::storage::datasets::DatasetLifecycle::Sealed
            {
                return Ok(false);
            }
        }
        Ok(true)
    }
    fn release(&mut self, handle: &ValueHandle) -> Result<bool, StoreError> {
        if self
            .datasets
            .as_ref()
            .map(|port| port.protects_value(handle))
            .transpose()?
            .unwrap_or(false)
        {
            return Err(StoreError::Limit(
                "retained checkpoint source; remove its dependent root first",
            ));
        }
        let loaded = if self
            .value_root(handle)?
            .is_some_and(|root| !root.prefixes.is_empty())
        {
            self.values.read(handle)?
        } else {
            None
        };
        let released = self.values.release(handle)?;
        if let Some(loaded) = loaded {
            if has_datasets(&loaded.value)? {
                self.datasets_mut()?
                    .set_root(DatasetRootRequest {
                        identity: handle.clone(),
                        kind: DatasetRootKind::Value,
                        prefixes: vec![],
                        captures: vec![],
                        retention: Retention::Temporary,
                        policy: loaded.value.provenance().policy().clone(),
                    })
                    .map_err(|source| StoreError::Released {
                        handle: handle.clone(),
                        source: Box::new(source),
                    })?;
            }
        }
        Ok(released)
    }
    fn keep(&mut self, handle: &ValueHandle) -> Result<bool, StoreError> {
        self.keep_with_reason(handle, Retention::Protected)
    }
    fn keep_with_reason(
        &mut self,
        handle: &ValueHandle,
        reason: Retention,
    ) -> Result<bool, StoreError> {
        if self
            .value_root(handle)?
            .is_none_or(|root| root.prefixes.is_empty())
        {
            return self.values.keep_with_reason(handle, reason);
        }
        let Some(loaded) = self.values.read(handle)? else {
            return Ok(false);
        };
        if !loaded.value.provenance().policy().allows_retention() {
            return Err(StoreError::Restricted);
        }
        self.root_value(handle, &loaded.value, reason)?;
        self.values.keep_with_reason(handle, reason)
    }
    fn is_kept(&self, handle: &ValueHandle) -> Result<bool, StoreError> {
        if !self.values.is_kept(handle)? {
            return Ok(false);
        }
        let Some(root) = self.value_root(handle)? else {
            return Ok(true);
        };
        for prefix in &root.prefixes {
            self.datasets()?.inspect(prefix)?;
        }
        self.datasets()?
            .root_covers(handle, &root.prefixes, self.values.retention(handle)?)
    }
    fn retention(&self, handle: &ValueHandle) -> Result<Retention, StoreError> {
        self.values.retention(handle)
    }
    fn retention_usage(&self) -> Result<RetentionUsage, StoreError> {
        self.values.retention_usage()
    }
    fn take_evicted(&mut self, maximum: usize) -> Result<EvictionBatch, StoreError> {
        if self.pending_evictions.is_empty() {
            let batch = self.values.take_evicted(maximum)?;
            self.pending_evictions = batch.handles.into();
            self.evictions_more = batch.more;
        }
        // Retain an attempted batch until every root release succeeds. A failed
        // catalog acknowledgement cannot lose the backend's eviction notices.
        for handle in self.pending_evictions.iter().take(maximum) {
            if let Some(mut root) = self.value_root(handle)? {
                root.prefixes.clear();
                root.captures.clear();
                self.datasets
                    .as_mut()
                    .ok_or(StoreError::DatasetUnavailable)?
                    .set_root(root)?;
            }
        }
        let count = self.pending_evictions.len().min(maximum);
        Ok(EvictionBatch {
            handles: self.pending_evictions.drain(..count).collect(),
            more: self.evictions_more || !self.pending_evictions.is_empty(),
        })
    }
}
