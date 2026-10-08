//! Explicit exact-prefix roots. Keeping a prefix never protects a writer's future records.
use super::*;

enum ReferencePurpose {
    Content,
    WorkspaceRetention,
}

impl DatasetStore {
    pub(super) fn check_reference_gates(
        change: &ReferenceRootChange,
        previous: Option<&ReferenceRootChange>,
        gates: &BTreeMap<String, catalog::ReadGate>,
    ) -> Result<(), DatasetError> {
        for prefix in &change.prefixes {
            if let Some(gate) = gates.get(prefix.dataset()) {
                // Withdrawal blocks new read roots, not unchanged byte retention.
                // Deletion must remove every dependent prefix in the same frame.
                if gate.status == catalog::ReadGateStatus::Deleted
                    || (change.kind != catalog::RootKind::Workspace
                        && previous.is_none_or(|root| !root.prefixes.contains(prefix)))
                {
                    return Err(DatasetError::Withdrawn);
                }
            }
        }
        Ok(())
    }
    pub fn reference_root(&self, id: &str) -> Result<Option<ReferenceRootChange>, DatasetError> {
        self.ready()?;
        Ok(self.references.get(id).cloned())
    }

    pub fn is_protected(&self, reference: &DatasetRef) -> bool {
        self.references.values().any(|root| {
            root.retention == RootRetention::Protected
                && root.prefixes.iter().any(|prefix| prefix == reference)
        })
    }

    /// Runtime authorization precedes this owned-store operation. UUIDs are identities, not grants.
    pub fn update_references(
        &mut self,
        transaction: &str,
        changes: &[ReferenceRootChange],
        policy: &FlowPolicy,
    ) -> Result<ReferenceReceipt, DatasetError> {
        self.update_references_with_sync(transaction, changes, policy, |file| file.sync_all())
    }

    pub(super) fn update_references_with_sync(
        &mut self,
        transaction: &str,
        changes: &[ReferenceRootChange],
        policy: &FlowPolicy,
        sync: impl FnOnce(&cap_std::fs::File) -> io::Result<()>,
    ) -> Result<ReferenceReceipt, DatasetError> {
        self.update_references_for(
            transaction,
            changes,
            policy,
            ReferencePurpose::Content,
            sync,
        )
    }
    pub(super) fn update_workspace_references(
        &mut self,
        transaction: &str,
        changes: &[ReferenceRootChange],
    ) -> Result<ReferenceReceipt, DatasetError> {
        self.update_references_for(
            transaction,
            changes,
            &FlowPolicy::default(),
            ReferencePurpose::WorkspaceRetention,
            |file| file.sync_all(),
        )
    }
    fn update_references_for(
        &mut self,
        transaction: &str,
        changes: &[ReferenceRootChange],
        policy: &FlowPolicy,
        purpose: ReferencePurpose,
        sync: impl FnOnce(&cap_std::fs::File) -> io::Result<()>,
    ) -> Result<ReferenceReceipt, DatasetError> {
        let content = matches!(purpose, ReferencePurpose::Content);
        if !content
            && changes.iter().any(|root| {
                root.kind != catalog::RootKind::Workspace || root.owner_workspace.is_none()
            })
        {
            return Err(DatasetError::Conflict);
        }
        if policy.is_private() || policy.is_unknown() {
            return Err(DatasetError::Restricted);
        }
        self.ready()?;
        if content && changes.iter().any(|r| !r.prefixes.is_empty()) {
            self.check_policy_reads(policy.dataset_reads())?;
        }
        let mut commit = CatalogCommit {
            writes: vec![],
            store: self.store_id().into(),
            sequence: self.next_sequence,
            transaction: transaction.into(),
            previous_digest: self.previous_digest.clone(),
            roots: vec![],
            gates: vec![],
            references: changes.to_vec(),
        };
        // Validate all sizes and identifiers before policy checks or filesystem preparation.
        catalog::encode_commit(&commit, self.limits.catalog)?;
        let receipt = ReferenceReceipt {
            transaction: transaction.into(),
            roots: changes
                .iter()
                .map(|r| (r.root.clone(), r.generation))
                .collect(),
            persistence: self.files.durability(),
        };
        // Even an idempotent request must revalidate today's coarse authorization.
        for change in changes {
            for prefix in &change.prefixes {
                if !content
                    && self
                        .gates
                        .get(prefix.dataset())
                        .is_some_and(|gate| gate.status == catalog::ReadGateStatus::Deleted)
                {
                    return Err(DatasetError::Withdrawn);
                }
                let manifest = self.resolve_committed(prefix, &self.roots, content)?;
                if content
                    && (manifest
                        .origins
                        .iter()
                        .any(|o| !policy.origins().contains(o))
                        || manifest
                            .dataset_reads
                            .iter()
                            .any(|o| !policy.dataset_reads().contains(o)))
                {
                    return Err(DatasetError::Restricted);
                }
            }
        }
        if changes
            .iter()
            .all(|r| self.references.get(&r.root) == Some(r))
        {
            self.sync_existing()?;
            return Ok(receipt);
        }
        if self
            .references
            .values()
            .any(|r| r.transaction == transaction)
            || self.roots.values().any(|r| {
                self.files
                    .read_manifest(&r.manifest)
                    .is_ok_and(|m| m.transaction == transaction)
            })
        {
            return Err(DatasetError::Conflict);
        }
        let added = changes
            .iter()
            .filter(|r| !self.references.contains_key(&r.root))
            .count();
        if self.references.len().saturating_add(added) > self.limits.reference_roots {
            return Err(DatasetError::Limit("reference roots"));
        }
        for change in changes {
            self.check_reference_successor(change, self.references.get(&change.root))?;
        }
        self.prepare_catalog()?;
        commit.sequence = self.next_sequence;
        commit.previous_digest = self.previous_digest.clone();
        self.append_commit(commit, sync)?;
        Ok(receipt)
    }

    pub(super) fn check_reference_successor(
        &self,
        candidate: &ReferenceRootChange,
        previous: Option<&ReferenceRootChange>,
    ) -> Result<(), DatasetError> {
        if previous.map_or(0, |r| r.generation) != candidate.expected_generation
            || previous.is_some_and(|r| {
                r.kind != candidate.kind
                    || r.owner_dataset != candidate.owner_dataset
                    || r.owner_workspace != candidate.owner_workspace
                    || r.transaction == candidate.transaction
            })
        {
            return Err(DatasetError::Conflict);
        }
        // Unknown roots are conservative protection, never silently downgraded by recovery.
        if previous.is_some_and(|r| r.retention == RootRetention::Unknown)
            && candidate.retention != RootRetention::Unknown
        {
            return Err(DatasetError::Conflict);
        }
        Ok(())
    }
}
