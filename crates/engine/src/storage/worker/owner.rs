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
    pub(super) fn retention_capture_cost(
        &mut self,
        captures: Vec<super::super::datasets::RetentionCapture>,
    ) -> Result<(u64, u64), StoreError> {
        struct Object {
            digest: String,
            bytes: Option<u64>,
            shared: bool,
            children: Vec<ValueHandle>,
        }
        let mut objects = std::collections::BTreeMap::<ValueHandle, Object>::new();
        for capture in captures {
            let handle = ValueHandle::new(&capture.value.handle)?;
            if let Some(prior) = objects.get_mut(&handle) {
                if prior.digest != capture.value.digest || prior.bytes != Some(capture.value.bytes)
                {
                    return Err(StoreError::Conflict);
                }
                prior.shared |= capture.shared;
            } else {
                if objects.len() >= 128 {
                    return Err(StoreError::Limit("retention dependency traversal"));
                }
                objects.insert(
                    handle,
                    Object {
                        digest: capture.value.digest,
                        bytes: Some(capture.value.bytes),
                        shared: capture.shared,
                        children: vec![],
                    },
                );
            }
        }
        let mut pending = objects
            .keys()
            .cloned()
            .collect::<std::collections::VecDeque<_>>();
        while let Some(handle) = pending.pop_front() {
            self.check_scope(&handle)?;
            let policy = self
                .values
                .read_policy(&handle)?
                .ok_or(StoreError::MissingValue)?;
            self.check_policy(&policy)?;
            let object = &objects[&handle];
            let bytes = self.values.size(&handle)?.ok_or(StoreError::MissingValue)?;
            if self.values.digest(&handle)?.as_ref() != Some(&object.digest)
                || object.bytes.is_some_and(|expected| expected != bytes)
            {
                return Err(StoreError::Conflict);
            }
            let dependencies = self.values.dependencies(&handle)?;
            if dependencies.len() > 128 {
                return Err(StoreError::Limit("retention dependency traversal"));
            }
            let mut children = vec![];
            for dependency in dependencies {
                let child = dependency.validate()?;
                let child_policy = self
                    .values
                    .read_policy(&child)?
                    .ok_or(StoreError::MissingValue)?;
                if policy.join(&child_policy) != policy {
                    return Err(StoreError::Restricted);
                }
                if let Some(prior) = objects.get(&child) {
                    if prior.digest != dependency.digest {
                        return Err(StoreError::Conflict);
                    }
                } else {
                    if objects.len() >= 128 {
                        return Err(StoreError::Limit("retention dependency traversal"));
                    }
                    objects.insert(
                        child.clone(),
                        Object {
                            digest: dependency.digest,
                            bytes: None,
                            shared: false,
                            children: vec![],
                        },
                    );
                    pending.push_back(child.clone());
                }
                children.push(child);
            }
            let object = objects.get_mut(&handle).expect("inventoried source owner");
            object.bytes = Some(bytes);
            object.shared |= self.values.is_kept(&handle)?;
            object.children = children;
        }
        // A source shared with another protected prefix also shares its immutable closure.
        let mut shared = objects
            .iter()
            .filter(|(_, o)| o.shared)
            .map(|(h, _)| h.clone())
            .collect::<std::collections::VecDeque<_>>();
        while let Some(handle) = shared.pop_front() {
            for child in objects[&handle].children.clone() {
                let object = objects.get_mut(&child).expect("inventoried dependency");
                if !object.shared {
                    object.shared = true;
                    shared.push_back(child);
                }
            }
        }
        let mut total = 0u64;
        let mut shared = 0u64;
        for object in objects.values() {
            let bytes = object.bytes.expect("joined inventory");
            total = total
                .checked_add(bytes)
                .ok_or(StoreError::Limit("retention bytes"))?;
            if object.shared {
                shared = shared
                    .checked_add(bytes)
                    .ok_or(StoreError::Limit("retention bytes"))?;
            }
        }
        Ok((total, shared))
    }
    fn kept_closure(
        &self,
        handle: &ValueHandle,
        visiting: &mut std::collections::BTreeSet<ValueHandle>,
        work: &mut usize,
    ) -> Result<bool, StoreError> {
        *work += 1;
        if *work > 128 || !visiting.insert(handle.clone()) {
            return Err(StoreError::Limit("retained dependency traversal"));
        }
        let result = (|| {
            if !self.values.is_kept(handle)? {
                return Ok(false);
            }
            for dependency in self.values.dependencies(handle)? {
                let child = dependency.validate()?;
                if self.values.digest(&child)?.as_ref() != Some(&dependency.digest) {
                    return Err(StoreError::Conflict);
                }
                if !self.kept_closure(&child, visiting, work)? {
                    return Ok(false);
                }
            }
            let Some(root) = self.value_root(handle)? else {
                return Ok(true);
            };
            for prefix in &root.prefixes {
                self.datasets()?.inspect(prefix)?;
            }
            self.datasets()?
                .root_covers(handle, &root.prefixes, self.values.retention(handle)?)
        })();
        visiting.remove(handle);
        result
    }
    pub(super) fn owns_transient(&self, handle: &ValueHandle) -> bool {
        self.scope.as_ref().is_some_and(|scope| {
            scope.is_open()
                && self
                    .value_scopes
                    .get(handle)
                    .is_some_and(|(id, witness)| id == scope.id() && witness.is_open())
        })
    }
    fn owns_acquisition(&self, handle: &ValueHandle) -> bool {
        self.owns_transient(handle)
            || (self.scope.is_none()
                && self
                    .unscoped_acquisitions
                    .get(handle)
                    .is_some_and(|owner| owner.strong_count() > 0))
    }
    pub(super) fn check_scope(&self, handle: &ValueHandle) -> Result<(), StoreError> {
        if self.value_scopes.contains_key(handle)
            && !self.owns_transient(handle)
            && !self.is_kept(handle)?
        {
            return Err(StoreError::ScopeUnavailable);
        }
        Ok(())
    }
    fn checked_snapshot(
        &self,
        snapshot: Option<crate::storage::StoredSnapshot>,
    ) -> Result<Option<crate::storage::StoredSnapshot>, StoreError> {
        if let Some(snapshot) = &snapshot {
            self.check_policy(snapshot.provenance.policy())?;
            if snapshot.provenance.policy().is_private()
                || snapshot.provenance.policy().is_unknown()
            {
                // These values keep their existing bounded read/hold path; they cannot become
                // public deferred snapshots or acquire persistence permission through metadata.
                return Ok(None);
            }
            if !snapshot.provenance.policy().dataset_reads().is_empty() {
                self.datasets()?
                    .check_read_origins(snapshot.provenance.policy().dataset_reads())?;
            }
        }
        Ok(snapshot)
    }
    pub(super) fn route_evictions(&mut self, maximum: usize) -> Result<bool, StoreError> {
        if self.pending_evictions.is_empty() {
            let batch = self.values.take_evicted(maximum)?;
            self.pending_evictions = batch.handles.into();
            self.evictions_more = batch.more;
        }
        let count = self.pending_evictions.len().min(maximum);
        let ordinary = self
            .pending_evictions
            .iter()
            .take(count)
            .filter(|h| !self.value_scopes.contains_key(*h))
            .count();
        if self
            .deferred_evictions
            .get(&None)
            .map_or(0, |q| q.len())
            .saturating_add(ordinary)
            > wes_budgets::get("values.outputs") as usize
        {
            return Err(StoreError::Limit("pending eviction notices"));
        }
        // Keep the batch until every dependency-root acknowledgement succeeds.
        for handle in self.pending_evictions.iter().take(count) {
            if let Some(mut root) = self.value_root(handle)? {
                root.prefixes.clear();
                root.captures.clear();
                self.datasets
                    .as_mut()
                    .ok_or(StoreError::DatasetUnavailable)?
                    .set_root(root)?;
            }
        }
        for handle in self.pending_evictions.drain(..count) {
            let key = match self.value_scopes.get(&handle) {
                Some((id, witness)) if witness.is_open() => Some(id.clone()),
                Some(_) => {
                    self.value_scopes.remove(&handle);
                    continue;
                }
                None => None,
            };
            self.deferred_evictions
                .entry(key)
                .or_default()
                .push_back(handle);
        }
        Ok(self.evictions_more || !self.pending_evictions.is_empty())
    }
    fn value_root(&self, handle: &ValueHandle) -> Result<Option<DatasetRootRequest>, StoreError> {
        self.datasets
            .as_ref()
            .map(|port| port.value_root(handle))
            .transpose()
            .map(Option::flatten)
    }
    pub(super) fn validate_prefixes(&self, value: &Value) -> Result<Vec<DatasetRef>, StoreError> {
        self.check_policy(value.provenance().policy())?;
        if !value.provenance().policy().dataset_reads().is_empty() {
            self.datasets()?
                .check_read_origins(value.provenance().policy().dataset_reads())?;
        }
        let entries = prefixes(value)?;
        let mut refs = Vec::with_capacity(entries.len());
        for entry in entries {
            let info = self.dataset_info(entry.reference)?;
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
                || info.policy.provider_reads().iter().any(|origin| {
                    !value
                        .provenance()
                        .policy()
                        .provider_reads()
                        .contains(origin)
                })
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
        let mut captures = std::collections::BTreeMap::new();
        if retention == Retention::Protected {
            for prefix in &prefixes {
                for capture in self.datasets()?.retention_preview(prefix)?.captures {
                    captures.insert(capture.value.handle.clone(), capture.value);
                }
            }
        }
        self.datasets_mut()?.set_root(DatasetRootRequest {
            identity: handle.clone(),
            kind: DatasetRootKind::Value,
            prefixes,
            captures: captures.into_values().collect(),
            retention,
            policy: value.provenance().policy().clone(),
        })?;
        Ok(())
    }
}
impl<V: ValueStore> ValueStore for Owner<V> {
    fn recovery_snapshot(
        &self,
        handle: &ValueHandle,
        value: &Value,
    ) -> Result<Option<crate::storage::StoredSnapshot>, StoreError> {
        self.check_scope(handle)?;
        if self.read_policy(handle)?.is_none() {
            return Ok(None);
        }
        self.checked_snapshot(self.values.recovery_snapshot(handle, value)?)
    }
    fn dependencies(
        &self,
        handle: &ValueHandle,
    ) -> Result<Vec<super::super::SnapshotDependency>, StoreError> {
        self.check_scope(handle)?;
        if self.read_policy(handle)?.is_none() {
            return Err(StoreError::MissingValue);
        }
        self.values.dependencies(handle)
    }
    fn read_policy(
        &self,
        handle: &ValueHandle,
    ) -> Result<Option<wes_core::flow::FlowPolicy>, StoreError> {
        self.check_scope(handle)?;
        let policy = self.values.read_policy(handle)?;
        if let Some(policy) = &policy {
            self.check_policy(policy)?;
        }
        Ok(policy)
    }
    fn compose(
        &mut self,
        handle: &ValueHandle,
        members: &[super::super::SnapshotMember],
    ) -> Result<crate::storage::StoredSnapshot, StoreError> {
        for member in members {
            let child = ValueHandle::new(&member.handle)?;
            if &child == handle || !self.is_kept(&child)? {
                return Err(StoreError::Restricted);
            }
            let snapshot = self
                .describe(&child, &member.selection)?
                .ok_or(StoreError::MissingValue)?;
            if snapshot.digest != member.digest {
                return Err(StoreError::Conflict);
            }
        }
        self.values.compose(handle, members)
    }
    fn begin_acquisition(
        &mut self,
        maximum: u64,
        owner: std::sync::Weak<()>,
    ) -> Result<ValueHandle, StoreError> {
        if self.scope.as_ref().is_some_and(|scope| !scope.is_open()) || owner.strong_count() == 0 {
            return Err(StoreError::ScopeUnavailable);
        }
        self.unscoped_acquisitions
            .retain(|_, owner| owner.strong_count() > 0);
        if self.value_scopes.len() + self.unscoped_acquisitions.len()
            >= wes_budgets::get("values.outputs") as usize
        {
            return Err(StoreError::Limit("acquisition values"));
        }
        let handle = self.values.begin_acquisition(maximum, owner.clone())?;
        if let Some(scope) = &self.scope {
            self.value_scopes
                .insert(handle.clone(), (scope.id().into(), scope.witness()));
        } else {
            self.unscoped_acquisitions.insert(handle.clone(), owner);
        }
        Ok(handle)
    }
    fn append_acquisition(
        &mut self,
        handle: &ValueHandle,
        port: &str,
        bytes: &[u8],
    ) -> Result<(), StoreError> {
        if !self.owns_acquisition(handle) {
            return Err(StoreError::ScopeUnavailable);
        }
        self.values.append_acquisition(handle, port, bytes)
    }
    fn seal_acquisition(
        &mut self,
        handle: &ValueHandle,
        shell: &Value,
        leaves: &[super::super::LeafSelection],
    ) -> Result<crate::storage::StoredSnapshot, StoreError> {
        self.seal_acquisition_with_dependencies(handle, shell, leaves, &[])
    }
    fn seal_acquisition_with_dependencies(
        &mut self,
        handle: &ValueHandle,
        shell: &Value,
        leaves: &[super::super::LeafSelection],
        dependencies: &[super::super::SnapshotDependency],
    ) -> Result<crate::storage::StoredSnapshot, StoreError> {
        if !self.owns_acquisition(handle) {
            return Err(StoreError::ScopeUnavailable);
        }
        if self.scope.is_some() {
            self.validate_prefixes(shell)?;
        } else if has_datasets(shell)? {
            return Err(StoreError::Restricted);
        }
        if !shell.data().is_inline()
            || !shell.shape().is_inline()
            || shell.shape().contains_meta()
            || shell.provenance().policy().is_private()
            || shell.provenance().policy().is_unknown()
        {
            return Err(StoreError::Restricted);
        }
        if dependencies.len() > 128 {
            return Err(StoreError::Limit("snapshot dependencies"));
        }
        let mut seen = std::collections::BTreeSet::new();
        for dependency in dependencies {
            let child = dependency.validate()?;
            if &child == handle || !seen.insert(dependency.name.clone()) {
                return Err(StoreError::Conflict);
            }
            self.check_scope(&child)?;
            if self.values.digest(&child)?.as_ref() != Some(&dependency.digest) {
                return Err(StoreError::Conflict);
            }
            let selected = self
                .values
                .describe(&child, &dependency.selection)?
                .ok_or(StoreError::MissingValue)?;
            if selected.digest != dependency.digest {
                return Err(StoreError::Conflict);
            }
            let policy = self
                .values
                .read_policy(&child)?
                .ok_or(StoreError::MissingValue)?;
            if shell.provenance().policy().join(&policy) != *shell.provenance().policy() {
                return Err(StoreError::Restricted);
            }
        }
        self.values
            .seal_acquisition_with_dependencies(handle, shell, leaves, dependencies)
    }
    fn abort_acquisition(&mut self, handle: &ValueHandle) -> Result<(), StoreError> {
        if !self.owns_acquisition(handle) {
            return Err(StoreError::ScopeUnavailable);
        }
        self.values.abort_acquisition(handle)?;
        self.value_scopes.remove(handle);
        self.unscoped_acquisitions.remove(handle);
        Ok(())
    }
    fn describe(
        &self,
        handle: &ValueHandle,
        fields: &[String],
    ) -> Result<Option<crate::storage::StoredSnapshot>, StoreError> {
        self.check_scope(handle)?;
        if self.read_policy(handle)?.is_none() {
            return Ok(None);
        }
        self.checked_snapshot(self.values.describe(handle, fields)?)
    }
    fn describe_bounded(
        &self,
        handle: &ValueHandle,
        maximum: u64,
    ) -> Result<Option<crate::storage::StoredSnapshot>, StoreError> {
        self.check_scope(handle)?;
        if self.read_policy(handle)?.is_none() {
            return Ok(None);
        }
        self.checked_snapshot(self.values.describe_bounded(handle, maximum)?)
    }
    fn read_selected(
        &self,
        handle: &ValueHandle,
        fields: &[String],
        limit: u64,
    ) -> Result<Option<LoadedValue>, StoreError> {
        self.check_scope(handle)?;
        if self.read_policy(handle)?.is_none() {
            return Ok(None);
        }
        let loaded = self.values.read_selected(handle, fields, limit)?;
        if let Some(loaded) = &loaded {
            self.validate_prefixes(&loaded.value)?;
        }
        Ok(loaded)
    }
    fn source_range(
        &self,
        snapshot: &crate::storage::StoredSnapshot,
        from: u64,
        maximum: usize,
    ) -> Result<crate::storage::SourceRange, StoreError> {
        let current = self
            .describe(&snapshot.handle, &snapshot.selection)?
            .ok_or(StoreError::MissingValue)?;
        if current.handle != snapshot.handle
            || current.digest != snapshot.digest
            || current.selection != snapshot.selection
            || current.shape != snapshot.shape
            || current.provenance != snapshot.provenance
            || current.metadata != snapshot.metadata
        {
            return Err(StoreError::Conflict);
        }
        self.values.source_range(snapshot, from, maximum)
    }

