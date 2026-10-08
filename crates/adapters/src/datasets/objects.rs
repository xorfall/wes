//! Immutable physical objects. Owning this directory is not transport authorization.
use super::catalog::{ObjectRef, valid_digest, valid_uuid};
use super::{FormatError, FormatLimits, Record, SegmentHeader, SegmentReader, encode_segment};
use crate::filesystem::{
    DirectoryError, DirectoryKind, Durability, OwnedDirectory, private_options, sync_directory,
};
use cap_fs_ext::DirExt;
use cap_std::fs::{Dir, DirBuilder};
use sha2::{Digest, Sha256};
use std::{
    io::{self, Read, Write},
    path::Path,
};
use thiserror::Error;
use uuid::Uuid;
use wes_core::{
    contracts::{ResolvedContractBundle, SnapshotLimits},
    flow::FlowPolicy,
};

const MAGIC: &[u8; 8] = b"WESOBJ01";
const HEADER: usize = 8 + 2 + 1 + 16 + 16 + 8;
const CHECKSUM: usize = 32;
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Kind {
    Schema = 1,
    Segment = 2,
}
impl Kind {
    fn extension(self) -> &'static str {
        match self {
            Self::Schema => "schema",
            Self::Segment => "segment",
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
}
impl Default for ObjectLimits {
    fn default() -> Self {
        Self {
            disk_bytes: 1024 * 1024 * 1024,
            inventory_entries: 65536,
            schema: SnapshotLimits::default(),
            segment: FormatLimits::default(),
        }
    }
}
#[derive(Debug, Error)]
pub enum ObjectError {
    #[error("dataset objects directory is already in use")]
    Locked,
    #[error("dataset directory has invalid ownership or permissions")]
    Ownership,
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
    directory: OwnedDirectory,
    objects: Dir,
    pending: Dir,
    store: String,
    limits: ObjectLimits,
    used_bytes: u64,
    entries: usize,
    uncertain: bool,
}
impl ObjectFiles {
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
            directory,
            objects,
            pending,
            store,
            limits,
            used_bytes,
            entries,
            uncertain: false,
        })
    }
    pub fn store_id(&self) -> &str {
        &self.store
    }
    pub fn charged_bytes(&self) -> u64 {
        self.used_bytes
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
    ) -> Result<ResolvedContractBundle, ObjectError> {
        let bytes = self.read(Kind::Schema, reference, self.limits.schema.bytes)?;
        Ok(ResolvedContractBundle::decode(&bytes, self.limits.schema)?)
    }
    pub fn read_segment(
        &self,
        reference: &ObjectRef,
        dataset: &str,
        schema: &ResolvedContractBundle,
    ) -> Result<Vec<u8>, ObjectError> {
        let bytes = self.read(Kind::Segment, reference, self.limits.segment.segment_bytes)?;
        SegmentReader::open(&bytes, &self.store, dataset, schema, self.limits.segment)?;
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
            .checked_add(2)
            .is_none_or(|n| n > self.limits.inventory_entries)
        {
            return Err(ObjectError::Limit("inventory"));
        }
        let charged = self
            .used_bytes
            .checked_add(bytes as u64)
            .filter(|n| *n <= self.limits.disk_bytes)
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
    ) -> Result<Vec<u8>, ObjectError> {
        if !valid_uuid(&reference.id) || !valid_digest(&reference.digest) {
            return Err(ObjectError::Corrupt);
        }
        let maximum = limit
            .checked_add(HEADER + CHECKSUM)
            .ok_or(ObjectError::Limit("object"))?;
        if reference.bytes > maximum as u64 || reference.bytes < (HEADER + CHECKSUM) as u64 {
            return Err(ObjectError::Limit("object"));
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
            files.read_schema(&reference).unwrap().digest(),
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
            reopened.read_schema(&reference).unwrap().digest(),
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
            store: files.store_id().into(),
            dataset: dataset.clone(),
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
        let bytes = files.read_segment(&reference, &dataset, &schema).unwrap();
        let reader = SegmentReader::open(
            &bytes,
            files.store_id(),
            &dataset,
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
            foreign.read_segment(&reference, &dataset, &schema),
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
    fn complete_publication_without_sync_ack_poison_requires_local_reopen() {
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
        drop(files);
        // Read presence is recovery evidence, not a fresh commit/durability acknowledgement.
        let reopened = ObjectFiles::open(
            tmp.path(),
            Durability::FileAndDirectory,
            ObjectLimits::default(),
        )
        .unwrap();
        assert!(reopened.charged_bytes() > 0);
        assert_eq!(reopened.objects.entries().unwrap().count(), 1);
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
        assert!(files.read_schema(&forged).is_err());
        let path = tmp
            .path()
            .join("objects")
            .join(format!("{}.schema", reference.id));
        let mut bytes = std::fs::read(&path).unwrap();
        let end = bytes.len() - 1;
        bytes[end] ^= 1;
        std::fs::write(path, bytes).unwrap();
        assert!(matches!(
            files.read_schema(&reference),
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
