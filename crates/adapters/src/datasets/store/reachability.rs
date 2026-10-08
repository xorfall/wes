//! Bounded exact-prefix reachability. Shared objects count once within one selected prefix.
use super::*;

impl DatasetStore {
    pub(super) fn collect_owned(
        &mut self,
    ) -> Result<wes_engine::storage::datasets::DatasetCleanup, DatasetError> {
        self.ready()?;
        // Snapshot away superseded journal transactions before unlinking the
        // objects they used for successor validation. The selected prefixes and
        // their commit-proof manifest lineage remain reachable.
        let mut objects = BTreeMap::<String, ObjectRef>::new();
        let mut selected = Vec::new();
        for root in self.roots.values() {
            let mut reference = root.manifest.clone();
            let deleted = self
                .gates
                .get(&root.dataset)
                .is_some_and(|g| g.status == catalog::ReadGateStatus::Deleted);
            let mut first = true;
            let mut generation = root.generation;
            let mut lineage = BTreeSet::new();
            loop {
                if objects.len() >= self.limits.objects.index.traversal_nodes {
                    return Err(DatasetError::Limit("cleanup reachability"));
                }
                let manifest = self.files.read_manifest(&reference)?;
                if !lineage.insert(reference.id.clone())
                    || manifest.dataset != root.dataset
                    || manifest.generation != generation
                {
                    return Err(DatasetError::StorageCorrupt);
                }
                insert_object(
                    &mut objects,
                    &reference,
                    self.limits.objects.index.traversal_nodes,
                )?;
                if first && !deleted {
                    selected.push(manifest.clone());
                }
                first = false;
                match manifest.previous {
                    Some(previous) => {
                        generation = generation
                            .checked_sub(1)
                            .filter(|g| *g > 0)
                            .ok_or(DatasetError::StorageCorrupt)?;
                        reference = previous;
                    }
                    None => break,
                }
            }
        }
        for root in self.references.values() {
            for reference in &root.prefixes {
                if self
                    .gates
                    .get(reference.dataset())
                    .is_some_and(|g| g.status == catalog::ReadGateStatus::Deleted)
                {
                    return Err(DatasetError::Conflict);
                }
                selected.push(self.resolve_committed(reference, &self.roots, false)?);
                if selected.len()
                    > self
                        .limits
                        .reference_roots
                        .saturating_mul(self.limits.catalog.roots_per_frame)
                {
                    return Err(DatasetError::Limit("cleanup roots"));
                }
            }
        }
        let readers = self
            .readers
            .lock()
            .map_err(|_| DatasetError::StorageCorrupt)?;
        for reference in readers.values() {
            selected.push(self.resolve_committed(reference, &self.roots, false)?);
        }
        drop(readers);
        for manifest in selected {
            insert_object(
                &mut objects,
                &manifest.schema,
                self.limits.objects.index.traversal_nodes,
            )?;
            if let Some(checkpoint) = &manifest.checkpoint {
                insert_object(
                    &mut objects,
                    checkpoint,
                    self.limits.objects.index.traversal_nodes,
                )?;
                let cp = self.files.read_checkpoint(checkpoint, &manifest.dataset)?;
                for schema in [
                    &cp.state.schema,
                    &cp.context.schema,
                    &cp.bindings.item_schema,
                    &cp.bindings.output_schema,
                ] {
                    insert_object(
                        &mut objects,
                        schema,
                        self.limits.objects.index.traversal_nodes,
                    )?;
                }
            }
            let mut pending = manifest.index.into_iter().collect::<Vec<_>>();
            while let Some(reference) = pending.pop() {
                if !insert_object(
                    &mut objects,
                    &reference,
                    self.limits.objects.index.traversal_nodes,
                )? {
                    continue;
                }
                let node = self.files.read_index(&reference, &manifest.dataset)?;
                match node.entries {
                    Entries::Leaf { entries } => {
                        for entry in entries {
                            insert_object(
                                &mut objects,
                                &entry.segment,
                                self.limits.objects.index.traversal_nodes,
                            )?;
                        }
                    }
                    Entries::Branch { children } => {
                        if children
                            .len()
                            .saturating_add(pending.len())
                            .saturating_add(objects.len())
                            > self.limits.objects.index.traversal_nodes
                        {
                            return Err(DatasetError::Limit("cleanup index"));
                        }
                        pending.extend(children.into_iter().map(|c| c.node));
                    }
                }
            }
        }
        self.rotate()?;
        let (reclaimed_bytes, shared_bytes, pending_bytes) = self.files.collect(&objects)?;
        self.verified_nodes.clear();
        self.verified_segments.clear();
        Ok(wes_engine::storage::datasets::DatasetCleanup {
            reclaimed_bytes: Some(reclaimed_bytes),
            shared_bytes: Some(shared_bytes),
            pending_bytes: Some(pending_bytes),
            complete: pending_bytes == 0,
            released_captures: self
                .references
                .values()
                .filter(|r| r.kind == catalog::RootKind::Checkpoint && r.prefixes.is_empty())
                .flat_map(|r| r.captures.iter().cloned())
                .collect(),
        })
    }
    pub fn retention_bytes(&self, reference: &DatasetRef) -> Result<u64, DatasetError> {
        self.retention_bytes_many(std::slice::from_ref(reference))
    }
    pub fn retention_bytes_many(&self, references: &[DatasetRef]) -> Result<u64, DatasetError> {
        self.retention_inventory(references, true)?.total()
    }
    fn retention_inventory(
        &self,
        references: &[DatasetRef],
        authorize: bool,
    ) -> Result<Footprint, DatasetError> {
        self.ready()?;
        let limit = self.limits.objects.index.traversal_nodes;
        let mut work = limit;
        let mut objects = BTreeMap::<String, ObjectRef>::new();
        let mut prefixes = references.to_vec();
        if prefixes.len() > limit {
            return Err(DatasetError::Limit("retention roots"));
        }
        let mut visited = BTreeSet::<String>::new();
        let mut captures = BTreeMap::<String, CapturedValue>::new();
        while let Some(reference) = prefixes.pop() {
            if !visited.insert(reference.manifest().to_owned()) {
                continue;
            }
            if visited.len().saturating_add(prefixes.len()) > limit {
                return Err(DatasetError::Limit("retention prefixes"));
            }
            let generation = self
                .roots
                .get(reference.dataset())
                .ok_or(DatasetError::Unavailable)?
                .generation;
            let distance = generation
                .checked_sub(reference.generation())
                .ok_or(DatasetError::Unavailable)?;
            debit(&mut work, distance.count_ones() as usize + 1)?;
            let selected = self.resolve_committed(&reference, &self.roots, authorize)?;
            debit(&mut work, 1)?;
            let (mut object, mut manifest) = self
                .root(reference.dataset())?
                .ok_or(DatasetError::Unavailable)?;
            let mut lineage = BTreeSet::new();
            loop {
                if !lineage.insert(object.id.clone()) || lineage.len() > limit {
                    return Err(DatasetError::StorageCorrupt);
                }
                insert_object(&mut objects, &object, limit)?;
                if manifest.generation == reference.generation() {
                    break;
                }
                let previous = manifest.previous.ok_or(DatasetError::StorageCorrupt)?;
                debit(&mut work, 1)?;
                let prior = self.files.read_manifest(&previous)?;
                if prior.dataset != reference.dataset()
                    || prior.generation.checked_add(1) != Some(manifest.generation)
                {
                    return Err(DatasetError::StorageCorrupt);
                }
                object = previous;
                manifest = prior;
            }
            insert_object(&mut objects, &selected.schema, limit)?;
            let mut pending = selected.index.into_iter().collect::<Vec<_>>();
            while let Some(node) = pending.pop() {
                if !insert_object(&mut objects, &node, limit)? {
                    continue;
                }
                debit(&mut work, 1)?;
                let node = self.files.read_index(&node, reference.dataset())?;
                match node.entries {
                    Entries::Leaf { entries } => {
                        for entry in entries {
                            insert_object(&mut objects, &entry.segment, limit)?;
                        }
                    }
                    Entries::Branch { children } => {
                        if children
                            .len()
                            .saturating_add(pending.len())
                            .saturating_add(objects.len())
                            > limit
                        {
                            return Err(DatasetError::Limit("retention index"));
                        }
                        pending.extend(children.into_iter().map(|c| c.node));
                    }
                }
            }
            if let Some(checkpoint) = &selected.checkpoint {
                insert_object(&mut objects, checkpoint, limit)?;
                debit(&mut work, 1)?;
                let cp = self.files.read_checkpoint(checkpoint, &selected.dataset)?;
                let source = CapturedValue {
                    handle: cp.bindings.source_handle.clone(),
                    digest: cp.bindings.source_digest.clone(),
                    bytes: cp.bindings.source_bytes,
                };
                if let Some(prior) = captures.get(&source.handle) {
                    if prior != &source {
                        return Err(DatasetError::StorageCorrupt);
                    }
                } else {
                    if captures.len() >= self.limits.reference_roots {
                        return Err(DatasetError::Limit("retention captures"));
                    }
                    captures.insert(source.handle.clone(), source.clone());
                }
                if let Some(root) = self.references.get(&source.handle) {
                    if root.kind != catalog::RootKind::Value {
                        return Err(DatasetError::StorageCorrupt);
                    }
                    if root
                        .prefixes
                        .len()
                        .saturating_add(prefixes.len())
                        .saturating_add(visited.len())
                        > limit
                    {
                        return Err(DatasetError::Limit("retention captured prefixes"));
                    }
                    prefixes.extend(root.prefixes.iter().cloned());
                }
                if let Some(followed) = &cp.followed_source {
                    if prefixes.len().saturating_add(visited.len()) >= limit {
                        return Err(DatasetError::Limit("retention followed prefixes"));
                    }
                    prefixes.push(followed.prefix.clone());
                }
                for schema in [
                    &cp.state.schema,
                    &cp.context.schema,
                    &cp.bindings.item_schema,
                    &cp.bindings.output_schema,
                ] {
                    insert_object(&mut objects, schema, limit)?;
                }
            }
        }
        Ok(Footprint { objects, captures })
    }
    pub(super) fn preview_retention(
        &self,
        reference: &DatasetRef,
    ) -> Result<wes_engine::storage::datasets::DatasetRetentionInventory, DatasetError> {
        let selected = self.retention_inventory(std::slice::from_ref(reference), true)?;
        let roots = self
            .references
            .values()
            .filter(|root| root.retention != RootRetention::Temporary);
        let mut prefixes = Vec::new();
        let mut seen = std::collections::HashSet::new();
        for prefix in roots.clone().flat_map(|root| &root.prefixes) {
            if seen.insert(prefix.clone()) {
                if prefixes.len() >= self.limits.objects.index.traversal_nodes {
                    return Err(DatasetError::Limit("retention roots"));
                }
                prefixes.push(prefix.clone());
            }
        }
        // Only overlap counts escape this trusted metadata walk; it grants no content access.
        let mut retained = self.retention_inventory(&prefixes, false)?;
        for capture in roots.flat_map(|root| &root.captures) {
            if retained.captures.len() >= self.limits.reference_roots
                && !retained.captures.contains_key(&capture.handle)
            {
                return Err(DatasetError::Limit("retention captures"));
            }
            match retained.captures.get(&capture.handle) {
                Some(previous) if previous != capture => return Err(DatasetError::StorageCorrupt),
                Some(_) => {}
                None => {
                    retained
                        .captures
                        .insert(capture.handle.clone(), capture.clone());
                }
            }
        }
        let mut object_bytes = 0u64;
        let mut shared_object_bytes = 0u64;
        for (id, object) in &selected.objects {
            object_bytes = sum(object_bytes, object.bytes)?;
            if let Some(shared) = retained.objects.get(id) {
                if shared != object {
                    return Err(DatasetError::StorageCorrupt);
                }
                shared_object_bytes = sum(shared_object_bytes, object.bytes)?;
            }
        }
        let mut captures = Vec::new();
        for (id, value) in selected.captures {
            let shared = match retained.captures.get(&id) {
                Some(shared) if shared != &value => return Err(DatasetError::StorageCorrupt),
                Some(_) => true,
                None => false,
            };
            captures.push(wes_engine::storage::datasets::RetentionCapture { value, shared });
        }
        Ok(wes_engine::storage::datasets::DatasetRetentionInventory {
            reference: reference.clone(),
            object_bytes,
            shared_object_bytes,
            captures,
            catalog_revision: self
                .next_sequence
                .checked_sub(1)
                .ok_or(DatasetError::StorageCorrupt)?,
        })
    }
}
#[derive(Default)]
struct Footprint {
    objects: BTreeMap<String, ObjectRef>,
    captures: BTreeMap<String, CapturedValue>,
}
impl Footprint {
    fn total(&self) -> Result<u64, DatasetError> {
        self.captures
            .values()
            .map(|value| value.bytes)
            .chain(self.objects.values().map(|object| object.bytes))
            .try_fold(0u64, sum)
    }
}
fn sum(total: u64, bytes: u64) -> Result<u64, DatasetError> {
    total
        .checked_add(bytes)
        .ok_or(DatasetError::Limit("retention bytes"))
}
fn debit(left: &mut usize, amount: usize) -> Result<(), DatasetError> {
    *left = left
        .checked_sub(amount)
        .ok_or(DatasetError::Limit("retention traversal"))?;
    Ok(())
}
fn insert_object(
    objects: &mut BTreeMap<String, ObjectRef>,
    reference: &ObjectRef,
    limit: usize,
) -> Result<bool, DatasetError> {
    if let Some(previous) = objects.get(&reference.id) {
        if previous != reference {
            return Err(DatasetError::StorageCorrupt);
        }
        return Ok(false);
    }
    if objects.len() >= limit {
        return Err(DatasetError::Limit("cleanup objects"));
    }
    objects.insert(reference.id.clone(), reference.clone());
    Ok(true)
}
