//! Catalog-authoritative dataset mutations, serialized by the home's storage worker.
use super::{
    FormatError, Lifecycle, Manifest, ObjectError, ObjectFiles, ObjectLimits,
    catalog::{
        self, CatalogCommit, CatalogLimits, ObjectRef, ReferenceRootChange, RootChange,
        RootRetention,
    },
    format::bounded_json,
    manifest::Persistence,
    tree::Entries,
};
use crate::filesystem::{Durability, private_options, sync_directory};
use cap_std::fs::Dir;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet},
    io::{self, Read, Write},
    path::Path,
};
use thiserror::Error;
use wes_core::flow::FlowPolicy;
use wes_core::{DatasetRef, contracts::ResolvedContractBundle};
use wes_engine::storage::datasets::CapturedValue;

const MAGIC: &[u8; 8] = b"WESCPS04";
const END: &[u8; 8] = b"CPSEND04";
const OVERHEAD: usize = 8 + 2 + 4 + 32 + 8;
const ACTIVE: &str = "active";

mod checkpoints;
mod management;
mod port;
mod reachability;
mod reconciliation;
mod references;
mod transaction;
mod traversal;
mod workspaces;
mod writer;
use management::check_gate;
use traversal::{IndexVisit, Visit, walk_index};

#[derive(Clone, Copy, Debug)]
pub struct StoreLimits {
    pub objects: ObjectLimits,
    pub catalog: CatalogLimits,
    pub snapshot_bytes: usize,
    pub datasets: usize,
    pub reference_roots: usize,
    pub workspace_roots: usize,
    pub workspace_root_bytes: usize,
}
impl Default for StoreLimits {
    fn default() -> Self {
        Self {
            objects: ObjectLimits::default(),
            catalog: CatalogLimits::default(),
            snapshot_bytes: 1024 * 1024,
            datasets: wes_budgets::get("dataset.roots") as usize,
            reference_roots: wes_budgets::get("dataset.reference.roots") as usize,
            workspace_roots: wes_budgets::get("dataset.workspace.roots") as usize,
            workspace_root_bytes: wes_budgets::get("dataset.workspace.root.bytes") as usize,
        }
    }
}
#[derive(Debug, Error)]
pub enum DatasetError {
    #[error(transparent)]
    Objects(ObjectError),
    #[error(transparent)]
    Format(#[from] FormatError),
    #[error("dataset catalog filesystem operation failed")]
    Io(#[from] io::Error),
    #[error("dataset expected generation or transaction does not match")]
    Conflict,
    #[error("a newer analysis attempt owns the latest checkpoint")]
    NewerAttempt,
    #[error("dataset committed storage is corrupt")]
    StorageCorrupt,
    #[error("dataset transaction {transaction} has an unconfirmed commit outcome")]
    CommitUnconfirmed { transaction: String },
    #[error(
        "dataset write {transaction} has an unconfirmed admission {admission}; no data mutation was entered"
    )]
    AdmissionUnconfirmed {
        transaction: String,
        admission: String,
    },
    #[error("dataset catalog needs explicit reconciliation")]
    NeedsReconciliation,
    #[error("dataset operation exceeds its {0} budget")]
    Limit(&'static str),
    #[error("a dataset row exceeds its logical charge limit ({limit})")]
    RowCharge { limit: u64 },
    #[error("a dataset row exceeds its encoded page-byte limit ({limit})")]
    RowBytes { limit: u64 },
    #[error("dataset input is private or has unknown policy")]
    Restricted,
    #[error(
        "dataset descriptor is missing, belongs to another store or is not a committed generation"
    )]
    Unavailable,
    #[error("dataset read authorization has been withdrawn")]
    Withdrawn,
    #[error("dataset ordinal is outside the selected committed extent")]
    Range,
}
impl From<ObjectError> for DatasetError {
    fn from(error: ObjectError) -> Self {
        match error {
            ObjectError::Format(FormatError::Restricted) => Self::Restricted,
            error => Self::Objects(error),
        }
    }
}
#[derive(Clone, Copy, Debug)]
pub struct PageLimits {
    pub rows: usize,
    /// Sum of stored row payload bytes, excluding frames, schema and reply envelopes.
    pub bytes: usize,
    pub segments: usize,
    pub charge: Option<wes_engine::storage::datasets::PageCharge>,
}
impl Default for PageLimits {
    fn default() -> Self {
        Self {
            rows: wes_budgets::get("dataset.page.rows") as usize,
            bytes: wes_budgets::get("dataset.page.bytes") as usize,
            segments: wes_budgets::get("dataset.page.segments") as usize,
            charge: None,
        }
    }
}
#[derive(Clone, Debug)]
pub struct DatasetPage {
    pub reference: DatasetRef,
    pub schema: ResolvedContractBundle,
    pub first: u64,
    pub next: u64,
    pub rows: Vec<super::Record>,
    /// Exhaustion of this selected immutable extent, never proof of producer EOF.
    pub extent_exhausted: bool,
    pub limited_by: Option<&'static str>,
    pub encoded_row_bytes: usize,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CommitReceipt {
    pub transaction: String,
    pub dataset: String,
    pub generation: u64,
    pub manifest: ObjectRef,
    pub records: u64,
    pub persistence: Durability,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Reconciliation {
    Committed(CommitReceipt),
    ReferencesCommitted(ReferenceReceipt),
    WriteAdmitted,
    /// Absence is only confirmed for the locally pending or exact latest attempted transaction.
    Absent,
    Unknown,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReferenceReceipt {
    pub transaction: String,
    pub roots: Vec<(String, u64)>,
    pub persistence: Durability,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Snapshot {
    version: u16,
    store: String,
    next_sequence: u64,
    previous_digest: String,
    roots: BTreeMap<String, RootChange>,
    references: BTreeMap<String, ReferenceRootChange>,
    gates: BTreeMap<String, catalog::ReadGate>,
    writes: BTreeMap<String, catalog::WriteWitness>,
}
pub struct DatasetStore {
    files: ObjectFiles,
    catalog_dir: Dir,
    limits: StoreLimits,
    snapshot: Snapshot,
    roots: BTreeMap<String, RootChange>,
    references: BTreeMap<String, ReferenceRootChange>,
    gates: BTreeMap<String, catalog::ReadGate>,
    writes: BTreeMap<String, catalog::WriteWitness>,
    next_sequence: u64,
    previous_digest: String,
    frames: usize,
    physical_bytes: usize,
    valid_bytes: usize,
    uncertain: Option<String>,
    uncertain_references: BTreeSet<String>,
    pending_admission: Option<catalog::WriteWitness>,
    torn_tail: bool,
    verified_nodes: BTreeSet<(super::Stream, String, String, String, String)>,
    /// Immutable proof reuse across copy-on-write leaf revisions. Exact entry,
    /// schema and policy bindings are hashed; no payload or read authority is cached.
    verified_segments: BTreeSet<(super::Stream, String, String, String, String, String)>,
    recovery_failed: bool,
    /// Process-owned writer admission; restoring a catalog never restores execution permission.
    active_writers: std::sync::Arc<std::sync::Mutex<BTreeMap<String, String>>>,
    readers: std::sync::Arc<std::sync::Mutex<BTreeMap<String, DatasetRef>>>,
    deletion_plans: BTreeMap<String, management::Plan>,
    changes: tokio::sync::watch::Sender<wes_engine::storage::datasets::DatasetChanges>,
}
impl DatasetStore {
    pub fn open(
        path: &Path,
        durability: Durability,
        limits: StoreLimits,
    ) -> Result<Self, DatasetError> {
        Self::open_protected(path, durability, limits, None)
    }
    pub fn open_protected(
        path: &Path,
        durability: Durability,
        limits: StoreLimits,
        protection: Option<std::sync::Arc<crate::protected_storage::ObjectProtection>>,
    ) -> Result<Self, DatasetError> {
        if limits.datasets == 0
            || limits.reference_roots == 0
            || limits.workspace_roots == 0
            || limits.workspace_root_bytes == 0
            || limits.snapshot_bytes < OVERHEAD
            || limits.catalog.frames < 2
            || limits.catalog.recovery_bytes < limits.catalog.frame_bytes
            // Every reachable tree object is a physical inventory entry. Cached append proofs
            // must never admit a home whose uncached recovery or cleanup cannot walk it.
            || limits.objects.inventory_entries > limits.objects.index.traversal_nodes
        {
            return Err(DatasetError::Limit("configuration"));
        }
        let mut files = ObjectFiles::open_protected(path, durability, limits.objects, protection)?;
        let catalog_dir = files.catalog_directory()?;
        let existing =
            read_active_protected(&catalog_dir, limits, files.protection(), files.store_id())?;
        let snapshot = if let Some(bytes) = &existing {
            decode_snapshot(&bytes.bytes, files.store_id(), limits)?.0
        } else {
            if files.has_objects() || catalog_dir.entries()?.next().is_some() {
                return Err(DatasetError::StorageCorrupt);
            }
            let snapshot = Snapshot {
                version: 4,
                store: files.store_id().into(),
                next_sequence: 1,
                previous_digest: catalog::GENESIS.into(),
                roots: BTreeMap::new(),
                references: BTreeMap::new(),
                gates: BTreeMap::new(),
                writes: BTreeMap::new(),
            };
            let bytes = encode_snapshot(&snapshot, limits)?;
            let bytes = crate::protected_storage::encode_log_frame(
                files.protection(),
                &format!("dataset/{}/catalog", files.store_id()),
                0,
                &bytes,
            )?;
            files.account_external(bytes.len() as u64, 1)?;
            let mut options = private_options();
            options.write(true).create_new(true);
            let mut file = catalog_dir.open_with(ACTIVE, &options)?;
            file.write_all(&bytes)?;
            file.sync_all()?;
            if durability == Durability::FileAndDirectory {
                sync_directory(&catalog_dir)?;
            }
            snapshot
        };
        let mut store = Self {
            files,
            catalog_dir,
            limits,
            roots: snapshot.roots.clone(),
            references: snapshot.references.clone(),
            gates: snapshot.gates.clone(),
            writes: snapshot.writes.clone(),
            next_sequence: snapshot.next_sequence,
            previous_digest: snapshot.previous_digest.clone(),
            snapshot,
            frames: 0,
            physical_bytes: 0,
            valid_bytes: 0,
            uncertain: None,
            uncertain_references: Default::default(),
            pending_admission: None,
            torn_tail: false,
            verified_nodes: BTreeSet::new(),
            verified_segments: BTreeSet::new(),
            recovery_failed: false,
            active_writers: Default::default(),
            readers: Default::default(),
            deletion_plans: Default::default(),
            changes: tokio::sync::watch::channel(Default::default()).0,
        };
        store.reload()?;
        Ok(store)
    }
    pub fn store_id(&self) -> &str {
        self.files.store_id()
    }
    pub fn charged_bytes(&self) -> u64 {
        self.files.charged_bytes()
    }
    /// Immutable object preparation confers no root visibility or actor grant.
    pub fn objects(&self) -> &ObjectFiles {
        &self.files
    }
    pub fn prepare(&mut self) -> Result<&mut ObjectFiles, DatasetError> {
        self.ready()?;
        Ok(&mut self.files)
    }
    pub fn root(&self, dataset: &str) -> Result<Option<(ObjectRef, Manifest)>, DatasetError> {
        if self.recovery_failed {
            return Err(DatasetError::StorageCorrupt);
        }
        let Some(root) = self.roots.get(dataset) else {
            return Ok(None);
        };
        let manifest = self
            .files
            .read_manifest(&root.manifest, None)
            .map_err(|_| DatasetError::StorageCorrupt)?;
        if manifest.dataset != dataset || manifest.generation != root.generation {
            return Err(DatasetError::StorageCorrupt);
        }
        Ok(Some((root.manifest.clone(), manifest)))
    }
    pub fn descriptor(&self, dataset: &str) -> Result<DatasetRef, DatasetError> {
        let (reference, manifest) = self.root(dataset)?.ok_or(DatasetError::Unavailable)?;
        descriptor(&reference, &manifest)
    }
    /// Storage identity validation is additional to, never a replacement for, the runtime's
    /// guarded result read. An uncommitted physical manifest cannot be laundered into a read.
    pub fn read_exact(&self, reference: &DatasetRef) -> Result<Manifest, DatasetError> {
        self.resolve_committed(reference, &self.roots, true)
    }
    fn resolve_committed(
        &self,
        reference: &DatasetRef,
        roots: &BTreeMap<String, RootChange>,
        authorize: bool,
    ) -> Result<Manifest, DatasetError> {
        self.resolve_with_work(reference, roots, authorize, None)
    }
    fn resolve_with_work(
        &self,
        reference: &DatasetRef,
        roots: &BTreeMap<String, RootChange>,
        authorize: bool,
        work: Option<&wes_engine::storage::datasets::ReadWork>,
    ) -> Result<Manifest, DatasetError> {
        let (object, manifest) =
            self.committed_generation(reference, reference.generation(), roots, authorize, work)?;
        if &descriptor(&object, &manifest)? != reference {
            return Err(DatasetError::Unavailable);
        }
        Ok(manifest)
    }
    fn committed_generation(
        &self,
        anchor: &DatasetRef,
        generation: u64,
        roots: &BTreeMap<String, RootChange>,
        authorize: bool,
        work: Option<&wes_engine::storage::datasets::ReadWork>,
    ) -> Result<(ObjectRef, Manifest), DatasetError> {
        let reference = anchor;
        if generation == 0 {
            return Err(DatasetError::Unavailable);
        }
        if self.recovery_failed && std::ptr::eq(roots, &self.roots) {
            return Err(DatasetError::StorageCorrupt);
        }
        if authorize && self.gates.contains_key(reference.dataset()) {
            return Err(DatasetError::Withdrawn);
        }
        if reference.store() != self.store_id() {
            return Err(DatasetError::Unavailable);
        }
        let root = roots
            .get(reference.dataset())
            .ok_or(DatasetError::Unavailable)?;
        let mut object = root.manifest.clone();

        let current = self
            .files
            .read_manifest(&object, work)
            .map_err(DatasetError::from)?;
        if current.dataset != reference.dataset() || current.generation != root.generation {
            return Err(DatasetError::StorageCorrupt);
        }
        if authorize
            && (current.authorization_generation != reference.authorization_generation()
                || matches!(
                    current.lifecycle,
                    Lifecycle::Restricted | Lifecycle::Deleted
                ))
        {
            return Err(DatasetError::Withdrawn);
        }
        if current.generation < generation {
            return Err(DatasetError::Unavailable);
        }
        let mut manifest = current;
        for _ in 0..u64::BITS {
            if manifest.generation == generation {
                if authorize {
                    self.check_policy_reads(&manifest.dataset_reads.iter().cloned().collect())?;
                }
                return Ok((object, manifest));
            }
            let distance = manifest.generation - generation;
            let jump = (u64::BITS - 1 - distance.leading_zeros()) as usize;
            let expected = manifest
                .generation
                .checked_sub(1u64 << jump)
                .ok_or(DatasetError::StorageCorrupt)?;
            object = manifest
                .ancestors
                .get(jump)
                .cloned()
                .ok_or(DatasetError::StorageCorrupt)?;

            let previous = self
                .files
                .read_manifest(&object, work)
                .map_err(DatasetError::from)?;
            if previous.dataset != reference.dataset() || previous.generation != expected {
                return Err(DatasetError::StorageCorrupt);
            }
            manifest = previous;
        }
        Err(DatasetError::Limit("generation traversal"))
    }
    pub fn page(
        &self,
        reference: &DatasetRef,
        from: u64,
        requested: usize,
        limits: PageLimits,
    ) -> Result<DatasetPage, DatasetError> {
        self.page_with_work(
            reference,
            super::Stream::Outputs,
            from,
            requested,
            limits,
            None,
        )
    }
    fn page_with_work(
        &self,
        reference: &DatasetRef,
        stream: super::Stream,
        from: u64,
        requested: usize,
        limits: PageLimits,
        work: Option<&wes_engine::storage::datasets::ReadWork>,
    ) -> Result<DatasetPage, DatasetError> {
        if requested == 0 || requested > limits.rows || limits.bytes == 0 || limits.segments == 0 {
            return Err(DatasetError::Limit("page configuration"));
        }
        let manifest = self.resolve_with_work(reference, &self.roots, true, work)?;
        let selected = manifest
            .streams()
            .find(|root| root.stream == stream)
            .ok_or(DatasetError::Unavailable)?;
        if from > selected.summary.end {
            return Err(DatasetError::Range);
        }

        let schema = self
            .files
            .read_schema(selected.schema, work)
            .map_err(DatasetError::from)?;
        let mut page = DatasetPage {
            reference: reference.clone(),
            schema,
            first: from,
            next: from,
            rows: Vec::new(),
            extent_exhausted: from == selected.summary.end,
            limited_by: None,
            encoded_row_bytes: 0,
        };
        let mut index = selected
            .index
            .map(|root| {
                self.files.range(
                    root,
                    &manifest.dataset,
                    stream,
                    selected.summary,
                    from,
                    work,
                )
            })
            .transpose()?;
        let mut segments = 0;
        let mut retained_charge = 0;
        while page.next < selected.summary.end && page.rows.len() < requested {
            if segments == limits.segments {
                page.limited_by = Some("segments");
                break;
            }
            let entry = index
                .as_mut()
                .ok_or(DatasetError::StorageCorrupt)?
                .next_entry()?
                .ok_or(DatasetError::StorageCorrupt)?;

            let bytes = self
                .files
                .read_segment(
                    &entry.segment,
                    &manifest.dataset,
                    stream,
                    &page.schema,
                    work,
                )
                .map_err(DatasetError::from)?;
            let reader = super::SegmentReader::open(
                &bytes,
                self.store_id(),
                &manifest.dataset,
                stream,
                &page.schema,
                self.limits.objects.segment,
            )?;
            if reader.header().first != entry.summary.first
                || reader.header().first.checked_add(reader.header().count)
                    != Some(entry.summary.end)
                || reader.header().source != entry.source
                || !segment_coverage_matches(reader.header(), &entry, &manifest, stream)
            {
                return Err(DatasetError::StorageCorrupt);
            }
            segments += 1;
            while page.next < entry.summary.end && page.rows.len() < requested {
                let encoded_bytes = reader
                    .encoded_row_bytes(page.next)
                    .ok_or(DatasetError::StorageCorrupt)?;
                if encoded_bytes > limits.bytes - page.encoded_row_bytes {
                    if !page.rows.is_empty() {
                        page.limited_by = Some("bytes");
                        return Ok(page);
                    }
                    return Err(DatasetError::RowBytes {
                        limit: limits.bytes as u64,
                    });
                }
                let mut row = reader.row(page.next)?.ok_or(DatasetError::StorageCorrupt)?;
                let policy = manifest.policy();
                if policy.join(row.value.provenance().policy()) != policy {
                    return Err(DatasetError::Restricted);
                }
                row.value = row
                    .value
                    .with_provenance(row.value.provenance().clone().with_policy(&policy));
                let row_charge = if let Some(budget) = limits.charge {
                    use wes_engine::storage::datasets::PageChargeRefusal;
                    match budget.admit(&row.value, retained_charge) {
                        Ok(amount) => amount,
                        Err(PageChargeRefusal::Retained) if !page.rows.is_empty() => {
                            page.limited_by = Some("logical charge");
                            return Ok(page);
                        }
                        Err(PageChargeRefusal::Row { limit }) => {
                            if !page.rows.is_empty() {
                                page.limited_by = Some("logical charge");
                                return Ok(page);
                            }
                            return Err(DatasetError::RowCharge { limit });
                        }
                        // An admitted row fits an empty window by construction.
                        Err(PageChargeRefusal::Retained) => {
                            return Err(DatasetError::StorageCorrupt);
                        }
                    }
                } else {
                    0
                };
                page.encoded_row_bytes += encoded_bytes;
                retained_charge += row_charge;
                page.rows.push(row);
                page.next += 1;
            }
        }
        page.extent_exhausted = page.next == selected.summary.end;
        if !page.extent_exhausted && page.rows.len() == requested {
            page.limited_by = Some("rows");
        }
        Ok(page)
    }
    fn committed_head(
        &self,
        reference: &DatasetRef,
        recording_only: bool,
        work: Option<&wes_engine::storage::datasets::ReadWork>,
    ) -> Result<wes_engine::storage::datasets::DatasetInfo, DatasetError> {
        self.ready()?;
        let original = self.resolve_with_work(reference, &self.roots, true, work)?;
        if recording_only && original.recording.is_none() {
            return Err(DatasetError::Conflict);
        }
        let root = self
            .roots
            .get(reference.dataset())
            .ok_or(DatasetError::Unavailable)?;

        let latest = self
            .files
            .read_manifest(&root.manifest, work)
            .map_err(DatasetError::from)?;
        self.check_same_attempt(&original, &latest, work)?;
        self.check_policy_reads(&latest.dataset_reads.iter().cloned().collect())?;
        let head = descriptor(&root.manifest, &latest)?;
        self.info_from_manifest(&head, latest, work)
    }
    fn check_same_attempt(
        &self,
        original: &Manifest,
        target: &Manifest,
        work: Option<&wes_engine::storage::datasets::ReadWork>,
    ) -> Result<(), DatasetError> {
        if target.schema_digest != original.schema_digest
            || target.kind != original.kind
            || target.source.identity != original.source.identity
            || target
                .recording
                .as_ref()
                .map(|r| (&r.run, &r.epoch, r.first))
                != original
                    .recording
                    .as_ref()
                    .map(|r| (&r.run, &r.epoch, r.first))
        {
            return Err(DatasetError::Conflict);
        }
        match (&original.checkpoint, &target.checkpoint) {
            (Some(old), Some(new)) => {
                let old = self.files.read_checkpoint(old, &original.dataset, work)?;
                let new = self.files.read_checkpoint(new, &target.dataset, work)?;
                if old.analysis != new.analysis || old.attempt != new.attempt || old.run != new.run
                {
                    return Err(DatasetError::Conflict);
                }
            }
            (None, None) => {}
            _ => return Err(DatasetError::Conflict),
        }
        Ok(())
    }
    fn snapshot_owned(
        &self,
        anchor: &DatasetRef,
        selection: wes_engine::storage::datasets::DatasetSnapshot,
    ) -> Result<wes_engine::storage::datasets::DatasetInfo, DatasetError> {
        self.ready()?;
        if selection.basis != anchor.manifest_digest() || selection.generation < anchor.generation()
        {
            return Err(DatasetError::Conflict);
        }
        let original = self.read_exact(anchor)?;
        let (object, target) =
            self.committed_generation(anchor, selection.generation, &self.roots, true, None)?;
        if object.digest != selection.digest {
            return Err(DatasetError::Conflict);
        }
        self.check_same_attempt(&original, &target, None)?;
        let reference = descriptor(&object, &target)?;
        self.info_from_manifest(&reference, target, None)
    }
    fn ancestry_links(
        &self,
        dataset: &str,
        generation: u64,
        previous: Option<&ObjectRef>,
    ) -> Result<Vec<ObjectRef>, DatasetError> {
        if generation == 1 && previous.is_none() {
            return Ok(vec![]);
        }
        if generation < 2 {
            return Err(DatasetError::Conflict);
        }
        let mut links = vec![previous.cloned().ok_or(DatasetError::Conflict)?];
        let count = (u64::BITS - (generation - 1).leading_zeros()) as usize;
        for index in 0..count {
            let ancestor = self.files.read_manifest(&links[index], None)?;
            let expected = generation
                .checked_sub(1u64 << index)
                .ok_or(DatasetError::Conflict)?;
            if ancestor.dataset != dataset || ancestor.generation != expected {
                return Err(DatasetError::Conflict);
            }
            if index + 1 < count {
                links.push(
                    ancestor
                        .ancestors
                        .get(index)
                        .cloned()
                        .ok_or(DatasetError::StorageCorrupt)?,
                );
            }
        }
        Ok(links)
    }

    pub fn commit(
        &mut self,
        candidate: &Manifest,
        policy: &FlowPolicy,
    ) -> Result<CommitReceipt, DatasetError> {
        self.commit_with_sync(candidate, policy, |file| file.sync_all())
    }
    fn commit_with_sync(
        &mut self,
        candidate: &Manifest,
        policy: &FlowPolicy,
        sync: impl FnOnce(&cap_std::fs::File) -> io::Result<()>,
    ) -> Result<CommitReceipt, DatasetError> {
        self.files.check_policy(policy)?;
        self.ready()?;
        if self.gates.contains_key(&candidate.dataset) {
            return Err(DatasetError::Withdrawn);
        }
        self.next_sequence
            .checked_add(1)
            .ok_or(DatasetError::Limit("catalog sequence"))?;
        self.check_policy_reads(policy.dataset_reads())?;
        if candidate.protection != policy.is_confidential().then_some(policy.residence())
            || candidate.origins != policy.origins().iter().cloned().collect::<Vec<_>>()
            || candidate.dataset_reads != policy.dataset_reads().iter().cloned().collect::<Vec<_>>()
        {
            return Err(DatasetError::Restricted);
        }
        let prior = self.root(&candidate.dataset)?;
        if let Some((reference, manifest)) = &prior {
            if candidate.transaction == manifest.transaction {
                if candidate == manifest {
                    self.sync_existing()?;
                    return Ok(self.receipt(reference.clone(), manifest));
                }
                return Err(DatasetError::Conflict);
            }
        }
        self.check_successor(
            candidate,
            prior.as_ref(),
            self.writes.get(&candidate.dataset),
        )?;
        self.prepare_catalog()?;
        self.verify_manifest(candidate)?;
        let reference = self.files.publish_manifest(candidate, policy)?;
        let references = self.commit_roots(candidate, &reference)?;
        let commit = CatalogCommit {
            writes: self.confirm_write(candidate, &reference)?,
            store: self.store_id().into(),
            sequence: self.next_sequence,
            transaction: candidate.transaction.clone(),
            previous_digest: self.previous_digest.clone(),
            references,
            gates: vec![],
            roots: vec![RootChange {
                dataset: candidate.dataset.clone(),
                expected_generation: candidate.generation - 1,
                generation: candidate.generation,
                manifest: reference.clone(),
            }],
        };
        self.append_commit(commit, sync)?;
        Ok(self.receipt(reference, candidate))
    }
    /// Establish fresh sync evidence without executing source reads or transition code.
    pub fn reconcile(&mut self, transaction: &str) -> Result<Reconciliation, DatasetError> {
        if !catalog::valid_uuid(transaction) {
            return Err(DatasetError::Conflict);
        }
        let pending = self.uncertain.as_deref() == Some(transaction);
        self.recovery_failed = true;
        self.reload()?;
        let mut options = private_options();
        options.write(true);
        let file = self.catalog_dir.open_with(ACTIVE, &options)?;
        if self.torn_tail {
            file.set_len(self.valid_bytes as u64)?;
        }
        file.sync_all()?;
        if self.files.durability() == Durability::FileAndDirectory {
            sync_directory(&self.catalog_dir)?;
        }
        self.physical_bytes = self.valid_bytes;
        self.torn_tail = false;
        self.reserve_catalog(0, 0)?;
        self.files.reconcile()?;
        self.uncertain = None;
        self.uncertain_references.clear();
        self.changes.send_modify(|changes| changes.all_changed());
        self.recovery_failed = false;
        for root in self.roots.values() {
            let manifest = self
                .files
                .read_manifest(&root.manifest, None)
                .map_err(|_| DatasetError::StorageCorrupt)?;
            if manifest.transaction == transaction {
                return Ok(Reconciliation::Committed(
                    self.receipt(root.manifest.clone(), &manifest),
                ));
            }
        }
        let roots: Vec<_> = self
            .references
            .values()
            .filter(|r| r.transaction == transaction)
            .map(|r| (r.root.clone(), r.generation))
            .collect();
        if !roots.is_empty() {
            return Ok(Reconciliation::ReferencesCommitted(ReferenceReceipt {
                transaction: transaction.into(),
                roots,
                persistence: self.files.durability(),
            }));
        }
        if self
            .writes
            .values()
            .any(|write| write.admission == transaction)
        {
            return Ok(Reconciliation::WriteAdmitted);
        }
        Ok(if pending {
            Reconciliation::Absent
        } else {
            Reconciliation::Unknown
        })
    }
    fn ready(&self) -> Result<(), DatasetError> {
        if self.recovery_failed {
            return Err(DatasetError::StorageCorrupt);
        }
        if let Some(transaction) = &self.uncertain {
            if let Some(pending) = self
                .pending_admission
                .as_ref()
                .filter(|pending| &pending.admission == transaction)
            {
                return Err(DatasetError::AdmissionUnconfirmed {
                    transaction: pending.transaction.clone(),
                    admission: pending.admission.clone(),
                });
            }
            return Err(DatasetError::CommitUnconfirmed {
                transaction: transaction.clone(),
            });
        }
        if self.torn_tail {
            return Err(DatasetError::NeedsReconciliation);
        }
        self.files.mutation_ready()?;
        Ok(())
    }
    fn sync_existing(&self) -> Result<(), DatasetError> {
        let mut options = private_options();
        options.write(true);
        let file = self.catalog_dir.open_with(ACTIVE, &options)?;
        file.sync_all()?;
        if self.files.durability() == Durability::FileAndDirectory {
            sync_directory(&self.catalog_dir)?;
        }
        Ok(())
    }
    fn receipt(&self, reference: ObjectRef, manifest: &Manifest) -> CommitReceipt {
        CommitReceipt {
            transaction: manifest.transaction.clone(),
            dataset: manifest.dataset.clone(),
            generation: manifest.generation,
            manifest: reference,
            records: manifest.summary.end,
            persistence: self.files.durability(),
        }
    }
    fn check_successor(
        &self,
        candidate: &Manifest,
        prior: Option<&(ObjectRef, Manifest)>,
        witness: Option<&catalog::WriteWitness>,
    ) -> Result<(), DatasetError> {
        candidate.encode(self.limits.objects.manifest)?;
        if prior.is_none_or(|(_, manifest)| manifest.checkpoint.is_none()) {
            if let Some(reference) = &candidate.checkpoint {
                let first = self
                    .files
                    .read_checkpoint(reference, &candidate.dataset, None)?;
                if first.previous_attempt.is_some()
                    || first.budget.previous.is_some()
                    || first.budget.issued_attempt != first.attempt
                {
                    return Err(DatasetError::Conflict);
                }
            }
        }
        if candidate.store != self.store_id()
            || candidate.requested
                != match self.files.durability() {
                    Durability::File => Persistence::FileSynced,
                    Durability::FileAndDirectory => Persistence::FileAndDirectorySynced,
                }
        {
            return Err(DatasetError::Conflict);
        }
        if let Some((reference, previous)) = prior {
            if candidate.ancestors
                != self.ancestry_links(
                    &candidate.dataset,
                    candidate.generation,
                    candidate.previous.as_ref(),
                )?
                || candidate.previous.as_ref() != Some(reference)
                || previous.generation.checked_add(1) != Some(candidate.generation)
                || candidate.policy().join(&previous.policy()) != candidate.policy()
                || candidate.kind != previous.kind
                || candidate.schema != previous.schema
                || candidate.schema_digest != previous.schema_digest
                || candidate.source.identity != previous.source.identity
                || candidate.source.unit != previous.source.unit
                || candidate.source.start != previous.source.start
                || candidate.source.end < previous.source.end
                || candidate.summary.end < previous.summary.end
                || candidate.summary.segment_bytes < previous.summary.segment_bytes
                || previous
                    .origins
                    .iter()
                    .any(|origin| !candidate.origins.contains(origin))
                || previous
                    .dataset_reads
                    .iter()
                    .any(|origin| !candidate.dataset_reads.contains(origin))
                || candidate.authorization_generation < previous.authorization_generation
                || candidate.authorization_generation
                    > previous.authorization_generation.saturating_add(1)
            {
                return Err(DatasetError::Conflict);
            }
            match (&previous.coverage, &candidate.coverage) {
                (None, None) => {}
                (Some(old), Some(new))
                    if old.schema == new.schema
                        && old.schema_digest == new.schema_digest
                        && new.progress.extends(&old.progress)
                        && new.summary.end >= old.summary.end
                        && new.summary.segment_bytes >= old.summary.segment_bytes => {}
                _ => return Err(DatasetError::Conflict),
            }
            match (&previous.recording, &candidate.recording) {
                (None, None) => {}
                (Some(old), Some(new))
                    if old.run == new.run
                        && old.epoch == new.epoch
                        && old.first == new.first
                        && old.termination.is_none()
                        && new.accepted_through >= old.accepted_through
                        && new.committed_through >= old.committed_through
                        && new.rejected >= old.rejected => {}
                _ => return Err(DatasetError::Conflict),
            }
            let continuation = if let (Some(old), Some(new)) =
                (&previous.checkpoint, &candidate.checkpoint)
            {
                let old = self.files.read_checkpoint(old, &candidate.dataset, None)?;
                let new = self.files.read_checkpoint(new, &candidate.dataset, None)?;
                let resumed = new.attempt != old.attempt;
                let kind = if witness.is_some_and(|w| {
                    w.transaction == candidate.transaction
                        && w.operation == catalog::WriteOperation::Continue
                }) {
                    wes_engine::storage::datasets::AnalysisAttemptKind::Continue
                } else {
                    wes_engine::storage::datasets::AnalysisAttemptKind::Resume
                };
                if resumed {
                    if !matches!(
                        previous.lifecycle,
                        Lifecycle::Open
                            | Lifecycle::Incomplete
                            | Lifecycle::Interrupted
                            | Lifecycle::Cancelled
                    ) || candidate.lifecycle != Lifecycle::Open
                        || new.previous_attempt.as_deref() != Some(old.attempt.as_str())
                        || new.run == old.run
                        || new.next_position != old.next_position
                        || new.next_ordinal != old.next_ordinal
                        || new.output_end != old.output_end
                        || new.coverage != old.coverage
                        || new.state != old.state
                        || new.decoder_carry != old.decoder_carry
                        || new.followed_source != old.followed_source
                        || !(wes_engine::storage::datasets::AttemptAccounting {
                            budget: &new.budget,
                            work: &new.work,
                            usage: &new.usage,
                            duration: &new.duration,
                        })
                        .follows(
                            &wes_engine::storage::datasets::AttemptAccounting {
                                budget: &old.budget,
                                work: &old.work,
                                usage: &old.usage,
                                duration: &old.duration,
                            },
                            &new.attempt,
                            kind,
                        )
                        || new.stop.is_some()
                        || (kind == wes_engine::storage::datasets::AnalysisAttemptKind::Continue
                            && old.stop.is_some_and(|s| {
                                !s.permits_raise(new.budget.totals, old.budget.totals)
                            }))
                        || new.finish_applied
                        || old.finish_applied
                        || candidate.summary != previous.summary
                        || candidate.index != previous.index
                        || candidate.coverage != previous.coverage
                    {
                        return Err(DatasetError::Conflict);
                    }
                } else if new.run != old.run || new.previous_attempt != old.previous_attempt {
                    return Err(DatasetError::Conflict);
                }
                resumed
            } else {
                false
            };
            if previous.lifecycle != Lifecycle::Open
                && (candidate.summary != previous.summary
                    || candidate.index != previous.index
                    || candidate.coverage != previous.coverage
                    || (!continuation && candidate.checkpoint != previous.checkpoint))
            {
                return Err(DatasetError::Conflict);
            }
            if matches!(
                previous.lifecycle,
                Lifecycle::Restricted | Lifecycle::Deleted
            ) && candidate.lifecycle != previous.lifecycle
            {
                return Err(DatasetError::Conflict);
            }
            if let (Some(old), Some(new)) = (&previous.checkpoint, &candidate.checkpoint) {
                if old != new {
                    let old = self.files.read_checkpoint(old, &candidate.dataset, None)?;
                    let new = self.files.read_checkpoint(new, &candidate.dataset, None)?;
                    if !new.bindings_extend(&old)
                        || new.analysis != old.analysis
                        || new.state.schema != old.state.schema
                        || new.context != old.context
                        || new.next_position < old.next_position
                        || new.next_ordinal < old.next_ordinal
                        || new.output_end < old.output_end
                        || match (&old.coverage, &new.coverage) {
                            (None, None) => false,
                            (Some(old), Some(new)) => !new.extends(old),
                            _ => true,
                        }
                        || (!continuation && new.budget != old.budget)
                        || (!continuation && old.stop.is_some() && new.stop != old.stop)
                        || new.work.granted < old.work.granted
                        || new.work.completed < old.work.completed
                        || new.work.charged < old.work.charged
                        || new.work.grants < old.work.grants
                        || (!continuation
                            && !new.duration.settles(
                                &old.duration,
                                new.budget.totals.duration_ms,
                                candidate.lifecycle == Lifecycle::Open,
                            ))
                        || new.usage.input_bytes < old.usage.input_bytes
                        || new.usage.output_bytes < old.usage.output_bytes
                        || new.usage.high_water_bytes < old.usage.high_water_bytes
                        || new.usage.work_allowance < old.usage.work_allowance
                        || old.finish_applied
                    {
                        return Err(DatasetError::Conflict);
                    }
                    if !continuation
                        && new.work.granted > old.work.granted
                        && (old.work.outstanding != 0
                            || new.work.outstanding != new.work.granted - old.work.granted
                            || new.work.charged != old.work.charged
                            || old.work.grants.checked_add(1) != Some(new.work.grants)
                            || new.work.completed != old.work.completed
                            || new.usage != old.usage
                            || new.next_position != old.next_position
                            || new.next_ordinal != old.next_ordinal
                            || new.output_end != old.output_end
                            || new.coverage != old.coverage
                            || new.state != old.state
                            || new.decoder_carry != old.decoder_carry)
                    {
                        return Err(DatasetError::Conflict);
                    }
                    if new.work.granted == old.work.granted && new.work.grants != old.work.grants {
                        return Err(DatasetError::Conflict);
                    }
                    if !continuation
                        && new.work.granted == old.work.granted
                        && old.work.outstanding != 0
                    {
                        let measured = new.work.completed - old.work.completed;
                        let charged = new.work.charged - old.work.charged;
                        if new.work.outstanding != 0
                            || measured > old.work.outstanding
                            || (charged != measured
                                && !(charged == old.work.outstanding && measured == 0))
                        {
                            return Err(DatasetError::Conflict);
                        }
                    }
                }
            } else if previous.checkpoint.is_some() {
                return Err(DatasetError::Conflict);
            }
        } else if candidate.generation != 1
            || candidate.previous.is_some()
            || self.roots.len() >= self.limits.datasets
        {
            return Err(DatasetError::Conflict);
        }
        Ok(())
    }
    fn verify_manifest(&mut self, manifest: &Manifest) -> Result<(), DatasetError> {
        if let Some(reference) = &manifest.checkpoint {
            let checkpoint = self
                .files
                .read_checkpoint(reference, &manifest.dataset, None)
                .map_err(|_| DatasetError::StorageCorrupt)?;
            let unchanged = manifest
                .previous
                .as_ref()
                .map(|prior| self.files.read_manifest(prior, None))
                .transpose()?
                .is_some_and(|prior| prior.checkpoint.as_ref() == Some(reference));
            if manifest.policy().join(&checkpoint.policy()) != manifest.policy()
                || (!unchanged
                    && (checkpoint.transaction != manifest.transaction
                        || checkpoint.lifecycle != manifest.lifecycle))
                || checkpoint.output_end != manifest.summary.end
                || checkpoint.coverage.as_ref() != manifest.coverage.as_ref().map(|c| &c.progress)
                || checkpoint
                    .origins
                    .iter()
                    .any(|origin| !manifest.origins.contains(origin))
                || checkpoint
                    .dataset_reads
                    .iter()
                    .any(|origin| !manifest.dataset_reads.contains(origin))
                || checkpoint.bindings.output_schema != manifest.schema
                || checkpoint.bindings.output_schema_digest != manifest.schema_digest
                || checkpoint.bindings.source != manifest.source
            {
                return Err(DatasetError::StorageCorrupt);
            }
        }
        let mut verified = Vec::new();
        let mut verified_segments = Vec::new();
        let limit = self.limits.objects.index.traversal_nodes;
        let mut work = limit;
        for stream in manifest.streams() {
            let schema = self
                .files
                .read_schema(stream.schema, None)
                .map_err(|_| DatasetError::StorageCorrupt)?;
            if schema.digest() != stream.schema_digest
                || (stream.stream == super::Stream::Coverage
                    && schema.digest()
                        != wes_engine::storage::datasets::rejection_schema().digest())
            {
                return Err(DatasetError::StorageCorrupt);
            }
            walk_index(
                &self.files,
                &manifest.dataset,
                stream.stream,
                stream.index,
                stream.summary,
                &manifest.source,
                limit,
                &mut work,
                |event| {
                    match event {
                        IndexVisit::Node(reference) => {
                            let cache_key = (
                                stream.stream,
                                reference.id.clone(),
                                reference.digest.clone(),
                                manifest.dataset.clone(),
                                stream.schema_digest.to_owned(),
                            );
                            if self.verified_nodes.contains(&cache_key) {
                                return Ok(Visit::Skip);
                            }
                            verified.push(cache_key);
                        }
                        IndexVisit::Segment(entry) => {
                            let binding = super::format::bounded_json(
                                &(&entry, &manifest.origins, &manifest.dataset_reads),
                                self.limits.objects.manifest.bytes,
                            )?;
                            let segment_key = (
                                stream.stream,
                                entry.segment.id.clone(),
                                entry.segment.digest.clone(),
                                manifest.dataset.clone(),
                                stream.schema_digest.to_owned(),
                                format!("{:x}", sha2::Sha256::digest(&binding)),
                            );
                            // Immutable proofs avoid decoding historical rows on each append.
                            // Content reads still check bytes and the current authorization gate.
                            if self.verified_segments.contains(&segment_key) {
                                return Ok(Visit::Descend);
                            }
                            let bytes = self
                                .files
                                .read_segment(
                                    &entry.segment,
                                    &manifest.dataset,
                                    stream.stream,
                                    &schema,
                                    None,
                                )
                                .map_err(|_| DatasetError::StorageCorrupt)?;
                            let reader = super::SegmentReader::open(
                                &bytes,
                                self.store_id(),
                                &manifest.dataset,
                                stream.stream,
                                &schema,
                                self.limits.objects.segment,
                            )?;
                            if reader.header().first != entry.summary.first
                                || reader.header().first.checked_add(reader.header().count)
                                    != Some(entry.summary.end)
                                || reader.header().source != entry.source
                                || !segment_coverage_matches(
                                    reader.header(),
                                    entry,
                                    manifest,
                                    stream.stream,
                                )
                            {
                                return Err(DatasetError::StorageCorrupt);
                            }
                            for ordinal in entry.summary.first..entry.summary.end {
                                let row =
                                    reader.row(ordinal)?.ok_or(DatasetError::StorageCorrupt)?;
                                if manifest.policy().join(row.value.provenance().policy())
                                    != manifest.policy()
                                {
                                    return Err(DatasetError::Restricted);
                                }
                                if row
                                    .value
                                    .provenance()
                                    .policy()
                                    .origins()
                                    .iter()
                                    .any(|origin| !manifest.origins.contains(origin))
                                    || row
                                        .value
                                        .provenance()
                                        .policy()
                                        .dataset_reads()
                                        .iter()
                                        .any(|origin| !manifest.dataset_reads.contains(origin))
                                {
                                    return Err(DatasetError::Restricted);
                                }
                            }
                            verified_segments.push(segment_key);
                        }
                    }
                    Ok(Visit::Descend)
                },
            )?;
        }
        if self
            .verified_nodes
            .len()
            .saturating_add(verified.len())
            .saturating_add(self.verified_segments.len())
            .saturating_add(verified_segments.len())
            > self.limits.objects.index.traversal_nodes
        {
            self.verified_nodes.clear();
            self.verified_segments.clear();
        }
        self.verified_nodes.extend(verified);
        self.verified_segments.extend(verified_segments);
        Ok(())
    }
    fn reload(&mut self) -> Result<(), DatasetError> {
        let physical = read_active_protected(
            &self.catalog_dir,
            self.limits,
            self.files.protection(),
            self.store_id(),
        )?
        .ok_or(DatasetError::StorageCorrupt)?;
        let bytes = &physical.bytes;
        let (snapshot, prefix) = decode_snapshot(bytes, self.store_id(), self.limits)?;
        // Restore/reconcile establishes trust from disk, never from a former verification cache.
        self.verified_nodes.clear();
        self.verified_segments.clear();
        let recovery = catalog::recover_catalog(
            &bytes[prefix..],
            self.store_id(),
            snapshot.next_sequence,
            &snapshot.previous_digest,
            self.limits.catalog,
        )?;
        let mut roots = snapshot.roots.clone();
        let mut references = snapshot.references.clone();
        let mut gates = snapshot.gates.clone();
        let mut writes = snapshot.writes.clone();
        self.validate_write_map(&writes, &roots, &gates)?;
        for (dataset, root) in &roots {
            let manifest = self
                .files
                .read_manifest(&root.manifest, None)
                .map_err(|_| DatasetError::StorageCorrupt)?;
            if &manifest.dataset != dataset || manifest.generation != root.generation {
                return Err(DatasetError::StorageCorrupt);
            }
        }
        for entry in &recovery.commits {
            self.apply_write_frame(&entry.commit, &mut writes, &roots, &gates)?;

            for gate in &entry.commit.gates {
                check_gate(
                    gate,
                    gates.get(&gate.dataset),
                    roots.contains_key(&gate.dataset),
                )?;
                gates.insert(gate.dataset.clone(), gate.clone());
            }
            for change in &entry.commit.roots {
                if gates.contains_key(&change.dataset) {
                    return Err(DatasetError::StorageCorrupt);
                }
                let prior = roots.get(&change.dataset);
                if prior.map_or(0, |p| p.generation) != change.expected_generation {
                    return Err(DatasetError::StorageCorrupt);
                }
                let manifest = self
                    .files
                    .read_manifest(&change.manifest, None)
                    .map_err(|_| DatasetError::StorageCorrupt)?;
                if manifest.transaction != entry.commit.transaction
                    || manifest.dataset != change.dataset
                    || manifest.generation != change.generation
                    || manifest.previous.as_ref() != prior.map(|p| &p.manifest)
                {
                    return Err(DatasetError::StorageCorrupt);
                }
                let prior_manifest = prior
                    .map(|p| {
                        self.files
                            .read_manifest(&p.manifest, None)
                            .map(|m| (p.manifest.clone(), m))
                    })
                    .transpose()
                    .map_err(|_| DatasetError::StorageCorrupt)?;
                self.check_successor(
                    &manifest,
                    prior_manifest.as_ref(),
                    writes.get(&manifest.dataset),
                )
                .map_err(|_| DatasetError::StorageCorrupt)?;
                roots.insert(change.dataset.clone(), change.clone());
                if roots.len() > self.limits.datasets {
                    return Err(DatasetError::Limit("dataset roots"));
                }
            }
            for change in &entry.commit.references {
                Self::check_reference_gates(change, references.get(&change.root), &gates)
                    .map_err(|_| DatasetError::StorageCorrupt)?;
                self.check_reference_successor(change, references.get(&change.root))
                    .map_err(|_| DatasetError::StorageCorrupt)?;
                if change.kind == catalog::RootKind::Workspace && change.prefixes.is_empty() {
                    references.remove(&change.root);
                } else {
                    references.insert(change.root.clone(), change.clone());
                }
            }
            if references.len() > self.limits.reference_roots {
                return Err(DatasetError::Limit("reference roots"));
            }
        }
        self.validate_write_map(&writes, &roots, &gates)?;
        for write in writes.values() {
            for prefix in write.predecessor.iter().chain(write.committed.iter()) {
                self.resolve_committed(prefix, &roots, false)?;
            }
            if let Some(prefix) = write.committed.as_ref().or(write.predecessor.as_ref()) {
                let manifest = self.resolve_committed(prefix, &roots, false)?;
                if !matches!(
                    (write.owner.role, manifest.kind),
                    (
                        wes_engine::storage::datasets::DatasetWriteRole::Analysis,
                        super::DatasetKind::Analysis
                    ) | (
                        wes_engine::storage::datasets::DatasetWriteRole::Recording,
                        super::DatasetKind::EventLog
                    )
                ) {
                    return Err(DatasetError::StorageCorrupt);
                }
                if let Some(checkpoint) = manifest.checkpoint {
                    let cp = self
                        .files
                        .read_checkpoint(&checkpoint, &manifest.dataset, None)?;
                    let same_attempt = write.state == catalog::WriteState::Committed
                        || write.operation == catalog::WriteOperation::Append;
                    if cp.analysis != write.owner.lineage
                        || (same_attempt && cp.run != write.owner.run)
                        || (!same_attempt && cp.run == write.owner.run)
                    {
                        return Err(DatasetError::StorageCorrupt);
                    }
                }
            }
        }
        for reference in references.values() {
            for prefix in &reference.prefixes {
                let manifest = self.resolve_committed(prefix, &roots, false)?;
                self.verify_manifest(&manifest)?;
            }
        }
        for root in roots.values() {
            if gates
                .get(&root.dataset)
                .is_some_and(|g| g.status == catalog::ReadGateStatus::Deleted)
            {
                continue;
            }
            self.verify_manifest(
                &self
                    .files
                    .read_manifest(&root.manifest, None)
                    .map_err(|_| DatasetError::StorageCorrupt)?,
            )?;
        }
        let valid_bytes = prefix + recovery.commits.last().map_or(0, |entry| entry.end);
        let next_sequence = recovery
            .commits
            .last()
            .map_or(snapshot.next_sequence, |entry| entry.commit.sequence + 1);
        let previous_digest = recovery.commits.last().map_or_else(
            || snapshot.previous_digest.clone(),
            |entry| entry.digest.clone(),
        );
        let (external_bytes, external_entries) = catalog_inventory(&self.catalog_dir, self.limits)?;
        self.files
            .account_external(external_bytes, external_entries)?;
        self.snapshot = snapshot;
        self.roots = roots;
        self.references = references;
        self.gates = gates;
        self.writes = writes;
        self.next_sequence = next_sequence;
        self.previous_digest = previous_digest;
        self.frames = recovery.commits.len();
        self.physical_bytes = physical.physical;
        self.valid_bytes = physical.boundary(valid_bytes)?;
        self.torn_tail = recovery.torn_tail.is_some() || physical.torn;
        Ok(())
    }
    fn valid_snapshot_bytes(&self) -> Result<usize, DatasetError> {
        Ok(encode_snapshot(&self.snapshot, self.limits)?.len()
            + if self.files.protected() {
                crate::protected_storage::OVERHEAD + 4
            } else {
                0
            })
    }
    fn reserve_catalog(&mut self, extra: u64, entries: usize) -> Result<(), DatasetError> {
        let (used, count) = catalog_inventory(&self.catalog_dir, self.limits)?;
        self.files.account_external(
            used.checked_add(extra)
                .ok_or(DatasetError::Limit("catalog disk"))?,
            count
                .checked_add(entries)
                .ok_or(DatasetError::Limit("catalog inventory"))?,
        )?;
        Ok(())
    }
    fn rotate(&mut self) -> Result<(), DatasetError> {
        self.ready()?;
        let snapshot = Snapshot {
            version: 4,
            store: self.store_id().into(),
            next_sequence: self.next_sequence,
            previous_digest: self.previous_digest.clone(),
            roots: self.roots.clone(),
            references: self.references.clone(),
            gates: self.gates.clone(),
            writes: self.writes.clone(),
        };
        let bytes = encode_snapshot(&snapshot, self.limits)?;
        let bytes = crate::protected_storage::encode_log_frame(
            self.files.protection(),
            &format!("dataset/{}/catalog", self.store_id()),
            0,
            &bytes,
        )?;
        let (used, entries) = catalog_inventory(&self.catalog_dir, self.limits)?;
        self.files.account_external(
            used.checked_add(bytes.len() as u64)
                .ok_or(DatasetError::Limit("catalog rotation"))?,
            entries + 1,
        )?;
        let name = format!("{}.pending", uuid::Uuid::new_v4());
        let mut options = private_options();
        options.write(true).create_new(true);
        let mut file = self.catalog_dir.open_with(&name, &options)?;
        file.write_all(&bytes)?;
        file.sync_all()?;
        drop(file);
        self.catalog_dir.rename(&name, &self.catalog_dir, ACTIVE)?;
        // The new pointer is visible; failure must block further mutation until reconciliation.
        self.torn_tail = true;
        if self.files.durability() == Durability::FileAndDirectory {
            sync_directory(&self.catalog_dir)?;
        }
        self.snapshot = snapshot;
        self.frames = 0;
        self.physical_bytes = bytes.len();
        self.valid_bytes = bytes.len();
        self.torn_tail = false;
        let (used, entries) = catalog_inventory(&self.catalog_dir, self.limits)?;
        self.files.account_external(used, entries)?;
        Ok(())
    }
}
fn encode_snapshot(snapshot: &Snapshot, limits: StoreLimits) -> Result<Vec<u8>, DatasetError> {
    if snapshot.version != 4
        || !catalog::valid_uuid(&snapshot.store)
        || snapshot.next_sequence == 0
        || !catalog::valid_digest(&snapshot.previous_digest)
        || snapshot.roots.len() > limits.datasets
        || snapshot.references.len() > limits.reference_roots
        || snapshot.gates.len() > limits.datasets
        || snapshot.writes.len() > limits.datasets
    {
        return Err(DatasetError::StorageCorrupt);
    }
    for (dataset, write) in &snapshot.writes {
        if dataset != &write.dataset || snapshot.gates.contains_key(dataset) {
            return Err(DatasetError::StorageCorrupt);
        }
        write.validate(&snapshot.store)?;
    }
    for (dataset, root) in &snapshot.roots {
        if dataset != &root.dataset
            || !catalog::valid_uuid(dataset)
            || root.generation == 0
            || root.expected_generation.checked_add(1) != Some(root.generation)
        {
            return Err(DatasetError::StorageCorrupt);
        }
        super::tree::validate_reference(&root.manifest)?;
    }
    for (dataset, gate) in &snapshot.gates {
        if dataset != &gate.dataset
            || !catalog::valid_uuid(dataset)
            || !snapshot.roots.contains_key(dataset)
            || gate.expected_generation.checked_add(1) != Some(gate.generation)
        {
            return Err(DatasetError::StorageCorrupt);
        }
    }
    for (id, change) in &snapshot.references {
        if id != &change.root
            || !catalog::valid_uuid(id)
            || !catalog::valid_uuid(&change.transaction)
            || change.expected_generation.checked_add(1) != Some(change.generation)
            || (change.kind == catalog::RootKind::Workspace) != change.owner_workspace.is_some()
            || change
                .owner_workspace
                .as_ref()
                .is_some_and(|id| !catalog::valid_uuid(id))
            || (change.kind == catalog::RootKind::Workspace
                && (!change.captures.is_empty()
                    || change.prefixes.is_empty()
                    || change.retention != RootRetention::Protected))
            || (change.kind == catalog::RootKind::Checkpoint) != change.owner_dataset.is_some()
            || change.owner_dataset.as_ref().is_some_and(|owner| {
                !catalog::valid_uuid(owner)
                    || (!change.prefixes.is_empty()
                        && change
                            .prefixes
                            .iter()
                            .filter(|prefix| prefix.dataset() == owner)
                            .count()
                            != 1)
            })
            || change.prefixes.len() > limits.catalog.roots_per_frame
            || change.prefixes.iter().any(|p| {
                p.store() != snapshot.store
                    || snapshot
                        .gates
                        .get(p.dataset())
                        .is_some_and(|gate| gate.status == catalog::ReadGateStatus::Deleted)
            })
        {
            return Err(DatasetError::StorageCorrupt);
        }
        let mut seen = BTreeSet::new();
        if change
            .prefixes
            .iter()
            .any(|p| !seen.insert((p.dataset(), p.generation(), p.manifest())))
        {
            return Err(DatasetError::StorageCorrupt);
        }
    }
    let payload = bounded_json(snapshot, limits.snapshot_bytes - OVERHEAD)?;
    let mut bytes = Vec::with_capacity(payload.len() + OVERHEAD);
    bytes.extend_from_slice(MAGIC);
    bytes.extend_from_slice(&4u16.to_le_bytes());
    bytes.extend_from_slice(&(payload.len() as u32).to_le_bytes());
    bytes.extend_from_slice(&payload);
    let digest = Sha256::digest(&bytes);
    bytes.extend_from_slice(&digest);
    bytes.extend_from_slice(END);
    Ok(bytes)
}
/// Recovery and paged reads require the same physical evidence for aggregate metadata.
fn segment_coverage_matches(
    header: &super::SegmentHeader,
    entry: &super::IndexEntry,
    manifest: &Manifest,
    stream: super::Stream,
) -> bool {
    header.coverage.as_ref().map(|c| &c.span) == entry.summary.coverage.as_ref()
        && header.coverage.as_ref().map(|c| c.policy)
            == manifest
                .coverage
                .as_ref()
                .filter(|_| stream == super::Stream::Coverage)
                .map(|c| c.progress.policy)
}

fn descriptor(reference: &ObjectRef, manifest: &Manifest) -> Result<DatasetRef, DatasetError> {
    DatasetRef::new(
        manifest.store.clone(),
        manifest.dataset.clone(),
        manifest.generation,
        reference.id.clone(),
        reference.digest.clone(),
        reference.bytes,
        manifest.schema_digest.clone(),
        manifest.summary.end,
        manifest.authorization_generation,
    )
    .map_err(|_| DatasetError::StorageCorrupt)
}

impl wes_engine::storage::datasets::DatasetStorage for DatasetStore {
    fn supports_confidential(&self) -> bool {
        self.files.protected()
    }
    fn read_access(&self) -> Result<BTreeSet<String>, wes_engine::storage::StoreError> {
        self.ready().map_err(storage_error)?;
        Ok(self.gates.keys().cloned().collect())
    }
    fn check_read_origins(
        &self,
        origins: &BTreeSet<wes_core::flow::DatasetReadOrigin>,
    ) -> Result<(), wes_engine::storage::StoreError> {
        self.check_policy_reads(origins).map_err(storage_error)
    }
    fn plan_delete(
        &mut self,
        reference: &DatasetRef,
    ) -> Result<wes_engine::storage::datasets::DatasetDeletePlan, wes_engine::storage::StoreError>
    {
        self.plan_delete_owned(reference).map_err(storage_error)
    }
    fn delete(
        &mut self,
        token: &str,
        references: bool,
        protected: bool,
    ) -> Result<wes_engine::storage::datasets::DatasetCleanup, wes_engine::storage::StoreError>
    {
        self.delete_owned(token, references, protected)
            .map_err(storage_error)
    }
    fn withdraw(
        &mut self,
        reference: &DatasetRef,
    ) -> Result<Vec<String>, wes_engine::storage::StoreError> {
        self.withdraw_owned(reference).map_err(storage_error)
    }
    fn collect(
        &mut self,
    ) -> Result<wes_engine::storage::datasets::DatasetCleanup, wes_engine::storage::StoreError>
    {
        self.collect_owned().map_err(storage_error)
    }
    fn acknowledge_cleanup(
        &mut self,
        captures: &[CapturedValue],
    ) -> Result<(), wes_engine::storage::StoreError> {
        self.acknowledge_captures(captures).map_err(storage_error)
    }
    fn checkpoint(
        &self,
        reference: &DatasetRef,
    ) -> Result<
        Option<wes_engine::storage::datasets::AnalysisCheckpoint>,
        wes_engine::storage::StoreError,
    > {
        self.read_analysis_checkpoint(reference)
            .map_err(storage_error)
    }
    fn protect_workspace(
        &mut self,
        generation: &str,
        handles: &[wes_engine::storage::ValueHandle],
    ) -> Result<(), wes_engine::storage::StoreError> {
        self.protect_workspace_owned(generation, handles)
            .map_err(storage_error)
    }
    fn retire_workspace(
        &mut self,
        generation: &str,
    ) -> Result<(), wes_engine::storage::StoreError> {
        self.retire_workspace_owned(generation)
            .map_err(storage_error)
    }
    fn protects_value(
        &self,
        identity: &wes_engine::storage::ValueHandle,
    ) -> Result<bool, wes_engine::storage::StoreError> {
        self.ready().map_err(storage_error)?;
        Ok(self.references.values().any(|root| {
            !root.prefixes.is_empty()
                && root
                    .captures
                    .iter()
                    .any(|capture| capture.handle == identity.as_str())
        }))
    }
    fn value_root(
        &self,
        identity: &wes_engine::storage::ValueHandle,
    ) -> Result<
        Option<wes_engine::storage::datasets::DatasetRootRequest>,
        wes_engine::storage::StoreError,
    > {
        use wes_engine::storage::{
            Retention,
            datasets::{DatasetRootKind, DatasetRootRequest},
        };
        let Some(root) = self
            .reference_root(identity.as_str())
            .map_err(storage_error)?
        else {
            return Ok(None);
        };
        if root.kind != catalog::RootKind::Value {
            return Err(wes_engine::storage::StoreError::Conflict);
        }
        Ok(Some(DatasetRootRequest {
            identity: identity.clone(),
            kind: DatasetRootKind::Value,
            prefixes: root.prefixes,
            captures: root.captures,
            retention: match root.retention {
                RootRetention::Temporary => Retention::Temporary,
                RootRetention::Automatic => Retention::Automatic,
                RootRetention::Protected => Retention::Protected,
                RootRetention::Unknown => Retention::Unknown,
            },
            policy: FlowPolicy::default(),
        }))
    }
    fn create(
        &mut self,
        request: wes_engine::storage::datasets::DatasetCreate,
    ) -> Result<
        wes_engine::storage::datasets::DatasetWriterAdmission,
        wes_engine::storage::StoreError,
    > {
        self.create_owned(request).map_err(storage_error)
    }
    fn append(
        &mut self,
        request: wes_engine::storage::datasets::DatasetAppend,
    ) -> Result<DatasetRef, wes_engine::storage::StoreError> {
        self.append_owned(request).map_err(storage_error)
    }
    fn reconcile_store(
        &mut self,
    ) -> Result<wes_engine::history::Persistence, wes_engine::storage::StoreError> {
        DatasetStore::reconcile_store(self).map_err(storage_error)
    }
    fn reconcile_owned(
        &mut self,
        owner: &wes_engine::storage::datasets::DatasetWriteSelection,
    ) -> Result<wes_engine::storage::datasets::DatasetReconciliation, wes_engine::storage::StoreError>
    {
        self.reconcile_owner(owner).map_err(storage_error)
    }
    fn resume(
        &mut self,
        request: wes_engine::storage::datasets::DatasetResume,
    ) -> Result<
        wes_engine::storage::datasets::DatasetWriterAdmission,
        wes_engine::storage::StoreError,
    > {
        self.resume_owned(request).map_err(storage_error)
    }
    fn analysis_status(
        &self,
        reference: &DatasetRef,
    ) -> Result<wes_engine::storage::datasets::AnalysisStatus, wes_engine::storage::StoreError>
    {
        self.read_exact(reference).map_err(storage_error)?;
        let (head, manifest) = self
            .root(reference.dataset())
            .map_err(storage_error)?
            .ok_or(wes_engine::storage::StoreError::DatasetMissing)?;
        let head = descriptor(&head, &manifest).map_err(storage_error)?;
        let active_writer = self
            .active_writers
            .lock()
            .map_err(|_| wes_engine::storage::StoreError::DatasetCorrupt)?
            .contains_key(reference.dataset());
        Ok(wes_engine::storage::datasets::AnalysisStatus {
            lifecycle: self.inspect(reference)?.lifecycle,
            latest: head == *reference,
            active_writer,
        })
    }
    fn continuation(&self, run: &str) -> Result<DatasetRef, wes_engine::storage::StoreError> {
        let selection = wes_engine::storage::datasets::DatasetWriteSelection {
            role: wes_engine::storage::datasets::DatasetWriteRole::Analysis,
            run: run.into(),
        };
        if let Some(write) = self.selected_write(&selection).map_err(storage_error)? {
            if write.state == catalog::WriteState::Requested {
                return Err(wes_engine::storage::StoreError::DatasetRecoveryUnknown);
            }
            let prefix = write
                .committed
                .as_ref()
                .or(write.predecessor.as_ref())
                .ok_or(wes_engine::storage::StoreError::DatasetMissing)?;
            let cp = self
                .read_analysis_checkpoint(prefix)
                .map_err(storage_error)?
                .ok_or(wes_engine::storage::StoreError::DatasetMissing)?;
            if cp.analysis != write.owner.lineage {
                return Err(wes_engine::storage::StoreError::DatasetCorrupt);
            }
            return Ok(prefix.clone());
        }
        let root = self
            .reference_root(run)
            .map_err(storage_error)?
            .ok_or(wes_engine::storage::StoreError::DatasetMissing)?;
        if root.kind != catalog::RootKind::Checkpoint || !(1..=2).contains(&root.prefixes.len()) {
            return Err(wes_engine::storage::StoreError::Conflict);
        }
        let owner = root
            .owner_dataset
            .as_deref()
            .ok_or(wes_engine::storage::StoreError::Conflict)?;
        let reference = root
            .prefixes
            .iter()
            .find(|prefix| prefix.dataset() == owner)
            .cloned()
            .ok_or(wes_engine::storage::StoreError::Conflict)?;
        let cp = self
            .read_analysis_checkpoint(&reference)
            .map_err(storage_error)?
            .ok_or(wes_engine::storage::StoreError::DatasetMissing)?;
        let expected_prefixes = std::iter::once(reference.clone())
            .chain(
                cp.followed_source
                    .as_ref()
                    .map(|source| source.prefix.clone()),
            )
            .collect::<Vec<_>>();
        if root.prefixes.len() != expected_prefixes.len()
            || root
                .prefixes
                .iter()
                .any(|prefix| !expected_prefixes.contains(prefix))
        {
            return Err(wes_engine::storage::StoreError::Conflict);
        }
        if cp.run != run && cp.analysis != run {
            return Err(wes_engine::storage::StoreError::Restricted);
        }
        Ok(reference)
    }
    fn retention_size(
        &self,
        reference: &DatasetRef,
    ) -> Result<u64, wes_engine::storage::StoreError> {
        self.retention_bytes(reference).map_err(storage_error)
    }
    fn retention_size_many(
        &self,
        references: &[DatasetRef],
    ) -> Result<u64, wes_engine::storage::StoreError> {
        self.retention_bytes_many(references).map_err(storage_error)
    }
    fn retention_preview(
        &self,
        reference: &DatasetRef,
    ) -> Result<
        wes_engine::storage::datasets::DatasetRetentionInventory,
        wes_engine::storage::StoreError,
    > {
        self.preview_retention(reference).map_err(storage_error)
    }
    fn root_covers(
        &self,
        identity: &wes_engine::storage::ValueHandle,
        prefixes: &[DatasetRef],
        retention: wes_engine::storage::Retention,
    ) -> Result<bool, wes_engine::storage::StoreError> {
        self.root_covers_owned(identity, prefixes, retention)
    }
    fn set_root(
        &mut self,
        request: wes_engine::storage::datasets::DatasetRootRequest,
    ) -> Result<wes_engine::storage::datasets::DatasetRootReceipt, wes_engine::storage::StoreError>
    {
        self.set_owned_root(request)
    }
    fn read_charge(&self) -> u64 {
        let limits = self.limits.objects;
        (limits.segment.segment_bytes as u64)
            .saturating_mul(32)
            .saturating_add((limits.schema.bytes as u64).saturating_mul(32))
            .saturating_add(
                (limits.index.bytes as u64)
                    .saturating_mul(limits.index.depth as u64)
                    .saturating_mul(4),
            )
            .saturating_add((limits.manifest.bytes as u64).saturating_mul(64))
            .saturating_add((PageLimits::default().bytes as u64).saturating_mul(32))
    }
    fn changes(
        &self,
    ) -> Option<tokio::sync::watch::Receiver<wes_engine::storage::datasets::DatasetChanges>> {
        Some(self.changes.subscribe())
    }
    fn eventlog_head(
        &self,
        reference: &DatasetRef,
        work: Option<&wes_engine::storage::datasets::ReadWork>,
    ) -> Result<wes_engine::storage::datasets::DatasetInfo, wes_engine::storage::StoreError> {
        if let Some(work) = work {
            work.charge(512)?;
        }
        self.committed_head(reference, true, work)
            .map_err(storage_error)
    }
    fn head(
        &self,
        reference: &DatasetRef,
    ) -> Result<wes_engine::storage::datasets::DatasetInfo, wes_engine::storage::StoreError> {
        self.committed_head(reference, false, None)
            .map_err(storage_error)
    }
    fn snapshot(
        &self,
        reference: &DatasetRef,
        selection: wes_engine::storage::datasets::DatasetSnapshot,
    ) -> Result<wes_engine::storage::datasets::DatasetInfo, wes_engine::storage::StoreError> {
        self.snapshot_owned(reference, selection)
            .map_err(storage_error)
    }
    fn inspect(
        &self,
        reference: &DatasetRef,
    ) -> Result<wes_engine::storage::datasets::DatasetInfo, wes_engine::storage::StoreError> {
        let manifest = self.read_exact(reference).map_err(storage_error)?;
        self.info_from_manifest(reference, manifest, None)
            .map_err(storage_error)
    }
    fn page(
        &self,
        reference: &DatasetRef,
        request: wes_engine::storage::datasets::PageRequest,
    ) -> Result<wes_engine::storage::datasets::DatasetPage, wes_engine::storage::StoreError> {
        self.leased_page(reference, super::Stream::Outputs, request)
    }
    fn coverage_page(
        &self,
        reference: &DatasetRef,
        request: wes_engine::storage::datasets::PageRequest,
    ) -> Result<wes_engine::storage::datasets::DatasetPage, wes_engine::storage::StoreError> {
        self.leased_page(reference, super::Stream::Coverage, request)
    }
}
impl DatasetStore {
    fn leased_page(
        &self,
        reference: &DatasetRef,
        stream: super::Stream,
        request: wes_engine::storage::datasets::PageRequest,
    ) -> Result<wes_engine::storage::datasets::DatasetPage, wes_engine::storage::StoreError> {
        use wes_engine::storage::{
            StoreError,
            datasets::{DatasetPage as Page, DatasetRow},
        };
        let cap = PageLimits::default();
        if request.bytes > cap.bytes || request.segments > cap.segments {
            return Err(StoreError::Limit("dataset page"));
        }
        let lease = self.acquire_reader(reference).map_err(storage_error)?;
        let page = self
            .page_with_work(
                reference,
                stream,
                request.from,
                request.rows,
                PageLimits {
                    bytes: request.bytes,
                    segments: request.segments,
                    charge: request.charge,
                    ..cap
                },
                request.work.as_ref(),
            )
            .map_err(storage_error)?;
        let rows = page
            .rows
            .into_iter()
            .enumerate()
            .map(|(i, row)| DatasetRow {
                ordinal: page.first + i as u64,
                source_start: row.source_start,
                source_end: row.source_end,
                value: row.value,
            })
            .collect();
        Ok(Page {
            reference: page.reference,
            schema: page.schema,
            first: page.first,
            next: page.next,
            rows,
            extent_exhausted: page.extent_exhausted,
            limited_by: page.limited_by,
            encoded_row_bytes: page.encoded_row_bytes,
            lease: Some(lease),
        })
    }
}

fn storage_error(error: DatasetError) -> wes_engine::storage::StoreError {
    use wes_engine::storage::StoreError as S;
    match error {
        DatasetError::RowCharge { limit } => S::DatasetRowCharge { limit },
        DatasetError::RowBytes { limit } => S::DatasetRowBytes { limit },
        DatasetError::Objects(ObjectError::ReadWork(refusal)) => S::ReadWork(refusal),
        DatasetError::Unavailable => S::DatasetMissing,
        DatasetError::Withdrawn => S::DatasetWithdrawn,
        DatasetError::Restricted => S::Restricted,
        DatasetError::CommitUnconfirmed { transaction } => S::DatasetUnconfirmed { transaction },
        DatasetError::AdmissionUnconfirmed {
            transaction,
            admission,
        } => S::DatasetAdmissionUnconfirmed {
            transaction,
            admission,
        },
        DatasetError::Limit(dimension)
        | DatasetError::Objects(ObjectError::Limit(dimension))
        | DatasetError::Format(FormatError::Limit(dimension)) => S::Limit(dimension),
        DatasetError::Objects(ObjectError::Format(FormatError::Restricted))
        | DatasetError::Format(FormatError::Restricted) => S::Restricted,
        DatasetError::Conflict => S::Conflict,
        DatasetError::NewerAttempt => S::DatasetNewerAttempt,
        DatasetError::Range => S::Limit("dataset ordinal range"),
        DatasetError::NeedsReconciliation | DatasetError::Objects(ObjectError::Unconfirmed) => {
            S::DatasetNeedsReconciliation
        }
        DatasetError::StorageCorrupt
        | DatasetError::Format(_)
        | DatasetError::Objects(_)
        | DatasetError::Io(_) => S::DatasetCorrupt,
    }
}
fn decode_snapshot(
    bytes: &[u8],
    store: &str,
    limits: StoreLimits,
) -> Result<(Snapshot, usize), DatasetError> {
    if bytes.len() < OVERHEAD
        || &bytes[..8] != MAGIC
        || u16::from_le_bytes(bytes[8..10].try_into().unwrap()) != 4
    {
        return Err(DatasetError::StorageCorrupt);
    }
    let length = u32::from_le_bytes(bytes[10..14].try_into().unwrap()) as usize;
    let end = length
        .checked_add(OVERHEAD)
        .filter(|n| *n <= limits.snapshot_bytes && *n <= bytes.len())
        .ok_or(DatasetError::StorageCorrupt)?;
    let payload_end = 14 + length;
    if &bytes[end - 8..end] != END
        || Sha256::digest(&bytes[..payload_end]).as_slice() != &bytes[payload_end..end - 8]
    {
        return Err(DatasetError::StorageCorrupt);
    }
    let snapshot: Snapshot = serde_json::from_slice(&bytes[14..payload_end])
        .map_err(|_| DatasetError::StorageCorrupt)?;
    if snapshot.store != store || encode_snapshot(&snapshot, limits)? != bytes[..end] {
        return Err(DatasetError::StorageCorrupt);
    }
    Ok((snapshot, end))
}
#[cfg(test)]
fn read_active(dir: &Dir, limits: StoreLimits) -> Result<Option<Vec<u8>>, DatasetError> {
    Ok(read_active_protected(dir, limits, None, "")?.map(|log| log.bytes))
}
fn read_active_protected(
    dir: &Dir,
    limits: StoreLimits,
    protection: Option<&crate::protected_storage::ObjectProtection>,
    store: &str,
) -> Result<Option<crate::protected_storage::DecodedLog>, DatasetError> {
    let mut options = private_options();
    options.read(true);
    let file = match dir.open_with(ACTIVE, &options) {
        Ok(file) => file,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e.into()),
    };
    let logical = limits
        .snapshot_bytes
        .checked_add(limits.catalog.recovery_bytes)
        .ok_or(DatasetError::Limit("catalog recovery"))?;
    let maximum = logical
        .checked_add(if protection.is_some() {
            limits
                .catalog
                .frames
                .checked_add(1)
                .and_then(|n| n.checked_mul(crate::protected_storage::OVERHEAD + 4))
                .ok_or(DatasetError::Limit("catalog frames"))?
        } else {
            0
        })
        .ok_or(DatasetError::Limit("catalog recovery"))?;
    if !file.metadata()?.is_file() || file.metadata()?.len() > maximum as u64 {
        return Err(DatasetError::StorageCorrupt);
    }
    let mut bytes = Vec::new();
    file.take(maximum as u64 + 1).read_to_end(&mut bytes)?;
    if bytes.len() > maximum {
        return Err(DatasetError::Limit("catalog recovery"));
    }
    Ok(Some(crate::protected_storage::decode_log(
        protection,
        &format!("dataset/{store}/catalog"),
        bytes,
        logical,
        limits.catalog.frames + 1,
        limits.snapshot_bytes.max(limits.catalog.frame_bytes),
    )?))
}
fn catalog_inventory(dir: &Dir, limits: StoreLimits) -> Result<(u64, usize), DatasetError> {
    let mut bytes = 0u64;
    let mut count = 0usize;
    for entry in dir.entries()? {
        count += 1;
        if count > limits.objects.inventory_entries {
            return Err(DatasetError::Limit("catalog inventory"));
        }
        let metadata = dir.symlink_metadata(entry?.file_name())?;
        if !metadata.is_file() {
            return Err(DatasetError::StorageCorrupt);
        }
        bytes = bytes
            .checked_add(metadata.len())
            .filter(|n| *n <= limits.objects.disk_bytes)
            .ok_or(DatasetError::Limit("catalog disk"))?;
    }
    Ok((bytes, count))
}

#[cfg(test)]
mod tests;

impl DatasetStore {
    fn info_from_manifest(
        &self,
        reference: &DatasetRef,
        manifest: Manifest,
        work: Option<&wes_engine::storage::datasets::ReadWork>,
    ) -> Result<wes_engine::storage::datasets::DatasetInfo, DatasetError> {
        use wes_engine::{
            history::Persistence as P,
            storage::datasets::{DatasetInfo, DatasetLifecycle as L},
        };

        let schema = self
            .files
            .read_schema(&manifest.schema, work)
            .map_err(DatasetError::from)?;
        let lifecycle = match manifest.lifecycle {
            Lifecycle::Open
                if self
                    .roots
                    .get(reference.dataset())
                    .is_some_and(|root| root.generation > reference.generation()) =>
            {
                L::Prefix
            }
            Lifecycle::Open
                if !self
                    .active_writers
                    .lock()
                    .map_err(|_| DatasetError::StorageCorrupt)?
                    .contains_key(reference.dataset()) =>
            {
                L::Interrupted
            }
            Lifecycle::Open => L::Open,
            Lifecycle::Sealed => L::Sealed,
            Lifecycle::Incomplete => L::Incomplete,
            Lifecycle::Interrupted => L::Interrupted,
            Lifecycle::Cancelled => L::Cancelled,
            Lifecycle::Restricted => L::Restricted,
            Lifecycle::Deleted => L::Deleted,
        };
        let policy = manifest.policy();
        let persistence = match manifest.established {
            Persistence::FileSynced => P::FileSynced,
            Persistence::FileAndDirectorySynced => P::FileAndDirectorySynced,
        };
        Ok(DatasetInfo {
            coverage: manifest
                .coverage
                .as_ref()
                .map(|root| {
                    Ok::<_, DatasetError>(wes_engine::storage::datasets::CoverageInfo {
                        progress: root.progress.clone(),
                        schema: self.files.read_schema(&root.schema, work)?,
                        segment_bytes: root.summary.segment_bytes,
                    })
                })
                .transpose()?,
            reference: reference.clone(),
            schema,
            lifecycle,
            policy,
            segment_bytes: manifest
                .summary
                .segment_bytes
                .checked_add(
                    manifest
                        .coverage
                        .as_ref()
                        .map_or(0, |c| c.summary.segment_bytes),
                )
                .ok_or(DatasetError::Limit("dataset segment bytes"))?,
            protected: self.is_protected(reference),
            persistence,
            recording: manifest.recording,
        })
    }
}
