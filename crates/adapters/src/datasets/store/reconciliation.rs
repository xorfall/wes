//! Durable local-write evidence shares the catalog publication boundary.
use super::super::DatasetKind;
use super::*;
use catalog::{WriteOperation, WriteState, WriteWitness};
use wes_engine::storage::datasets::{
    DatasetReconciliation, DatasetWriteOutcome, DatasetWriteOwner, DatasetWriteRole,
    DatasetWriteSelection,
};

impl DatasetStore {
    /// Store-level repair is an explicit local effect. It reads no payload and
    /// establishes no producer outcome or permission to replay an external call.
    pub(super) fn reconcile_store(
        &mut self,
    ) -> Result<wes_engine::history::Persistence, DatasetError> {
        if !self
            .active_writers
            .lock()
            .map_err(|_| DatasetError::StorageCorrupt)?
            .is_empty()
        {
            return Err(DatasetError::Limit(
                "active dataset writers; stop their owners before store recovery",
            ));
        }
        self.recover_local_catalog(&uuid::Uuid::new_v4().to_string())?;
        // All physical writers joined before this effect. A Requested witness
        // still matching its predecessor proves only local data publication absent.
        let requested = self
            .writes
            .values()
            .filter(|write| write.state == WriteState::Requested)
            .cloned()
            .collect::<Vec<_>>();
        for batch in requested.chunks(self.limits.catalog.roots_per_frame) {
            self.confirm_absent(batch)?;
        }
        Ok(match self.files.durability() {
            Durability::File => wes_engine::history::Persistence::FileSynced,
            Durability::FileAndDirectory => {
                wes_engine::history::Persistence::FileAndDirectorySynced
            }
        })
    }

