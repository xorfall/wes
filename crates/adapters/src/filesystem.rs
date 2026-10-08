//! Shared ownership/permission policy for persistent directories. No domain records live here.
use crate::file_lock::ExclusiveLock;
use cap_fs_ext::{FollowSymlinks, OpenOptionsFollowExt, OpenOptionsSyncExt};
use cap_std::{
    ambient_authority,
    fs::{Dir, OpenOptions},
};
use std::{
    io::{self, Read, Write},
    ops::Deref,
    path::Path,
};
use thiserror::Error;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Durability {
    /// File synchronization, without a directory-entry power-loss claim.
    File,
    /// Also require directory synchronization; unsupported filesystems fail explicitly.
    FileAndDirectory,
}

impl Durability {
    /// Whether this host can establish the mode, as opposed to merely attempt it.
    ///
    /// Synchronizing a file is supported everywhere. That a directory's entries are on stable
    /// storage is a documented guarantee of a POSIX directory fsync. Windows flushes a
    /// directory handle without stating that guarantee, so a successful flush there is not
    /// evidence of it and the stronger mode is not offered.
    pub fn supported(self) -> bool {
        self == Self::File || cfg!(not(windows))
    }
}

pub(crate) enum DirectoryKind {
    Values,
    History,
    Workspaces,
    Datasets,
}
impl DirectoryKind {
    fn names(&self) -> (&'static str, &'static str, &'static [u8]) {
        match self {
            Self::Datasets => (".wes-datasets", ".wes-datasets.lock", b"wes.datasets\n1\n"),
            Self::Values => (
                ".wes-value-store",
                ".wes-values.lock",
                b"wes.value-store\n1\n",
            ),
            Self::Workspaces => (
                ".wes-workspaces",
                ".wes-workspaces.lock",
                b"wes.workspaces\n1\n",
            ),
            Self::History => (".wes-history", ".wes-history.lock", b"wes.history\n1\n"),
        }
    }
}
#[derive(Debug, Error)]
pub(crate) enum DirectoryError {
    #[error("directory already has a writer")]
    Locked,
    #[error("directory is not private")]
    Insecure,
    #[error("directory does not have the expected ownership")]
    Unowned,
    #[error("managed entry is not a regular file")]
    NotRegular,
    #[error("filesystem ownership operation failed")]
    Io(#[from] io::Error),
}

/// The capability and its writer lock share one lifetime; neither can be cloned independently.
pub(crate) struct OwnedDirectory {
    dir: Dir,
    _lock: ExclusiveLock,
    durability: Durability,
}
impl OwnedDirectory {
    /// Inspect existing ownership without initializing an unmarked directory. Used by maintenance,
    /// where a plausible pathname alone must never confer authority to remove content.
    pub(crate) fn existing(
        dir: Dir,
        kind: DirectoryKind,
        durability: Durability,
    ) -> Result<Self, DirectoryError> {
        let (marker, _, expected) = kind.names();
        verify_marker(&dir, marker, expected)?;
        Self::from_dir(dir, kind, durability)
    }
    /// Release a retired generation's lock before removing its files on platforms that prohibit
    /// deleting an open lock. The caller retains the exclusive parent and has removed this path
    /// from the loadable generation namespace; no application can acquire a new writer there.
    pub(crate) fn into_retired(self) -> Dir {
        let Self { dir, _lock, .. } = self;
        drop(_lock);
        dir
    }
    pub fn open(
        path: &Path,
        kind: DirectoryKind,
        durability: Durability,
    ) -> Result<Self, DirectoryError> {
        let mut builder = std::fs::DirBuilder::new();
        builder.recursive(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::DirBuilderExt;
            builder.mode(0o700);
        }
        builder.create(path)?;
        let dir = Dir::open_ambient_dir(path, ambient_authority())?;
        Self::from_dir(dir, kind, durability)
    }
    pub(crate) fn from_dir(
        dir: Dir,
        kind: DirectoryKind,
        durability: Durability,
    ) -> Result<Self, DirectoryError> {
        // A requirement this host cannot establish is refused before a marker or a lock is
        // written; no receipt can then claim it.
        if !durability.supported() {
            return Err(DirectoryError::Io(io::Error::new(
                io::ErrorKind::Unsupported,
                "directory-entry durability is not established on this platform; use file durability",
            )));
        }
        #[cfg(unix)]
        {
            use cap_std::fs::PermissionsExt;
            if dir.dir_metadata()?.permissions().mode() & 0o077 != 0 {
                return Err(DirectoryError::Insecure);
            }
        }
        let (marker_name, lock_name, marker_bytes) = kind.names();
        let owned = marker_exists(&dir, marker_name)?;
        if owned {
            verify_marker(&dir, marker_name, marker_bytes)?;
        } else {
            for entry in dir.entries()? {
                if entry?.file_name() != lock_name {
                    return Err(DirectoryError::Unowned);
                }
            }
        }
        let mut options = private_options();
        options.read(true).write(true).create(true);
        let lock = dir.open_with(lock_name, &options)?.into_std();
        if !lock.metadata()?.is_file() {
            return Err(DirectoryError::NotRegular);
        }
        let lock = ExclusiveLock::try_lock(lock).map_err(|error| match error {
            std::fs::TryLockError::WouldBlock => DirectoryError::Locked,
            std::fs::TryLockError::Error(error) => DirectoryError::Io(error),
        })?;
        // Another initializer may have completed between the first inspection and lock acquisition.
        if marker_exists(&dir, marker_name)? {
            verify_marker(&dir, marker_name, marker_bytes)?;
        } else {
            let mut options = private_options();
            options.write(true).create_new(true);
            let mut marker = dir.open_with(marker_name, &options)?;
            let written = marker
                .write_all(marker_bytes)
                .and_then(|()| marker.sync_all());
            drop(marker);
            if let Err(error) = written {
                let _ = dir.remove_file(marker_name);
                return Err(error.into());
            }
        }
        let owned = Self {
            dir,
            _lock: lock,
            durability,
        };
        owned.sync()?;
        Ok(owned)
    }
    pub fn sync(&self) -> io::Result<()> {
        match self.durability {
            Durability::File => Ok(()),
            Durability::FileAndDirectory => sync_directory(&self.dir),
        }
    }
    pub fn durability(&self) -> Durability {
        self.durability
    }
}
impl Deref for OwnedDirectory {
    type Target = Dir;
    fn deref(&self) -> &Dir {
        &self.dir
    }
}
/// Flush a directory's entries. A capability directory can be a path-only handle (Linux opens it
/// with `O_PATH`), which the OS refuses to synchronize; a readable handle to the same directory
/// is opened through the capability instead. Windows flushes a directory only through a handle
/// that may write to it, and opens a directory as a file only with backup semantics; a file
/// system that cannot flush one reports the failure.
///
/// # Errors
/// Returns the I/O error when the directory cannot be reopened or synchronized.
pub fn sync_directory(dir: &Dir) -> io::Result<()> {
    #[cfg(windows)]
    {
        use cap_std::fs::OpenOptionsExt;
        const FILE_FLAG_BACKUP_SEMANTICS: u32 = 0x0200_0000;
        let mut options = OpenOptions::new();
        options.write(true).custom_flags(FILE_FLAG_BACKUP_SEMANTICS);
        dir.open_with(".", &options)?.sync_all()
    }
    #[cfg(not(windows))]
    {
        dir.open(".")?.sync_all()
    }
}
/// Whether flushing a file requires a handle that may write to it. Windows refuses to flush
/// through a read-only handle; elsewhere read access is enough and keeps read-only records
/// openable.
const FLUSH_NEEDS_WRITE_ACCESS: bool = cfg!(windows);

/// Flush a file whose bytes are already complete, reached through a directory capability.
/// Nothing is written through the handle and the file is neither created nor truncated.
///
/// # Errors
/// Returns the I/O error when the file cannot be opened for the flush or the flush fails.
pub fn sync_existing_file_in(dir: &Dir, path: impl AsRef<Path>) -> io::Result<()> {
    let mut options = OpenOptions::new();
    options.read(true).write(FLUSH_NEEDS_WRITE_ACCESS);
    dir.open_with(path, &options)?.sync_all()
}
/// The same operation for a caller that holds an ambient path instead of a capability.
///
/// # Errors
/// Returns the I/O error when the file cannot be opened for the flush or the flush fails.
pub fn sync_existing_file(path: &Path) -> io::Result<()> {
    std::fs::OpenOptions::new()
        .read(true)
        .write(FLUSH_NEEDS_WRITE_ACCESS)
        .open(path)?
        .sync_all()
}
fn marker_exists(dir: &Dir, name: &str) -> Result<bool, DirectoryError> {
    match dir.symlink_metadata(name) {
        Ok(metadata) if metadata.is_file() => Ok(true),
        Ok(_) => Err(DirectoryError::NotRegular),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(error.into()),
    }
}
fn verify_marker(dir: &Dir, name: &str, expected: &[u8]) -> Result<(), DirectoryError> {
    let mut options = private_options();
    options.read(true);
    let file = dir.open_with(name, &options)?;
    if !file.metadata()?.is_file() {
        return Err(DirectoryError::NotRegular);
    }
    let mut bytes = vec![];
    file.take(expected.len() as u64 + 1)
        .read_to_end(&mut bytes)?;
    if bytes != expected {
        return Err(DirectoryError::Unowned);
    }
    Ok(())
}
pub(crate) fn private_options() -> OpenOptions {
    let mut options = OpenOptions::new();
    options.follow(FollowSymlinks::No).nonblock(true);
    #[cfg(unix)]
    {
        use cap_std::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    options
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Both entry points flush a complete file without changing it, and report a file they
    /// cannot open for the flush instead of passing over it.
    #[test]
    fn an_existing_file_is_flushed_unchanged_through_either_entry_point() {
        let temporary = tempfile::tempdir().unwrap();
        let path = temporary.path().join("record.json");
        std::fs::write(&path, b"complete bytes").unwrap();
        let dir = Dir::open_ambient_dir(temporary.path(), ambient_authority()).unwrap();
        sync_existing_file_in(&dir, "record.json").unwrap();
        sync_existing_file(&path).unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), b"complete bytes");

        assert_eq!(
            sync_existing_file_in(&dir, "absent.json")
                .unwrap_err()
                .kind(),
            io::ErrorKind::NotFound
        );
        assert_eq!(
            sync_existing_file(&temporary.path().join("absent.json"))
                .unwrap_err()
                .kind(),
            io::ErrorKind::NotFound
        );
        assert!(!temporary.path().join("absent.json").exists());
        // A capability does not reach outside its directory for the flush either.
        assert!(sync_existing_file_in(&dir, "../record.json").is_err());

        // A record the platform will not let this handle flush is an error, not a skipped step.
        let mut read_only = std::fs::metadata(&path).unwrap().permissions();
        read_only.set_readonly(true);
        std::fs::set_permissions(&path, read_only).unwrap();
        let outcomes = [
            sync_existing_file_in(&dir, "record.json"),
            sync_existing_file(&path),
        ];
        for outcome in outcomes {
            assert_eq!(outcome.is_err(), FLUSH_NEEDS_WRITE_ACCESS);
        }
        let mut writable = std::fs::metadata(&path).unwrap().permissions();
        #[allow(clippy::permissions_set_readonly_false)]
        writable.set_readonly(false);
        std::fs::set_permissions(&path, writable).unwrap();
    }

