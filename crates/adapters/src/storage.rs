//! Capability-scoped result files. This adapter never opens a result through an ambient path.
use crate::codec::{self, Limits};
pub use crate::filesystem::Durability;
use crate::filesystem::{DirectoryError, DirectoryKind, OwnedDirectory, private_options};
use std::{
    collections::{HashMap, VecDeque},
    io::{self, Read, Write},
    num::NonZeroU64,
    path::Path,
    sync::{Arc, Weak},
    time::SystemTime,
};
use uuid::Uuid;
use wes_core::Value;
use wes_engine::storage::{
    EvictionBatch, LoadedValue, Retention, RetentionUsage, StoreError, ValueHandle, ValueLease,
    ValuePlacement, ValueStore,
};
mod composite;
mod memory;
mod segmented;
mod temporary;

/// Owned acquisition allocations and physical input, independently of process RSS.
#[derive(Clone, Copy, Debug, Default)]
pub struct AcquisitionUsage {
    pub drafts: usize,
    pub payload_bytes: u64,
    pub metadata_charge: u64,
    pub buffer_bytes: usize,
}

pub struct FileValues {
    directory: OwnedDirectory,
    limits: Limits,
    budget: Option<NonZeroU64>,
    evicted: VecDeque<ValueHandle>,
    // Publication order of this session's results. File timestamps can be coarser than
    // successive writes (Linux records equal times), so ties must not fall back to handle order.
    published: HashMap<ValueHandle, u64>,
    next_publication: u64,
    holds: HashMap<ValueHandle, Weak<()>>,
    drafts: HashMap<ValueHandle, segmented::Draft>,
    manifests: Arc<std::sync::Mutex<segmented::ManifestCache>>,
    protection: Option<std::sync::Arc<crate::protected_storage::ObjectProtection>>,
}
impl FileValues {
    /// Opens a private directory. Existing directories are never chmod'ed or recursively cleaned.
    pub fn open(
        path: &Path,
        limits: Limits,
        durability: Durability,
        budget: Option<NonZeroU64>,
    ) -> Result<Self, StoreError> {
        Self::open_protected(path, limits, durability, budget, None)
    }
    pub fn open_protected(
        path: &Path,
        limits: Limits,
        durability: Durability,
        budget: Option<NonZeroU64>,
        protection: Option<std::sync::Arc<crate::protected_storage::ObjectProtection>>,
    ) -> Result<Self, StoreError> {
        let directory = OwnedDirectory::open(path, DirectoryKind::Values, durability)
            .map_err(directory_error)?;
        crate::protected_storage::check_mode(&directory, "values", protection.as_deref())
            .map_err(|e| StoreError::backend("opening storage protection", e))?;
        directory
            .sync()
            .map_err(|e| StoreError::backend("syncing storage protection", e))?;
        Ok(Self {
            directory,
            limits,
            budget,
            protection,
            evicted: VecDeque::new(),
            published: HashMap::new(),
            next_publication: 0,
            holds: HashMap::new(),
            drafts: HashMap::new(),
            manifests: Default::default(),
        })
    }
    fn protect_object(&self, handle: &ValueHandle, bytes: &[u8]) -> Result<Vec<u8>, StoreError> {
        match &self.protection {
            Some(p) => p
                .seal("values", handle.as_str(), bytes)
                .map_err(|e| StoreError::backend("protecting a snapshot", e)),
            None => Ok(bytes.to_vec()),
        }
    }
    fn snapshot_header(
        &self,
        handle: &ValueHandle,
        prefix: &[u8],
        maximum: usize,
    ) -> Result<Option<Vec<u8>>, StoreError> {
        use std::io::{Seek, SeekFrom};
        let Some(mut file) = self.file(handle)? else {
            return Ok(None);
        };
        let size = file
            .metadata()
            .map_err(|e| StoreError::backend("sizing snapshot metadata", e))?
            .len();
        if self.protection.is_none() {
            let mut probe = vec![0; prefix.len()];
            let n = file
                .read(&mut probe)
                .map_err(|e| StoreError::backend("reading snapshot format", e))?;
            if !probe[..n].starts_with(prefix) {
                return Ok(None);
            }
            file.seek(SeekFrom::Start(0))
                .map_err(|e| StoreError::backend("seeking snapshot metadata", e))?;
        }
        let physical = maximum.saturating_add(
            self.protection
                .as_ref()
                .map_or(0, |_| crate::protected_storage::OVERHEAD),
        );
        if size > physical as u64 {
            return if self.protection.is_some() {
                Ok(None)
            } else {
                Err(StoreError::Limit("snapshot metadata"))
            };
        }
        let mut bytes = Vec::new();
        file.take(physical as u64 + 1)
            .read_to_end(&mut bytes)
            .map_err(|e| StoreError::backend("reading snapshot metadata", e))?;
        if bytes.len() > physical {
            return Err(StoreError::Limit("snapshot metadata"));
        }
        let bytes = match &self.protection {
            Some(p) => p
                .open("values", handle.as_str(), &bytes, maximum)
                .map_err(|_| StoreError::Corrupt)?
                .to_vec(),
            None => bytes,
        };
        Ok(bytes.starts_with(prefix).then_some(bytes))
    }
    pub fn handles(&self) -> Result<Vec<ValueHandle>, StoreError> {
        let mut handles = self
            .files()?
            .into_iter()
            .map(|file| file.handle)
            .collect::<Vec<_>>();
        handles.sort();
        Ok(handles)
    }
    fn held(&self, handle: &ValueHandle) -> bool {
        self.holds
            .get(handle)
            .is_some_and(|owner| owner.strong_count() != 0)
    }
    /// Hard admission for explicitly spillable results. No archived copy is selected for removal.
    fn admit_spill(&mut self, bytes: u64) -> Result<(), StoreError> {
        self.admit_spill_admission(bytes, 1)
    }
    fn admit_spill_admission(&mut self, bytes: u64, artifacts: usize) -> Result<(), StoreError> {
        let limit = self
            .budget
            .map_or_else(|| wes_budgets::get("storage.live.bytes"), NonZeroU64::get);
        if bytes > limit {
            return Err(StoreError::Limit("temporary result bytes"));
        }
        let maximum = wes_budgets::get("storage.temporary.files") as usize;
        let mut files = self.files()?;
        files.sort_by(|a, b| {
            a.publication
                .cmp(&b.publication)
                .then_with(|| a.modified.cmp(&b.modified))
                .then_with(|| a.handle.cmp(&b.handle))
        });
        let draft_bytes = self
            .drafts
            .values()
            .fold(0u64, |n, d| n.saturating_add(d.disk_charge()));
        let mut used = files
            .iter()
            .fold(draft_bytes, |n, f| n.saturating_add(f.bytes));
        let mut count = files
            .iter()
            .map(|f| f.artifacts)
            .sum::<usize>()
            .saturating_add(self.drafts.len().saturating_mul(2));
        let (free_count, free_bytes) = files
            .iter()
            .filter(|f| !self.held(&f.handle))
            .fold((0usize, 0u64), |(n, b), f| {
                (n + f.artifacts, b.saturating_add(f.bytes))
            });
        if used.saturating_sub(free_bytes).saturating_add(bytes) > limit
            || count.saturating_sub(free_count).saturating_add(artifacts) > maximum
        {
            return Err(StoreError::Limit("protected temporary results"));
        }
        for file in files {
            if used.saturating_add(bytes) <= limit && count.saturating_add(artifacts) <= maximum {
                break;
            }
            if self.held(&file.handle) {
                continue;
            }
            if self.release(&file.handle)? {
                used -= file.bytes;
                count -= file.artifacts;
                self.evicted.push_back(file.handle);
            }
        }
        Ok(())
    }
    fn adopt_spill(&mut self, handle: &ValueHandle, bytes: &[u8]) -> Result<(), StoreError> {
        if self.size(handle)?.is_none() {
            self.admit_spill(
                bytes.len() as u64
                    + self
                        .protection
                        .as_ref()
                        .map_or(0, |_| crate::protected_storage::OVERHEAD as u64),
            )?;
        }
        self.adopt(handle, bytes)
    }
    /// Adopt bytes under their existing identity. It never overwrites a file, including on races.
    pub fn adopt(&mut self, handle: &ValueHandle, encoded: &[u8]) -> Result<(), StoreError> {
        self.adopt_with_existing_sync(handle, encoded, |file| file.sync_all())
    }
    fn adopt_with_existing_sync(
        &mut self,
        handle: &ValueHandle,
        encoded: &[u8],
        sync: impl FnOnce(&cap_std::fs::File) -> io::Result<()>,
    ) -> Result<(), StoreError> {
        let decoded = codec::decode_value(encoded, self.limits)
            .map_err(|e| codec_error("validating adopted bytes", e))?;
        if decoded.value.provenance().policy().is_confidential() && self.protection.is_none() {
            return Err(StoreError::Restricted);
        }
        if let Some(existing) = self.encoded(handle)? {
            return if existing == encoded {
                self.confirm_existing(handle, sync)
            } else {
                Err(StoreError::Conflict)
            };
        }
        let sealed;
        let encoded = match &self.protection {
            Some(protection) => {
                sealed = protection
                    .seal("values", handle.as_str(), encoded)
                    .map_err(|e| StoreError::backend("encrypting a result", e))?;
                &sealed
            }
            None => encoded,
        };
        let temporary = format!(".wes-pending-{}", Uuid::new_v4());
        let mut options = private_options();
        options.write(true).create_new(true);
        let file = self
            .directory
            .open_with(&temporary, &options)
            .map_err(|e| StoreError::backend("creating a pending result", e))?;
        let prepared = write_and_sync(file, encoded, |file| file.sync_all());
        if let Err(error) = prepared {
            let _ = self.directory.remove_file(&temporary);
            return Err(StoreError::backend("writing and syncing a result", error));
        }
        let destination = file_name(handle);
        // A new hard link publishes a complete, synced file without replacing an existing name.
        if let Err(error) = self
            .directory
            .hard_link(&temporary, &self.directory, &destination)
        {
            let _ = self.directory.remove_file(&temporary);
            return Err(if error.kind() == io::ErrorKind::AlreadyExists {
                StoreError::Conflict
            } else {
                StoreError::backend("publishing a result", error)
            });
        }
        self.published.insert(handle.clone(), self.next_publication);
        self.next_publication += 1;
        let completed = self
            .directory
            .remove_file(&temporary)
            .and_then(|()| self.sync_directory());
        if let Err(error) = completed {
            return Err(StoreError::Published {
                handle: handle.clone(),
                source: Box::new(error),
            });
        }
        self.enforce_budget(handle)
            .map_err(|error| StoreError::Published {
                handle: handle.clone(),
                source: Box::new(error),
            })?;
        Ok(())
    }
    /// Existing bytes are not a fresh durability receipt. Re-establish the file/directory policy
    /// before acknowledging idempotent adoption or an archive-only keep after reopening.
    fn confirm_existing(
        &self,
        handle: &ValueHandle,
        sync: impl FnOnce(&cap_std::fs::File) -> io::Result<()>,
    ) -> Result<(), StoreError> {
        let file = self
            .open_file(handle, true)?
            .ok_or(StoreError::MissingValue)?;
        sync(&file)
            .and_then(|()| self.sync_directory())
            .map_err(|source| StoreError::Published {
                handle: handle.clone(),
                source: Box::new(source),
            })
    }
    fn sync_directory(&self) -> io::Result<()> {
        self.directory.sync()
    }
    fn file(&self, handle: &ValueHandle) -> Result<Option<cap_std::fs::File>, StoreError> {
        self.open_file(handle, false)
    }
    fn open_file(
        &self,
        handle: &ValueHandle,
        writable: bool,
    ) -> Result<Option<cap_std::fs::File>, StoreError> {
        let mut options = private_options();
        // FlushFileBuffers requires writable access on Windows. This never creates or truncates.
        options.read(true).write(writable);
        let file = match self.directory.open_with(file_name(handle), &options) {
            Ok(file) => file,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(StoreError::backend("opening a result", e)),
        };
        if !file
            .metadata()
            .map_err(|e| StoreError::backend("checking a result", e))?
            .is_file()
        {
            return Err(StoreError::NotRegular);
        }
        Ok(Some(file))
    }
    fn files(&self) -> Result<Vec<HeldFile>, StoreError> {
        let mut files = vec![];
        for (index, entry) in self
            .directory
            .entries()
            .map_err(|e| StoreError::backend("listing results", e))?
            .enumerate()
        {
            if index >= 100_000 {
                return Err(StoreError::Limit("stored entries"));
            }
            let entry = entry.map_err(|e| StoreError::backend("listing a result", e))?;
            let name = entry.file_name();
            let Some(name) = name.to_str() else {
                continue;
            };
            let Some(handle) = name
                .strip_suffix(".json")
                .and_then(|name| ValueHandle::new(name).ok())
            else {
                continue;
            };
            let metadata = self
                .directory
                .symlink_metadata(name)
                .map_err(|e| StoreError::backend("inspecting a stored entry", e))?;
            if !metadata.is_file() {
                return Err(StoreError::NotRegular);
            }
            let segmented = self.segmented_size(&handle)?.is_some();
            let physical = if segmented {
                metadata.len().saturating_add(
                    self.binary_file(&handle, false)?
                        .metadata()
                        .map_err(|e| StoreError::backend("sizing stored extents", e))?
                        .len(),
                )
            } else {
                metadata.len()
            };
            files.push(HeldFile {
                publication: self.published.get(&handle).copied(),
                handle: handle.clone(),
                bytes: physical,
                artifacts: if segmented { 2 } else { 1 },
                modified: metadata
                    .modified()
                    .map(|time| time.into_std())
                    .unwrap_or(SystemTime::UNIX_EPOCH),
            });
        }
        Ok(files)
    }
    fn enforce_budget(&mut self, newest: &ValueHandle) -> Result<(), StoreError> {
        let Some(budget) = self.budget else {
            return Ok(());
        };
        let mut files = self.files()?;
        files.sort_by(|a, b| {
            a.modified
                .cmp(&b.modified)
                .then_with(|| a.publication.cmp(&b.publication))
                .then_with(|| a.handle.cmp(&b.handle))
        });
        let mut bytes = files
            .iter()
            .map(|file| u128::from(file.bytes))
            .sum::<u128>();
        for file in files {
            if bytes <= u128::from(budget.get()) {
                break;
            }
            if file.handle == *newest || self.held(&file.handle) {
                continue;
            } // The soft budget never evicts the result just returned.
            match self.release(&file.handle) {
                Ok(true) => {
                    bytes = bytes.saturating_sub(u128::from(file.bytes));
                    self.evicted.push_back(file.handle);
                }
                Ok(false) => {}
                Err(error @ StoreError::Released { .. }) => {
                    self.evicted.push_back(file.handle);
                    return Err(error);
                }
                Err(error) => return Err(error),
            }
        }
        Ok(())
    }
}
struct HeldFile {
    artifacts: usize,
    handle: ValueHandle,
    /// Earlier sessions' results have no recorded publication and order before this session's.
    publication: Option<u64>,
    bytes: u64,
    modified: SystemTime,
}
fn directory_error(error: DirectoryError) -> StoreError {
    match error {
        DirectoryError::Locked => StoreError::Locked,
        DirectoryError::Insecure => StoreError::InsecureDirectory,
        DirectoryError::Unowned => StoreError::UnownedDirectory,
        DirectoryError::NotRegular => StoreError::NotRegular,
        DirectoryError::Io(error) => {
            StoreError::backend("opening the owned store directory", error)
        }
    }
}
fn file_name(handle: &ValueHandle) -> String {
    format!("{handle}.json")
}
fn codec_error(operation: &'static str, error: codec::CodecError) -> StoreError {
    match error {
        codec::CodecError::Bytes => StoreError::Limit("stored result codec bytes"),
        codec::CodecError::Work => StoreError::Limit("stored result codec work"),
        _ => StoreError::backend(operation, error),
    }
}
fn stored_decode_error(error: codec::CodecError) -> StoreError {
    match error {
        codec::CodecError::Bytes => StoreError::Limit("stored result codec bytes"),
        codec::CodecError::Work => StoreError::Limit("stored result codec work"),
        codec::CodecError::Depth => StoreError::Limit("stored result codec depth"),
        _ => StoreError::Corrupt,
    }
}
// The owned file closes on every path before publication or cleanup. Tests inject short writes
// and sync failures here without replacing the filesystem abstraction with a mock framework.
fn write_and_sync<W: Write>(
    mut file: W,
    bytes: &[u8],
    sync: impl FnOnce(&W) -> io::Result<()>,
) -> io::Result<()> {
    file.write_all(bytes)?;
    sync(&file)
}
impl ValueStore for FileValues {
    fn digest(&self, handle: &ValueHandle) -> Result<Option<String>, StoreError> {
        use sha2::{Digest, Sha256};
        if let Some((_, bytes)) = self.composite_header(handle)? {
            return Ok(Some(format!("sha256:{:x}", Sha256::digest(bytes))));
        }
        if let Some(snapshot) = self.describe_segmented(handle, &[])? {
            return Ok(Some(snapshot.digest.clone()));
        }
        Ok(self
            .encoded(handle)?
            .map(|bytes| format!("sha256:{:x}", Sha256::digest(bytes))))
    }

