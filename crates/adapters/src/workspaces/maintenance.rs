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
            if id.is_some() {
                let retired = format!(".wes-retired-{}", Uuid::new_v4().simple());
                self.directory
                    .rename(&file, &self.directory, &retired)
                    .map_err(storage)?;
                self.directory.sync().map_err(storage)?;
            }
            let directory = directory.into_retired();
            // Marker last: interrupted deletion can be resumed while ownership evidence remains.
            for name in [
                "journal.jsonl",
                "recovery.jsonl",
                ".wes-history.lock",
                ".wes-history",
            ] {
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
fn valid_id(id: &str) -> bool {
    id.len() == 32
        && id
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}