    #[cfg(not(windows))]
    #[test]
    fn directory_durability_synchronizes_through_a_capability_handle() {
        let temporary = tempfile::tempdir().unwrap();
        let path = temporary.path().join("values");

        let owned =
            OwnedDirectory::open(&path, DirectoryKind::Values, Durability::FileAndDirectory)
                .unwrap();

        owned.sync().unwrap();
        sync_directory(&Dir::open_ambient_dir(&path, ambient_authority()).unwrap()).unwrap();
    }

    /// A host that cannot establish directory-entry durability refuses the requirement before
    /// it creates anything, and stays fully usable with file durability.
    #[test]
    fn an_unsupported_durability_is_refused_and_leaves_the_directory_for_a_supported_one() {
        assert!(Durability::File.supported());
        assert_eq!(Durability::FileAndDirectory.supported(), cfg!(not(windows)));
        let temporary = tempfile::tempdir().unwrap();
        let path = temporary.path().join("values");
        let strong =
            OwnedDirectory::open(&path, DirectoryKind::Values, Durability::FileAndDirectory);
        if Durability::FileAndDirectory.supported() {
            assert_eq!(strong.unwrap().durability(), Durability::FileAndDirectory);
            return;
        }
        let Err(DirectoryError::Io(refused)) = strong else {
            panic!("an unsupported durability was accepted");
        };
        assert_eq!(refused.kind(), io::ErrorKind::Unsupported);
        // Neither the marker nor the lock was written under a claim that cannot be kept.
        assert_eq!(std::fs::read_dir(&path).unwrap().count(), 0);
        let owned = OwnedDirectory::open(&path, DirectoryKind::Values, Durability::File).unwrap();
        assert_eq!(owned.durability(), Durability::File);
        owned.sync().unwrap();
        drop(owned);
        // The directory it now owns is still refused for the stronger mode, and reopens.
        assert!(
            OwnedDirectory::open(&path, DirectoryKind::Values, Durability::FileAndDirectory)
                .is_err()
        );
        OwnedDirectory::open(&path, DirectoryKind::Values, Durability::File).unwrap();
    }