    pub(super) fn admit_write(
        &mut self,
        owner: Option<&DatasetWriteOwner>,
        transaction: &str,
        dataset: &str,
        predecessor: Option<&DatasetRef>,
        kind: DatasetKind,
        operation: WriteOperation,
    ) -> Result<(), DatasetError> {
        self.admit_write_with_sync(
            owner,
            transaction,
            dataset,
            predecessor,
            kind,
            operation,
            |file| file.sync_all(),
        )
    }
    pub(super) fn admit_write_with_sync(
        &mut self,
        owner: Option<&DatasetWriteOwner>,
        transaction: &str,
        dataset: &str,
        predecessor: Option<&DatasetRef>,
        kind: DatasetKind,
        operation: WriteOperation,
        sync: impl FnOnce(&cap_std::fs::File) -> io::Result<()>,
    ) -> Result<(), DatasetError> {
        let Some(owner) = owner else {
            // Primitive fixture ports cannot bypass an existing owned writer.
            if self.writes.contains_key(dataset) {
                return Err(DatasetError::Conflict);
            }
            return Ok(());
        };
        self.ready()?;
        if !matches!(
            (owner.role, kind),
            (DatasetWriteRole::Analysis, DatasetKind::Analysis)
                | (DatasetWriteRole::Recording, DatasetKind::EventLog)
        ) {
            return Err(DatasetError::Conflict);
        }
        let admission = uuid::Uuid::new_v4().to_string();
        let witness = WriteWitness {
            dataset: dataset.into(),
            owner: owner.clone(),
            transaction: transaction.into(),
            predecessor: predecessor.cloned(),
            committed: None,
            state: WriteState::Requested,
            operation,
            admission: admission.clone(),
        };
        witness.validate(self.store_id())?;
        let mut commit = CatalogCommit {
            store: self.store_id().into(),
            sequence: self.next_sequence,
            transaction: admission.clone(),
            previous_digest: self.previous_digest.clone(),
            roots: vec![],
            references: vec![],
            gates: vec![],
            writes: vec![witness.clone()],
        };
        // Resolved failed creates own no objects. Under admission pressure, retire only
        // these receipts; unresolved requests and committed datasets are never evicted.
        let count = self
            .roots
            .keys()
            .chain(self.writes.keys())
            .collect::<BTreeSet<_>>()
            .len();
        if !self.roots.contains_key(dataset)
            && !self.writes.contains_key(dataset)
            && count >= self.limits.datasets
        {
            let Some(mut retired) = self
                .writes
                .values()
                .find(|write| {
                    write.state == WriteState::Absent
                        && write.predecessor.is_none()
                        && !self.roots.contains_key(&write.dataset)
                })
                .cloned()
            else {
                return Err(DatasetError::Limit("dataset write witnesses"));
            };
            retired.state = WriteState::Retired;
            commit.writes.insert(0, retired);
        }
        let mut preview = self.writes.clone();
        self.apply_write_frame(&commit, &mut preview, &self.roots, &self.gates)?;
        self.validate_write_map(&preview, &self.roots, &self.gates)?;
        let snapshot = Snapshot {
            version: 4,
            store: self.store_id().into(),
            next_sequence: self
                .next_sequence
                .checked_add(1)
                .ok_or(DatasetError::Limit("catalog sequence"))?,
            previous_digest: self.previous_digest.clone(),
            roots: self.roots.clone(),
            references: self.references.clone(),
            gates: self.gates.clone(),
            writes: preview,
        };
        encode_snapshot(&snapshot, self.limits)?;
        self.prepare_catalog()?;
        commit.sequence = self.next_sequence;
        commit.previous_digest = self.previous_digest.clone();
        let previous_pending = self.pending_admission.replace(witness);
        match self.append_commit(commit, sync) {
            Err(DatasetError::CommitUnconfirmed { .. }) => {
                Err(DatasetError::AdmissionUnconfirmed {
                    transaction: transaction.into(),
                    admission,
                })
            }
            Ok(()) => {
                self.pending_admission = None;
                Ok(())
            }
            Err(error) => {
                self.pending_admission = previous_pending;
                Err(error)
            }
        }
    }
    pub(super) fn confirm_write(
        &self,
        candidate: &Manifest,
        manifest: &ObjectRef,
    ) -> Result<Vec<WriteWitness>, DatasetError> {
        let Some(prior) = self.writes.get(&candidate.dataset) else {
            return Ok(vec![]);
        };
        if prior.state != WriteState::Requested || prior.transaction != candidate.transaction {
            return Err(DatasetError::Conflict);
        }
        let mut witness = prior.clone();
        witness.state = WriteState::Committed;
        witness.committed = Some(descriptor(manifest, candidate)?);
        Ok(vec![witness])
    }
    pub(super) fn apply_write_frame(
        &self,
        commit: &CatalogCommit,
        writes: &mut BTreeMap<String, WriteWitness>,
        roots: &BTreeMap<String, RootChange>,
        gates: &BTreeMap<String, catalog::ReadGate>,
    ) -> Result<(), DatasetError> {
        for next in &commit.writes {
            next.validate(self.store_id())?;
            if gates.contains_key(&next.dataset)
                || commit.gates.iter().any(|g| g.dataset == next.dataset)
            {
                return Err(DatasetError::StorageCorrupt);
            }
            let current = roots
                .get(&next.dataset)
                .map(|r| {
                    let manifest = self.files.read_manifest(&r.manifest, None)?;
                    descriptor(&r.manifest, &manifest)
                })
                .transpose()?;
            let prior = writes.get(&next.dataset);
            match next.state {
                WriteState::Requested => {
                    let continuity = match (next.operation, prior) {
                        (WriteOperation::Create, None) => current.is_none(),
                        (WriteOperation::Append, Some(prior)) => prior.owner == next.owner,
                        (WriteOperation::Resume | WriteOperation::Continue, Some(prior)) => {
                            prior.state != WriteState::Requested
                                && prior.owner.role == next.owner.role
                                && prior.owner.lineage == next.owner.lineage
                                && prior.owner.run != next.owner.run
                        }
                        // Primitive storage fixtures may adopt a confirmed unowned analysis
                        // only through the same checkpoint-validated Resume operation.
                        (WriteOperation::Resume | WriteOperation::Continue, None) => {
                            current.is_some() && next.owner.role == DatasetWriteRole::Analysis
                        }
                        _ => false,
                    };
                    if next.admission != commit.transaction
                        || next.predecessor != current
                        || !continuity
                        || prior.is_some_and(|p| p.transaction == next.transaction)
                        || commit.roots.iter().any(|r| r.dataset == next.dataset)
                    {
                        return Err(DatasetError::Conflict);
                    }
                }
                WriteState::Committed => {
                    let Some(prior) = prior else {
                        return Err(DatasetError::StorageCorrupt);
                    };
                    if prior.state != WriteState::Requested
                        || prior.owner != next.owner
                        || prior.transaction != next.transaction
                        || prior.predecessor != next.predecessor
                        || prior.operation != next.operation
                        || prior.admission != next.admission
                        || next.transaction != commit.transaction
                        || current != next.predecessor
                    {
                        return Err(DatasetError::StorageCorrupt);
                    }
                    let root = commit
                        .roots
                        .iter()
                        .find(|r| r.dataset == next.dataset)
                        .ok_or(DatasetError::StorageCorrupt)?;
                    let manifest = self.files.read_manifest(&root.manifest, None)?;
                    if next.committed.as_ref() != Some(&descriptor(&root.manifest, &manifest)?)
                        || root.generation != manifest.generation
                        || manifest.transaction != next.transaction
                        || !matches!(
                            (next.owner.role, manifest.kind),
                            (DatasetWriteRole::Analysis, DatasetKind::Analysis)
                                | (DatasetWriteRole::Recording, DatasetKind::EventLog)
                        )
                    {
                        return Err(DatasetError::StorageCorrupt);
                    }
                    if next.owner.role == DatasetWriteRole::Analysis {
                        if let Some(cp) = &manifest.checkpoint {
                            let cp = self.files.read_checkpoint(cp, &manifest.dataset, None)?;
                            if cp.run != next.owner.run || cp.analysis != next.owner.lineage {
                                return Err(DatasetError::StorageCorrupt);
                            }
                        }
                    }
                }
                WriteState::Absent => {
                    let Some(prior) = prior else {
                        return Err(DatasetError::StorageCorrupt);
                    };
                    if prior.state != WriteState::Requested
                        || prior.owner != next.owner
                        || prior.transaction != next.transaction
                        || prior.predecessor != next.predecessor
                        || prior.operation != next.operation
                        || prior.admission != next.admission
                        || current != next.predecessor
                        || commit.roots.iter().any(|r| r.dataset == next.dataset)
                    {
                        return Err(DatasetError::StorageCorrupt);
                    }
                }
                WriteState::Retired => {
                    let Some(prior) = prior else {
                        return Err(DatasetError::StorageCorrupt);
                    };
                    let mut expected = prior.clone();
                    expected.state = WriteState::Retired;
                    if *next != expected
                        || prior.state != WriteState::Absent
                        || current.is_some()
                        || next.predecessor.is_some()
                        || commit.roots.iter().any(|r| r.dataset == next.dataset)
                    {
                        return Err(DatasetError::StorageCorrupt);
                    }
                    writes.remove(&next.dataset);
                    continue;
                }
            }
            writes.insert(next.dataset.clone(), next.clone());
        }
        for gate in &commit.gates {
            writes.remove(&gate.dataset);
        }
        Ok(())
    }
    pub(super) fn validate_write_map(
        &self,
        writes: &BTreeMap<String, WriteWitness>,
        roots: &BTreeMap<String, RootChange>,
        gates: &BTreeMap<String, catalog::ReadGate>,
    ) -> Result<(), DatasetError> {
        let count = roots
            .keys()
            .chain(writes.keys())
            .collect::<BTreeSet<_>>()
            .len();
        if count > self.limits.datasets {
            return Err(DatasetError::Limit("dataset write witnesses"));
        }
        let mut owners = BTreeSet::new();
        for (dataset, write) in writes {
            write.validate(self.store_id())?;
            if dataset != &write.dataset
                || gates.contains_key(dataset)
                || write.state == WriteState::Retired
                || !owners.insert(&write.owner.run)
            {
                return Err(DatasetError::StorageCorrupt);
            }
            let expected = if write.state == WriteState::Committed {
                &write.committed
            } else {
                &write.predecessor
            };
            match (roots.get(dataset), expected) {
                (None, None) => {}
                (Some(root), Some(prefix))
                    if root.generation == prefix.generation()
                        && root.manifest.id == prefix.manifest()
                        && root.manifest.digest == prefix.manifest_digest()
                        && root.manifest.bytes == prefix.manifest_bytes() => {}
                _ => return Err(DatasetError::StorageCorrupt),
            }
        }
        Ok(())
    }
    fn recover_local_catalog(&mut self, transaction: &str) -> Result<(), DatasetError> {
        let pending = self.pending_admission.clone();
        self.reconcile(
            pending
                .as_ref()
                .map_or(transaction, |w| w.admission.as_str()),
        )?;
        if let Some(pending) = pending {
            if self
                .writes
                .get(&pending.dataset)
                .is_none_or(|w| w.transaction != pending.transaction)
            {
                // The interrupted admission never entered a data mutation. Re-establish
                // its bounded identity after fresh recovery, then record its proven absence.
                self.admit_write(
                    Some(&pending.owner),
                    &pending.transaction,
                    &pending.dataset,
                    pending.predecessor.as_ref(),
                    match pending.owner.role {
                        DatasetWriteRole::Analysis => DatasetKind::Analysis,
                        DatasetWriteRole::Recording => DatasetKind::EventLog,
                    },
                    pending.operation,
                )?;
            }
        }
        self.pending_admission = None;
        Ok(())
    }
    fn confirm_absent(&mut self, requested: &[WriteWitness]) -> Result<(), DatasetError> {
        if requested.is_empty() {
            return Ok(());
        }
        let mut writes = requested.to_vec();
        for write in &mut writes {
            write.state = WriteState::Absent;
        }
        self.prepare_catalog()?;
        let commit = CatalogCommit {
            store: self.store_id().into(),
            sequence: self.next_sequence,
            transaction: uuid::Uuid::new_v4().to_string(),
            previous_digest: self.previous_digest.clone(),
            roots: vec![],
            references: vec![],
            gates: vec![],
            writes,
        };
        self.append_commit(commit, |file| file.sync_all())
    }
    pub(super) fn selected_write(
        &self,
        selection: &DatasetWriteSelection,
    ) -> Result<Option<WriteWitness>, DatasetError> {
        if !catalog::valid_uuid(&selection.run) {
            return Err(DatasetError::Conflict);
        }
        if let Some(write) = self
            .pending_admission
            .as_ref()
            .filter(|w| w.owner.role == selection.role && w.owner.run == selection.run)
        {
            return Ok(Some(write.clone()));
        }
        if let Some(write) = self
            .writes
            .values()
            .find(|w| w.owner.role == selection.role && w.owner.run == selection.run)
        {
            if self.gates.contains_key(&write.dataset) {
                return Err(DatasetError::Withdrawn);
            }
            return Ok(Some(write.clone()));
        }
        if selection.role != DatasetWriteRole::Analysis {
            return Ok(None);
        }
        let Some(root) = self.references.get(&selection.run) else {
            return Ok(None);
        };
        if root.kind != catalog::RootKind::Checkpoint || !(1..=2).contains(&root.prefixes.len()) {
            return Err(DatasetError::Conflict);
        }
        let dataset = root
            .owner_dataset
            .as_deref()
            .ok_or(DatasetError::StorageCorrupt)?;
        if self.gates.contains_key(dataset) {
            return Err(DatasetError::Withdrawn);
        }
        let prefix = root
            .prefixes
            .iter()
            .find(|p| p.dataset() == dataset)
            .ok_or(DatasetError::StorageCorrupt)?;
        // This is ownership selection only, before global recovery. Validate the
        // exact object binding without granting a content read on a closed store.
        let object = ObjectRef {
            id: prefix.manifest().into(),
            digest: prefix.manifest_digest().into(),
            bytes: prefix.manifest_bytes(),
        };
        let manifest = self.files.read_manifest(&object, None)?;
        if descriptor(&object, &manifest)? != *prefix {
            return Err(DatasetError::StorageCorrupt);
        }
        let checkpoint = manifest
            .checkpoint
            .as_ref()
            .ok_or(DatasetError::StorageCorrupt)?;
        let cp = self.files.read_checkpoint(checkpoint, dataset, None)?;
        if cp.run != selection.run && cp.analysis != selection.run {
            return Err(DatasetError::Conflict);
        }
        let Some(write) = self
            .pending_admission
            .as_ref()
            .filter(|w| w.dataset == dataset)
            .or_else(|| self.writes.get(dataset))
        else {
            return Ok(None);
        };
        if write.owner.role != DatasetWriteRole::Analysis || write.owner.lineage != cp.analysis {
            return Err(DatasetError::StorageCorrupt);
        }
        Ok(Some(write.clone()))
    }
    fn refuse_active_reconciliation(&self, write: &WriteWitness) -> Result<(), DatasetError> {
        if self
            .active_writers
            .lock()
            .map_err(|_| DatasetError::StorageCorrupt)?
            .contains_key(&write.dataset)
        {
            return Err(DatasetError::Conflict);
        }
        Ok(())
    }
    /// The engine's FIFO worker is the barrier for all previously entered local I/O.
    pub(super) fn reconcile_owner(
        &mut self,
        selection: &DatasetWriteSelection,
    ) -> Result<DatasetReconciliation, DatasetError> {
        let mut receipt = DatasetReconciliation {
            selection: selection.clone(),
            owner: None,
            policy: FlowPolicy::default(),
            transaction: None,
            outcome: DatasetWriteOutcome::Unknown,
            predecessor: None,
            committed: None,
            persistence: wes_engine::history::Persistence::Volatile,
            execution_unknown: true,
        };
        let Some(selected) = self.selected_write(selection)? else {
            return Ok(receipt);
        };
        // Refuse before global recovery can truncate, sync or consume another writer's evidence.
        self.refuse_active_reconciliation(&selected)?;
        if self
            .pending_admission
            .as_ref()
            .is_some_and(|w| w.transaction != selected.transaction)
        {
            return Err(DatasetError::NeedsReconciliation);
        }
        self.recover_local_catalog(&selected.transaction)?;
        let Some(mut witness) = self.selected_write(selection)? else {
            return Ok(receipt);
        };
        self.refuse_active_reconciliation(&witness)?;
        if witness.state == WriteState::Requested {
            self.confirm_absent(std::slice::from_ref(&witness))?;
            witness.state = WriteState::Absent;
        }
        if let Some(prefix) = witness.committed.as_ref().or(witness.predecessor.as_ref()) {
            let manifest = self.read_exact(prefix)?;
            let policy = manifest.policy().read_from_dataset(prefix);
            if policy.is_private() || policy.is_unknown() {
                return Err(DatasetError::Restricted);
            }
            receipt.policy = policy;
        }
        receipt.owner = Some(witness.owner);
        receipt.transaction = Some(witness.transaction);
        receipt.predecessor = witness.predecessor;
        receipt.committed = witness.committed;
        receipt.persistence = match self.files.durability() {
            Durability::File => wes_engine::history::Persistence::FileSynced,
            Durability::FileAndDirectory => {
                wes_engine::history::Persistence::FileAndDirectorySynced
            }
        };
        receipt.outcome = match witness.state {
            WriteState::Committed => DatasetWriteOutcome::Committed,
            WriteState::Absent => DatasetWriteOutcome::Absent,
            _ => return Err(DatasetError::StorageCorrupt),
        };
        Ok(receipt)
    }
}
