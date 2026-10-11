//! The catalog is the single publication point for dataset and reference roots.
use super::*;

impl DatasetStore {
    pub(super) fn prepare_catalog(&mut self) -> Result<(), DatasetError> {
        self.ready()?;
        self.next_sequence
            .checked_add(1)
            .ok_or(DatasetError::Limit("catalog sequence"))?;
        self.reserve_catalog(
            self.limits.catalog.frame_bytes as u64
                + if self.files.protected() {
                    (crate::protected_storage::OVERHEAD + 4) as u64
                } else {
                    0
                },
            1,
        )?;
        if self.frames + 1 >= self.limits.catalog.frames
            || self
                .physical_bytes
                .saturating_sub(self.valid_snapshot_bytes()?)
                .saturating_add(self.limits.catalog.frame_bytes)
                > self.limits.catalog.recovery_bytes
        {
            self.rotate()?;
            self.reserve_catalog(
                self.limits.catalog.frame_bytes as u64
                    + if self.files.protected() {
                        (crate::protected_storage::OVERHEAD + 4) as u64
                    } else {
                        0
                    },
                1,
            )?;
        }
        Ok(())
    }

    pub(super) fn append_commit(
        &mut self,
        commit: CatalogCommit,
        sync: impl FnOnce(&cap_std::fs::File) -> io::Result<()>,
    ) -> Result<(), DatasetError> {
        let bytes = catalog::encode_commit(&commit, self.limits.catalog)?;
        // Every acknowledged state must fit a future bounded rotation. Check before writing.
        let mut snapshot = Snapshot {
            version: 4,
            store: self.store_id().into(),
            next_sequence: self.next_sequence + 1,
            previous_digest: format!("sha256:{:x}", Sha256::digest(&bytes)),
            roots: self.roots.clone(),
            references: self.references.clone(),
            gates: self.gates.clone(),
            writes: self.writes.clone(),
        };
        self.apply_write_frame(&commit, &mut snapshot.writes, &self.roots, &self.gates)?;
        for gate in &commit.gates {
            check_gate(
                gate,
                self.gates.get(&gate.dataset),
                self.roots.contains_key(&gate.dataset),
            )?;
            snapshot.gates.insert(gate.dataset.clone(), gate.clone());
        }
        for root in &commit.roots {
            snapshot.roots.insert(root.dataset.clone(), root.clone());
        }
        for root in &commit.references {
            Self::check_reference_gates(root, self.references.get(&root.root), &snapshot.gates)?;
            if root.kind == catalog::RootKind::Workspace && root.prefixes.is_empty() {
                snapshot.references.remove(&root.root);
            } else {
                snapshot.references.insert(root.root.clone(), root.clone());
            }
        }
        self.validate_write_map(&snapshot.writes, &snapshot.roots, &snapshot.gates)?;
        encode_snapshot(&snapshot, self.limits)?;
        let bytes = crate::protected_storage::encode_log_frame(
            self.files.protection(),
            &format!("dataset/{}/catalog", self.store_id()),
            self.physical_bytes,
            &bytes,
        )?;
        let mut options = private_options();
        options.append(true);
        let mut file = self.catalog_dir.open_with(ACTIVE, &options)?;
        if !file.metadata()?.is_file() || file.metadata()?.len() != self.physical_bytes as u64 {
            return Err(DatasetError::StorageCorrupt);
        }
        self.uncertain = Some(commit.transaction.clone());
        self.uncertain_references = commit
            .references
            .iter()
            .map(|root| root.root.clone())
            .collect();
        if file.write_all(&bytes).and_then(|()| sync(&file)).is_err() {
            self.changes.send_modify(|changes| changes.all_changed());
            return Err(DatasetError::CommitUnconfirmed {
                transaction: commit.transaction,
            });
        }
        self.physical_bytes += bytes.len();
        self.valid_bytes = self.physical_bytes;
        // Accounting failure after publication also has an unconfirmed acknowledgement.
        if self.reserve_catalog(0, 0).is_err() {
            self.changes.send_modify(|changes| changes.all_changed());
            return Err(DatasetError::CommitUnconfirmed {
                transaction: commit.transaction,
            });
        }
        self.roots = snapshot.roots;
        self.references = snapshot.references;
        self.gates = snapshot.gates;
        self.writes = snapshot.writes;
        self.next_sequence = snapshot.next_sequence;
        self.previous_digest = snapshot.previous_digest;
        self.frames += 1;
        self.uncertain = None;
        self.uncertain_references.clear();
        self.changes.send_modify(|changes| {
            for root in &commit.roots {
                changes.changed(&root.dataset);
            }
            for gate in &commit.gates {
                changes.changed(&gate.dataset);
            }
        });
        Ok(())
    }
}
