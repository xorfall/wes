//! Application workspace storage port. Leaf I/O runs on the application's joined blocking jobs.
//! An opened name owns a live writable generation, not merely a saved snapshot. Implementations
//! must preserve that generation's durable appends across restart and never silently fall back.
use std::path::Path;
pub use wes_adapters::workspaces::{
    CollectionReport, PayloadReferences, StorageCensus, WorkspaceDeletion, WorkspaceIdentity,
};
use wes_adapters::{
    journal::{Durability, ReadLimits},
    workspaces::{FileWorkspaces, WorkspaceFileError},
};
use wes_engine::{
    history::{
        AppendReceipt, HistoryCaptureLimits, HistoryCursor, HistoryImage, HistoryPage,
        HistoryPageLimits, JournalSink, Record, RecordError,
    },
    storage::ValueHandle,
    workspace::WorkspaceName,
};

type Cause = Box<dyn std::error::Error + Send + Sync>;
#[derive(Debug, thiserror::Error)]
pub enum BackendError {
    #[error("there is no saved workspace with this name")]
    Missing,
    #[error("saved workspace metadata is invalid")]
    Invalid,
    #[error("workspace storage capacity reached")]
    Capacity,
    #[error("workspace backend operation unsupported: {0}")]
    Unsupported(&'static str),
    #[error("workspace storage operation failed")]
    Storage(#[source] Cause),
    /// Includes an unknown publication result. Callers must not assume the old binding survived.
    #[error("workspace publication reached an uncertain completion; reconcile before retrying")]
    Published(#[source] Cause),
    #[error(transparent)]
    History(#[from] RecordError),
}
impl From<WorkspaceFileError> for BackendError {
    fn from(error: WorkspaceFileError) -> Self {
        match error {
            WorkspaceFileError::Missing => Self::Missing,
            WorkspaceFileError::Invalid => Self::Invalid,
            WorkspaceFileError::Capacity => Self::Capacity,
            WorkspaceFileError::Storage(e) => Self::Storage(e),
            WorkspaceFileError::Published(e) => Self::Published(Box::new(e)),
            WorkspaceFileError::History(e) => Self::History(e),
        }
    }
}
/// Forward every JournalSink operation; using its default page/capture would lose functionality.
pub struct BackendHistory(Box<dyn JournalSink>);
impl BackendHistory {
    pub fn new(sink: impl JournalSink + 'static) -> Self {
        Self(Box::new(sink))
    }
}
impl JournalSink for BackendHistory {
    fn claim_request(
        &mut self,
        record: wes_engine::history::RequestRecord,
        current: bool,
    ) -> Result<wes_engine::history::RequestClaim, RecordError> {
        self.0.claim_request(record, current)
    }
    fn find_request(
        &mut self,
        namespace: &str,
        request: &str,
    ) -> Result<Option<wes_engine::history::RequestRecord>, RecordError> {
        self.0.find_request(namespace, request)
    }
    fn append(&mut self, record: &Record) -> Result<AppendReceipt, RecordError> {
        self.0.append(record)
    }
    fn page(
        &mut self,
        cursor: Option<HistoryCursor>,
        limits: HistoryPageLimits,
    ) -> Result<HistoryPage, RecordError> {
        self.0.page(cursor, limits)
    }
    fn capture(&mut self, limits: HistoryCaptureLimits) -> Result<HistoryImage, RecordError> {
        self.0.capture(limits)
    }
}
#[derive(Clone, Copy, Debug, Default)]
pub struct BackendCapabilities {
    /// Complete reference census and replacement/cleanup semantics needed for destructive work.
    pub retention: bool,
    /// Explicit opt-in to existing startup/save orphan collection and admitted cleanup replay.
    pub automatic_cleanup: bool,
}
pub trait WorkspaceBackend: Send {
    fn set_retained_dataset_publication(
        &mut self,
        _: std::sync::Arc<dyn wes_engine::history::RetainedDatasetPublication>,
    ) -> Result<(), BackendError> {
        Err(BackendError::Unsupported("workspace dataset retention"))
    }
    fn capabilities(&self) -> BackendCapabilities {
        BackendCapabilities::default()
    }
    fn names(&self) -> Result<Vec<WorkspaceName>, BackendError>;
    fn identity(&self, _: &WorkspaceName) -> Result<Option<WorkspaceIdentity>, BackendError> {
        Err(BackendError::Unsupported("workspace deletion"))
    }
    fn has_deletions(&self) -> Result<bool, BackendError> {
        Ok(false)
    }
    fn pending_deletions(&self) -> Result<Vec<WorkspaceDeletion>, BackendError> {
        Ok(vec![])
    }
    fn delete_workspace(&mut self, _: &WorkspaceDeletion) -> Result<(), BackendError> {
        Err(BackendError::Unsupported("workspace deletion"))
    }
    fn finish_deletion(&mut self, _: &str) -> Result<(), BackendError> {
        Err(BackendError::Unsupported("workspace deletion"))
    }

    /// Open an exclusive live generation and capture it without executing providers or grants.
    /// A failed candidate must leave the prior active generation untouched.
    fn load(
        &mut self,
        name: &WorkspaceName,
    ) -> Result<(BackendHistory, HistoryImage), BackendError>;
    /// Publish a named generation from an inert image. Publication uncertainty must be Published.
    fn save(&mut self, name: &WorkspaceName, image: &HistoryImage) -> Result<(), BackendError>;
    /// Synchronize/save the active name without detaching its live writer from restart's binding.
    /// No default: a backend must explicitly justify live generation vs snapshot behavior.
    fn save_current(
        &mut self,
        name: &WorkspaceName,
        image: &HistoryImage,
    ) -> Result<(), BackendError>;
    fn collect_unused(&mut self) -> Result<CollectionReport, BackendError> {
        Err(BackendError::Unsupported("collection"))
    }
    fn payload_references(
        &mut self,
        _: &ValueHandle,
        _: &WorkspaceName,
        _: bool,
        _: u64,
    ) -> Result<PayloadReferences, BackendError> {
        Err(BackendError::Unsupported("payload references"))
    }
    /// Paused images for every other live writer. Backends must never reopen those writers.
    fn storage_census_live(
        &mut self,
        name: &WorkspaceName,
        image: &HistoryImage,
        live: &std::collections::BTreeMap<WorkspaceName, HistoryImage>,
    ) -> Result<StorageCensus, BackendError> {
        if live.is_empty() {
            self.storage_census(name, image)
        } else {
            Err(BackendError::Unsupported("live workspace census"))
        }
    }
    fn payload_references_live(
        &mut self,
        handle: &ValueHandle,
        name: &WorkspaceName,
        image: &HistoryImage,
        live: &std::collections::BTreeMap<WorkspaceName, HistoryImage>,
    ) -> Result<PayloadReferences, BackendError> {
        if live.is_empty() {
            let mut references = self.payload_references(
                handle,
                name,
                image
                    .journal()
                    .iter()
                    .any(|e| e.payload_reference() == Some(handle)),
                image.charged_bytes(),
            )?;
            for (view, saved) in image
                .retained_view_inputs()
                .map_err(|_| BackendError::Unsupported("invalid view references"))?
            {
                if &saved == handle {
                    references
                        .retained_views
                        .push((name.as_str().into(), view.as_str().into()));
                }
            }
            references.retained_views.sort();
            references.retained_views.dedup();
            Ok(references)
        } else {
            let census = self.storage_census_live(name, image, live)?;
            let referenced = census.references.get(handle);
            Ok(PayloadReferences {
                retained_views: census
                    .retained_views
                    .get(handle)
                    .map(|views| views.iter().cloned().collect())
                    .unwrap_or_default(),
                names: census
                    .names
                    .into_iter()
                    .map(|(name, generation)| {
                        let used = referenced.is_some_and(|names| names.contains(&name));
                        (name, generation, used)
                    })
                    .collect(),
            })
        }
    }
    fn storage_census(
        &mut self,
        _: &WorkspaceName,
        _: &HistoryImage,
    ) -> Result<StorageCensus, BackendError> {
        Err(BackendError::Unsupported("storage census"))
    }
}
/// Existing local filesystem behavior. Construct on a blocking worker before injecting.
pub struct FileBackend(FileWorkspaces);
impl FileBackend {
    pub fn open(
        path: &Path,
        limits: ReadLimits,
        durability: Durability,
    ) -> Result<Self, BackendError> {
        Ok(Self(FileWorkspaces::open(path, limits, durability)?))
    }
}
impl WorkspaceBackend for FileBackend {
    fn set_retained_dataset_publication(
        &mut self,
        publication: std::sync::Arc<dyn wes_engine::history::RetainedDatasetPublication>,
    ) -> Result<(), BackendError> {
        self.0.set_retained_dataset_publication(publication);
        Ok(())
    }
    fn identity(&self, name: &WorkspaceName) -> Result<Option<WorkspaceIdentity>, BackendError> {
        Ok(self.0.identity(name)?)
    }
    fn has_deletions(&self) -> Result<bool, BackendError> {
        Ok(self.0.has_deletions()?)
    }
    fn pending_deletions(&self) -> Result<Vec<WorkspaceDeletion>, BackendError> {
        Ok(self.0.pending_deletions()?)
    }
    fn delete_workspace(&mut self, plan: &WorkspaceDeletion) -> Result<(), BackendError> {
        Ok(self.0.delete_workspace(plan)?)
    }
    fn finish_deletion(&mut self, id: &str) -> Result<(), BackendError> {
        Ok(self.0.finish_deletion(id)?)
    }

    fn capabilities(&self) -> BackendCapabilities {
        BackendCapabilities {
            retention: true,
            automatic_cleanup: true,
        }
    }
    fn names(&self) -> Result<Vec<WorkspaceName>, BackendError> {
        Ok(self.0.names()?)
    }
    fn load(
        &mut self,
        name: &WorkspaceName,
    ) -> Result<(BackendHistory, HistoryImage), BackendError> {
        let (sink, image) = self.0.load(name)?;
        Ok((BackendHistory::new(sink), image))
    }
    fn save(&mut self, name: &WorkspaceName, image: &HistoryImage) -> Result<(), BackendError> {
        Ok(self.0.save(name, image)?)
    }
    fn save_current(&mut self, _: &WorkspaceName, _: &HistoryImage) -> Result<(), BackendError> {
        // The active FileHistory already appends to this named generation; capture synchronized it.
        Ok(())
    }
    fn collect_unused(&mut self) -> Result<CollectionReport, BackendError> {
        Ok(self.0.collect_unused()?)
    }
    fn payload_references(
        &mut self,
        handle: &ValueHandle,
        name: &WorkspaceName,
        referenced: bool,
        bytes: u64,
    ) -> Result<PayloadReferences, BackendError> {
        Ok(self.0.payload_references(handle, name, referenced, bytes)?)
    }
    fn storage_census_live(
        &mut self,
        name: &WorkspaceName,
        image: &HistoryImage,
        live: &std::collections::BTreeMap<WorkspaceName, HistoryImage>,
    ) -> Result<StorageCensus, BackendError> {
        Ok(self.0.storage_census_live(name, image, live)?)
    }
    fn storage_census(
        &mut self,
        name: &WorkspaceName,
        image: &HistoryImage,
    ) -> Result<StorageCensus, BackendError> {
        Ok(self.0.storage_census(name, image)?)
    }
}
