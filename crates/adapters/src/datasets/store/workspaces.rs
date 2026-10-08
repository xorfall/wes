//! Saved-generation protection is admitted before journal bytes, and removed after physical retirement.
use super::*;
use sha2::{Digest, Sha256};
use wes_engine::storage::ValueHandle;

fn root_identity(generation: &str, handle: &ValueHandle) -> String {
    let mut hash = Sha256::new();
    hash.update(b"wes.workspace.dataset-root\0");
    hash.update(generation.as_bytes());
    hash.update(b"\0");
    hash.update(handle.as_str().as_bytes());
    let digest = hash.finalize();
    let mut bytes: [u8; 16] = digest[..16].try_into().expect("digest size");
    bytes[6] = (bytes[6] & 0x0f) | 0x80;
    bytes[8] = (bytes[8] & 0x3f) | 0x80;
    uuid::Uuid::from_bytes(bytes).to_string()
}

impl DatasetStore {
    pub(super) fn protect_workspace_owned(
        &mut self,
        generation: &str,
        handles: &[ValueHandle],
    ) -> Result<(), DatasetError> {
        if self.recovery_failed {
            return Err(DatasetError::StorageCorrupt);
        }
        if !catalog::valid_uuid(generation) {
            return Err(DatasetError::Conflict);
        }
        if handles.len() > 100_000 {
            return Err(DatasetError::Limit("workspace references"));
        }
        let mut admitted = BTreeSet::new();
        let mut changes = Vec::new();
        let mut has_prefixes = false;
        // Preflight the whole bounded set, including capacity and coarse access, before publication.
        for handle in handles {
            if !admitted.insert(handle.as_str()) {
                continue;
            }
            if self.uncertain_references.contains(handle.as_str()) {
                self.ready()?;
            }
            let Some(value) = self.references.get(handle.as_str()).cloned() else {
                continue;
            };
            if value.kind != catalog::RootKind::Value {
                return Err(DatasetError::Conflict);
            }
            if value.prefixes.is_empty() {
                continue;
            }
            if value.retention == RootRetention::Temporary {
                return Err(DatasetError::Conflict);
            }
            has_prefixes = true;
            let identity = root_identity(generation, handle);
            if self.uncertain_references.contains(&identity) {
                self.ready()?;
            }
            let previous = self.references.get(&identity);
            if let Some(previous) = previous {
                if previous.kind != catalog::RootKind::Workspace
                    || previous.owner_workspace.as_deref() != Some(generation)
                    || previous.prefixes != value.prefixes
                    || previous.retention != RootRetention::Protected
                {
                    return Err(DatasetError::Conflict);
                }
                continue;
            }
            for prefix in &value.prefixes {
                // This trusted metadata protects bytes only; it does not merge all
                // saved values into one data flow or create a content-read grant.
                self.resolve_committed(prefix, &self.roots, false)?;
            }
            changes.push(ReferenceRootChange {
                root: identity,
                kind: catalog::RootKind::Workspace,
                expected_generation: 0,
                generation: 1,
                prefixes: value.prefixes,
                owner_dataset: None,
                owner_workspace: Some(generation.into()),
                captures: vec![],
                retention: RootRetention::Protected,
                transaction: uuid::Uuid::new_v4().to_string(),
            });
            if changes.len()
                > self
                    .limits
                    .reference_roots
                    .saturating_sub(self.references.len())
            {
                return Err(DatasetError::Limit("workspace roots"));
            }
        }
        if !has_prefixes {
            return Ok(());
        }
        if !changes.is_empty() {
            self.ready()?;
            let mut references = self.references.clone();
            for change in &changes {
                references.insert(change.root.clone(), change.clone());
            }
            let workspaces = references
                .iter()
                .filter(|(_, root)| root.kind == catalog::RootKind::Workspace)
                .map(|(id, root)| (id.clone(), root.clone()))
                .collect::<BTreeMap<_, _>>();
            let max_roots = self
                .limits
                .workspace_roots
                .min((self.limits.reference_roots / 2).max(1));
            if workspaces.len() > max_roots {
                return Err(DatasetError::Limit("workspace roots"));
            }
            bounded_json(
                &workspaces,
                self.limits
                    .workspace_root_bytes
                    .min(self.limits.snapshot_bytes / 2),
            )?;
            let snapshot = Snapshot {
                version: 4,
                store: self.store_id().into(),
                next_sequence: self
                    .next_sequence
                    .checked_add(1)
                    .ok_or(DatasetError::Limit("catalog sequence"))?,
                previous_digest: self.previous_digest.clone(),
                roots: self.roots.clone(),
                references,
                gates: self.gates.clone(),
                writes: self.writes.clone(),
            };
            encode_snapshot(&snapshot, self.limits)?;
        }
        self.publish_workspace_changes(changes)
    }

    pub(super) fn retire_workspace_owned(&mut self, generation: &str) -> Result<(), DatasetError> {
        if self.recovery_failed {
            return Err(DatasetError::StorageCorrupt);
        }
        if !catalog::valid_uuid(generation) {
            return Err(DatasetError::Conflict);
        }
        let changes = self
            .references
            .values()
            .filter(|root| root.owner_workspace.as_deref() == Some(generation))
            .map(|root| {
                let mut change = root.clone();
                change.expected_generation = root.generation;
                change.generation = root
                    .generation
                    .checked_add(1)
                    .ok_or(DatasetError::Limit("reference generation"))?;
                change.prefixes.clear();
                change.retention = RootRetention::Temporary;
                Ok(change)
            })
            .collect::<Result<Vec<_>, DatasetError>>()?;
        self.publish_workspace_changes(changes)
    }

    fn publish_workspace_changes(
        &mut self,
        changes: Vec<ReferenceRootChange>,
    ) -> Result<(), DatasetError> {
        if changes.is_empty() {
            return Ok(());
        }
        let maximum = self.limits.catalog.roots_per_frame;
        let mut chunk = Vec::new();
        let mut prefixes = 0usize;
        for change in changes {
            if !chunk.is_empty()
                && (chunk.len() == maximum || prefixes + change.prefixes.len() > maximum)
            {
                self.publish_workspace_chunk(&mut chunk)?;
                prefixes = 0;
            }
            prefixes += change.prefixes.len();
            chunk.push(change);
        }
        self.publish_workspace_chunk(&mut chunk)
    }
    fn publish_workspace_chunk(
        &mut self,
        chunk: &mut Vec<ReferenceRootChange>,
    ) -> Result<(), DatasetError> {
        let transaction = uuid::Uuid::new_v4().to_string();
        for change in chunk.iter_mut() {
            change.transaction = transaction.clone();
        }
        self.update_workspace_references(&transaction, chunk)?;
        chunk.clear();
        Ok(())
    }
}
