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

pub(crate) enum DirectoryKind {
    Values,
    History,
    Workspaces,
}
impl DirectoryKind {
    fn names(&self) -> (&'static str, &'static str, &'static [u8]) {
        match self {
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
/// is opened through the capability instead.
///
/// # Errors
/// Returns the I/O error when the directory cannot be reopened or synchronized.
pub fn sync_directory(dir: &Dir) -> io::Result<()> {
    dir.open(".")?.sync_all()
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
}