    fn supports_confidential(&self) -> bool {
        self.values.supports_confidential()
    }
    fn retained_persistence(&self) -> crate::history::Persistence {
        self.values.retained_persistence()
    }
    fn store(&mut self, value: &Value) -> Result<ValueHandle, StoreError> {
        self.store_with_placement(value, ValuePlacement::ManagedFile)
    }
    fn store_with_placement(
        &mut self,
        value: &Value,
        placement: ValuePlacement,
    ) -> Result<ValueHandle, StoreError> {
        if self.scope.is_some()
            && !matches!(
                placement,
                ValuePlacement::Memory | ValuePlacement::MemoryAndDisk
            )
        {
            return Err(StoreError::Restricted);
        }
        let refs = self.validate_prefixes(value)?;
        if !refs.is_empty()
            && (value.provenance().policy().is_private()
                || value.provenance().policy().is_unknown())
        {
            return Err(StoreError::Restricted);
        }
        if self.scope.is_some()
            && self.value_scopes.len() >= wes_budgets::get("values.outputs") as usize
        {
            return Err(StoreError::Limit("transient values"));
        }
        // Wake every attached scope; one child's publication may evict another's
        // copy. Read/notice jobs do not wake readers again.
        self.eviction_updates
            .send_modify(|revision| *revision = revision.saturating_add(1));
        let handle = self.values.store_with_placement(value, placement)?;
        if let Some(scope) = &self.scope {
            self.value_scopes
                .insert(handle.clone(), (scope.id().into(), scope.witness()));
        }
        self.root_value(&handle, value, Retention::Temporary)
            .map_err(|source| StoreError::Published {
                handle: handle.clone(),
                source: Box::new(source),
            })?;
        Ok(handle)
    }
    fn has_temporary_file(&self, handle: &ValueHandle) -> Result<bool, StoreError> {
        self.values.has_temporary_file(handle)
    }
    fn requires_ownership_record(&self, handle: &ValueHandle) -> Result<bool, StoreError> {
        Ok(self.values.requires_ownership_record(handle)?
            || self
                .value_root(handle)?
                .is_some_and(|root| !root.prefixes.is_empty() || !root.captures.is_empty()))
    }
    fn hold(&mut self, handle: &ValueHandle) -> Result<Option<ValueLease>, StoreError> {
        self.check_scope(handle)?;
        if self.holding.len() >= 128 || !self.holding.insert(handle.clone()) {
            return Err(StoreError::Limit("value hold dependency traversal"));
        }
        let result = (|| {
            let snapshot = self.describe(handle, &[])?;
            let dependencies = self.values.dependencies(handle)?;
            if dependencies.len() > wes_budgets::get("values.outputs") as usize {
                return Err(StoreError::Limit("value hold dependencies"));
            }
            let mut members = Vec::new();
            for dependency in &dependencies {
                let child = dependency.validate()?;
                if self.values.digest(&child)?.as_ref() != Some(&dependency.digest) {
                    return Err(StoreError::Conflict);
                }
                members.push(self.hold(&child)?.ok_or(StoreError::MissingValue)?);
            }
            if !dependencies.is_empty()
                || snapshot.as_ref().is_some_and(|snapshot| {
                    snapshot.shape.is_inline()
                        && snapshot.shape != Shape::Unknown
                        && !snapshot.shape.contains_meta()
                })
            {
                return Ok(self
                    .values
                    .hold(handle)?
                    .map(|hold| ValueLease::retain((hold, members))));
            }
            let Some(value) = self.values.read(handle)? else {
                return Ok(None);
            };
            let prefixes = self.validate_prefixes(&value.value)?;
            let Some(value) = self.values.hold(handle)? else {
                return Ok(None);
            };
            let mut leases = Vec::new();
            let mut seen = std::collections::HashSet::new();
            for prefix in prefixes {
                if seen.insert(prefix.clone()) {
                    leases.push(self.datasets()?.hold(&prefix)?);
                }
            }
            Ok(Some(ValueLease::retain((value, leases))))
        })();
        self.holding.remove(handle);
        result
    }
    fn read(&self, handle: &ValueHandle) -> Result<Option<LoadedValue>, StoreError> {
        self.check_scope(handle)?;
        if self.read_policy(handle)?.is_none() {
            return Ok(None);
        }
        let value = self.values.read(handle)?;
        if let Some(loaded) = &value {
            self.validate_prefixes(&loaded.value)?;
        }
        Ok(value)
    }
    fn encoded(&self, handle: &ValueHandle) -> Result<Option<Vec<u8>>, StoreError> {
        self.check_scope(handle)?;
        if self.read_policy(handle)?.is_none() {
            return Ok(None);
        }
        if let Some(loaded) = self.values.read(handle)? {
            self.validate_prefixes(&loaded.value)?;
        }
        self.values.encoded(handle)
    }
    fn size(&self, handle: &ValueHandle) -> Result<Option<u64>, StoreError> {
        self.check_scope(handle)?;
        self.values.size(handle)
    }
    fn digest(&self, handle: &ValueHandle) -> Result<Option<String>, StoreError> {
        self.check_scope(handle)?;
        self.values.digest(handle)
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
        self.value_scopes.remove(handle);
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
        self.check_scope(handle)?;
        if self.scope.is_some() && reason != Retention::Protected {
            return Err(StoreError::Restricted);
        }
        let dependencies = self.values.dependencies(handle)?;
        let datasets = self
            .value_root(handle)?
            .is_some_and(|root| !root.prefixes.is_empty());
        if dependencies.is_empty() && !datasets {
            return self.values.keep_with_reason(handle, reason);
        }
        let Some(policy) = self.values.read_policy(handle)? else {
            return Ok(false);
        };
        if !policy.allows_retention() {
            return Err(StoreError::Restricted);
        }
        if dependencies.len() > 128
            || self.retaining.len() >= 128
            || !self.retaining.insert(handle.clone())
        {
            return Err(StoreError::Limit("retention dependency traversal"));
        }
        let result = (|| {
            for dependency in dependencies {
                let child = dependency.validate()?;
                if self.values.digest(&child)?.as_ref() != Some(&dependency.digest) {
                    return Err(StoreError::Conflict);
                }
                let child_policy = self
                    .values
                    .read_policy(&child)?
                    .ok_or(StoreError::MissingValue)?;
                if policy.join(&child_policy) != policy {
                    return Err(StoreError::Restricted);
                }
                if !self.keep_with_reason(&child, Retention::Protected)? {
                    return Err(StoreError::MissingValue);
                }
            }
            if datasets {
                let loaded = self.values.read(handle)?.ok_or(StoreError::MissingValue)?;
                let mut captures = std::collections::BTreeMap::new();
                for prefix in self.validate_prefixes(&loaded.value)? {
                    for capture in self.datasets()?.retention_preview(&prefix)?.captures {
                        captures.insert(capture.value.handle.clone(), capture.value);
                    }
                }
                for capture in captures.into_values() {
                    let source = ValueHandle::new(&capture.handle)?;
                    if !self.keep_with_reason(&source, Retention::Protected)? {
                        return Err(StoreError::MissingValue);
                    }
                }
                self.root_value(handle, &loaded.value, reason)?;
            }
            self.values.keep_with_reason(handle, reason)
        })();
        self.retaining.remove(handle);
        result
    }
    fn is_kept(&self, handle: &ValueHandle) -> Result<bool, StoreError> {
        self.kept_closure(handle, &mut std::collections::BTreeSet::new(), &mut 0)
    }
    fn retention(&self, handle: &ValueHandle) -> Result<Retention, StoreError> {
        self.values.retention(handle)
    }
    fn retention_usage(&self) -> Result<RetentionUsage, StoreError> {
        self.values.retention_usage()
    }
    fn take_evicted(&mut self, maximum: usize) -> Result<EvictionBatch, StoreError> {
        // Deliver an already routed queue before pulling more backend notices.
        // A full ordinary queue must not prevent its own reader from draining it.
        let key = self.scope.as_ref().map(|scope| scope.id().to_owned());
        let routed = if self
            .deferred_evictions
            .get(&key)
            .is_none_or(|q| q.is_empty())
        {
            self.route_evictions(maximum)?
        } else {
            self.evictions_more || !self.pending_evictions.is_empty()
        };
        let queue = self.deferred_evictions.entry(key.clone()).or_default();
        let count = queue.len().min(maximum);
        let handles = queue.drain(..count).collect::<Vec<_>>();
        let queued = !queue.is_empty();
        if !queued {
            self.deferred_evictions.remove(&key);
        }
        for handle in &handles {
            self.value_scopes.remove(handle);
        }
        Ok(EvictionBatch {
            handles,
            more: routed || queued,
        })
    }
}
