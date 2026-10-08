//! Capability-scoped result files. This adapter never opens a result through an ambient path.
use crate::codec::{self, Limits};
pub use crate::filesystem::Durability;
use crate::filesystem::{DirectoryError, DirectoryKind, OwnedDirectory, private_options};
use std::{
    collections::{HashMap, VecDeque},
    io::{self, Read, Write},
    num::NonZeroU64,
    path::Path,
    time::SystemTime,
};
use uuid::Uuid;
use wes_core::Value;
use wes_engine::storage::{
    EvictionBatch, LoadedValue, Retention, RetentionUsage, StoreError, ValueHandle, ValueStore,
};

pub struct FileValues {
    directory: OwnedDirectory,
    limits: Limits,
    budget: Option<NonZeroU64>,
    evicted: VecDeque<ValueHandle>,
    // Publication order of this session's results. File timestamps can be coarser than
    // successive writes (Linux records equal times), so ties must not fall back to handle order.
    published: HashMap<ValueHandle, u64>,
    next_publication: u64,
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
        })
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
            .map_err(|e| StoreError::backend("validating adopted bytes", e))?;
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
            files.push(HeldFile {
                publication: self.published.get(&handle).copied(),
                handle,
                bytes: metadata.len(),
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
            if file.handle == *newest {
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
    fn supports_confidential(&self) -> bool {
        self.protection.is_some()
    }
    fn store(&mut self, value: &Value) -> Result<ValueHandle, StoreError> {
        let encoded = match &self.protection {
            Some(_) => codec::encode_protected_value(value, self.limits),
            None => codec::encode_value(value, self.limits),
        }
        .map_err(|e| StoreError::backend("encoding a result", e))?;
        let handle = ValueHandle::fresh();
        self.adopt(&handle, &encoded)?;
        Ok(handle)
    }
    fn read(&self, handle: &ValueHandle) -> Result<Option<LoadedValue>, StoreError> {
        self.encoded(handle)?
            .map(|bytes| {
                codec::decode_value(&bytes, self.limits)
                    .map(|decoded| LoadedValue {
                        value: decoded.value,
                    })
                    .map_err(|e| StoreError::backend("decoding a result", e))
            })
            .transpose()
    }
    fn encoded(&self, handle: &ValueHandle) -> Result<Option<Vec<u8>>, StoreError> {
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
            return Err(StoreError::backend(
                "reading a result",
                codec::CodecError::Bytes,
            ));
        }
        let mut bytes = vec![];
        file.take((maximum as u64).saturating_add(1))
            .read_to_end(&mut bytes)
            .map_err(|e| StoreError::backend("reading a result", e))?;
        if bytes.len() > maximum {
            return Err(StoreError::backend(
                "reading a result",
                codec::CodecError::Bytes,
            ));
        }
        let bytes = match &self.protection {
            Some(p) => p
                .open("values", handle.as_str(), &bytes, self.limits.bytes)
                .map_err(|e| StoreError::backend("decrypting a result", e))?
                .to_vec(),
            None => bytes,
        };
        let value = codec::decode_value(&bytes, self.limits)
            .map_err(|e| StoreError::backend("checking stored policy", e))?;
        if value.value.provenance().policy().is_confidential() && self.protection.is_none() {
            return Err(StoreError::Restricted);
        }
        Ok(Some(bytes))
    }
    fn size(&self, handle: &ValueHandle) -> Result<Option<u64>, StoreError> {
        self.file(handle)?
            .map(|file| {
                file.metadata()
                    .map(|metadata| metadata.len())
                    .map_err(|e| StoreError::backend("sizing a result", e))
            })
            .transpose()
    }
    fn release(&mut self, handle: &ValueHandle) -> Result<bool, StoreError> {
        // Verify the exact managed target. remove_file itself never follows the final symlink.
        if self.file(handle)?.is_none() {
            return Ok(false);
        }
        self.directory
            .remove_file(file_name(handle))
            .map_err(|e| StoreError::backend("releasing a result", e))?;
        self.published.remove(handle);
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
}

/// Live-budget eviction can never remove an archive copy. Both tiers retain the same handle.
pub struct TieredValues {
    live: FileValues,
    archive: FileValues,
}
impl TieredValues {
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
        Ok(Self {
            live: FileValues::open_protected(
                live,
                limits,
                durability,
                live_budget,
                protection.clone(),
            )?,
            archive: FileValues::open_protected(archive, limits, durability, None, protection)?,
        })
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
    fn read(&self, handle: &ValueHandle) -> Result<Option<LoadedValue>, StoreError> {
        match self.live.read(handle)? {
            Some(value) => Ok(Some(value)),
            None => self.archive.read(handle),
        }
    }
    fn encoded(&self, handle: &ValueHandle) -> Result<Option<Vec<u8>>, StoreError> {
        match self.live.encoded(handle)? {
            Some(bytes) => Ok(Some(bytes)),
            None => self.archive.encoded(handle),
        }
    }
    fn size(&self, handle: &ValueHandle) -> Result<Option<u64>, StoreError> {
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
            .read(handle)?
            .is_some_and(|v| !v.value.provenance().policy().allows_retention())
        {
            return Err(StoreError::Restricted);
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
        // Validate the exact evidence before changing any bytes. Unknown evidence is
        // allowed only by an explicit, reviewed release; malformed evidence refuses it.
        for reason in [Retention::Automatic, Retention::Protected] {
            self.reason_file(handle, reason)?;
        }
        let live = self.live.release(handle);
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
        // Only live may evict. Preserve pending notifications when archive inspection fails.
        let mut gone = vec![];
        let count = self.live.evicted.len().min(maximum);
        for handle in self.live.evicted.iter().take(count) {
            if self.archive.size(handle)?.is_none_or(|size| size == 0) {
                gone.push(handle.clone());
            }
        }
        self.live.evicted.drain(..count);
        Ok(EvictionBatch {
            handles: gone,
            more: !self.live.evicted.is_empty(),
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
