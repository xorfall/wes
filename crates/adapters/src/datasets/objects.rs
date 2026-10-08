//! Immutable physical objects. Owning this directory is not transport authorization.
use super::catalog::{ObjectRef, valid_digest, valid_uuid};
use super::tree::{Branch, Entries};
use super::{Checkpoint, CheckpointLimits};
use super::{
    FormatError, FormatLimits, Record, SegmentHeader, SegmentReader, Stream, encode_segment,
};
use super::{IndexEntry, IndexLimits, IndexNode, IndexSummary, Manifest, ManifestLimits};
use crate::filesystem::{
    DirectoryError, DirectoryKind, Durability, OwnedDirectory, private_options, sync_directory,
};
use cap_fs_ext::DirExt;
use cap_std::fs::{Dir, DirBuilder};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    io::{self, Read, Write},
    path::Path,
};
use thiserror::Error;
use uuid::Uuid;
use wes_core::{
    contracts::{ResolvedContractBundle, SnapshotLimits},
    flow::FlowPolicy,
};

mod range;
pub use range::IndexRange;

const MAGIC: &[u8; 8] = b"WESOBJ01";
const HEADER: usize = 8 + 2 + 1 + 16 + 16 + 8;
const CHECKSUM: usize = 32;
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Kind {
    Schema = 1,
    Segment = 2,
    Index = 3,
    Manifest = 4,
    Checkpoint = 5,
}
impl Kind {
    fn extension(self) -> &'static str {
        match self {
            Self::Schema => "schema",
            Self::Segment => "segment",
            Self::Index => "index",
            Self::Manifest => "manifest",
            Self::Checkpoint => "checkpoint",
        }
    }
}
#[derive(Clone, Copy, Debug)]
pub struct ObjectLimits {
    /// Physical object and pending bytes, including their envelopes. Catalog/status
    /// capacity is reserved separately by the home-wide transaction owner.
    pub disk_bytes: u64,
    pub inventory_entries: usize,
    pub schema: SnapshotLimits,
    pub segment: FormatLimits,
    pub index: IndexLimits,
    pub manifest: ManifestLimits,
    pub checkpoint: CheckpointLimits,
}
impl Default for ObjectLimits {
    fn default() -> Self {
        let inventory_entries = wes_budgets::get("dataset.inventory.entries") as usize;
        Self {
            disk_bytes: wes_budgets::get("dataset.disk.bytes"),
            inventory_entries,
            schema: SnapshotLimits::default(),
            segment: FormatLimits::default(),
            index: IndexLimits {
                traversal_nodes: inventory_entries,
                ..IndexLimits::default()
            },
            manifest: ManifestLimits::default(),
            checkpoint: CheckpointLimits::default(),
        }
    }
}
#[derive(Debug, Error)]
pub enum ObjectError {
    #[error("dataset objects directory is already in use")]
    Locked,
    #[error("dataset directory has invalid ownership or permissions")]
    Ownership,
    #[error(transparent)]
    ReadWork(#[from] wes_engine::storage::datasets::ReadWorkRefusal),
    #[error("dataset operation exceeds its {0} limit")]
    Limit(&'static str),
    #[error("dataset physical outcome must be reconciled before more mutations")]
    Unconfirmed,
    #[error("dataset object {object} was published but completion is unconfirmed")]
    Published {
        object: String,
        #[source]
        source: io::Error,
    },
    #[error("dataset object is missing or corrupt")]
    Corrupt,
    #[error("dataset filesystem operation failed")]
    Io(#[from] io::Error),
    #[error(transparent)]
    Format(#[from] FormatError),
    #[error("dataset schema is invalid")]
    Schema(#[from] wes_core::contracts::SnapshotError),
}
/// Owned I/O component, to be composed into the home's shared storage worker.
/// It creates no runtime task, capability grant, root or catalog commit by itself.
pub struct ObjectFiles {
    #[cfg(test)]
    pub(super) manifest_reads: std::sync::atomic::AtomicUsize,
    directory: OwnedDirectory,
    objects: Dir,
    pending: Dir,
    store: String,
    limits: ObjectLimits,
    used_bytes: u64,
    entries: usize,
    uncertain: bool,
    external_bytes: u64,
    external_entries: usize,
}
impl ObjectFiles {
    pub(crate) fn mutation_ready(&self) -> Result<(), ObjectError> {
        if self.uncertain {
            return Err(ObjectError::Unconfirmed);
        }
        Ok(())
    }

    /// Explicit owned cleanup. All committed reachability is established before
    /// this call; unknown names or non-regular entries refuse without unlinking.
    pub(crate) fn collect(
        &mut self,
        reachable: &BTreeMap<String, ObjectRef>,
    ) -> Result<(u64, u64, u64), ObjectError> {
        if self.uncertain {
            return Err(ObjectError::Unconfirmed);
        }
        let mut candidates = Vec::new();
        let mut shared = 0u64;
        let mut entries = 0usize;
        for (is_pending, dir) in [(false, &self.objects), (true, &self.pending)] {
            for entry in dir.entries()? {
                entries = entries
                    .checked_add(1)
                    .filter(|count| *count <= self.limits.inventory_entries)
                    .ok_or(ObjectError::Limit("cleanup inventory"))?;
                let entry = entry?;
                let name = entry
                    .file_name()
                    .to_str()
                    .ok_or(ObjectError::Corrupt)?
                    .to_owned();
                let (id, extension) = name.split_once('.').ok_or(ObjectError::Corrupt)?;
                if !valid_uuid(id) {
                    return Err(ObjectError::Corrupt);
                }
                let metadata = dir.symlink_metadata(&name)?;
                if !metadata.is_file() {
                    return Err(ObjectError::Corrupt);
                }
                if is_pending {
                    if extension != "pending" {
                        return Err(ObjectError::Corrupt);
                    }
                    candidates.push((true, name, metadata.len()));
                    continue;
                }
                let (kind, limit) = match extension {
                    "schema" => (Kind::Schema, self.limits.schema.bytes),
                    "segment" => (Kind::Segment, self.limits.segment.segment_bytes),
                    "index" => (Kind::Index, self.limits.index.bytes),
                    "manifest" => (Kind::Manifest, self.limits.manifest.bytes),
                    "checkpoint" => (Kind::Checkpoint, self.limits.checkpoint.bytes),
                    _ => return Err(ObjectError::Corrupt),
                };
                if let Some(reference) = reachable.get(id) {
                    // Validate even live entries; a mismatched name/size never licenses cleanup.
                    self.read(kind, reference, limit, None)?;
                    shared = shared
                        .checked_add(metadata.len())
                        .ok_or(ObjectError::Limit("cleanup bytes"))?;
                } else {
                    // Only the owned envelope is needed for unreachable bytes. A malformed
                    // payload from an abandoned preparation is not committed evidence.
                    let mut options = private_options();
                    options.read(true);
                    let file = dir.open_with(&name, &options)?;
                    if metadata.len() < (HEADER + CHECKSUM) as u64
                        || metadata.len() > (limit + HEADER + CHECKSUM) as u64
                    {
                        return Err(ObjectError::Corrupt);
                    }
                    let mut bytes = Vec::new();
                    file.take(metadata.len() + 1).read_to_end(&mut bytes)?;
                    if bytes.len() as u64 != metadata.len()
                        || &bytes[..8] != MAGIC
                        || u16::from_le_bytes(bytes[8..10].try_into().unwrap()) != 1
                        || bytes[10] != kind as u8
                        || &bytes[11..27] != Uuid::parse_str(&self.store).unwrap().as_bytes()
                        || &bytes[27..43] != Uuid::parse_str(id).unwrap().as_bytes()
                        || u64::from_le_bytes(bytes[43..51].try_into().unwrap())
                            != (bytes.len() - HEADER - CHECKSUM) as u64
                        || Sha256::digest(&bytes[..bytes.len() - CHECKSUM]).as_slice()
                            != &bytes[bytes.len() - CHECKSUM..]
                    {
                        return Err(ObjectError::Corrupt);
                    }
                    candidates.push((false, name, metadata.len()));
                }
            }
        }
        let mut reclaimed = 0u64;
        let mut pending = 0u64;
        for (staging, name, bytes) in candidates {
            let dir = if staging {
                &self.pending
            } else {
                &self.objects
            };
            match dir.remove_file(&name) {
                Ok(()) => {
                    reclaimed = reclaimed
                        .checked_add(bytes)
                        .ok_or(ObjectError::Limit("cleanup bytes"))?
                }
                Err(_) => {
                    pending = pending
                        .checked_add(bytes)
                        .ok_or(ObjectError::Limit("cleanup bytes"))?
                }
            }
        }
        self.uncertain = true;
        self.reconcile()?;
        Ok((reclaimed, shared, pending))
    }
    pub fn open(
        path: &Path,
        durability: Durability,
        limits: ObjectLimits,
    ) -> Result<Self, ObjectError> {
        if limits.disk_bytes == 0 || limits.inventory_entries < 8 {
            return Err(ObjectError::Limit("configuration"));
        }
        let directory = OwnedDirectory::open(path, DirectoryKind::Datasets, durability).map_err(
            |e| match e {
                DirectoryError::Locked => ObjectError::Locked,
                DirectoryError::Io(e) => ObjectError::Io(e),
                _ => ObjectError::Ownership,
            },
        )?;
        let objects = child(&directory, "objects")?;
        let pending = child(&directory, "pending")?;
        let store = store_identity(&directory)?;
        directory.sync()?;
        let (used_bytes, entries) = inventory(&objects, &pending, limits)?;
        Ok(Self {
            #[cfg(test)]
            manifest_reads: std::sync::atomic::AtomicUsize::new(0),
            directory,
            objects,
            pending,
            store,
            limits,
            used_bytes,
            entries,
            uncertain: false,
            external_bytes: 0,
            external_entries: 0,
        })
    }
    pub fn store_id(&self) -> &str {
        &self.store
    }
    pub(crate) fn has_objects(&self) -> bool {
        self.entries != 0
    }
    pub fn charged_bytes(&self) -> u64 {
        self.used_bytes + self.external_bytes
    }
    /// Establish local publication evidence without allocating a replacement object.
    /// Pending and unreachable files remain charged until an explicit collection.
    pub(crate) fn reconcile(&mut self) -> Result<(), ObjectError> {
        let (bytes, entries) = inventory(&self.objects, &self.pending, self.limits)?;
        if bytes
            .checked_add(self.external_bytes)
            .is_none_or(|n| n > self.limits.disk_bytes)
            || entries
                .checked_add(self.external_entries)
                .is_none_or(|n| n > self.limits.inventory_entries)
        {
            return Err(ObjectError::Limit("reconciliation inventory"));
        }
        if self.directory.durability() == Durability::FileAndDirectory {
            sync_directory(&self.objects)?;
            sync_directory(&self.pending)?;
        }
        self.directory.sync()?;
        self.used_bytes = bytes;
        self.entries = entries;
        self.uncertain = false;
        Ok(())
    }
    pub(crate) fn catalog_directory(&self) -> Result<Dir, ObjectError> {
        child(&self.directory, "catalog")
    }
    pub(crate) fn durability(&self) -> Durability {
        self.directory.durability()
    }
    /// Catalog/pointer/status bytes belong to the same admission budget as physical objects.
    pub(crate) fn account_external(
        &mut self,
        bytes: u64,
        entries: usize,
    ) -> Result<(), ObjectError> {
        if self
            .used_bytes
            .checked_add(bytes)
            .is_none_or(|n| n > self.limits.disk_bytes)
        {
            return Err(ObjectError::Limit("disk"));
        }
        if self
            .entries
            .checked_add(entries)
            .is_none_or(|n| n > self.limits.inventory_entries)
        {
            return Err(ObjectError::Limit("inventory"));
        }
        self.external_bytes = bytes;
        self.external_entries = entries;
        Ok(())
    }
    pub fn publish_manifest(
        &mut self,
        manifest: &Manifest,
        policy: &FlowPolicy,
    ) -> Result<ObjectRef, ObjectError> {
        check_policy(policy)?;
        if manifest.store != self.store {
            return Err(ObjectError::Corrupt);
        }
        self.publish(Kind::Manifest, &manifest.encode(self.limits.manifest)?)
    }
    pub fn read_manifest(
        &self,
        reference: &ObjectRef,
        work: Option<&wes_engine::storage::datasets::ReadWork>,
    ) -> Result<Manifest, ObjectError> {
        #[cfg(test)]
        self.manifest_reads
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let manifest = Manifest::decode(
            &self.read(Kind::Manifest, reference, self.limits.manifest.bytes, work)?,
            self.limits.manifest,
        )?;
        if manifest.store != self.store {
            return Err(ObjectError::Corrupt);
        }
        Ok(manifest)
    }
    pub fn publish_checkpoint(
        &mut self,
        checkpoint: &Checkpoint,
        policy: &FlowPolicy,
    ) -> Result<ObjectRef, ObjectError> {
        check_policy(policy)?;
        self.validate_checkpoint(checkpoint, None)?;
        if checkpoint.origins != policy.origins().iter().cloned().collect::<Vec<_>>()
            || checkpoint.dataset_reads
                != policy.dataset_reads().iter().cloned().collect::<Vec<_>>()
        {
            return Err(FormatError::Restricted.into());
        }
        self.publish(
            Kind::Checkpoint,
            &checkpoint.encode(self.limits.checkpoint)?,
        )
    }
    pub fn read_checkpoint(
        &self,
        reference: &ObjectRef,
        dataset: &str,
        work: Option<&wes_engine::storage::datasets::ReadWork>,
    ) -> Result<Checkpoint, ObjectError> {
        let checkpoint = Checkpoint::decode(
            &self.read(
                Kind::Checkpoint,
                reference,
                self.limits.checkpoint.bytes,
                work,
            )?,
            self.limits.checkpoint,
        )?;
        if checkpoint.dataset != dataset {
            return Err(ObjectError::Corrupt);
        }
        self.validate_checkpoint(&checkpoint, work)?;
        Ok(checkpoint)
    }
    fn validate_checkpoint(
        &self,
        checkpoint: &Checkpoint,
        work: Option<&wes_engine::storage::datasets::ReadWork>,
    ) -> Result<(), ObjectError> {
        checkpoint.encode(self.limits.checkpoint)?;
        if checkpoint.store != self.store {
            return Err(ObjectError::Corrupt);
        }
        for snapshot in [&checkpoint.state, &checkpoint.context] {
            let schema = self.read_schema(&snapshot.schema, work)?;
            let value = snapshot.value(&schema, self.limits.checkpoint)?;
            if value
                .provenance()
                .policy()
                .origins()
                .iter()
                .any(|origin| !checkpoint.origins.contains(origin))
                || value
                    .provenance()
                    .policy()
                    .dataset_reads()
                    .iter()
                    .any(|o| !checkpoint.dataset_reads.contains(o))
            {
                return Err(FormatError::Restricted.into());
            }
        }
        for (reference, digest) in [
            (
                &checkpoint.bindings.item_schema,
                &checkpoint.bindings.item_schema_digest,
            ),
            (
                &checkpoint.bindings.output_schema,
                &checkpoint.bindings.output_schema_digest,
            ),
        ] {
            if self.read_schema(reference, work)?.digest() != digest {
                return Err(ObjectError::Corrupt);
            }
        }
        Ok(())
    }
    pub fn publish_index(
        &mut self,
        node: &IndexNode,
        policy: &FlowPolicy,
    ) -> Result<ObjectRef, ObjectError> {
        check_policy(policy)?;
        if node.store != self.store {
            return Err(ObjectError::Corrupt);
        }
        self.publish(Kind::Index, &node.encode(self.limits.index)?)
    }
    pub fn read_index(
        &self,
        reference: &ObjectRef,
        dataset: &str,
        stream: Stream,
        work: Option<&wes_engine::storage::datasets::ReadWork>,
    ) -> Result<IndexNode, ObjectError> {
        let node = IndexNode::decode(
            &self.read(Kind::Index, reference, self.limits.index.bytes, work)?,
            self.limits.index,
        )?;
        if node.store != self.store || node.dataset != dataset || node.stream != stream {
            return Err(ObjectError::Corrupt);
        }
        Ok(node)
    }
    /// A segment is already immutable before admission to this index. No reference list grows in a manifest.
    pub fn append_index(
        &mut self,
        root: Option<&ObjectRef>,
        dataset: &str,
        stream: Stream,
        entry: IndexEntry,
        policy: &FlowPolicy,
    ) -> Result<(ObjectRef, IndexSummary), ObjectError> {
        check_policy(policy)?;
        if !valid_uuid(dataset) {
            return Err(ObjectError::Corrupt);
        }
        let (left, right) = if let Some(root) = root {
            self.append_path(root, dataset, stream, entry, policy, 0)?
        } else {
            let node = IndexNode {
                version: 3,
                store: self.store.clone(),
                dataset: dataset.into(),
                stream,
                height: 0,
                summary: entry.summary.clone(),
                entries: Entries::Leaf {
                    entries: vec![entry],
                },
            };
            let reference = self.publish_index(&node, policy)?;
            ((reference, node), None)
        };
        match right {
            None => Ok((left.0, left.1.summary)),
            Some(right) => {
                let mut parent = IndexNode {
                    version: 3,
                    store: self.store.clone(),
                    dataset: dataset.into(),
                    stream,
                    height: left.1.height.checked_add(1).ok_or(ObjectError::Corrupt)?,
                    summary: left.1.summary.clone(),
                    entries: Entries::Branch {
                        children: vec![
                            Branch {
                                summary: left.1.summary,
                                node: left.0,
                            },
                            Branch {
                                summary: right.1.summary,
                                node: right.0,
                            },
                        ],
                    },
                };
                parent.refresh_summary()?;
                let reference = self.publish_index(&parent, policy)?;
                Ok((reference, parent.summary))
            }
        }
    }
    #[allow(clippy::type_complexity)]
    fn append_path(
        &mut self,
        reference: &ObjectRef,
        dataset: &str,
        stream: Stream,
        entry: IndexEntry,
        policy: &FlowPolicy,
        depth: u8,
    ) -> Result<((ObjectRef, IndexNode), Option<(ObjectRef, IndexNode)>), ObjectError> {
        if depth > self.limits.index.depth {
            return Err(ObjectError::Limit("index depth"));
        }
        let mut node = self.read_index(reference, dataset, stream, None)?;
        if entry.summary.first != node.summary.end {
            return Err(ObjectError::Corrupt);
        }
        match &mut node.entries {
            Entries::Leaf { entries } => entries.push(entry),
            Entries::Branch { children } => {
                let last = children.last().ok_or(ObjectError::Corrupt)?.clone();
                let actual = self.read_index(&last.node, dataset, stream, None)?;
                if actual.height.checked_add(1) != Some(node.height)
                    || actual.summary != last.summary
                {
                    return Err(ObjectError::Corrupt);
                }
                let (left, right) =
                    self.append_path(&last.node, dataset, stream, entry, policy, depth + 1)?;
                *children.last_mut().unwrap() = Branch {
                    summary: left.1.summary,
                    node: left.0,
                };
                if let Some(right) = right {
                    children.push(Branch {
                        summary: right.1.summary,
                        node: right.0,
                    });
                }
            }
        }
        let count = match &node.entries {
            Entries::Leaf { entries } => entries.len(),
            Entries::Branch { children } => children.len(),
        };
        let right = if count > self.limits.index.fanout {
            let mut right = node.clone();
            right.entries = match &mut node.entries {
                Entries::Leaf { entries } => Entries::Leaf {
                    entries: entries.split_off(count.div_ceil(2)),
                },
                Entries::Branch { children } => Entries::Branch {
                    children: children.split_off(count.div_ceil(2)),
                },
            };
            right.refresh_summary()?;
            Some((self.publish_index(&right, policy)?, right))
        } else {
            None
        };
        node.refresh_summary()?;
        Ok(((self.publish_index(&node, policy)?, node), right))
    }
    pub fn publish_schema(
        &mut self,
        schema: &ResolvedContractBundle,
        policy: &FlowPolicy,
    ) -> Result<ObjectRef, ObjectError> {
        check_policy(policy)?;
        // Independent storage validation, even for a locally captured bundle.
        ResolvedContractBundle::decode(schema.encoded(), self.limits.schema)?;
        self.publish(Kind::Schema, schema.encoded())
    }
    pub fn publish_segment(
        &mut self,
        header: &SegmentHeader,
        records: &[Record],
        schema: &ResolvedContractBundle,
        policy: &FlowPolicy,
    ) -> Result<ObjectRef, ObjectError> {
        check_policy(policy)?;
        if header.store != self.store {
            return Err(ObjectError::Corrupt);
        }
        ResolvedContractBundle::decode(schema.encoded(), self.limits.schema)?;
        let bytes = encode_segment(header, records, schema, self.limits.segment)?;
        self.publish(Kind::Segment, &bytes)
    }
    pub fn read_schema(
        &self,
        reference: &ObjectRef,
        work: Option<&wes_engine::storage::datasets::ReadWork>,
    ) -> Result<ResolvedContractBundle, ObjectError> {
        let bytes = self.read(Kind::Schema, reference, self.limits.schema.bytes, work)?;
        Ok(ResolvedContractBundle::decode(&bytes, self.limits.schema)?)
    }
    pub fn read_segment(
        &self,
        reference: &ObjectRef,
        dataset: &str,
        stream: Stream,
        schema: &ResolvedContractBundle,
        work: Option<&wes_engine::storage::datasets::ReadWork>,
    ) -> Result<Vec<u8>, ObjectError> {
        let bytes = self.read(
            Kind::Segment,
            reference,
            self.limits.segment.segment_bytes,
            work,
        )?;
        SegmentReader::open(
            &bytes,
            &self.store,
            dataset,
            stream,
            schema,
            self.limits.segment,
        )?;
        Ok(bytes)
    }
    fn publish(&mut self, kind: Kind, payload: &[u8]) -> Result<ObjectRef, ObjectError> {
        self.publish_with_directory_sync(kind, payload, sync_directory)
    }
    fn publish_with_directory_sync(
        &mut self,
        kind: Kind,
        payload: &[u8],
        sync: impl Fn(&Dir) -> io::Result<()>,
    ) -> Result<ObjectRef, ObjectError> {
        if self.uncertain {
            return Err(ObjectError::Unconfirmed);
        }
        let bytes = payload
            .len()
            .checked_add(HEADER + CHECKSUM)
            .ok_or(ObjectError::Limit("object"))?;
        if self
            .entries
            .checked_add(self.external_entries)
            .and_then(|n| n.checked_add(2))
            .is_none_or(|n| n > self.limits.inventory_entries)
        {
            return Err(ObjectError::Limit("inventory"));
        }
        let charged = self
            .used_bytes
            .checked_add(bytes as u64)
            .filter(|n| {
                n.checked_add(self.external_bytes)
                    .is_some_and(|total| total <= self.limits.disk_bytes)
            })
            .ok_or(ObjectError::Limit("disk"))?;
        let id = Uuid::new_v4();
        let store = Uuid::parse_str(&self.store).map_err(|_| ObjectError::Corrupt)?;
        let mut prefix = Vec::with_capacity(HEADER);
        prefix.extend_from_slice(MAGIC);
        prefix.extend_from_slice(&1u16.to_le_bytes());
        prefix.push(kind as u8);
        prefix.extend_from_slice(store.as_bytes());
        prefix.extend_from_slice(id.as_bytes());
        prefix.extend_from_slice(&(payload.len() as u64).to_le_bytes());
        let mut hash = Sha256::new();
        hash.update(&prefix);
        hash.update(payload);
        let digest = hash.finalize();
        let reference = ObjectRef {
            id: id.to_string(),
            digest: format!("sha256:{digest:x}"),
            bytes: bytes as u64,
        };
        let name = format!("{}.{}", reference.id, kind.extension());
        let temporary = format!("{}.pending", reference.id);
        let mut options = private_options();
        options.write(true).create_new(true);
        let mut file = self.pending.open_with(&temporary, &options)?;
        self.used_bytes = charged;
        self.entries += 1;
        let prepared = file
            .write_all(&prefix)
            .and_then(|()| file.write_all(payload))
            .and_then(|()| file.write_all(&digest))
            .and_then(|()| file.sync_all());
        drop(file);
        if let Err(error) = prepared {
            if self.pending.remove_file(&temporary).is_ok() {
                self.used_bytes -= bytes as u64;
                self.entries -= 1;
            } else {
                self.uncertain = true;
            }
            return Err(ObjectError::Io(error));
        }
        if let Err(error) = self.pending.hard_link(&temporary, &self.objects, &name) {
            if self.pending.remove_file(&temporary).is_ok() {
                self.used_bytes -= bytes as u64;
                self.entries -= 1;
            } else {
                self.uncertain = true;
            }
            return Err(ObjectError::Io(error));
        }
        self.entries += 1;
        let completed = self.pending.remove_file(&temporary).and_then(|()| {
            self.entries -= 1;
            if self.directory.durability() == Durability::FileAndDirectory {
                sync(&self.objects)?;
                sync(&self.pending)?;
            }
            Ok(())
        });
        if let Err(source) = completed {
            self.uncertain = true;
            return Err(ObjectError::Published {
                object: reference.id,
                source,
            });
        }
        Ok(reference)
    }
    fn read(
        &self,
        kind: Kind,
        reference: &ObjectRef,
        limit: usize,
        work: Option<&wes_engine::storage::datasets::ReadWork>,
    ) -> Result<Vec<u8>, ObjectError> {
        if !valid_uuid(&reference.id) || !valid_digest(&reference.digest) {
            return Err(ObjectError::Corrupt);
        }
        let maximum = limit
            .checked_add(HEADER + CHECKSUM)
            .ok_or(ObjectError::Limit("object"))?;
        if reference.bytes < (HEADER + CHECKSUM) as u64 {
            return Err(ObjectError::Corrupt);
        }
        if reference.bytes > maximum as u64 {
            return Err(ObjectError::Limit("object"));
        }
        if let Some(work) = work {
            // Canonical identity and the kind's size bounds precede accounting.
            // One charge covers the physical object and its bounded checksum,
            // contract/codec validation and page encoding; nested physical reads
            // enter this same admission port with the same owned counter.
            let cost = reference
                .bytes
                .checked_mul(64)
                .and_then(|n| n.checked_add(4096))
                .ok_or(ObjectError::Limit("analysis read work"))?;
            work.charge(cost)?;
        }
        let mut options = private_options();
        options.read(true);
        let mut file = self
            .objects
            .open_with(format!("{}.{}", reference.id, kind.extension()), &options)
            .map_err(|e| {
                if e.kind() == io::ErrorKind::NotFound {
                    ObjectError::Corrupt
                } else {
                    ObjectError::Io(e)
                }
            })?;
        let metadata = file.metadata()?;
        if !metadata.is_file() || metadata.len() != reference.bytes {
            return Err(ObjectError::Corrupt);
        }
        let mut bytes = Vec::new();
        std::io::Read::by_ref(&mut file)
            .take(maximum as u64 + 1)
            .read_to_end(&mut bytes)?;
        if bytes.len() as u64 != reference.bytes || bytes.len() > maximum {
            return Err(ObjectError::Corrupt);
        }
        if &bytes[..8] != MAGIC
            || u16::from_le_bytes(bytes[8..10].try_into().unwrap()) != 1
            || bytes[10] != kind as u8
            || bytes[11..27] != *Uuid::parse_str(&self.store).unwrap().as_bytes()
            || bytes[27..43] != *Uuid::parse_str(&reference.id).unwrap().as_bytes()
            || u64::from_le_bytes(bytes[43..51].try_into().unwrap())
                != bytes.len() as u64 - (HEADER + CHECKSUM) as u64
        {
            return Err(ObjectError::Corrupt);
        }
        let end = bytes.len() - CHECKSUM;
        let digest = Sha256::digest(&bytes[..end]);
        if digest.as_slice() != &bytes[end..] || format!("sha256:{digest:x}") != reference.digest {
            return Err(ObjectError::Corrupt);
        }
        Ok(bytes[HEADER..end].to_vec())
    }
}
fn check_policy(policy: &FlowPolicy) -> Result<(), ObjectError> {
    if policy.is_private() || policy.is_unknown() {
        return Err(FormatError::Restricted.into());
    }
    Ok(())
}
fn child(directory: &OwnedDirectory, name: &str) -> Result<Dir, ObjectError> {
    let mut builder = DirBuilder::new();
    #[cfg(unix)]
    {
        use cap_std::fs::DirBuilderExt;
        builder.mode(0o700);
    }
    match directory.create_dir_with(name, &builder) {
        Ok(()) => {}
        Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {}
        Err(e) => return Err(e.into()),
    }
    let child = directory.open_dir_nofollow(name)?;
    #[cfg(unix)]
    {
        use cap_std::fs::PermissionsExt;
        if child.dir_metadata()?.permissions().mode() & 0o077 != 0 {
            return Err(ObjectError::Ownership);
        }
    }
    Ok(child)
}
fn store_identity(directory: &OwnedDirectory) -> Result<String, ObjectError> {
    const NAME: &str = ".store-id";
    let mut options = private_options();
    options.read(true);
    match directory.open_with(NAME, &options) {
        Ok(file) => {
            if !file.metadata()?.is_file() {
                return Err(ObjectError::Corrupt);
            }
            let mut bytes = vec![];
            file.take(37).read_to_end(&mut bytes)?;
            let id = std::str::from_utf8(&bytes).map_err(|_| ObjectError::Corrupt)?;
            if !valid_uuid(id) {
                return Err(ObjectError::Corrupt);
            }
            Ok(id.into())
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            // A missing identity must not reinterpret existing objects as a new store.
            for name in ["objects", "pending"] {
                if directory
                    .open_dir_nofollow(name)?
                    .entries()?
                    .next()
                    .is_some()
                {
                    return Err(ObjectError::Corrupt);
                }
            }
            match directory.open_dir_nofollow("catalog") {
                Ok(catalog) if catalog.entries()?.next().is_some() => {
                    return Err(ObjectError::Corrupt);
                }
                Ok(_) => {}
                Err(error) if error.kind() == io::ErrorKind::NotFound => {}
                Err(error) => return Err(error.into()),
            }
            let id = Uuid::new_v4().to_string();
            let mut options = private_options();
            options.write(true).create_new(true);
            let mut file = directory.open_with(NAME, &options)?;
            file.write_all(id.as_bytes())?;
            file.sync_all()?;
            Ok(id)
        }
        Err(error) => Err(error.into()),
    }
}
fn inventory(
    objects: &Dir,
    pending: &Dir,
    limits: ObjectLimits,
) -> Result<(u64, usize), ObjectError> {
    let mut bytes = 0u64;
    let mut count = 0usize;
    for dir in [objects, pending] {
        for entry in dir.entries()? {
            count = count
                .checked_add(1)
                .filter(|n| *n <= limits.inventory_entries)
                .ok_or(ObjectError::Limit("inventory"))?;
            let entry = entry?;
            let metadata = dir.symlink_metadata(entry.file_name())?;
            if !metadata.is_file() {
                return Err(ObjectError::Corrupt);
            }
            bytes = bytes
                .checked_add(metadata.len())
                .filter(|n| *n <= limits.disk_bytes)
                .ok_or(ObjectError::Limit("disk"))?;
        }
    }
    // Pending/orphan objects count even without roots. No destructive startup cleanup.
    Ok((bytes, count))
}

#[cfg(test)]
mod tests {
    use super::*;
    use wes_core::{Data, Provenance, Value, contracts::ContractRegistry};
    fn home() -> tempfile::TempDir {
        let mut builder = tempfile::Builder::new();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            builder.permissions(std::fs::Permissions::from_mode(0o700));
        }
        builder.tempdir().unwrap()
    }
    fn schema() -> ResolvedContractBundle {
        ResolvedContractBundle::capture(
            ContractRegistry::new().resolve("Int").unwrap(),
            SnapshotLimits::default(),
        )
        .unwrap()
    }
    #[test]
    fn ownership_lock_identity_and_schema_survive_reopen() {
        let tmp = home();
        let mut files = ObjectFiles::open(
            tmp.path(),
            Durability::FileAndDirectory,
            ObjectLimits::default(),
        )
        .unwrap();
        let store = files.store_id().to_owned();
        let reference = files
            .publish_schema(&schema(), &FlowPolicy::default())
            .unwrap();
        assert!(matches!(
            ObjectFiles::open(tmp.path(), Durability::File, ObjectLimits::default()),
            Err(ObjectError::Locked)
        ));
        assert_eq!(
            files.read_schema(&reference, None).unwrap().digest(),
            schema().digest()
        );
        let charge = files.charged_bytes();
        drop(files);
        let reopened = ObjectFiles::open(
            tmp.path(),
            Durability::FileAndDirectory,
            ObjectLimits::default(),
        )
        .unwrap();
        assert_eq!(reopened.store_id(), store);
        assert_eq!(reopened.charged_bytes(), charge);
        assert_eq!(
            reopened.read_schema(&reference, None).unwrap().digest(),
            schema().digest()
        );
    }
    #[test]
    fn objects_are_immutable_and_foreign_store_cannot_resolve_them() {
        let tmp = home();
        let mut files =
            ObjectFiles::open(tmp.path(), Durability::File, ObjectLimits::default()).unwrap();
        let schema = schema();
        let dataset = Uuid::new_v4().to_string();
        let header = SegmentHeader {
            coverage: None,
            store: files.store_id().into(),
            dataset: dataset.clone(),
            stream: Stream::Outputs,
            schema: schema.digest().into(),
            first: 0,
            count: 1,
            source: super::super::SourceRange {
                identity: "synthetic-source".into(),
                unit: super::super::PositionUnit::Records,
                start: 0,
                end: 1,
            },
        };
        let record = Record {
            source_start: 0,
            source_end: 1,
            value: Value::new(schema.root().shape(), Data::Int(7), Provenance::default()).unwrap(),
        };
        let reference = files
            .publish_segment(&header, &[record], &schema, &FlowPolicy::default())
            .unwrap();
        let bytes = files
            .read_segment(&reference, &dataset, Stream::Outputs, &schema, None)
            .unwrap();
        let reader = SegmentReader::open(
            &bytes,
            files.store_id(),
            &dataset,
            Stream::Outputs,
            &schema,
            FormatLimits::default(),
        )
        .unwrap();
        assert_eq!(reader.row(0).unwrap().unwrap().value.data(), &Data::Int(7));
        let other = home();
        let foreign =
            ObjectFiles::open(other.path(), Durability::File, ObjectLimits::default()).unwrap();
        // Even copying both bytes and the descriptor cannot transplant the store identity.
        std::fs::copy(
            tmp.path()
                .join("objects")
                .join(format!("{}.segment", reference.id)),
            other
                .path()
                .join("objects")
                .join(format!("{}.segment", reference.id)),
        )
        .unwrap();
        assert!(matches!(
            foreign.read_segment(&reference, &dataset, Stream::Outputs, &schema, None),
            Err(ObjectError::Corrupt)
        ));
    }
    #[test]
    fn private_unknown_and_disk_limit_fail_before_creating_payload_files() {
        let tmp = home();
        let mut files = ObjectFiles::open(
            tmp.path(),
            Durability::File,
            ObjectLimits {
                disk_bytes: 1,
                ..Default::default()
            },
        )
        .unwrap();
        for policy in [
            FlowPolicy::default().private(),
            FlowPolicy::default().unknown(),
            FlowPolicy::default(),
        ] {
            assert!(files.publish_schema(&schema(), &policy).is_err());
            assert_eq!(files.objects.entries().unwrap().count(), 0);
            assert_eq!(files.pending.entries().unwrap().count(), 0);
            assert_eq!(files.charged_bytes(), 0);
        }
    }
    #[test]
    fn complete_publication_without_sync_ack_requires_explicit_reconciliation() {
        let tmp = home();
        let mut files = ObjectFiles::open(
            tmp.path(),
            Durability::FileAndDirectory,
            ObjectLimits::default(),
        )
        .unwrap();
        let schema = schema();
        let result = files.publish_with_directory_sync(Kind::Schema, schema.encoded(), |_| {
            Err(io::Error::other("synthetic directory sync fault"))
        });
        let ObjectError::Published { object, .. } = result.unwrap_err() else {
            panic!("must preserve uncertain publication");
        };
        assert!(matches!(
            files.publish_schema(&schema, &FlowPolicy::default()),
            Err(ObjectError::Unconfirmed)
        ));
        let name = format!("{object}.schema");
        assert!(tmp.path().join("objects").join(&name).is_file());
        files.reconcile().unwrap();
        assert_eq!(files.objects.entries().unwrap().count(), 1);
        assert_eq!(files.pending.entries().unwrap().count(), 0);
        // Reconciliation preserves the original object rather than retrying its write.
        assert!(tmp.path().join("objects").join(&name).is_file());
        files
            .publish_schema(&schema, &FlowPolicy::default())
            .unwrap();
        drop(files);
        // Read presence is recovery evidence, not a fresh commit/durability acknowledgement.
        let reopened = ObjectFiles::open(
            tmp.path(),
            Durability::FileAndDirectory,
            ObjectLimits::default(),
        )
        .unwrap();
        assert!(reopened.charged_bytes() > 0);
        assert_eq!(reopened.objects.entries().unwrap().count(), 2);
    }
    #[test]
    fn pending_objects_count_against_quota_and_missing_store_identity_never_reinitializes() {
        let tmp = home();
        let files =
            ObjectFiles::open(tmp.path(), Durability::File, ObjectLimits::default()).unwrap();
        drop(files);
        std::fs::write(
            tmp.path().join("pending").join("unconfirmed.pending"),
            [0u8; 32],
        )
        .unwrap();
        assert!(matches!(
            ObjectFiles::open(
                tmp.path(),
                Durability::File,
                ObjectLimits {
                    disk_bytes: 31,
                    ..Default::default()
                }
            ),
            Err(ObjectError::Limit("disk"))
        ));
        std::fs::remove_file(tmp.path().join(".store-id")).unwrap();
        assert!(matches!(
            ObjectFiles::open(tmp.path(), Durability::File, ObjectLimits::default()),
            Err(ObjectError::Corrupt)
        ));
        assert!(!tmp.path().join(".store-id").exists());
        assert!(
            tmp.path()
                .join("pending")
                .join("unconfirmed.pending")
                .exists()
        );
    }
    #[test]
    fn malformed_reference_and_changed_bytes_never_escape_the_owned_directory() {
        let tmp = home();
        let mut files =
            ObjectFiles::open(tmp.path(), Durability::File, ObjectLimits::default()).unwrap();
        let reference = files
            .publish_schema(&schema(), &FlowPolicy::default())
            .unwrap();
        let mut forged = reference.clone();
        forged.id = "../outside".into();
        assert!(files.read_schema(&forged, None).is_err());
        let path = tmp
            .path()
            .join("objects")
            .join(format!("{}.schema", reference.id));
        let mut bytes = std::fs::read(&path).unwrap();
        let end = bytes.len() - 1;
        bytes[end] ^= 1;
        std::fs::write(path, bytes).unwrap();
        assert!(matches!(
            files.read_schema(&reference, None),
            Err(ObjectError::Corrupt)
        ));
    }
    #[cfg(unix)]
    #[test]
    fn symlinks_and_unowned_directories_refuse_without_cleanup() {
        use std::os::unix::fs::symlink;
        let unowned = home();
        std::fs::write(unowned.path().join("user-file"), b"keep").unwrap();
        assert!(matches!(
            ObjectFiles::open(unowned.path(), Durability::File, ObjectLimits::default()),
            Err(ObjectError::Ownership)
        ));
        assert!(unowned.path().join("user-file").exists());
        let tmp = home();
        let files =
            ObjectFiles::open(tmp.path(), Durability::File, ObjectLimits::default()).unwrap();
        let external = home();
        drop(files);
        symlink(
            external.path().join("missing"),
            tmp.path().join("objects").join("link"),
        )
        .unwrap();
        assert!(matches!(
            ObjectFiles::open(tmp.path(), Durability::File, ObjectLimits::default()),
            Err(ObjectError::Corrupt)
        ));
    }
}