    fn supports_confidential(&self) -> bool {
        self.protection.is_some()
    }
    fn store(&mut self, value: &Value) -> Result<ValueHandle, StoreError> {
        let encoded = match &self.protection {
            Some(_) => codec::encode_protected_value(value, self.limits),
            None => codec::encode_value(value, self.limits),
        }
        .map_err(|e| codec_error("encoding a result", e))?;
        let handle = ValueHandle::fresh();
        self.adopt(&handle, &encoded)?;
        Ok(handle)
    }
    fn has_temporary_file(&self, handle: &ValueHandle) -> Result<bool, StoreError> {
        Ok(self.size(handle)?.is_some())
    }
    fn describe(
        &self,
        handle: &ValueHandle,
        fields: &[String],
    ) -> Result<Option<wes_engine::storage::StoredSnapshot>, StoreError> {
        if let Some(s) = self.describe_composite(handle, fields)? {
            return Ok(Some(s));
        }
        if let Some(s) = self.describe_segmented(handle, fields)? {
            return Ok(Some(s));
        }
        let Some(v) = self.read(handle)? else {
            return Ok(None);
        };
        let v =
            wes_engine::plan::project_value(&v.value, fields).map_err(|_| StoreError::Conflict)?;
        let mut snapshot = wes_engine::storage::StoredSnapshot::capture(
            handle.clone(),
            self.digest(handle)?.ok_or(StoreError::MissingValue)?,
            &v,
        )
        .ok_or(StoreError::Limit("snapshot metadata"))?;
        snapshot = snapshot.with_selection(fields.to_vec());
        Ok(Some(snapshot))
    }
    fn dependencies(&self, handle: &ValueHandle) -> Result<Vec<ValueHandle>, StoreError> {
        self.composite_header(handle)?
            .map_or(Ok(vec![]), |(root, _)| {
                root.members
                    .iter()
                    .map(|m| ValueHandle::new(&m.handle))
                    .collect()
            })
    }
    fn read_selected(
        &self,
        h: &ValueHandle,
        fields: &[String],
        limit: u64,
    ) -> Result<Option<LoadedValue>, StoreError> {
        if let Some(v) = self.read_composite(h, fields, limit)? {
            return Ok(Some(v));
        }
        if let Some(v) = self.read_segmented(h, fields, limit)? {
            return Ok(Some(v));
        }
        let Some(v) = self.read(h)? else {
            return Ok(None);
        };
        let v =
            wes_engine::plan::project_value(&v.value, fields).map_err(|_| StoreError::Conflict)?;
        Ok(Some(LoadedValue { value: v }))
    }
    fn source_range(
        &self,
        s: &wes_engine::storage::StoredSnapshot,
        from: u64,
        maximum: usize,
    ) -> Result<wes_engine::storage::SourceRange, StoreError> {
        if let Some(range) = self.composite_range(s, from, maximum)? {
            return Ok(range);
        }
        if let Some(range) = self.segmented_range(s, from, maximum)? {
            return Ok(range);
        }
        ordinary_range(self, s, from, maximum)
    }
    fn read(&self, handle: &ValueHandle) -> Result<Option<LoadedValue>, StoreError> {
        if let Some(v) = self.read_composite(handle, &[], self.limits.bytes as u64)? {
            return Ok(Some(v));
        }
        if let Some(v) = self.read_segmented(handle, &[], self.limits.bytes as u64)? {
            return Ok(Some(v));
        }
        self.encoded(handle)?
            .map(|bytes| {
                codec::decode_value(&bytes, self.limits)
                    .map(|decoded| LoadedValue {
                        value: decoded.value,
                    })
                    .map_err(stored_decode_error)
            })
            .transpose()
    }
    fn encoded(&self, handle: &ValueHandle) -> Result<Option<Vec<u8>>, StoreError> {
        if self.composite_header(handle)?.is_some() || self.segmented_size(handle)?.is_some() {
            return Err(StoreError::Limit(
                "segmented encoding export; use bounded selections or ranges",
            ));
        }
        let Some(file) = self.file(handle)? else {
            return Ok(None);
        };
        let size = file
            .metadata()
            .map_err(|e| StoreError::backend("sizing a result", e))?
            .len();
        let maximum = self
            .limits
            .bytes
            .saturating_add(if self.protection.is_some() {
                crate::protected_storage::OVERHEAD
            } else {
                0
            });
        if size > maximum as u64 {
            return Err(StoreError::Limit("stored result read bytes"));
        }
        let mut bytes = vec![];
        file.take((maximum as u64).saturating_add(1))
            .read_to_end(&mut bytes)
            .map_err(|e| StoreError::backend("reading a result", e))?;
        if bytes.len() > maximum {
            return Err(StoreError::Limit("stored result read bytes"));
        }
        let bytes = match &self.protection {
            Some(p) => p
                .open("values", handle.as_str(), &bytes, self.limits.bytes)
                .map_err(|_| StoreError::Corrupt)?
                .to_vec(),
            None => bytes,
        };
        let value = codec::decode_value(&bytes, self.limits).map_err(stored_decode_error)?;
        if value.value.provenance().policy().is_confidential() && self.protection.is_none() {
            return Err(StoreError::Restricted);
        }
        Ok(Some(bytes))
    }
    fn size(&self, handle: &ValueHandle) -> Result<Option<u64>, StoreError> {
        if let Some(size) = self.segmented_size(handle)? {
            return Ok(Some(size));
        }
        self.file(handle)?
            .map(|file| {
                file.metadata()
                    .map(|metadata| {
                        metadata.len().saturating_sub(
                            self.protection
                                .as_ref()
                                .map_or(0, |_| crate::protected_storage::OVERHEAD as u64),
                        )
                    })
                    .map_err(|e| StoreError::backend("sizing a result", e))
            })
            .transpose()
    }
    fn release(&mut self, handle: &ValueHandle) -> Result<bool, StoreError> {
        if self.held(handle) {
            return Err(StoreError::Limit("active value holds"));
        }
        // Verify the exact managed target. remove_file itself never follows the final symlink.
        if self.file(handle)?.is_none() {
            return Ok(false);
        }
        self.remove_segmented(handle)?;
        self.directory
            .remove_file(file_name(handle))
            .map_err(|e| StoreError::backend("releasing a result", e))?;
        self.published.remove(handle);
        self.holds.remove(handle);
        self.sync_directory().map_err(|e| StoreError::Released {
            handle: handle.clone(),
            source: Box::new(e),
        })?;
        Ok(true)
    }
    fn take_evicted(&mut self, maximum: usize) -> Result<EvictionBatch, StoreError> {
        let count = self.evicted.len().min(maximum);
        let handles = self.evicted.drain(..count).collect();
        Ok(EvictionBatch {
            handles,
            more: !self.evicted.is_empty(),
        })
    }
    fn hold(&mut self, handle: &ValueHandle) -> Result<Option<ValueLease>, StoreError> {
        if self.size(handle)?.is_none() {
            return Ok(None);
        }
        let children = self.composite_holds(handle)?;
        if let Some(owner) = self.holds.get(handle).and_then(Weak::upgrade) {
            return Ok(Some(ValueLease::retain((owner, children))));
        }
        self.holds.retain(|_, owner| owner.strong_count() != 0);
        if self.holds.len() >= wes_budgets::get("values.outputs") as usize {
            return Err(StoreError::Limit("value holds"));
        }
        let owner = Arc::new(());
        self.holds.insert(handle.clone(), Arc::downgrade(&owner));
        Ok(Some(ValueLease::retain((owner, children))))
    }
}