    /// The explicit synchronization is performed and its refusal reported. On Windows the
    /// flush needs a handle that may write to the directory; while another holder refuses to
    /// share that access it cannot be made. That the flush succeeds otherwise is an attempt,
    /// not the stronger durability mode, which this host does not offer.
    #[cfg(windows)]
    #[test]
    fn directory_synchronization_is_performed_and_its_refusal_is_reported() {
        use std::os::windows::fs::OpenOptionsExt;
        const FILE_SHARE_READ: u32 = 0x1;
        const FILE_FLAG_BACKUP_SEMANTICS: u32 = 0x0200_0000;
        const ERROR_SHARING_VIOLATION: i32 = 32;
        let temporary = tempfile::tempdir().unwrap();
        let path = temporary.path().join("values");
        let owned = OwnedDirectory::open(&path, DirectoryKind::Values, Durability::File).unwrap();
        let capability = Dir::open_ambient_dir(&path, ambient_authority()).unwrap();
        sync_directory(&capability).unwrap();

        let holder = std::fs::OpenOptions::new()
            .read(true)
            .share_mode(FILE_SHARE_READ)
            .custom_flags(FILE_FLAG_BACKUP_SEMANTICS)
            .open(&path)
            .unwrap();
        assert_eq!(
            sync_directory(&capability).unwrap_err().raw_os_error(),
            Some(ERROR_SHARING_VIOLATION)
        );
        // File durability makes no directory claim and asks nothing of the file system.
        owned.sync().unwrap();
        drop(holder);
        sync_directory(&capability).unwrap();
    }
}
