//! Home-owned leases and management. A reader never grants writer admission.
use super::*;
use std::time::{Duration, Instant};
use wes_engine::storage::datasets::DatasetReadLease;
use wes_engine::storage::{
    Retention,
    datasets::{DatasetCleanup, DatasetDeletePlan, DatasetRootKind, DatasetRootSummary},
};

pub(super) struct Plan {
    reference: DatasetRef,
    sequence: u64,
    expires: Instant,
}
pub(super) fn check_gate(
    gate: &catalog::ReadGate,
    previous: Option<&catalog::ReadGate>,
    exists: bool,
) -> Result<(), DatasetError> {
    if !exists
        || !catalog::valid_uuid(&gate.dataset)
        || previous.map_or(0, |g| g.generation) != gate.expected_generation
        || gate.expected_generation.checked_add(1) != Some(gate.generation)
        || previous.is_some_and(|g| g.status == catalog::ReadGateStatus::Deleted)
    {
        return Err(DatasetError::Conflict);
    }
    Ok(())
}

struct Reader {
    id: String,
    readers: std::sync::Arc<std::sync::Mutex<BTreeMap<String, DatasetRef>>>,
}
struct Writer {
    dataset: String,
    token: String,
    active: std::sync::Arc<std::sync::Mutex<BTreeMap<String, String>>>,
    changes: tokio::sync::watch::Sender<wes_engine::storage::datasets::DatasetChanges>,
}
impl Drop for Writer {
    fn drop(&mut self) {
        if let Ok(mut active) = self.active.lock() {
            if active.get(&self.dataset) == Some(&self.token) {
                active.remove(&self.dataset);
                self.changes
                    .send_modify(|changes| changes.changed(&self.dataset));
            }
        }
    }
}
impl Drop for Reader {
    fn drop(&mut self) {
        if let Ok(mut readers) = self.readers.lock() {
            readers.remove(&self.id);
        }
        // Poisoning keeps conservative protection; drop performs no filesystem I/O.
    }
}
impl DatasetStore {
    pub(super) fn admit_writer(
        &mut self,
        reference: DatasetRef,
    ) -> Result<wes_engine::storage::datasets::DatasetWriterAdmission, DatasetError> {
        let token = uuid::Uuid::new_v4().to_string();
        let mut active = self
            .active_writers
            .lock()
            .map_err(|_| DatasetError::StorageCorrupt)?;
        if active.contains_key(reference.dataset()) {
            return Err(DatasetError::Conflict);
        }
        active.insert(reference.dataset().to_owned(), token.clone());
        drop(active);
        self.changes
            .send_modify(|changes| changes.changed(reference.dataset()));
        let lease = DatasetReadLease::retain(Writer {
            dataset: reference.dataset().to_owned(),
            token,
            active: self.active_writers.clone(),
            changes: self.changes.clone(),
        });
        Ok(wes_engine::storage::datasets::DatasetWriterAdmission { reference, lease })
    }
    pub(super) fn acknowledge_captures(
        &mut self,
        captures: &[CapturedValue],
    ) -> Result<(), DatasetError> {
        self.ready()?;
        if captures.len()
            > self
                .limits
                .reference_roots
                .saturating_mul(self.limits.catalog.roots_per_frame)
        {
            return Err(DatasetError::Limit("cleanup captures"));
        }
        let mut pending = Vec::new();
        for root in self
            .references
            .values()
            .filter(|r| r.kind == catalog::RootKind::Checkpoint && r.prefixes.is_empty())
        {
            let remaining = root
                .captures
                .iter()
                .filter(|c| !captures.contains(c))
                .cloned()
                .collect::<Vec<_>>();
            if remaining.len() != root.captures.len() {
                let mut change = root.clone();
                change.expected_generation = root.generation;
                change.generation = root
                    .generation
                    .checked_add(1)
                    .ok_or(DatasetError::Limit("cleanup root generation"))?;
                change.captures = remaining;
                pending.push(change);
            }
        }
        for batch in pending.chunks_mut(self.limits.catalog.roots_per_frame) {
            let transaction = uuid::Uuid::new_v4().to_string();
            for root in batch.iter_mut() {
                root.transaction = transaction.clone();
            }
            self.update_references(&transaction, batch, &FlowPolicy::default())?;
        }
        Ok(())
    }
    pub(super) fn check_policy_reads(
        &self,
        origins: &BTreeSet<wes_core::flow::DatasetReadOrigin>,
    ) -> Result<(), DatasetError> {
        self.ready()?;
        for origin in origins {
            if origin.store() != self.store_id() || !self.roots.contains_key(origin.dataset()) {
                return Err(DatasetError::Unavailable);
            }
            if self.gates.contains_key(origin.dataset()) {
                return Err(DatasetError::Withdrawn);
            }
        }
        Ok(())
    }
    pub(super) fn acquire_reader(
        &self,
        reference: &DatasetRef,
    ) -> Result<DatasetReadLease, DatasetError> {
        // The common page path subsequently validates exact committed identity.
        // Register before I/O; a failing read releases this owned lifetime.
        if reference.store() != self.store_id() {
            return Err(DatasetError::Unavailable);
        }
        let mut readers = self
            .readers
            .lock()
            .map_err(|_| DatasetError::StorageCorrupt)?;
        if readers.len() >= self.limits.reference_roots {
            return Err(DatasetError::Limit("reader leases"));
        }
        let id = uuid::Uuid::new_v4().to_string();
        readers.insert(id.clone(), reference.clone());
        Ok(DatasetReadLease::retain(Reader {
            id,
            readers: self.readers.clone(),
        }))
    }
    fn reader_count(&self, dataset: &str) -> Result<u64, DatasetError> {
        let readers = self
            .readers
            .lock()
            .map_err(|_| DatasetError::StorageCorrupt)?;
        Ok(readers.values().filter(|r| r.dataset() == dataset).count() as u64)
    }
    pub(super) fn plan_delete_owned(
        &mut self,
        reference: &DatasetRef,
    ) -> Result<DatasetDeletePlan, DatasetError> {
        self.ready()?;
        self.read_exact(reference)?;
        let now = Instant::now();
        self.deletion_plans.retain(|_, p| p.expires > now);
        if self.deletion_plans.len() >= 64 {
            return Err(DatasetError::Limit("deletion plans"));
        }
        let references = self
            .references
            .values()
            .filter(|r| {
                r.prefixes
                    .iter()
                    .any(|p| p.dataset() == reference.dataset())
            })
            .map(|r| DatasetRootSummary {
                identity: r.root.clone(),
                kind: match r.kind {
                    catalog::RootKind::Value => DatasetRootKind::Value,
                    catalog::RootKind::Workspace => DatasetRootKind::Workspace,
                    catalog::RootKind::Keep => DatasetRootKind::Keep,
                    catalog::RootKind::Pin => DatasetRootKind::Pin,
                    catalog::RootKind::Checkpoint => DatasetRootKind::Checkpoint,
                    catalog::RootKind::Recording => DatasetRootKind::Recording,
                },
                retention: match r.retention {
                    RootRetention::Temporary => Retention::Temporary,
                    RootRetention::Automatic => Retention::Automatic,
                    RootRetention::Protected => Retention::Protected,
                    RootRetention::Unknown => Retention::Unknown,
                },
            })
            .collect::<Vec<_>>();
        let protected_bytes = if references
            .iter()
            .any(|r| r.retention == Retention::Protected || r.retention == Retention::Unknown)
        {
            self.retention_bytes(reference)?
        } else {
            0
        };
        let token = uuid::Uuid::new_v4().to_string();
        self.deletion_plans.insert(
            token.clone(),
            Plan {
                reference: reference.clone(),
                sequence: self.next_sequence,
                expires: now + Duration::from_secs(300),
            },
        );
        Ok(DatasetDeletePlan {
            token,
            reference: reference.clone(),
            references,
            protected_bytes,
            active_readers: self.reader_count(reference.dataset())?,
            active_writer: self
                .active_writers
                .lock()
                .map_err(|_| DatasetError::StorageCorrupt)?
                .contains_key(reference.dataset()),
        })
    }
    pub(super) fn delete_owned(
        &mut self,
        token: &str,
        remove_references: bool,
        protected: bool,
    ) -> Result<DatasetCleanup, DatasetError> {
        self.ready()?;
        let plan = self
            .deletion_plans
            .get(token)
            .ok_or(DatasetError::Conflict)?;
        if plan.expires <= Instant::now() || plan.sequence != self.next_sequence {
            return Err(DatasetError::Conflict);
        }
        let reference = plan.reference.clone();
        self.read_exact(&reference)?;
        if self
            .active_writers
            .lock()
            .map_err(|_| DatasetError::StorageCorrupt)?
            .contains_key(reference.dataset())
            || self.reader_count(reference.dataset())? != 0
        {
            return Err(DatasetError::Limit(
                "active dataset reader or writer; stop its owner explicitly",
            ));
        }
        let transaction = uuid::Uuid::new_v4().to_string();
        let mut changes = Vec::new();
        let mut released = Vec::new();
        for root in self.references.values().filter(|r| {
            r.prefixes
                .iter()
                .any(|p| p.dataset() == reference.dataset())
        }) {
            if !remove_references
                || root.retention == RootRetention::Unknown
                || (root.retention == RootRetention::Protected && !protected)
            {
                return Err(DatasetError::Limit(
                    "dependent or protected roots need explicit removal",
                ));
            }
            let mut change = root.clone();
            change.expected_generation = root.generation;
            change.generation = root
                .generation
                .checked_add(1)
                .ok_or(DatasetError::Limit("root generation"))?;
            change.transaction = transaction.clone();
            if change.owner_dataset.as_deref() == Some(reference.dataset()) {
                change.prefixes.clear();
            } else {
                change
                    .prefixes
                    .retain(|p| p.dataset() != reference.dataset());
            }
            if change.prefixes.is_empty() {
                released.extend(change.captures.iter().cloned());
                change.retention = RootRetention::Temporary;
            }
            changes.push(change);
        }
        let prior = self.gates.get(reference.dataset());
        let expected_generation = prior.map_or(0, |g| g.generation);
        let gate = catalog::ReadGate {
            dataset: reference.dataset().into(),
            expected_generation,
            generation: expected_generation
                .checked_add(1)
                .ok_or(DatasetError::Limit("read gate generation"))?,
            status: catalog::ReadGateStatus::Deleted,
        };
        let commit = CatalogCommit {
            writes: vec![],
            store: self.store_id().into(),
            sequence: self.next_sequence,
            transaction,
            previous_digest: self.previous_digest.clone(),
            roots: vec![],
            references: changes,
            gates: vec![gate],
        };
        // Bound and revalidate before consuming authority or writing any control fact.
        catalog::encode_commit(&commit, self.limits.catalog)?;
        self.prepare_catalog()?;
        self.deletion_plans.remove(token);
        self.append_commit(commit, |file| file.sync_all())?;
        // The gate/root transaction has already committed. A cleanup failure
        // cannot turn that acknowledged deletion into an apparently retryable
        // mutation. Unknown physical counts remain unknown; Collect can retry
        // local reclamation explicitly without replaying deletion authority.
        let mut cleanup = self.collect_owned().unwrap_or_default();
        released.sort_by(|a, b| a.handle.cmp(&b.handle));
        released.dedup_by(|a, b| a.handle == b.handle);
        cleanup.released_captures = released;
        Ok(cleanup)
    }
    /// Restart-safe revocation contains only opaque identities and a coarse bit.
    pub(super) fn withdraw_owned(
        &mut self,
        reference: &DatasetRef,
    ) -> Result<Vec<String>, DatasetError> {
        self.ready()?;
        self.resolve_committed(reference, &self.roots, false)?;
        let mut affected = BTreeSet::from([reference.dataset().to_owned()]);
        for _ in 0..self.limits.reference_roots {
            let before = affected.len();
            for root in self.roots.values() {
                let manifest = self.files.read_manifest(&root.manifest, None)?;
                if manifest
                    .dataset_reads
                    .iter()
                    .any(|o| o.store() == self.store_id() && affected.contains(o.dataset()))
                {
                    affected.insert(root.dataset.clone());
                }
            }
            for root in self.references.values() {
                let tainted = root.captures.iter().any(|capture| {
                    self.references.get(&capture.handle).is_some_and(|source| {
                        source
                            .prefixes
                            .iter()
                            .any(|p| affected.contains(p.dataset()))
                    })
                });
                if tainted {
                    if let Some(owner) = &root.owner_dataset {
                        affected.insert(owner.clone());
                    } else {
                        affected.extend(root.prefixes.iter().map(|p| p.dataset().to_owned()));
                    }
                }
            }
            if affected.len() == before {
                break;
            }
        }
        let gates = affected
            .iter()
            .filter(|id| !self.gates.contains_key(*id))
            .map(|id| catalog::ReadGate {
                dataset: id.clone(),
                expected_generation: 0,
                generation: 1,
                status: catalog::ReadGateStatus::Restricted,
            })
            .collect::<Vec<_>>();
        if gates.is_empty() {
            return Ok(affected.into_iter().collect());
        }
        let commit = CatalogCommit {
            writes: vec![],
            store: self.store_id().into(),
            sequence: self.next_sequence,
            transaction: uuid::Uuid::new_v4().to_string(),
            previous_digest: self.previous_digest.clone(),
            roots: vec![],
            references: vec![],
            gates,
        };
        catalog::encode_commit(&commit, self.limits.catalog)?;
        self.prepare_catalog()?;
        self.append_commit(commit, |file| file.sync_all())?;
        self.active_writers
            .lock()
            .map_err(|_| DatasetError::StorageCorrupt)?
            .retain(|id, _| !affected.contains(id));
        Ok(affected.into_iter().collect())
    }
}
