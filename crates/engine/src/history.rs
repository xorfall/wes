//! Durable facts are not executable commands. This module owns their meaning, not their format.
pub mod deletion;
mod requests;
pub use requests::{RequestClaim, RequestRecord};
mod pages;
use crate::{
    graph::{NodeId, NodeState},
    runtime::{Observation, RestoredState, RunId, StaleReason},
    storage::ValueHandle,
};
use indexmap::{IndexMap, IndexSet};
pub use pages::{HistoricalEntry, HistoryCursor, HistoryPage, HistoryPageLimits};
use std::sync::Arc;
use thiserror::Error;
use wes_core::{ErrorValue, Timestamp, Value};
use wes_language::Diagnostic;

#[derive(Clone, Debug, Error, PartialEq, Eq)]
#[error("invalid history record: {0}")]
pub struct InvalidRecord(pub &'static str);

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CommandRecord {
    pub source_name: String,
    pub source_start: wes_language::Position,
    pub document: Option<String>,
    pub revision_of: Option<String>,
    pub environments: Option<wes_core::environments::EnvironmentContext>,
    pub cell: String,
    pub text: String,
    pub nodes: Vec<NodeId>,
    /// Resolved nodes whose arguments this command changes, unique in source order.
    pub changed_nodes: Vec<NodeId>,
    pub type_sources: IndexMap<String, String>,
    /// Exact versioned calculation package for this accepted source; absent for non-calculation records.
    pub calculation_package: Option<String>,
    /// Accepted importer input evidence, never executable authority or a live-file fallback.
    pub imports: Vec<crate::imports::ImportSnapshot>,
    /// Captured normalized declaration; it must not be reconstructed from today's descriptors.
    pub replay: String,
}
impl CommandRecord {
    pub fn validate(&self) -> Result<(), InvalidRecord> {
        crate::source::validate_source_name(&self.source_name)
            .map_err(|_| InvalidRecord("invalid source name"))?;
        crate::source::SourceInput::new(self.cell.clone(), String::new())
            .and_then(|i| i.with_source_start(self.source_start))
            .map_err(|_| InvalidRecord("invalid source position"))?;
        crate::source::validate_source(&self.cell, &self.text)
            .map_err(|_| InvalidRecord("invalid command source or cell identity"))?;
        if self
            .document
            .as_ref()
            .is_some_and(|s| s.len() > crate::source::max_source_bytes())
        {
            return Err(InvalidRecord("document input exceeds its byte limit"));
        }
        if let Some(origin) = &self.revision_of {
            crate::source::SourceInput::new(self.cell.clone(), self.text.clone())
                .and_then(|i| i.with_revision(origin.clone()))
                .map_err(|_| InvalidRecord("invalid definition revision"))?;
        }
        if let Some(context) = &self.environments {
            context
                .validate()
                .map_err(|_| InvalidRecord("invalid environment context"))?;
        }
        crate::imports::validate_snapshots(&self.imports)
            .map_err(|_| InvalidRecord("invalid or excessive captured import inputs"))?;
        if let Some(package) = &self.calculation_package {
            wes_language::calc::Package::load(package)
                .map_err(|_| InvalidRecord("invalid captured calculation package"))?;
        }
        let unique = self.nodes.iter().collect::<IndexSet<_>>();
        if unique.len() != self.nodes.len() {
            return Err(InvalidRecord("duplicate command node identity"));
        }
        if self.changed_nodes.iter().collect::<IndexSet<_>>().len() != self.changed_nodes.len() {
            return Err(InvalidRecord("duplicate changed node identity"));
        }
        Ok(())
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RetainedResult {
    pub retention: crate::storage::Retention,
    pub node: NodeId,
    pub handle: ValueHandle,
    /// Exact execution that produced this retained result.
    pub run: RunId,
}

/// One live publication, committed only at a retention boundary. The source epoch
/// anchors this evidence to admitted work, without journaling each event's lease.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LiveSnapshot {
    pub source: NodeId,
    pub epoch: RunId,
    /// Per-consumer delivery index; absent for stages that have not entered an invocation.
    pub delivery: Option<u64>,
    pub observation: ExecutionRecord,
    pub result: Option<RetainedResult>,
}
impl LiveSnapshot {
    pub fn validate(&self) -> Result<(), InvalidRecord> {
        if self.delivery == Some(0) {
            return Err(InvalidRecord("live delivery must be positive"));
        }
        if let Some(result) = &self.result {
            if !matches!(
                result.retention,
                crate::storage::Retention::Automatic
                    | crate::storage::Retention::Protected
                    | crate::storage::Retention::Unknown
            ) || self.delivery.is_none()
                || self.observation.state() != NodeState::Ready
                || self.observation.run().is_none()
                || self.observation.node() != &result.node
                || self.observation.run() != Some(&result.run)
            {
                return Err(InvalidRecord(
                    "live snapshot result does not match its execution",
                ));
            }
        }
        Ok(())
    }
}

/// Completed submission presentation, never a declaration or execution authority.
/// Diagnostics are separate journal facts. Order is the session admission order.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SubmissionRecord {
    pub source_name: String,
    pub source_start: wes_language::Position,
    pub refreshed: Vec<NodeId>,
    pub document: Option<String>,
    pub revision_of: Option<String>,
    pub id: String,
    pub cell: String,
    pub text: String,
    pub client: String,
    pub context: Option<wes_core::environments::EnvironmentContext>,
    pub order: u64,
    pub nodes: Vec<NodeId>,
    pub repeat: Option<crate::source::Repeat>,
    pub run: Option<RunId>,
}
impl SubmissionRecord {
    pub fn input(&self) -> Result<crate::source::SourceInput, InvalidRecord> {
        let invalid = |_| InvalidRecord("invalid submission presentation");
        let mut input = crate::source::SourceInput::new(self.cell.clone(), self.text.clone())
            .and_then(|i| i.with_source_name(self.source_name.clone()))
            .and_then(|i| i.with_source_start(self.source_start))
            .and_then(|i| i.with_document(self.document.clone()))
            .and_then(|i| i.with_client(self.client.clone()))
            .map_err(invalid)?;
        if let Some(context) = &self.context {
            input = input.with_environments(context.clone()).map_err(invalid)?;
        }
        if let Some(origin) = &self.revision_of {
            input = input.with_revision(origin.clone()).map_err(invalid)?;
        }
        if let Some(repeat) = &self.repeat {
            input = input
                .with_repeat(repeat.origin.clone(), repeat.acknowledge_effects)
                .and_then(|i| i.with_repeat_from(repeat.from.clone()))
                .map_err(invalid)?;
        }
        if self.order >= 10_000
            || self.nodes.len() > 1000
            || self.refreshed.len() > 10_000
            || self.refreshed.iter().collect::<IndexSet<_>>().len() != self.refreshed.len()
            || self.refreshed.iter().any(|id| self.nodes.contains(id))
            || self.nodes.iter().collect::<IndexSet<_>>().len() != self.nodes.len()
            || self.id.is_empty()
            || self.id.len() > 256
            || self.id.chars().any(char::is_control)
        {
            return Err(InvalidRecord("invalid submission presentation"));
        }
        Ok(input)
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ExecutionRecord {
    stale_reason: Option<StaleReason>,
    id: String,
    node: NodeId,
    run: Option<RunId>,
    at: Timestamp,
    state: NodeState,
    error: Option<ErrorValue>,
}
impl ExecutionRecord {
    pub fn new(
        id: String,
        node: NodeId,
        run: Option<RunId>,
        at: Timestamp,
        state: NodeState,
        error: Option<ErrorValue>,
    ) -> Result<Self, InvalidRecord> {
        NodeId::new(&id).map_err(|_| InvalidRecord("observation identity must not be blank"))?;
        if matches!(state, NodeState::Failed | NodeState::Cancelled) != error.is_some() {
            return Err(InvalidRecord(
                "only failure and cancellation must carry an error",
            ));
        }
        Ok(Self {
            stale_reason: None,
            id,
            node,
            run,
            at,
            state,
            error,
        })
    }
    pub fn capture(
        id: String,
        at: Timestamp,
        observation: &Observation,
    ) -> Result<Self, InvalidRecord> {
        Self::new(
            id,
            observation.node.clone(),
            observation.run.clone(),
            at,
            observation.state,
            observation.error.clone(),
        )?
        .with_stale_reason(observation.stale_reason)
    }
    pub fn with_stale_reason(mut self, reason: Option<StaleReason>) -> Result<Self, InvalidRecord> {
        if reason.is_some() && self.state != NodeState::Stale {
            return Err(InvalidRecord(
                "only stale observations may carry a stale reason",
            ));
        }
        self.stale_reason = reason;
        Ok(self)
    }
    pub fn stale_reason(&self) -> Option<StaleReason> {
        self.stale_reason
    }
    pub fn id(&self) -> &str {
        &self.id
    }
    pub fn node(&self) -> &NodeId {
        &self.node
    }
    pub fn run(&self) -> Option<&RunId> {
        self.run.as_ref()
    }
    pub fn at(&self) -> Timestamp {
        self.at
    }
    pub fn state(&self) -> NodeState {
        self.state
    }
    pub fn error(&self) -> Option<&ErrorValue> {
        self.error.as_ref()
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DiagnosticRecord {
    id: String,
    at: Timestamp,
    cell: String,
    source: Arc<str>,
    diagnostic: Diagnostic,
}
impl DiagnosticRecord {
    pub fn new(
        id: String,
        at: Timestamp,
        cell: String,
        source: String,
        diagnostic: Diagnostic,
    ) -> Result<Self, InvalidRecord> {
        Self::with_shared_source(id, at, cell, source.into(), diagnostic)
    }
    pub(crate) fn with_shared_source(
        id: String,
        at: Timestamp,
        cell: String,
        source: Arc<str>,
        diagnostic: Diagnostic,
    ) -> Result<Self, InvalidRecord> {
        NodeId::new(&id).map_err(|_| InvalidRecord("diagnostic identity must not be blank"))?;
        if diagnostic.code.trim().is_empty() {
            return Err(InvalidRecord("diagnostic code must not be blank"));
        }
        if !diagnostic.valid_public_message() {
            return Err(InvalidRecord(
                "public diagnostic explanation must be nonblank and at most 512 bytes",
            ));
        }
        source
            .get(diagnostic.span.start()..diagnostic.span.end())
            .ok_or(InvalidRecord("diagnostic span must belong to its source"))?;
        Ok(Self {
            id,
            at,
            cell,
            source,
            diagnostic,
        })
    }
    pub fn id(&self) -> &str {
        &self.id
    }
    pub fn at(&self) -> Timestamp {
        self.at
    }
    pub fn cell(&self) -> &str {
        &self.cell
    }
    pub fn source(&self) -> &str {
        &self.source
    }
    pub fn diagnostic(&self) -> &Diagnostic {
        &self.diagnostic
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum JournalEntry {
    Views(crate::views::ViewRecord),
    Snapshot(LiveSnapshot),
    /// Explicit retention of one immutable execution's existing result, trace and declaration evidence.
    /// Never a hydration candidate: protecting an older run cannot replace today's output.
    ProtectedRun {
        node: NodeId,
        run: RunId,
        handle: ValueHandle,
    },
    /// Ownership evidence for a public non-retained finite output. Not a Keep
    /// receipt and never a hydration candidate; absent bytes remain absent.
    Payload {
        node: NodeId,
        run: RunId,
        handle: ValueHandle,
    },
    Retired(RetiredWork),
    Requested(RequestRecord),
    Submitted(SubmissionRecord),
    Trace(crate::trace::TraceRecord),
    Environments(crate::environments::EnvironmentRecord),
    Command(CommandRecord),
    Result(RetainedResult),
    Observed(ExecutionRecord),
    Diagnosed(DiagnosticRecord),
    Noticed(NoticeRecord),
}
impl JournalEntry {
    pub fn observation(&self) -> Option<&ExecutionRecord> {
        match self {
            Self::Observed(o) => Some(o),
            Self::Snapshot(s) => Some(&s.observation),
            _ => None,
        }
    }
    pub fn retained_result(&self) -> Option<&RetainedResult> {
        match self {
            Self::Result(r) => Some(r),
            Self::Snapshot(s) => s.result.as_ref(),
            _ => None,
        }
    }
    /// Work-owned content, including acknowledged Temporary ownership and uncertain
    /// publication handles. Cleanup intent alone is deliberately not a reference.
    pub fn payload_reference(&self) -> Option<&ValueHandle> {
        match self {
            Self::Payload { handle, .. } | Self::ProtectedRun { handle, .. } => Some(handle),
            Self::Result(result) => Some(&result.handle),
            Self::Snapshot(snapshot) => snapshot.result.as_ref().map(|r| &r.handle),
            Self::Noticed(notice) if notice.context().node().is_some() => notice.context().handle(),
            _ => None,
        }
    }
}

/// Identity reservations and explicitly authorized cleanup, never executable source.
/// Kept in compacted histories so a crash cannot resurrect work or abandon cleanup.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RetiredWork {
    pub nodes: Vec<NodeId>,
    pub payloads: Vec<ValueHandle>,
    pub protected: Vec<ValueHandle>,
}
impl RetiredWork {
    pub fn validate(&self) -> Result<(), InvalidRecord> {
        if self.nodes.len() > 10_000
            || self.payloads.len() > 10_000
            || self.nodes.iter().any(|n| n.as_str().len() > 256)
            || self.nodes.iter().collect::<IndexSet<_>>().len() != self.nodes.len()
            || self.payloads.iter().collect::<IndexSet<_>>().len() != self.payloads.len()
            || self.protected.iter().collect::<IndexSet<_>>().len() != self.protected.len()
            || self.protected.iter().any(|h| !self.payloads.contains(h))
        {
            return Err(InvalidRecord("invalid retirement evidence"));
        }
        Ok(())
    }
}

/// Operational context is distinct from a node outcome. A notice never dispatches a failure branch.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum NoticeContext {
    Execution {
        node: NodeId,
        run: RunId,
    },
    Publication {
        node: NodeId,
        run: RunId,
        handle: Option<ValueHandle>,
    },
    Keep {
        handle: ValueHandle,
        may_have_applied: bool,
    },
    Release {
        handle: ValueHandle,
        may_have_applied: bool,
    },
    Eviction,
    StorageWorker,
    WorkspaceShutdown,
}
impl NoticeContext {
    pub fn node(&self) -> Option<&NodeId> {
        match self {
            Self::Execution { node, .. } | Self::Publication { node, .. } => Some(node),
            _ => None,
        }
    }
    pub fn run(&self) -> Option<&RunId> {
        match self {
            Self::Execution { run, .. } | Self::Publication { run, .. } => Some(run),
            _ => None,
        }
    }
    pub fn handle(&self) -> Option<&ValueHandle> {
        match self {
            Self::Keep { handle, .. } | Self::Release { handle, .. } => Some(handle),
            Self::Publication { handle, .. } => handle.as_ref(),
            _ => None,
        }
    }
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NoticeRecord {
    id: String,
    at: Timestamp,
    context: NoticeContext,
    error: ErrorValue,
}
impl NoticeRecord {
    pub fn new(
        id: String,
        at: Timestamp,
        context: NoticeContext,
        error: ErrorValue,
    ) -> Result<Self, InvalidRecord> {
        NodeId::new(&id).map_err(|_| InvalidRecord("notice identity must not be blank"))?;
        Ok(Self {
            id,
            at,
            context,
            error,
        })
    }
    pub fn id(&self) -> &str {
        &self.id
    }
    pub fn at(&self) -> Timestamp {
        self.at
    }
    pub fn context(&self) -> &NoticeContext {
        &self.context
    }
    pub fn error(&self) -> &ErrorValue {
        &self.error
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Record {
    Journal(JournalEntry),
    Recovery(RecoveryEntry),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Persistence {
    /// Explicitly ephemeral sessions only. This must not be displayed as durable.
    Volatile,
    FileSynced,
    FileAndDirectorySynced,
}

/// Shared by command admission, call recovery and execution logging. A volatile sink cannot
/// satisfy a persistent session; file-and-directory sync is stronger than file sync alone.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RequiredPersistence {
    Volatile,
    FileSynced,
    FileAndDirectorySynced,
}
impl RequiredPersistence {
    pub fn accepts(self, persistence: Persistence) -> bool {
        match self {
            Self::Volatile => true,
            Self::FileSynced => persistence != Persistence::Volatile,
            Self::FileAndDirectorySynced => persistence == Persistence::FileAndDirectorySynced,
        }
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AppendReceipt {
    pub persistence: Persistence,
    /// End byte offset in the corresponding journal/recovery stream, including its newline.
    pub end_offset: u64,
}

#[derive(Debug, Error)]
pub enum RecordError {
    #[error("history backend does not support durable request identities")]
    RequestUnsupported,
    #[error("request_id already belongs to different source/context")]
    RequestConflict,
    #[error("Workspace environment changed; read workspace_context and review before executing.")]
    RequestContext,
    #[error("history backend does not support paged reads")]
    PageUnsupported,
    #[error("the saved-history reader is busy")]
    ReadBusy,
    #[error("history cursor does not belong to this readable journal prefix")]
    InvalidCursor,
    #[error("history backend does not support synchronized capture")]
    CaptureUnsupported,
    #[error("history writer is unavailable")]
    Closed,
    #[error("history writer requires recovery before further writes")]
    Poisoned,
    #[error("history record exceeds the {0} budget")]
    Limit(&'static str),
    #[error("history operation failed: {operation}")]
    Backend {
        operation: &'static str,
        /// A failed acknowledgement is not proof that the record was absent from disk.
        may_have_appended: bool,
        #[source]
        source: Box<dyn std::error::Error + Send + Sync>,
    },
}
impl RecordError {
    pub fn backend(
        operation: &'static str,
        may_have_appended: bool,
        source: impl std::error::Error + Send + Sync + 'static,
    ) -> Self {
        Self::Backend {
            operation,
            may_have_appended,
            source: Box::new(source),
        }
    }
}

/// A synchronous leaf port, called only by the recording worker. Successful durable receipts
/// acknowledge the configured sync boundary. Implementations must not invoke the engine.
pub trait JournalSink: Send {
    /// Atomic on the single writer. Unknown keys may be reserved only when current context matches.
    fn claim_request(&mut self, _: RequestRecord, _: bool) -> Result<RequestClaim, RecordError> {
        Err(RecordError::RequestUnsupported)
    }
    fn find_request(&mut self, _: &str, _: &str) -> Result<Option<RequestRecord>, RecordError> {
        Err(RecordError::RequestUnsupported)
    }
    /// Read only operational/diagnostic/execution history from a stable prefix. Never execute source.
    /// The existing writer owns and joins the read; implementations enforce limits while collecting.
    fn page(
        &mut self,
        _: Option<HistoryCursor>,
        _: HistoryPageLimits,
    ) -> Result<HistoryPage, RecordError> {
        Err(RecordError::PageUnsupported)
    }
    fn append(&mut self, record: &Record) -> Result<AppendReceipt, RecordError>;
    /// Capture both streams at this writer's current boundary, with actual synchronization
    /// receipts. No reopening by pathname, append, repair or provider execution is permitted.
    /// Implementations must enforce limits while reading, not after collecting an unbounded image.
    fn capture(&mut self, _: HistoryCaptureLimits) -> Result<HistoryImage, RecordError> {
        Err(RecordError::CaptureUnsupported)
    }
}

/// A synchronized prefix of both owned history streams, not a claim that any remote call finished.
#[derive(Clone, Copy, Debug)]
pub struct HistoryCheckpoint {
    pub journal: AppendReceipt,
    pub recovery: AppendReceipt,
}
#[derive(Clone, Copy, Debug)]
pub struct HistoryCaptureLimits {
    pub records: usize,
    /// Conservative decoded record charge; encoded input/line limits remain reader-owned.
    pub bytes: u64,
}
impl Default for HistoryCaptureLimits {
    fn default() -> Self {
        Self {
            records: wes_budgets::get("history.capture.records") as usize,
            bytes: wes_budgets::get("history.capture.bytes") as u64,
        }
    }
}
/// Owned, validated history for a reconstruction coordinator. The capturing adapter establishes
/// the checkpoint on the same writer used by the recording worker, at startup or a queued capture.
#[derive(Clone, Debug)]
pub struct HistoryImage {
    journal: Vec<JournalEntry>,
    recovery: Vec<RecoveryEntry>,
    checkpoint: HistoryCheckpoint,
    bytes: u64,
}
impl HistoryImage {
    pub fn journal(&self) -> &[JournalEntry] {
        &self.journal
    }
    pub fn retained_view_inputs(&self) -> Result<Vec<(NodeId, ValueHandle)>, InvalidRecord> {
        self.journal
            .iter()
            .rev()
            .find_map(|entry| match entry {
                JournalEntry::Views(record) => Some(record.retained_inputs()),
                _ => None,
            })
            .unwrap_or_else(|| Ok(vec![]))
    }
    pub fn recovery(&self) -> &[RecoveryEntry] {
        &self.recovery
    }
    pub fn checkpoint(&self) -> HistoryCheckpoint {
        self.checkpoint
    }
    pub fn charged_bytes(&self) -> u64 {
        self.bytes
    }
}
/// Incremental admission prevents collecting an unbounded decoded image before checking its size.
pub struct HistoryCapture {
    limits: HistoryCaptureLimits,
    journal: Vec<JournalEntry>,
    recovery: Vec<RecoveryEntry>,
    bytes: u64,
}
impl HistoryCapture {
    pub fn new(limits: HistoryCaptureLimits) -> Self {
        Self {
            limits,
            journal: vec![],
            recovery: vec![],
            bytes: 0,
        }
    }
    pub fn push(&mut self, record: Record) -> Result<(), RecordError> {
        if self.journal.len().saturating_add(self.recovery.len()) >= self.limits.records {
            return Err(RecordError::Limit("captured history record count"));
        }
        let charge = crate::recording::record_charge(&record);
        if charge > self.limits.bytes.saturating_sub(self.bytes) {
            return Err(RecordError::Limit("captured history payload"));
        }
        self.bytes += charge;
        match record {
            Record::Journal(entry) => {
                self.journal.push(entry);
            }
            Record::Recovery(entry) => {
                self.recovery.push(entry);
            }
        }
        Ok(())
    }
    /// Adapters must supply actual checkpoint receipts; a pathname or successful parse is not one.
    pub fn finish(self, checkpoint: HistoryCheckpoint) -> HistoryImage {
        HistoryImage {
            journal: self.journal,
            recovery: self.recovery,
            checkpoint,
            bytes: self.bytes,
        }
    }
}

/// Distinct from declarations and results. Named local snapshots retain this uncertainty evidence;
/// reading it never retries a call or asserts its remote outcome.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RecoveryEntry {
    Accepted {
        cell: String,
    },
    Calling(CallRecord),
    Called {
        node: NodeId,
        run: RunId,
        produced: bool,
    },
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CallRecord {
    pub node: NodeId,
    pub run: RunId,
    pub cell: String,
    pub capability: String,
    pub safe: bool,
    pub at: Timestamp,
}

/// A pure projection. Callers replay declarations with execution disabled before using this index.
/// The index never reads files, invokes a provider, or decides to retry an interrupted call.
#[derive(Debug)]
pub struct RestoreIndex<'a> {
    pub commands: Vec<&'a CommandRecord>,
    pub observations: IndexMap<NodeId, &'a ExecutionRecord>,
    pub retained: IndexMap<NodeId, &'a RetainedResult>,
    pub diagnostics: Vec<&'a DiagnosticRecord>,
    pub accepted_cells: IndexSet<&'a str>,
}
impl<'a> RestoreIndex<'a> {
    pub fn build(entries: &'a [JournalEntry]) -> Self {
        Self::from_entries(entries.iter())
    }
    pub(crate) fn from_entries(entries: impl IntoIterator<Item = &'a JournalEntry>) -> Self {
        let entries: Vec<_> = entries.into_iter().collect();
        let mut index = Self {
            commands: vec![],
            observations: IndexMap::new(),
            retained: IndexMap::new(),
            diagnostics: vec![],
            accepted_cells: IndexSet::new(),
        };
        for &entry in &entries {
            match entry {
                JournalEntry::Command(command) => {
                    index.commands.push(command);
                    index.accepted_cells.insert(&command.cell);
                }
                JournalEntry::Observed(record)
                | JournalEntry::Snapshot(LiveSnapshot {
                    observation: record,
                    ..
                }) => {
                    index.observations.insert(record.node.clone(), record);
                }
                JournalEntry::Diagnosed(record) => index.diagnostics.push(record),
                JournalEntry::ProtectedRun { .. }
                | JournalEntry::Payload { .. }
                | JournalEntry::Retired(_)
                | JournalEntry::Requested(_)
                | JournalEntry::Submitted(_)
                | JournalEntry::Trace(_)
                | JournalEntry::Result(_)
                | JournalEntry::Noticed(_)
                | JournalEntry::Environments(_)
                | JournalEntry::Views(_) => {}
            }
        }
        let latest_snapshots: IndexMap<_, _> = entries
            .iter()
            .filter_map(|entry| match entry {
                JournalEntry::Snapshot(s) => Some((s.observation.node(), s)),
                _ => None,
            })
            .collect();
        for entry in entries {
            if let Some(result) = entry.retained_result() {
                let matches_latest = index.observations.get(&result.node).is_none_or(|latest| {
                    latest.state == NodeState::Ready && latest.run.as_ref() == Some(&result.run)
                });
                let snapshot_matches = latest_snapshots.get(&result.node).is_none_or(|s| {
                    index
                        .observations
                        .get(&result.node)
                        .is_none_or(|latest| latest.id() != s.observation.id())
                        || s.result.as_ref().is_some_and(|saved| {
                            saved.node == result.node
                                && saved.run == result.run
                                && saved.handle == result.handle
                        })
                });
                if matches_latest && snapshot_matches {
                    index.retained.insert(result.node.clone(), result);
                }
            }
        }
        index
    }
    /// `retained_value` must come from this node's selected retained handle. Missing/corrupt bytes
    /// are reported by the storage boundary and leave an otherwise successful node stale.
    pub fn state(&self, node: &NodeId, retained_value: Option<Value>) -> RestoredState {
        let value = self
            .retained
            .contains_key(node)
            .then_some(retained_value)
            .flatten();
        match self.observations.get(node) {
            Some(record) => match record.state {
                NodeState::Failed => {
                    RestoredState::Failed(record.error.clone().expect("validated error"))
                }
                NodeState::Cancelled => {
                    RestoredState::Cancelled(record.error.clone().expect("validated error"))
                }
                NodeState::Skipped => RestoredState::Skipped,
                NodeState::Ready => value.map_or(
                    RestoredState::StaleBecause(if self.retained.contains_key(node) {
                        StaleReason::RestoreUnavailable
                    } else {
                        StaleReason::RestoreNotRetained
                    }),
                    RestoredState::Ready,
                ),
                NodeState::Stale => {
                    RestoredState::StaleBecause(record.stale_reason.unwrap_or(StaleReason::Unknown))
                }
                _ => RestoredState::StaleBecause(StaleReason::RestoreUnfinished),
            },
            None => value.map_or(
                RestoredState::StaleBecause(if self.retained.contains_key(node) {
                    StaleReason::RestoreUnavailable
                } else {
                    StaleReason::RestoreUnfinished
                }),
                RestoredState::Ready,
            ),
        }
    }
}

/// A completion closes only its exact node and run, including when it arrives late.
pub fn unresolved_calls(entries: &[RecoveryEntry]) -> Vec<&CallRecord> {
    let mut pending = IndexMap::new();
    for entry in entries {
        match entry {
            RecoveryEntry::Calling(call) => {
                pending.insert((&call.node, &call.run), call);
            }
            RecoveryEntry::Called { node, run, .. } => {
                pending.shift_remove(&(node, run));
            }
            RecoveryEntry::Accepted { .. } => {}
        }
    }
    pending.into_values().collect()
}
