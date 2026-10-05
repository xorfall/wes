//! Bounded read-only metadata for explicitly configured storage directories. No writer ownership,
//! content reads, synchronization, cleanup or implied durability acknowledgement.
use cap_fs_ext::DirExt;
use cap_std::{ambient_authority, fs::Dir};
use std::{io, path::Path};
use thiserror::Error;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StoragePlace {
    pub name: String,
    pub location: String,
    pub holds: String,
    /// Intended lifetime of this category, not a receipt for the currently counted files.
    pub durable: bool,
    pub files: u64,
    pub bytes: u64,
}
#[derive(Debug, Error)]
pub enum InventoryError {
    #[error("storage inventory exceeds its metadata budget")]
    Capacity,
    #[error("storage inventory contains a link or unexpected directory entry")]
    Invalid,
    #[error("storage inventory is unavailable")]
    Unavailable(#[source] io::Error),
}
struct Directory {
    dir: Dir,
    description: StoragePlace,
    depth: usize,
}
#[derive(Default)]
pub struct FileInventory {
    directories: Vec<Directory>,
}
impl FileInventory {
    pub fn new() -> Self {
        Self::default()
    }
    /// Directories must already exist. Capture their capability now, not a pathname to reopen
    /// on a later request. `depth` permits at most two explicitly configured subdirectory levels.
    pub fn add(
        &mut self,
        name: &str,
        path: &Path,
        holds: &str,
        durable: bool,
        depth: usize,
    ) -> Result<(), InventoryError> {
        if self.directories.len() >= 16 || name.len() > 128 || holds.len() > 2048 || depth > 2 {
            return Err(InventoryError::Capacity);
        }
        let location = path.canonicalize().map_err(InventoryError::Unavailable)?;
        let location = location.to_str().ok_or(InventoryError::Invalid)?;
        if location.len() > 4096 {
            return Err(InventoryError::Capacity);
        }
        let dir = Dir::open_ambient_dir(location, ambient_authority())
            .map_err(InventoryError::Unavailable)?;
        self.directories.push(Directory {
            dir,
            description: StoragePlace {
                name: name.into(),
                location: location.into(),
                holds: holds.into(),
                durable,
                files: 0,
                bytes: 0,
            },
            depth,
        });
        Ok(())
    }
    /// Point-in-time metadata, not an atomic snapshot across concurrent writer operations.
    /// Counts regular files (including store metadata); missing/unreadable is never fabricated zero.
    pub fn read(&self) -> Result<Vec<StoragePlace>, InventoryError> {
        let mut remaining = 100_000;
        self.directories
            .iter()
            .map(|entry| {
                let mut result = entry.description.clone();
                count(&entry.dir, entry.depth, &mut remaining, &mut result)?;
                Ok(result)
            })
            .collect()
    }
}
fn count(
    dir: &Dir,
    depth: usize,
    remaining: &mut usize,
    result: &mut StoragePlace,
) -> Result<(), InventoryError> {
    for entry in dir.entries().map_err(InventoryError::Unavailable)? {
        *remaining = remaining.checked_sub(1).ok_or(InventoryError::Capacity)?;
        let entry = entry.map_err(InventoryError::Unavailable)?;
        let name = entry.file_name();
        let metadata = dir
            .symlink_metadata(&name)
            .map_err(InventoryError::Unavailable)?;
        if metadata.is_file() {
            result.files += 1;
            result.bytes = result
                .bytes
                .checked_add(metadata.len())
                .filter(|size| *size <= 9_007_199_254_740_991)
                .ok_or(InventoryError::Capacity)?;
        } else if metadata.is_dir() && depth > 0 {
            let child = dir
                .open_dir_nofollow(&name)
                .map_err(InventoryError::Unavailable)?;
            count(&child, depth - 1, remaining, result)?;
        } else {
            return Err(InventoryError::Invalid);
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn exhausted_entry_or_exact_integer_budget_is_an_error() {
        let root = tempfile::tempdir().unwrap();
        std::fs::write(root.path().join("value"), [1]).unwrap();
        let mut inventory = FileInventory::new();
        inventory
            .add("test", root.path(), "fixture", false, 0)
            .unwrap();
        let entry = &inventory.directories[0];
        let mut result = entry.description.clone();
        assert!(matches!(
            count(&entry.dir, 0, &mut 0, &mut result),
            Err(InventoryError::Capacity)
        ));
        result.bytes = 9_007_199_254_740_991;
        assert!(matches!(
            count(&entry.dir, 0, &mut 1, &mut result),
            Err(InventoryError::Capacity)
        ));
    }
}