/// Live-budget eviction can never remove an archive copy. Both tiers retain the same handle.
pub struct TieredValues {
    live: FileValues,
    archive: FileValues,
    memory: memory::MemoryValues,
    temporary: temporary::TemporaryValues,
}
impl TieredValues {
    pub fn acquisition_usage(&self) -> AcquisitionUsage {
        self.temporary.acquisition_usage()
    }
    pub fn open(
        live: &Path,
        archive: &Path,
        limits: Limits,
        durability: Durability,
        live_budget: Option<NonZeroU64>,
    ) -> Result<Self, StoreError> {
        Self::open_protected(live, archive, limits, durability, live_budget, None)
    }
    pub fn open_protected(
        live: &Path,
        archive: &Path,
        limits: Limits,
        durability: Durability,
        live_budget: Option<NonZeroU64>,
        protection: Option<std::sync::Arc<crate::protected_storage::ObjectProtection>>,
    ) -> Result<Self, StoreError> {
        let live =
            FileValues::open_protected(live, limits, durability, live_budget, protection.clone())?;
        let mut temporary = temporary::TemporaryValues::open(
            &live.directory,
            limits,
            live_budget,
            protection.clone(),
        )?;
        let mut archive =
            FileValues::open_protected(archive, limits, durability, None, protection)?;
        temporary.manifests = live.manifests.clone();
        archive.manifests = live.manifests.clone();
        Ok(Self {
            live,
            temporary,
            archive,
            memory: memory::MemoryValues::new(
                NonZeroU64::new(wes_budgets::get("storage.bytes")).expect("positive budget"),
                wes_budgets::get("values.outputs") as usize,
            ),
        })
    }
    /// Explicit cache admission limits; ordinary file publication keeps its existing budget.
    pub fn with_memory_budget(
        mut self,
        bytes: NonZeroU64,
        values: usize,
    ) -> Result<Self, StoreError> {
        if self.memory.handles().next().is_some() || self.memory.has_evictions() {
            return Err(StoreError::Conflict);
        }
        self.memory = memory::MemoryValues::new(bytes, values);
        Ok(self)
    }
    pub fn archived_handles(&self) -> Result<Vec<ValueHandle>, StoreError> {
        self.archive.handles()
    }
    // Immutable per-reason evidence. Promotion adds a protected marker, never rewrites
    // automatic evidence. Publish only after payload acknowledgement; reconfirm existing
    // markers on an explicit Keep. Missing markers are Unknown, not a migration guess.
    fn reason_name(handle: &ValueHandle, reason: Retention) -> String {
        format!("{handle}.{}", reason.as_str())
    }
    fn reason_file(
        &self,
        handle: &ValueHandle,
        reason: Retention,
    ) -> Result<Option<cap_std::fs::File>, StoreError> {
        let mut options = private_options();
        options.read(true).write(true);
        let file = match self
            .archive
            .directory
            .open_with(Self::reason_name(handle, reason), &options)
        {
            Ok(file) => file,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(StoreError::backend("reading retention evidence", error)),
        };
        let metadata = file
            .metadata()
            .map_err(|e| StoreError::backend("checking retention evidence", e))?;
        if !metadata.is_file() || metadata.len() != 0 {
            return Err(StoreError::Conflict);
        }
        Ok(Some(file))
    }
    fn record_reason(&self, handle: &ValueHandle, reason: Retention) -> Result<(), StoreError> {
        if !matches!(reason, Retention::Automatic | Retention::Protected) {
            return Err(StoreError::Conflict);
        }
        let file = match self.reason_file(handle, reason)? {
            Some(file) => file,
            None => {
                let mut options = private_options();
                options.write(true).create_new(true);
                self.archive
                    .directory
                    .open_with(Self::reason_name(handle, reason), &options)
                    .map_err(|e| StoreError::backend("publishing retention evidence", e))?
            }
        };
        file.sync_all()
            .and_then(|()| self.archive.sync_directory())
            .map_err(|source| StoreError::Published {
                handle: handle.clone(),
                source: Box::new(source),
            })
    }
}
impl ValueStore for TieredValues {
    fn dependencies(&self, h: &ValueHandle) -> Result<Vec<ValueHandle>, StoreError> {
        if self.memory.size(h).is_some() {
            return Ok(vec![]);
        }
        if self.temporary.size(h)?.is_some() {
            return self.temporary.dependencies(h);
        }
        if self.live.size(h)?.is_some() {
            return self.live.dependencies(h);
        }
        self.archive.dependencies(h)
    }
    fn compose(
        &mut self,
        handle: &ValueHandle,
        members: &[wes_engine::storage::SnapshotMember],
    ) -> Result<wes_engine::storage::StoredSnapshot, StoreError> {
        self.archive.compose_retained(handle, members)
    }
    fn begin_acquisition(&mut self, max: u64, owner: Weak<()>) -> Result<ValueHandle, StoreError> {
        self.temporary.begin_segmented(max, owner)
    }
    fn append_acquisition(&mut self, h: &ValueHandle, p: &str, b: &[u8]) -> Result<(), StoreError> {
        self.temporary.append_segmented(h, p, b)
    }
    fn seal_acquisition(
        &mut self,
        h: &ValueHandle,
        shell: &Value,
        leaves: &[wes_engine::storage::LeafSelection],
    ) -> Result<wes_engine::storage::StoredSnapshot, StoreError> {
        self.temporary.seal_segmented(h, shell, leaves)
    }
    fn abort_acquisition(&mut self, h: &ValueHandle) -> Result<(), StoreError> {
        self.temporary.abort_segmented(h)
    }
    fn describe(
        &self,
        h: &ValueHandle,
        fields: &[String],
    ) -> Result<Option<wes_engine::storage::StoredSnapshot>, StoreError> {
        if let Some(bytes) = self.memory.encoded(h) {
            let v = codec::decode_value(&bytes, self.live.limits)
                .map_err(stored_decode_error)?
                .value;
            let v =
                wes_engine::plan::project_value(&v, fields).map_err(|_| StoreError::Conflict)?;
            use sha2::{Digest, Sha256};
            let mut snapshot = wes_engine::storage::StoredSnapshot::capture(
                h.clone(),
                format!("sha256:{:x}", Sha256::digest(&bytes)),
                &v,
            )
            .ok_or(StoreError::Limit("snapshot metadata"))?;
            snapshot = snapshot.with_selection(fields.to_vec());
            return Ok(Some(snapshot));
        }
        if let Some(s) = self.temporary.describe(h, fields)? {
            return Ok(Some(s));
        }
        if let Some(s) = self.live.describe(h, fields)? {
            return Ok(Some(s));
        }
        self.archive.describe(h, fields)
    }
    fn read_selected(
        &self,
        h: &ValueHandle,
        f: &[String],
        limit: u64,
    ) -> Result<Option<LoadedValue>, StoreError> {
        if let Some(bytes) = self.memory.encoded(h) {
            let v = codec::decode_value(&bytes, self.live.limits)
                .map_err(stored_decode_error)?
                .value;
            return wes_engine::plan::project_value(&v, f)
                .map(|value| Some(LoadedValue { value }))
                .map_err(|_| StoreError::Conflict);
        }
        if let Some(v) = self.temporary.read_selected(h, f, limit)? {
            return Ok(Some(v));
        }
        if let Some(v) = self.live.read_selected(h, f, limit)? {
            return Ok(Some(v));
        }
        self.archive.read_selected(h, f, limit)
    }
    fn source_range(
        &self,
        s: &wes_engine::storage::StoredSnapshot,
        from: u64,
        maximum: usize,
    ) -> Result<wes_engine::storage::SourceRange, StoreError> {
        if self.temporary.size(&s.handle)?.is_some() {
            return self.temporary.source_range(s, from, maximum);
        }
        if self.live.size(&s.handle)?.is_some() {
            return self.live.source_range(s, from, maximum);
        }
        if self.archive.size(&s.handle)?.is_some() {
            return self.archive.source_range(s, from, maximum);
        }
        ordinary_range(self, s, from, maximum)
    }

