//! Nonblocking file-lock ownership shared by persistent stores and settings.
use std::fs::{File, TryLockError};

/// Owns the lock itself, not just a descriptor that a forked child may inherit.
pub struct ExclusiveLock {
    file: File,
}
impl ExclusiveLock {
    pub fn try_lock(file: File) -> Result<Self, TryLockError> {
        file.try_lock()?;
        Ok(Self { file })
    }
}
impl Drop for ExclusiveLock {
    fn drop(&mut self) {
        // Closing one descriptor does not release flock while a child still owns
        // another. Unlock the shared description when the transaction ends.
        let _ = self.file.unlock();
    }
}

#[cfg(all(test, unix))]
mod lock_tests {
    use crate::api_library::exclusive_lock;

    #[test]
    fn owner_drop_releases_lock_even_with_an_inherited_descriptor() {
        let root = tempfile::tempdir().unwrap();
        let directory = root.path().canonicalize().unwrap();
        let owner = exclusive_lock(&directory, ".library.lock").unwrap();
        assert!(exclusive_lock(&directory, ".library.lock").is_err());
        // A duplicated descriptor shares the same open-file description, just as a
        // child inherits between fork and exec. Keep it alive past the owner.
        let inherited = owner.file.try_clone().unwrap();
        drop(owner);
        let next = exclusive_lock(&directory, ".library.lock");
        assert!(
            next.is_ok(),
            "the inherited descriptor retained a completed lock"
        );
        drop(inherited);
        assert!(exclusive_lock(&directory, ".library.lock").is_err());
        drop(next);
        assert!(exclusive_lock(&directory, ".library.lock").is_ok());
    }
}
