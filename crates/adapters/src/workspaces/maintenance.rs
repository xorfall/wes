//! Reclaim only unreferenced, marked generations, while retaining exclusive parent ownership.
use super::*;
use crate::filesystem::DirectoryError;
use std::collections::BTreeSet;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct CollectionReport {
    pub removed: usize,
    pub active: usize,
    /// Unmarked directories, unknown entries, symlinks and incomplete ownership are preserved.
    pub preserved: usize,
}

impl FileWorkspaces {
    /// Synchronous joined maintenance. Validate every named pointer before any deletion. A history
    /// writer lock protects an unreferenced generation still owned by a current/paused session.
    /// Only four known regular files can be removed; this never recursively deletes a tree.
    pub fn collect_unused(&mut self) -> Result<CollectionReport, WorkspaceFileError> {
        let mut referenced = BTreeSet::new();
        let mut candidates = vec![];
        for (index, entry) in self.directory.entries().map_err(storage)?.enumerate() {
            if index >= max_entries() {
                return Err(WorkspaceFileError::Capacity);
            }
            let entry = entry.map_err(storage)?;
            let file = entry.file_name();
            let Some(file) = file.to_str() else { continue };
            if let Some(encoded) = file.strip_prefix("workspace-") {
                let name = decode_name(encoded)?;
                referenced.insert(self.generation(&name)?.ok_or(WorkspaceFileError::Invalid)?);
            } else if let Some(id) = file.strip_prefix("generation-") {
                if valid_id(id) {
                    candidates.push((file.to_owned(), Some(id.to_owned())));
                }
            } else if let Some(id) = file.strip_prefix(".wes-retired-")
                && valid_id(id)
            {
                candidates.push((file.to_owned(), None));
            }
        }
        let mut report = CollectionReport::default();
        for (file, id) in candidates {
            if id.as_ref().is_some_and(|id| referenced.contains(id)) {
                continue;
            }
            if !self
                .directory
                .symlink_metadata(&file)
                .map_err(storage)?
                .is_dir()
            {
                report.preserved += 1;
                continue;
            }
            let directory = self.directory.open_dir_nofollow(&file).map_err(storage)?;
            // Check before acquiring ownership: never initialize unexpected directories. A marked
            // retired directory may recreate its removed lock after an interrupted cleanup.
            let mut expected = true;
            for (index, entry) in directory.entries().map_err(storage)?.enumerate() {
                if index >= 4 {
                    expected = false;
                    break;
                }
                let entry = entry.map_err(storage)?;
                let name = entry.file_name();
                if !matches!(
                    name.to_str(),
                    Some("journal.jsonl" | "recovery.jsonl" | ".wes-history" | ".wes-history.lock")
                ) || !directory
                    .symlink_metadata(&name)
                    .map_err(storage)?
                    .is_file()
                {
                    expected = false;
                    break;
                }
            }
            if !expected {
                report.preserved += 1;
                continue;
            }
            let directory = match OwnedDirectory::existing(
                directory,
                DirectoryKind::History,
                self.durability,
            ) {
                Ok(directory) => directory,
                Err(DirectoryError::Locked) => {
                    report.active += 1;
                    continue;
                }
                Err(
                    DirectoryError::Unowned | DirectoryError::NotRegular | DirectoryError::Insecure,
                ) => {
                    report.preserved += 1;
                    continue;
                }
                Err(DirectoryError::Io(error)) if error.kind() == io::ErrorKind::NotFound => {
                    report.preserved += 1;
                    continue;
                }
                Err(error) => return Err(storage(error)),
            };
            // Unreferenced by every pointer, inside a store this call owns exclusively, and its
            // writer lock taken: no session can be using this generation or come to use it.
            let directory = match id {
                Some(_) => match self.retire(&file, directory)? {
                    Retirement::Held(directory) => directory,
                    Retirement::Busy => {
                        report.active += 1;
                        continue;
                    }
                },
                // Already under a retired name: an earlier collection was interrupted here.
                None => directory,
            };
            // The recorded work goes first, while the writer lock is still held.
            for name in ["journal.jsonl", "recovery.jsonl"] {
                match directory.remove_file(name) {
                    Ok(()) => (),
                    Err(error) if error.kind() == io::ErrorKind::NotFound => (),
                    Err(error) => return Err(storage(error)),
                }
            }
            let directory = directory.into_retired();
            // Marker last: interrupted deletion can be resumed while ownership evidence remains.
            for name in [".wes-history.lock", ".wes-history"] {
                match directory.remove_file(name) {
                    Ok(()) => (),
                    Err(error) if error.kind() == io::ErrorKind::NotFound => (),
                    Err(error) => return Err(storage(error)),
                }
            }
            directory.remove_open_dir().map_err(storage)?;
            self.directory.sync().map_err(storage)?;
            report.removed += 1;
        }
        Ok(report)
    }
}
/// What giving a generation a retired name came to.
enum Retirement {
    /// Out of the loadable namespace, with its writer lock held.
    Held(OwnedDirectory),
    /// Something still holds the generation open; it is left whole under the name it had or
    /// under its retired name, and a later collection takes it up again.
    Busy,
}
impl FileWorkspaces {
    /// Renames a generation out of the loadable namespace and returns it with its writer lock.
    ///
    /// The caller establishes that the generation is unreferenced and that it owns the store;
    /// this operation cannot, and relies on it wherever the lock is not held.
    #[cfg(not(windows))]
    fn retire(&self, name: &str, owned: OwnedDirectory) -> Result<Retirement, WorkspaceFileError> {
        let retired = format!(".wes-retired-{}", Uuid::new_v4().simple());
        self.directory
            .rename(name, &self.directory, &retired)
            .map_err(storage)?;
        self.directory.sync().map_err(storage)?;
        Ok(Retirement::Held(owned))
    }
    /// Windows refuses to rename a directory while any file inside it is open, the writer lock
    /// included, so the lock is released for the rename and taken again under the retired
    /// name. The rename proves nothing by succeeding: an open file inside prevents it, a
    /// handle on the directory itself does not. A refused rename and a lock that cannot be
    /// retaken are both a generation still in use.
    #[cfg(windows)]
    fn retire(&self, name: &str, owned: OwnedDirectory) -> Result<Retirement, WorkspaceFileError> {
        const ERROR_ACCESS_DENIED: i32 = 5;
        const ERROR_SHARING_VIOLATION: i32 = 32;
        drop(owned.into_retired());
        let retired = format!(".wes-retired-{}", Uuid::new_v4().simple());
        match self.directory.rename(name, &self.directory, &retired) {
            Ok(()) => {}
            Err(error)
                if matches!(
                    error.raw_os_error(),
                    Some(ERROR_ACCESS_DENIED | ERROR_SHARING_VIOLATION)
                ) =>
            {
                return Ok(Retirement::Busy);
            }
            Err(error) => return Err(storage(error)),
        }
        self.directory.sync().map_err(storage)?;
        let reopened = self
            .directory
            .open_dir_nofollow(&retired)
            .map_err(storage)?;
        match OwnedDirectory::existing(reopened, DirectoryKind::History, self.durability) {
            Ok(directory) => Ok(Retirement::Held(directory)),
            Err(DirectoryError::Locked) => Ok(Retirement::Busy),
            Err(error) => Err(storage(error)),
        }
    }
}
fn valid_id(id: &str) -> bool {
    id.len() == 32
        && id
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

#[cfg(test)]
mod tests {
    use super::*;
    use wes_engine::history::{
        AppendReceipt, HistoryCapture, HistoryCaptureLimits, HistoryCheckpoint, Persistence,
    };

    fn image() -> wes_engine::history::HistoryImage {
        let receipt = AppendReceipt {
            persistence: Persistence::Volatile,
            end_offset: 0,
        };
        HistoryCapture::new(HistoryCaptureLimits::default()).finish(HistoryCheckpoint {
            journal: receipt,
            recovery: receipt,
        })
    }

    /// A reader that holds a file of an unreferenced generation open, without its lock: the
    /// generation cannot be renamed while the file is open, is reported as in use and left
    /// whole under its own name, and is collected once the reader is gone.
    #[cfg(windows)]
    #[test]
    fn a_generation_that_cannot_be_renamed_is_in_use_and_keeps_its_name_and_files() {
        let root = tempfile::tempdir().unwrap();
        let mut files =
            FileWorkspaces::open(root.path(), ReadLimits::default(), Durability::File).unwrap();
        let name = WorkspaceName::new("qa".into()).unwrap();
        files.save(&name, &image()).unwrap();
        let old = files.generation(&name).unwrap().unwrap();
        files.save(&name, &image()).unwrap();
        let generation = root.path().join(format!("generation-{old}"));
        let entries = |path: &std::path::Path| std::fs::read_dir(path).unwrap().count();
        let before = entries(&generation);

        let reader = std::fs::File::open(generation.join("journal.jsonl")).unwrap();
        let report = files.collect_unused().unwrap();
        assert_eq!((report.removed, report.active, report.preserved), (0, 1, 0));
        assert_eq!(entries(&generation), before);
        assert!(std::fs::read_dir(root.path()).unwrap().all(|entry| {
            !entry
                .unwrap()
                .file_name()
                .to_string_lossy()
                .starts_with(".wes-retired-")
        }));

        drop(reader);
        let report = files.collect_unused().unwrap();
        assert_eq!((report.removed, report.active, report.preserved), (1, 0, 0));
        assert!(!generation.exists());
        assert!(files.load(&name).is_ok());
    }

    /// The writer lock, not the name, decides. A retired generation whose lock is held keeps
    /// every file until the holder is gone, whatever the platform lets a rename do.
    #[test]
    fn a_retired_generation_with_a_live_writer_is_left_whole_until_the_writer_ends() {
        let mut builder = tempfile::Builder::new();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            builder.permissions(std::fs::Permissions::from_mode(0o700));
        }
        let root = builder.tempdir().unwrap();
        let mut files =
            FileWorkspaces::open(root.path(), ReadLimits::default(), Durability::File).unwrap();
        let name = WorkspaceName::new("qa".into()).unwrap();
        files.save(&name, &image()).unwrap();
        let old = files.generation(&name).unwrap().unwrap();
        files.save(&name, &image()).unwrap();
        // An interrupted collection left the superseded generation under a retired name.
        let retired = format!(".wes-retired-{}", Uuid::new_v4().simple());
        std::fs::rename(
            root.path().join(format!("generation-{old}")),
            root.path().join(&retired),
        )
        .unwrap();
        let listed = || {
            let mut names: Vec<_> = std::fs::read_dir(root.path().join(&retired))
                .unwrap()
                .map(|entry| entry.unwrap().file_name().into_string().unwrap())
                .collect();
            names.sort();
            names
        };
        let before = listed();
        assert!(before.contains(&"journal.jsonl".to_owned()));

        let writer = OwnedDirectory::existing(
            files.directory.open_dir_nofollow(&retired).unwrap(),
            DirectoryKind::History,
            Durability::File,
        )
        .unwrap();
        let report = files.collect_unused().unwrap();
        assert_eq!((report.removed, report.active, report.preserved), (0, 1, 0));
        assert_eq!(listed(), before);

        drop(writer);
        let report = files.collect_unused().unwrap();
        assert_eq!((report.removed, report.active, report.preserved), (1, 0, 0));
        assert!(!root.path().join(&retired).exists());
        assert!(files.load(&name).is_ok());
    }
}