    fn digest(&self, handle: &ValueHandle) -> Result<Option<String>, StoreError> {
        use sha2::{Digest, Sha256};
        if let Some(bytes) = self.memory.encoded(handle) {
            return Ok(Some(format!("sha256:{:x}", Sha256::digest(bytes))));
        }
        if let Some(digest) = self.temporary.digest(handle)? {
            return Ok(Some(digest));
        }
        match self.live.digest(handle)? {
            Some(digest) => Ok(Some(digest)),
            None => self.archive.digest(handle),
        }
    }
    fn supports_confidential(&self) -> bool {
        self.live.supports_confidential() && self.archive.supports_confidential()
    }
    fn retained_persistence(&self) -> wes_engine::history::Persistence {
        match self.archive.directory.durability() {
            Durability::File => wes_engine::history::Persistence::FileSynced,
            Durability::FileAndDirectory => {
                wes_engine::history::Persistence::FileAndDirectorySynced
            }
        }
    }
    fn store(&mut self, value: &Value) -> Result<ValueHandle, StoreError> {
        self.live.store(value)
    }
    fn store_with_placement(
        &mut self,
        value: &Value,
        placement: ValuePlacement,
    ) -> Result<ValueHandle, StoreError> {
        if value.provenance().policy().is_confidential() && !self.supports_confidential() {
            return Err(StoreError::Restricted);
        }
        match placement {
            ValuePlacement::ManagedFile => self.store(value),
            ValuePlacement::Memory => self.memory.store(value, self.live.limits),
            ValuePlacement::MemoryAndDisk => {
                let archive = &self.archive;
                let live = &mut self.temporary;
                self.memory
                    .store_with_spill(value, live.limits, |handle, bytes| {
                        if archive.size(handle)?.is_none() {
                            live.adopt_spill(handle, bytes)?;
                        }
                        Ok(())
                    })
            }
        }
    }
    fn has_temporary_file(&self, handle: &ValueHandle) -> Result<bool, StoreError> {
        Ok(self.live.size(handle)?.is_some() || self.temporary.size(handle)?.is_some())
    }
    fn hold(&mut self, handle: &ValueHandle) -> Result<Option<ValueLease>, StoreError> {
        if let Some(lease) = self.memory.hold(handle) {
            return Ok(Some(lease));
        }
        // Both copies belong to this identity. A hold also protects a later archive promotion.
        let temporary = self.temporary.hold(handle)?;
        let live = self.live.hold(handle)?;
        let archive = self.archive.hold(handle)?;
        if temporary.is_none() && live.is_none() && archive.is_none() {
            Ok(None)
        } else {
            Ok(Some(ValueLease::retain((temporary, live, archive))))
        }
    }
    fn read(&self, handle: &ValueHandle) -> Result<Option<LoadedValue>, StoreError> {
        if let Some(bytes) = self.memory.encoded(handle) {
            return codec::decode_value(&bytes, self.live.limits)
                .map(|value| Some(LoadedValue { value: value.value }))
                .map_err(stored_decode_error);
        }
        if let Some(value) = self.temporary.read(handle)? {
            return Ok(Some(value));
        }
        match self.live.read(handle)? {
            Some(value) => Ok(Some(value)),
            None => self.archive.read(handle),
        }
    }
    fn encoded(&self, handle: &ValueHandle) -> Result<Option<Vec<u8>>, StoreError> {
        if let Some(bytes) = self.memory.encoded(handle) {
            return Ok(Some(bytes));
        }
        if let Some(value) = self.temporary.encoded(handle)? {
            return Ok(Some(value));
        }
        match self.live.encoded(handle)? {
            Some(bytes) => Ok(Some(bytes)),
            None => self.archive.encoded(handle),
        }
    }
    fn size(&self, handle: &ValueHandle) -> Result<Option<u64>, StoreError> {
        if let Some(size) = self.memory.size(handle) {
            return Ok(Some(size));
        }
        if let Some(size) = self.temporary.size(handle)? {
            return Ok(Some(size));
        }
        match self.live.size(handle)? {
            Some(bytes) => Ok(Some(bytes)),
            None => self.archive.size(handle),
        }
    }
    fn automatic_retention_allowed(&self, handle: &ValueHandle) -> Result<bool, StoreError> {
        Ok(self
            .read(handle)?
            .is_some_and(|v| !v.value.provenance().policy().is_confidential()))
    }
    fn keep(&mut self, handle: &ValueHandle) -> Result<bool, StoreError> {
        if self
            .describe(handle, &[])?
            .is_some_and(|v| !v.provenance.policy().allows_retention())
        {
            return Err(StoreError::Restricted);
        }
        if self.archive.composite_header(handle)?.is_some() {
            self.archive.confirm_composite(handle)?;
            return Ok(true);
        }
        if self.temporary.copy_segmented(handle, &mut self.archive)? {
            return Ok(true);
        }
        if self.archive.segmented_size(handle)?.is_some() {
            self.archive.confirm_segmented(handle)?;
            return Ok(true);
        }
        if let Some(bytes) = self.temporary.encoded(handle)? {
            self.archive.adopt(handle, &bytes)?;
            return Ok(true);
        }
        if let Some(bytes) = self.memory.encoded(handle) {
            self.archive.adopt(handle, &bytes)?;
            return Ok(true);
        }
        let Some(bytes) = self.live.encoded(handle)? else {
            if self.archive.read(handle)?.is_none() {
                return Ok(false);
            }
            self.archive
                .confirm_existing(handle, |file| file.sync_all())?;
            return Ok(true);
        };
        self.archive.adopt(handle, &bytes)?;
        Ok(true)
    }
    fn is_kept(&self, handle: &ValueHandle) -> Result<bool, StoreError> {
        Ok(self.archive.size(handle)?.is_some_and(|size| size > 0))
    }
    fn keep_with_reason(
        &mut self,
        handle: &ValueHandle,
        reason: Retention,
    ) -> Result<bool, StoreError> {
        if !self.keep(handle)? {
            return Ok(false);
        }
        self.record_reason(handle, reason)
            .map_err(|source| StoreError::Published {
                handle: handle.clone(),
                source: Box::new(source),
            })?;
        // Active holds remain charged until released; a resident copy is not the last copy.
        if !self.memory.held(handle) {
            self.memory.remove(handle);
        }
        Ok(true)
    }
    fn retention(&self, handle: &ValueHandle) -> Result<Retention, StoreError> {
        if !self.is_kept(handle)? {
            return Ok(Retention::Temporary);
        }
        // Validate both markers; corrupt evidence never becomes permission to delete.
        let automatic = self.reason_file(handle, Retention::Automatic)?.is_some();
        let protected = self.reason_file(handle, Retention::Protected)?.is_some();
        Ok(if protected {
            Retention::Protected
        } else if automatic {
            Retention::Automatic
        } else {
            Retention::Unknown
        })
    }
    fn retention_usage(&self) -> Result<RetentionUsage, StoreError> {
        let mut entries = std::collections::BTreeMap::new();
        let mut usage = RetentionUsage::default();
        for file in self.temporary.files()? {
            usage.live_bytes += file.bytes;
            entries.insert(file.handle, file.bytes);
        }
        for (handle, bytes) in self.memory.handles() {
            usage.memory_bytes += bytes;
            usage.memory_count += 1;
            entries.insert(handle.clone(), bytes);
        }
        for file in self.live.files()? {
            usage.live_bytes += file.bytes;
            entries.insert(file.handle, file.bytes);
        }
        for file in self.archive.files()? {
            usage.archive_bytes += file.bytes;
            entries.insert(file.handle, file.bytes);
        }
        usage.classes = [
            Retention::Temporary,
            Retention::Automatic,
            Retention::Protected,
            Retention::Unknown,
        ]
        .into_iter()
        .map(|reason| (reason, 0, 0))
        .collect();
        for (handle, bytes) in entries {
            let reason = self.retention(&handle)?;
            let (_, count, total) = usage
                .classes
                .iter_mut()
                .find(|(r, _, _)| *r == reason)
                .expect("all classes");
            *count += 1;
            *total += bytes;
        }
        Ok(usage)
    }
    fn release(&mut self, handle: &ValueHandle) -> Result<bool, StoreError> {
        if self.memory.held(handle)
            || self.temporary.held(handle)
            || self.live.held(handle)
            || self.archive.held(handle)
        {
            return Err(StoreError::Limit("active value holds"));
        }
        // Validate the exact evidence before changing any bytes. Unknown evidence is
        // allowed only by an explicit, reviewed release; malformed evidence refuses it.
        for reason in [Retention::Automatic, Retention::Protected] {
            self.reason_file(handle, reason)?;
        }
        self.live.file(handle)?;
        self.archive.file(handle)?;
        let temporary = self.temporary.release(handle)?;
        let memory = self.memory.remove(handle) || temporary;
        let live = self.live.release(handle).map(|removed| removed || memory);
        let archive = self.archive.release(handle);
        let removed = released_tiers(handle, live, archive)?;
        let clear = (|| -> Result<(), StoreError> {
            for reason in [Retention::Automatic, Retention::Protected] {
                if self.reason_file(handle, reason)?.is_some() {
                    self.archive
                        .directory
                        .remove_file(Self::reason_name(handle, reason))
                        .map_err(|e| StoreError::backend("removing retention evidence", e))?;
                }
            }
            self.archive
                .sync_directory()
                .map_err(|e| StoreError::backend("confirming retention removal", e))
        })();
        clear.map_err(|source| StoreError::Released {
            handle: handle.clone(),
            source: Box::new(source),
        })?;
        Ok(removed)
    }
    fn take_evicted(&mut self, maximum: usize) -> Result<EvictionBatch, StoreError> {
        let mut examined = 0usize;
        let mut candidates = Vec::new();
        let live_count = self.live.evicted.len().min(maximum);
        candidates.extend(self.live.evicted.iter().take(live_count).cloned());
        examined += live_count;
        let temporary_count = self
            .temporary
            .evicted
            .len()
            .min(maximum.saturating_sub(examined));
        candidates.extend(self.temporary.evicted.iter().take(temporary_count).cloned());
        examined += temporary_count;
        let memory_count = self
            .memory
            .pending_evictions(maximum.saturating_sub(examined))
            .count();
        candidates.extend(self.memory.pending_evictions(memory_count).cloned());
        let mut gone = Vec::new();
        for handle in candidates {
            if self.memory.size(&handle).is_none()
                && self.temporary.size(&handle)?.is_none()
                && self.live.size(&handle)?.is_none()
                && self.archive.size(&handle)?.is_none()
            {
                gone.push(handle);
            }
        }
        self.live.evicted.drain(..live_count);
        self.temporary.evicted.drain(..temporary_count);
        self.memory.drain_evictions(memory_count);
        Ok(EvictionBatch {
            handles: gone,
            more: !self.live.evicted.is_empty()
                || !self.temporary.evicted.is_empty()
                || self.memory.has_evictions(),
        })
    }
}
fn released_tiers(
    handle: &ValueHandle,
    live: Result<bool, StoreError>,
    archive: Result<bool, StoreError>,
) -> Result<bool, StoreError> {
    match (live, archive) {
        (Ok(live), Ok(archive)) => Ok(live || archive),
        (Err(source), _) | (_, Err(source)) => Err(StoreError::Released {
            handle: handle.clone(),
            source: Box::new(source),
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};
    #[test]
    fn identical_adoption_reconfirms_existing_bytes_and_reports_failed_sync_without_rewriting() {
        let mut builder = tempfile::Builder::new();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            builder.permissions(std::fs::Permissions::from_mode(0o700));
        }
        let root = builder.tempdir().unwrap();
        let mut store =
            FileValues::open(root.path(), Limits::default(), Durability::File, None).unwrap();
        let value = Value::new(
            wes_core::Shape::Unknown,
            wes_core::Data::Text("synthetic".into()),
            wes_core::Provenance::default(),
        )
        .unwrap();
        let handle = store.store(&value).unwrap();
        let before = store.encoded(&handle).unwrap().unwrap();
        let synced = std::cell::Cell::new(false);
        let result = store.adopt_with_existing_sync(&handle, &before, |_| {
            synced.set(true);
            Err(io::Error::other("injected confirmation failure"))
        });
        assert!(synced.get());
        assert!(
            matches!(result, Err(StoreError::Published { handle: failed, .. }) if failed == handle)
        );
        assert_eq!(store.encoded(&handle).unwrap().unwrap(), before);
        assert_eq!(store.handles().unwrap(), std::slice::from_ref(&handle));
        store.adopt(&handle, &before).unwrap();
        assert_eq!(store.encoded(&handle).unwrap().unwrap(), before);
    }
    #[test]
    fn partial_tier_release_never_reports_a_plain_failure_that_hides_other_tier_mutation() {
        let handle = ValueHandle::fresh();
        let fail = || StoreError::backend("synthetic", io::Error::other("private failure"));
        for (live, archive) in [
            (Err(fail()), Ok(true)),
            (Ok(true), Err(fail())),
            (Err(fail()), Err(fail())),
            (Ok(false), Err(fail())),
        ] {
            assert!(
                matches!(released_tiers(&handle, live, archive), Err(StoreError::Released { handle: failed, .. }) if failed == handle)
            );
        }
        assert!(!released_tiers(&handle, Ok(false), Ok(false)).unwrap());
        assert!(released_tiers(&handle, Ok(false), Ok(true)).unwrap());
    }
    struct Probe {
        written: usize,
        fail_after: Option<usize>,
        events: Arc<Mutex<Vec<&'static str>>>,
    }
    impl Write for Probe {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
            if self.fail_after == Some(self.written) {
                return Err(io::Error::other("injected write failure"));
            }
            let accepted = bytes.len().min(2);
            self.written += accepted;
            self.events.lock().unwrap().push("write");
            Ok(accepted)
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }
    impl Drop for Probe {
        fn drop(&mut self) {
            self.events.lock().unwrap().push("close");
        }
    }
    #[test]
    fn partial_write_failure_closes_without_sync_or_publication() {
        let events = Arc::new(Mutex::new(vec![]));
        let probe = Probe {
            written: 0,
            fail_after: Some(2),
            events: events.clone(),
        };
        assert!(
            write_and_sync(probe, b"abcdef", |_| panic!(
                "must not sync partial content"
            ))
            .is_err()
        );
        assert_eq!(*events.lock().unwrap(), ["write", "close"]);
    }
    #[test]
    fn sync_failure_closes_before_the_failed_prepare_returns() {
        let events = Arc::new(Mutex::new(vec![]));
        let probe = Probe {
            written: 0,
            fail_after: None,
            events: events.clone(),
        };
        assert!(
            write_and_sync(probe, b"abcdef", |file| {
                file.events.lock().unwrap().push("sync");
                Err(io::Error::other("injected sync failure"))
            })
            .is_err()
        );
        assert_eq!(
            *events.lock().unwrap(),
            ["write", "write", "write", "sync", "close"]
        );
    }
    #[test]
    fn successful_prepare_handles_short_writes_and_closes_before_publication() {
        let events = Arc::new(Mutex::new(vec![]));
        let probe = Probe {
            written: 0,
            fail_after: None,
            events: events.clone(),
        };
        write_and_sync(probe, b"abcdef", |file| {
            assert_eq!(file.written, 6);
            file.events.lock().unwrap().push("sync");
            Ok(())
        })
        .unwrap();
        events.lock().unwrap().push("publish");
        assert_eq!(
            *events.lock().unwrap(),
            ["write", "write", "write", "sync", "close", "publish"]
        );
    }
}

fn ordinary_range(
    store: &dyn ValueStore,
    snapshot: &wes_engine::storage::StoredSnapshot,
    from: u64,
    maximum: usize,
) -> Result<wes_engine::storage::SourceRange, StoreError> {
    if maximum == 0 || maximum > wes_engine::storage::CHUNK_BYTES {
        return Err(StoreError::Limit("source range"));
    }
    if store.digest(&snapshot.handle)?.as_ref() != Some(&snapshot.digest) {
        return Err(StoreError::Conflict);
    }
    let value = store
        .read_selected(
            &snapshot.handle,
            &snapshot.selection,
            wes_budgets::get("storage.bytes"),
        )?
        .ok_or(StoreError::MissingValue)?
        .value;
    if !snapshot.matches(&value) {
        return Err(StoreError::Conflict);
    }
    let (bytes, text) = match value.data() {
        wes_core::Data::Text(s) => (s.as_bytes(), true),
        wes_core::Data::Bytes(b) => (b.as_ref(), false),
        _ => return Err(StoreError::NonMaterialized),
    };
    let start = usize::try_from(from)
        .ok()
        .filter(|n| *n <= bytes.len())
        .ok_or(StoreError::SourceRange(
            "from is beyond the captured extent",
        ))?;
    Ok(wes_engine::storage::SourceRange {
        bytes: bytes[start..start.saturating_add(maximum).min(bytes.len())].to_vec(),
        extent: bytes.len() as u64,
        text,
    })
}
