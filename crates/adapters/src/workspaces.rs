//! Named local histories. A name points atomically to one fully written two-stream generation.
//! Calls are synchronous leaf I/O: the application must run them on a joined blocking worker.
mod deletion;
mod maintenance;
use crate::{
    filesystem::{DirectoryKind, OwnedDirectory, private_options},
    journal::{Durability, FileHistory, ReadLimits},
};
use cap_fs_ext::DirExt;
use cap_std::fs::DirBuilder;
pub use deletion::{WorkspaceDeletion, WorkspaceIdentity};
pub use maintenance::CollectionReport;
use std::{
    io::{self, Read, Write},
    path::Path,
};
use thiserror::Error;
use uuid::Uuid;
use wes_engine::{
    history::{HistoryCaptureLimits, HistoryImage, RecordError},
    workspace::WorkspaceName,
};

fn max_entries() -> usize {
    wes_budgets::get("workspace.catalogue") as usize
}
const MANIFEST_HEADER: &str = "wes.workspace\n1\n";
#[derive(Debug, Error)]
pub enum WorkspaceFileError {
    #[error("there is no saved workspace with this name")]
    Missing,
    #[error("saved workspace metadata is invalid")]
    Invalid,
    #[error("the workspace directory entry limit has been reached")]
    Capacity,
    #[error("workspace storage operation failed")]
    Storage(#[source] Box<dyn std::error::Error + Send + Sync>),
    #[error("the workspace name was replaced, but final directory synchronization failed")]
    Published(#[source] io::Error),
    #[error(transparent)]
    History(#[from] RecordError),
}
fn storage(error: impl std::error::Error + Send + Sync + 'static) -> WorkspaceFileError {
    WorkspaceFileError::Storage(Box::new(error))
}

pub struct FileWorkspaces {
    directory: OwnedDirectory,
    limits: ReadLimits,
    durability: Durability,
}
/// Derived from validated owned histories, never an independently maintained index.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PayloadReferences {
    pub retained_views: Vec<(String, String)>,
    pub names: Vec<(String, String, bool)>,
}
#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub struct StorageCensus {
    pub retained_views: std::collections::BTreeMap<
        wes_engine::storage::ValueHandle,
        std::collections::BTreeSet<(String, String)>,
    >,
    pub identities: Vec<(String, String)>,
    pub names: Vec<(String, String)>,
    pub references: std::collections::BTreeMap<
        wes_engine::storage::ValueHandle,
        std::collections::BTreeSet<String>,
    >,
    pub cleanup: std::collections::BTreeMap<wes_engine::storage::ValueHandle, bool>,
}
impl FileWorkspaces {
    /// One bounded pass over all named histories. An unreadable history is never
    /// interpreted as zero references. The active image comes from its paused writer.
    pub fn storage_census(
        &mut self,
        active: &WorkspaceName,
        image: &HistoryImage,
    ) -> Result<StorageCensus, WorkspaceFileError> {
        self.storage_census_live(active, image, &std::collections::BTreeMap::new())
    }
    /// The application supplies settled images for every concurrently owned live writer.
    pub fn storage_census_live(
        &mut self,
        active: &WorkspaceName,
        image: &HistoryImage,
        live: &std::collections::BTreeMap<WorkspaceName, HistoryImage>,
    ) -> Result<StorageCensus, WorkspaceFileError> {
        let names = self.names()?;
        if names.len() > 256 {
            return Err(WorkspaceFileError::Capacity);
        }
        let mut census = StorageCensus::default();
        let mut bytes = 0u64;
        for name in names {
            let generation = self.generation(&name)?.ok_or(WorkspaceFileError::Missing)?;
            let loaded;
            let history = if &name == active {
                image
            } else if let Some(live) = live.get(&name) {
                live
            } else {
                loaded = self.load(&name)?.1;
                &loaded
            };
            bytes = bytes.saturating_add(history.charged_bytes());
            if bytes > 128 * 1024 * 1024 {
                return Err(WorkspaceFileError::Capacity);
            }
            census
                .identities
                .push((name.as_str().into(), generation.clone()));
            // Live writers can append without replacing the named generation. Include their
            // durable boundaries so a preview cannot survive a peer history change.
            let generation = if live.contains_key(&name) {
                let checkpoint = history.checkpoint();
                format!(
                    "{generation}:{}:{}",
                    checkpoint.journal.end_offset, checkpoint.recovery.end_offset
                )
            } else {
                generation
            };
            census.names.push((name.as_str().into(), generation));
            for entry in history.journal() {
                match entry {
                    wes_engine::history::JournalEntry::Payload { handle, .. }
                    | wes_engine::history::JournalEntry::ProtectedRun { handle, .. } => {
                        census
                            .references
                            .entry(handle.clone())
                            .or_default()
                            .insert(name.as_str().into());
                    }
                    wes_engine::history::JournalEntry::Noticed(n)
                        if n.context().node().is_some() =>
                    {
                        if let Some(handle) = n.context().handle() {
                            census
                                .references
                                .entry(handle.clone())
                                .or_default()
                                .insert(name.as_str().into());
                        }
                    }
                    wes_engine::history::JournalEntry::Snapshot(s) => {
                        if let Some(r) = &s.result {
                            census
                                .references
                                .entry(r.handle.clone())
                                .or_default()
                                .insert(name.as_str().into());
                        }
                    }
                    wes_engine::history::JournalEntry::Result(r) => {
                        census
                            .references
                            .entry(r.handle.clone())
                            .or_default()
                            .insert(name.as_str().into());
                    }
                    wes_engine::history::JournalEntry::Retired(r) => {
                        for handle in &r.payloads {
                            *census.cleanup.entry(handle.clone()).or_default() |=
                                r.protected.contains(handle);
                        }
                    }
                    _ => {}
                }
            }
            for (view, handle) in history
                .retained_view_inputs()
                .map_err(|_| WorkspaceFileError::Invalid)?
            {
                census
                    .references
                    .entry(handle.clone())
                    .or_default()
                    .insert(name.as_str().into());
                census
                    .retained_views
                    .entry(handle)
                    .or_default()
                    .insert((name.as_str().into(), view.as_str().into()));
            }
            if census.references.len() > 100_000 || census.cleanup.len() > 100_000 {
                return Err(WorkspaceFileError::Capacity);
            }
        }
        Ok(census)
    }
    /// All named pointers are included for freshness, even those not referencing the
    /// payload. A corrupt/locked/oversized history is an error, not an empty result.
    pub fn payload_references(
        &mut self,
        handle: &wes_engine::storage::ValueHandle,
        active: &WorkspaceName,
        active_referenced: bool,
        active_bytes: u64,
    ) -> Result<PayloadReferences, WorkspaceFileError> {
        let active_generation = self
            .generation(active)?
            .ok_or(WorkspaceFileError::Missing)?;
        let names = self.names()?;
        if names.len() > 256 {
            return Err(WorkspaceFileError::Capacity);
        }
        let mut total = 0u64;
        let mut references = vec![];
        let mut retained_views = vec![];
        for name in names {
            let generation = self.generation(&name)?.ok_or(WorkspaceFileError::Missing)?;
            let referenced = if generation == active_generation {
                total = total.saturating_add(active_bytes);
                active_referenced
            } else {
                let (_, loaded) = self.load(&name)?;
                total = total.saturating_add(loaded.charged_bytes());
                for (view, saved) in loaded
                    .retained_view_inputs()
                    .map_err(|_| WorkspaceFileError::Invalid)?
                {
                    if &saved == handle {
                        retained_views.push((name.as_str().into(), view.as_str().into()));
                    }
                }
                loaded
                    .journal()
                    .iter()
                    .any(|entry| entry.payload_reference() == Some(handle))
            };
            if total > 128 * 1024 * 1024 {
                return Err(WorkspaceFileError::Capacity);
            }
            references.push((name.as_str().to_owned(), generation, referenced));
        }
        Ok(PayloadReferences {
            names: references,
            retained_views,
        })
    }
    pub fn open(
        path: &Path,
        limits: ReadLimits,
        durability: Durability,
    ) -> Result<Self, WorkspaceFileError> {
        Ok(Self {
            directory: OwnedDirectory::open(path, DirectoryKind::Workspaces, durability)
                .map_err(storage)?,
            limits,
            durability,
        })
    }
    /// Copy both streams, then replace one pointer. Any failure before publication leaves the prior
    /// name intact. No prior generation is deleted: it may still belong to a running/paused session.
    /// Incomplete and superseded generations count against the bounded directory limit.
    pub fn save(
        &mut self,
        name: &WorkspaceName,
        image: &HistoryImage,
    ) -> Result<(), WorkspaceFileError> {
        self.save_with_sync(name, image, |directory| directory.sync())
    }
    fn save_with_sync(
        &mut self,
        name: &WorkspaceName,
        image: &HistoryImage,
        sync: impl FnOnce(&OwnedDirectory) -> io::Result<()>,
    ) -> Result<(), WorkspaceFileError> {
        self.ensure_recreatable(name)?;
        let identity = self
            .identity(name)?
            .map(|i| i.id)
            .unwrap_or_else(|| Uuid::new_v4().simple().to_string());
        self.check_capacity(2)?;
        let generation = Uuid::new_v4().simple().to_string();
        let path = format!("generation-{generation}");
        let mut builder = DirBuilder::new();
        #[cfg(unix)]
        {
            use cap_std::fs::DirBuilderExt;
            builder.mode(0o700);
        }
        self.directory
            .create_dir_with(&path, &builder)
            .map_err(storage)?;
        let directory = self.directory.open_dir_nofollow(&path).map_err(storage)?;
        let directory =
            OwnedDirectory::from_dir(directory, DirectoryKind::History, self.durability)
                .map_err(storage)?;
        let mut history = FileHistory::from_directory(directory, self.limits, self.durability)?;
        history.seed(image)?;
        drop(history);
        // The generation entry must itself be durable before a durable pointer can refer to it.
        self.directory.sync().map_err(storage)?;
        let pending = format!(".wes-pending-{}", Uuid::new_v4().simple());
        let mut options = private_options();
        options.write(true).create_new(true);
        let mut pointer = self
            .directory
            .open_with(&pending, &options)
            .map_err(storage)?;
        let bytes = format!("{MANIFEST_HEADER}{identity}\n{generation}\n");
        let written = pointer
            .write_all(bytes.as_bytes())
            .and_then(|()| pointer.sync_all());
        drop(pointer);
        if let Err(error) = written {
            let _ = self.directory.remove_file(&pending);
            return Err(storage(error));
        }
        if let Err(error) = self
            .directory
            .rename(&pending, &self.directory, file_name(name))
        {
            let _ = self.directory.remove_file(&pending);
            return Err(storage(error));
        }
        sync(&self.directory).map_err(WorkspaceFileError::Published)
    }
    /// Return exclusive history ownership and its matching synchronized image. No session is
    /// stopped and no provider is run. The caller reconstructs this candidate before switching.
    pub fn load(
        &mut self,
        name: &WorkspaceName,
    ) -> Result<(FileHistory, HistoryImage), WorkspaceFileError> {
        let generation = self.generation(name)?.ok_or(WorkspaceFileError::Missing)?;
        let directory = self
            .directory
            .open_dir_nofollow(format!("generation-{generation}"))
            .map_err(storage)?;
        for file in [".wes-history", "journal.jsonl", "recovery.jsonl"] {
            if !directory.symlink_metadata(file).map_err(storage)?.is_file() {
                return Err(WorkspaceFileError::Invalid);
            }
        }
        let directory =
            OwnedDirectory::from_dir(directory, DirectoryKind::History, self.durability)
                .map_err(storage)?;
        let mut history = FileHistory::from_directory(directory, self.limits, self.durability)?;
        let image = history.capture(HistoryCaptureLimits::default())?;
        Ok((history, image))
    }
    pub fn names(&self) -> Result<Vec<WorkspaceName>, WorkspaceFileError> {
        let mut names = vec![];
        for (index, entry) in self.directory.entries().map_err(storage)?.enumerate() {
            if index >= max_entries() {
                return Err(WorkspaceFileError::Capacity);
            }
            let entry = entry.map_err(storage)?;
            let file = entry.file_name();
            let Some(file) = file.to_str() else { continue };
            let Some(encoded) = file.strip_prefix("workspace-") else {
                continue;
            };
            let name = decode_name(encoded)?;
            self.generation(&name)?.ok_or(WorkspaceFileError::Invalid)?;
            names.push(name);
        }
        names.sort();
        Ok(names)
    }
    fn check_capacity(&self, additional: usize) -> Result<(), WorkspaceFileError> {
        for (index, entry) in self.directory.entries().map_err(storage)?.enumerate() {
            entry.map_err(storage)?;
            if index >= max_entries() - additional {
                return Err(WorkspaceFileError::Capacity);
            }
        }
        Ok(())
    }
    fn generation(&self, name: &WorkspaceName) -> Result<Option<String>, WorkspaceFileError> {
        Ok(self.identity(name)?.map(|i| i.generation))
    }
}
fn file_name(name: &WorkspaceName) -> String {
    use std::fmt::Write;
    let mut encoded = String::from("workspace-");
    for byte in name.as_str().bytes() {
        write!(&mut encoded, "{byte:02x}").expect("writing to String");
    }
    encoded
}
fn decode_name(encoded: &str) -> Result<WorkspaceName, WorkspaceFileError> {
    if encoded.len() > 192
        || !encoded.len().is_multiple_of(2)
        || !encoded
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    {
        return Err(WorkspaceFileError::Invalid);
    }
    let bytes = (0..encoded.len())
        .step_by(2)
        .map(|i| {
            u8::from_str_radix(&encoded[i..i + 2], 16).map_err(|_| WorkspaceFileError::Invalid)
        })
        .collect::<Result<Vec<_>, _>>()?;
    WorkspaceName::new(String::from_utf8(bytes).map_err(|_| WorkspaceFileError::Invalid)?)
        .map_err(|_| WorkspaceFileError::Invalid)
}

#[cfg(test)]
mod tests {
    use super::*;
    use wes_engine::history::{
        AppendReceipt, CommandRecord, HistoryCapture, HistoryCheckpoint, JournalEntry, Persistence,
        Record,
    };
    #[test]
    fn a_final_sync_failure_reports_publication_and_keeps_the_complete_new_generation() {
        let mut builder = tempfile::Builder::new();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            builder.permissions(std::fs::Permissions::from_mode(0o700));
        }
        let root = builder.tempdir().unwrap();
        let mut store =
            FileWorkspaces::open(root.path(), ReadLimits::default(), Durability::File).unwrap();
        let name = WorkspaceName::new("kept".into()).unwrap();
        let receipt = AppendReceipt {
            persistence: Persistence::Volatile,
            end_offset: 0,
        };
        let checkpoint = HistoryCheckpoint {
            journal: receipt,
            recovery: receipt,
        };
        let mut capture = HistoryCapture::new(HistoryCaptureLimits::default());
        store.save(&name, &capture.finish(checkpoint)).unwrap();
        capture = HistoryCapture::new(HistoryCaptureLimits::default());
        let record = JournalEntry::Command(CommandRecord {
            source_name: "fixture.wes".into(),
            source_start: wes_language::Position { line: 1, column: 1 },
            changed_nodes: vec![],
            document: None,
            revision_of: None,
            environments: None,
            cell: "new".into(),
            text: ":help".into(),
            replay: ":help".into(),
            nodes: vec![],
            type_sources: Default::default(),
            calculation_package: None,
            imports: vec![],
        });
        capture.push(Record::Journal(record.clone())).unwrap();
        let result = store.save_with_sync(&name, &capture.finish(checkpoint), |_| {
            Err(io::Error::other("synthetic sync failure"))
        });
        assert!(matches!(result, Err(WorkspaceFileError::Published(_))));
        let (_, image) = store.load(&name).unwrap();
        assert_eq!(image.journal(), &[record]);
    }
}
