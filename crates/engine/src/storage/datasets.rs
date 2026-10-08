//! Semantic dataset I/O port. Codecs and filesystem paths remain in adapters.
pub use super::analysis_attempt::{AnalysisAttemptKind, AttemptAccounting, charge_interruption};
pub use super::analysis_budget::AnalysisBudget;
pub use super::analysis_coverage::{
    CoverageInfo, CoveragePolicy, CoverageProgress, CoverageSpan, read_rejection, rejection_charge,
    rejection_reservation, rejection_schema, rejection_value,
};
use super::{Retention, StoreError, ValueHandle};
use wes_core::{DatasetRef, Value, contracts::ResolvedContractBundle, flow::FlowPolicy};
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct DatasetAccess {
    pub withdrawn: std::collections::BTreeSet<String>,
    pub closed: bool,
}
impl DatasetAccess {
    pub fn blocks(&self, value: &Value) -> bool {
        if value
            .provenance()
            .policy()
            .dataset_reads()
            .iter()
            .any(|origin| self.closed || self.withdrawn.contains(origin.dataset()))
        {
            return true;
        }
        let mut pending = vec![(value.data(), 0usize)];
        let mut left = 1_000_000usize;
        while let Some((data, depth)) = pending.pop() {
            if depth > 128 || left == 0 {
                return true;
            }
            left -= 1;
            match data {
                wes_core::Data::Dataset(reference)
                    if self.closed || self.withdrawn.contains(reference.dataset()) =>
                {
                    return true;
                }
                wes_core::Data::List(items) => {
                    if items.len().saturating_add(pending.len()) > left {
                        return true;
                    }
                    pending.extend(items.iter().map(|v| (v, depth + 1)));
                }
                wes_core::Data::Record(fields) => {
                    if fields.len().saturating_add(pending.len()) > left {
                        return true;
                    }
                    pending.extend(fields.values().map(|v| (v, depth + 1)));
                }
                wes_core::Data::Option(Some(value)) => pending.push((value, depth + 1)),
                _ => {}
            }
        }
        false
    }
}

/// Process-local opaque wake revisions. No data identity in a wake grants access.
#[derive(Clone, Debug, Default)]
pub struct DatasetChanges {
    pub global: u64,
    pub datasets: std::collections::BTreeMap<String, u64>,
}
impl DatasetChanges {
    pub fn revision(&self, dataset: &str) -> (u64, u64) {
        (
            self.global,
            self.datasets.get(dataset).copied().unwrap_or(0),
        )
    }
    pub fn changed(&mut self, dataset: &str) {
        let version = self.datasets.entry(dataset.to_owned()).or_default();
        *version = version.saturating_add(1);
    }
    pub fn all_changed(&mut self) {
        self.global = self.global.saturating_add(1);
    }
}

/// A read shares the analysis owner's monotonic, prepaid work counter. Storage
/// can debit it before I/O and decoding, but cannot grant or refund work.
#[derive(Clone, Debug)]
pub struct ReadWork {
    counter: crate::work_budget::WorkCounter,
    start: u64,
}
/// A physically joined read stopped before entering its next immutable granule.
/// This process-only receipt grants neither storage nor execution authority.
#[derive(Clone, Debug, thiserror::Error)]
#[error("analysis read work reached its {dimension:?} bound")]
pub struct ReadWorkRefusal {
    dimension: ReadWorkDimension,
    needed: u64,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ReadWorkDimension {
    Absolute,
    Allowance,
    Prepaid,
}
impl ReadWorkRefusal {
    pub fn dimension(&self) -> ReadWorkDimension {
        self.dimension
    }
    /// Work already examined during this read plus its exact next refused charge.
    pub fn needed(&self) -> u64 {
        self.needed
    }
}
impl ReadWork {
    pub(crate) fn new(work: crate::work_budget::WorkCounter) -> Self {
        Self {
            start: work.used(),
            counter: work,
        }
    }
    pub fn charge(&self, amount: u64) -> Result<(), ReadWorkRefusal> {
        use crate::work_budget::WorkRefusal;
        self.counter
            .admit(amount)
            .map_err(|refusal| ReadWorkRefusal {
                dimension: match refusal.reason {
                    WorkRefusal::Absolute => ReadWorkDimension::Absolute,
                    WorkRefusal::Allowance => ReadWorkDimension::Allowance,
                    WorkRefusal::Prepaid => ReadWorkDimension::Prepaid,
                },
                needed: refusal
                    .used
                    .checked_sub(self.start)
                    .and_then(|used| used.checked_add(refusal.requested))
                    .unwrap_or(u64::MAX),
            })
    }
}
#[derive(Clone, Debug)]
pub struct PageRequest {
    pub from: u64,
    pub rows: usize,
    pub bytes: usize,
    pub segments: usize,
    pub charge: Option<PageCharge>,
    pub work: Option<ReadWork>,
}
/// Logical value ownership is independent of encoded page bytes. A consumer
/// supplies the row and retained-window charges it already reserved before I/O.
#[derive(Clone, Copy, Debug)]
pub struct PageCharge {
    row: u64,
    retained: u64,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PageChargeRefusal {
    Row { limit: u64 },
    Retained,
}
impl PageCharge {
    pub fn new(row: u64, retained: u64) -> Option<Self> {
        (row > 0 && row <= retained).then_some(Self { row, retained })
    }
    /// Returns this row's charge without changing either the consumer's
    /// retained count or its monotonic work owner. Refusal retains no new row.
    pub fn admit(&self, value: &Value, used: u64) -> Result<u64, PageChargeRefusal> {
        let amount = crate::value_size::value_charge(value, self.row)
            .ok_or(PageChargeRefusal::Row { limit: self.row })?;
        used.checked_add(amount)
            .filter(|n| *n <= self.retained)
            .ok_or(PageChargeRefusal::Retained)?;
        Ok(amount)
    }
}
#[derive(Clone, Debug)]
pub struct DatasetRow {
    pub ordinal: u64,
    pub source_start: u64,
    pub source_end: u64,
    pub value: Value,
}
#[derive(Clone, Debug)]
pub struct DatasetPage {
    pub reference: DatasetRef,
    pub schema: ResolvedContractBundle,
    pub first: u64,
    pub next: u64,
    pub rows: Vec<DatasetRow>,
    pub extent_exhausted: bool,
    pub limited_by: Option<&'static str>,
    /// Stored row payload bytes; schema, frames and transport encoding have separate bounds.
    pub encoded_row_bytes: usize,
    /// Owned read lifetime, released without I/O only when every consumer drops it.
    pub lease: Option<DatasetReadLease>,
}
#[derive(Clone)]
pub struct DatasetReadLease {
    _owner: std::sync::Arc<dyn Send + Sync>,
}
impl DatasetReadLease {
    pub fn retain(owner: impl Send + Sync + 'static) -> Self {
        Self {
            _owner: std::sync::Arc::new(owner),
        }
    }
}
impl std::fmt::Debug for DatasetReadLease {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DatasetReadLease").finish_non_exhaustive()
    }
}
/// Process-only writer ownership. Reopening bytes never recreates this lease.
pub type DatasetWriteLease = DatasetReadLease;
/// A committed prefix and its process ownership are delivered together. Abandoning
/// the receipt releases the writer without pretending the disk commit was undone.
#[derive(Debug)]
#[must_use = "retain the admitted writer for its complete lifetime"]
pub struct DatasetWriterAdmission {
    pub reference: DatasetRef,
    pub lease: DatasetWriteLease,
}
#[derive(Clone, Debug)]
pub struct DatasetSnapshot {
    pub basis: String,
    pub generation: u64,
    pub digest: String,
}
/// Storage-only evidence for a bounded footprint calculation. Never a read/control grant.
#[derive(Clone, Debug)]
pub struct RetentionCapture {
    pub value: CapturedValue,
    pub shared: bool,
}
#[derive(Clone, Debug)]
pub struct DatasetRetentionInventory {
    pub reference: DatasetRef,
    pub object_bytes: u64,
    pub shared_object_bytes: u64,
    pub captures: Vec<RetentionCapture>,
    pub catalog_revision: u64,
}
#[derive(Clone, Debug)]
pub struct DatasetRetentionCost {
    pub reference: DatasetRef,
    pub total_bytes: u64,
    pub shared_bytes: u64,
    pub exclusive_bytes: u64,
    pub captured_source_bytes: u64,
    pub catalog_revision: u64,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DatasetLifecycle {
    Open,
    /// An immutable open prefix superseded by later committed generations.
    /// This is read metadata, never a writer transition or a promise of activity.
    Prefix,
    Sealed,
    Incomplete,
    Interrupted,
    Cancelled,
    Restricted,
    Deleted,
}
#[derive(Clone, Debug)]
pub struct DatasetInfo {
    pub reference: DatasetRef,
    pub schema: ResolvedContractBundle,
    pub lifecycle: DatasetLifecycle,
    pub policy: FlowPolicy,
    pub segment_bytes: u64,
    pub protected: bool,
    pub persistence: crate::history::Persistence,
    pub recording: Option<EventLogCoverage>,
    pub coverage: Option<CoverageInfo>,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DatasetRootKind {
    Value,
    Workspace,
    Keep,
    Pin,
    Checkpoint,
    Recording,
}
#[derive(Clone, Debug)]
pub struct DatasetRootRequest {
    pub identity: ValueHandle,
    pub kind: DatasetRootKind,
    pub prefixes: Vec<DatasetRef>,
    pub captures: Vec<CapturedValue>,
    pub retention: Retention,
    pub policy: FlowPolicy,
}
#[derive(Clone, Debug)]
pub struct DatasetRootReceipt {
    pub transaction: String,
    pub generation: u64,
    pub persistence: crate::history::Persistence,
}
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CapturedValue {
    pub handle: String,
    pub digest: String,
    pub bytes: u64,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DatasetKind {
    Analysis,
    EventLog,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RecordingEnd {
    Natural,
    Manual,
    Cancelled,
    SourceFailed,
    Rejected,
    Overloaded,
    Limit,
    WriteFailed,
    Unconfirmed,
}
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EventLogCoverage {
    pub run: String,
    pub epoch: String,
    pub first: u64,
    pub accepted_through: u64,
    pub committed_through: u64,
    pub pending: Option<u64>,
    pub rejected: u64,
    pub termination: Option<RecordingEnd>,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SourceUnit {
    Bytes,
    Records,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SourceExtent {
    pub identity: String,
    pub unit: SourceUnit,
    pub start: u64,
    pub end: u64,
}
/// Exact local writer identity; data identity never grants runtime control.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DatasetWriteOwner {
    pub role: DatasetWriteRole,
    pub run: String,
    /// Immutable analysis identity, or the logical recording run (never the source run).
    pub lineage: String,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DatasetWriteSelection {
    pub role: DatasetWriteRole,
    pub run: String,
}
impl DatasetWriteOwner {
    pub fn selection(&self) -> DatasetWriteSelection {
        DatasetWriteSelection {
            role: self.role,
            run: self.run.clone(),
        }
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DatasetWriteRole {
    Analysis,
    Recording,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DatasetWriteOutcome {
    Committed,
    Absent,
    Unknown,
}
#[derive(Clone, Debug)]
pub struct DatasetReconciliation {
    pub selection: DatasetWriteSelection,
    pub owner: Option<DatasetWriteOwner>,
    pub policy: FlowPolicy,
    pub transaction: Option<String>,
    pub outcome: DatasetWriteOutcome,
    pub predecessor: Option<DatasetRef>,
    pub committed: Option<DatasetRef>,
    pub persistence: crate::history::Persistence,
    /// A local write receipt never establishes the outcome of a producer or whole execution.
    pub execution_unknown: bool,
}
#[derive(Clone, Debug)]
pub struct DatasetCreate {
    pub owner: Option<DatasetWriteOwner>,
    pub dataset: String,
    pub transaction: String,
    pub kind: DatasetKind,
    pub schema: ResolvedContractBundle,
    pub source: SourceExtent,
    pub policy: FlowPolicy,
    pub checkpoint: Option<AnalysisCheckpoint>,
    pub recording: Option<EventLogCoverage>,
    pub coverage: Option<CoveragePolicy>,
}
/// A batch has one acknowledgement. No item becomes a graph node or partial success.
#[derive(Clone, Debug)]
pub struct DatasetAppend {
    pub owner: Option<DatasetWriteOwner>,
    pub previous: DatasetRef,
    pub transaction: String,
    pub rows: Vec<DatasetRow>,
    pub source: SourceExtent,
    pub lifecycle: DatasetLifecycle,
    pub policy: FlowPolicy,
    pub checkpoint: Option<AnalysisCheckpoint>,
    pub recording: Option<EventLogCoverage>,
    pub coverage: Vec<wes_core::framing::Rejection>,
}
/// Explicit continuation of the latest captured prefix. It admits no external producer.
#[derive(Clone, Debug)]
pub struct DatasetResume {
    pub kind: AnalysisAttemptKind,
    pub previous: DatasetRef,
    pub transaction: String,
    pub checkpoint: AnalysisCheckpoint,
    pub policy: FlowPolicy,
}
#[derive(Clone, Debug)]
pub struct AnalysisStatus {
    pub lifecycle: DatasetLifecycle,
    pub latest: bool,
    pub active_writer: bool,
}
#[derive(Clone, Debug)]
pub struct DatasetContinuation {
    pub status: AnalysisStatus,
    pub reference: DatasetRef,
    pub checkpoint: AnalysisCheckpoint,
    pub source: Value,
}
#[derive(Clone, Debug)]
pub struct DatasetDeletePlan {
    pub token: String,
    pub reference: DatasetRef,
    pub references: Vec<DatasetRootSummary>,
    pub protected_bytes: u64,
    pub active_readers: u64,
    pub active_writer: bool,
}
#[derive(Clone, Debug)]
pub struct DatasetRootSummary {
    pub identity: String,
    pub kind: DatasetRootKind,
    pub retention: Retention,
}
#[derive(Clone, Debug, Default)]
pub struct DatasetCleanup {
    pub reclaimed_bytes: Option<u64>,
    pub shared_bytes: Option<u64>,
    pub pending_bytes: Option<u64>,
    pub complete: bool,
    pub released_captures: Vec<CapturedValue>,
}
/// Persisted cause, separate from human-readable failure text and execution outcome.
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(tag = "kind", content = "dimension", rename_all = "snake_case")]
pub enum AnalysisStop {
    Cumulative(crate::scan::ledger::Dimension),
    Cancelled,
    Deterministic,
    IncompleteSource,
}
impl AnalysisStop {
    pub fn cumulative(dimension: crate::scan::ledger::Dimension) -> bool {
        use crate::scan::ledger::Dimension::*;
        matches!(
            dimension,
            Work | WorkAllowance
                | Duration
                | InputBytes
                | InputRecords
                | OutputBytes
                | OutputRecords
        )
    }
    pub fn permits_raise(self, new: crate::scan::Totals, old: crate::scan::Totals) -> bool {
        use crate::scan::ledger::Dimension::*;
        match self {
            Self::Cancelled => true,
            Self::Deterministic | Self::IncompleteSource => false,
            Self::Cumulative(d) => match d {
                Work | WorkAllowance => new.work > old.work,
                Duration => new.duration_ms > old.duration_ms,
                InputBytes => new.input_bytes > old.input_bytes,
                InputRecords => new.input_records > old.input_records,
                OutputBytes => new.output_bytes > old.output_bytes,
                OutputRecords => new.output_records > old.output_records,
                _ => false,
            },
        }
    }
    pub fn valid(self) -> bool {
        match self {
            Self::Cumulative(d) => Self::cumulative(d),
            _ => true,
        }
    }
}
/// Storage-independent captured continuation. The adapter owns encoding and schema objects.
#[derive(Clone, Debug)]
pub struct AnalysisCheckpoint {
    pub stop: Option<AnalysisStop>,
    pub budget: AnalysisBudget,
    pub analysis: String,
    pub attempt: String,
    pub previous_attempt: Option<String>,
    pub run: String,
    pub task_revision: String,
    pub source: CapturedValue,
    /// Last admitted committed EventLog prefix, separate from the initial immutable capture.
    pub followed_source: Option<FollowedSource>,
    pub item_schema: ResolvedContractBundle,
    pub state_schema: ResolvedContractBundle,
    pub context_schema: ResolvedContractBundle,
    pub state: Value,
    pub context: Value,
    pub step_revision: String,
    pub finish_revision: Option<String>,
    pub profile_digest: String,
    pub initial_digest: String,
    pub captured_program: String,
    pub next_position: u64,
    pub next_ordinal: u64,
    pub decoder_carry: Vec<u8>,
    pub work: AnalysisWork,
    pub usage: AnalysisUsage,
    pub duration: AnalysisDuration,
    pub finish_applied: bool,
    pub coverage: Option<CoverageProgress>,
}
impl AnalysisCheckpoint {
    /// A continuation preserves captured semantics and the acknowledged position/state.
    /// Only its logical run/attempt and the next cumulative prepaid grant can change.
    pub fn resumes(&self, old: &Self) -> bool {
        self.continues(old, AnalysisAttemptKind::Resume)
    }
    pub fn continues(&self, old: &Self, kind: AnalysisAttemptKind) -> bool {
        (kind == AnalysisAttemptKind::Resume
            || old
                .stop
                .is_none_or(|s| s.permits_raise(self.budget.totals, old.budget.totals)))
            && !old.finish_applied
            && !self.finish_applied
            && self.previous_attempt.as_deref() == Some(old.attempt.as_str())
            && self.attempt != old.attempt
            && self.run != old.run
            && self.analysis == old.analysis
            && self.task_revision == old.task_revision
            && self.source == old.source
            && self.followed_source == old.followed_source
            && self.item_schema.digest() == old.item_schema.digest()
            && self.state_schema.digest() == old.state_schema.digest()
            && self.context_schema.digest() == old.context_schema.digest()
            && self.state == old.state
            && self.context == old.context
            && self.step_revision == old.step_revision
            && self.finish_revision == old.finish_revision
            && self.profile_digest == old.profile_digest
            && self.initial_digest == old.initial_digest
            && self.captured_program == old.captured_program
            && self.next_position == old.next_position
            && self.next_ordinal == old.next_ordinal
            && self.coverage == old.coverage
            && self.decoder_carry == old.decoder_carry
            && self.stop.is_none()
            && self.budget.analysis == self.analysis
            && AttemptAccounting {
                budget: &self.budget,
                work: &self.work,
                usage: &self.usage,
                duration: &self.duration,
            }
            .follows(
                &AttemptAccounting {
                    budget: &old.budget,
                    work: &old.work,
                    usage: &old.usage,
                    duration: &old.duration,
                },
                &self.attempt,
                kind,
            )
    }
}
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FollowedSource {
    pub prefix: DatasetRef,
    pub run: String,
    pub epoch: String,
    pub first: u64,
}
impl FollowedSource {
    pub fn extends(&self, prior: &Self) -> bool {
        self.run == prior.run
            && self.epoch == prior.epoch
            && self.first == prior.first
            && self.prefix.store() == prior.prefix.store()
            && self.prefix.dataset() == prior.prefix.dataset()
            && self.prefix.schema_digest() == prior.prefix.schema_digest()
            && self.prefix.authorization_generation() == prior.prefix.authorization_generation()
            && self.prefix.records() >= prior.prefix.records()
            && self.prefix.generation() >= prior.prefix.generation()
            && (self.prefix.generation() != prior.prefix.generation()
                || self.prefix == prior.prefix)
    }
}
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AnalysisWork {
    pub granted: u64,
    pub completed: u64,
    pub charged: u64,
    pub outstanding: u64,
    pub grants: u64,
}
#[derive(Clone, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AnalysisUsage {
    pub input_bytes: u64,
    pub output_bytes: u64,
    pub high_water_bytes: u64,
    pub work_allowance: u64,
}
/// Wall-clock reservation captured with the same atomic execution grant as work.
/// An unacknowledged active interval is charged conservatively on continuation.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AnalysisDuration {
    pub spent_ms: u64,
    pub outstanding_ms: u64,
}
impl AnalysisDuration {
    pub fn reservation_valid(&self, limit_ms: u64, active: bool) -> bool {
        self.valid(limit_ms)
            && if active {
                self.spent_ms.checked_add(self.outstanding_ms) == Some(limit_ms)
            } else {
                self.outstanding_ms == 0
            }
    }
    pub fn settles(&self, old: &Self, limit_ms: u64, active: bool) -> bool {
        self.reservation_valid(limit_ms, active)
            && self.spent_ms >= old.spent_ms
            && old
                .spent_ms
                .checked_add(old.outstanding_ms)
                .is_some_and(|end| self.spent_ms <= end)
    }
    pub fn valid(&self, limit_ms: u64) -> bool {
        (1..=86_400_000).contains(&limit_ms)
            && self
                .spent_ms
                .checked_add(self.outstanding_ms)
                .is_some_and(|sum| sum <= limit_ms)
    }
}
/// Actor and result-read authority is validated by the runtime before/after each
/// request. This port additionally validates same-home committed identity and revocation.
pub trait DatasetStorage: Send {
    fn read_access(&self) -> Result<std::collections::BTreeSet<String>, StoreError> {
        Ok(Default::default())
    }
    fn check_read_origins(
        &self,
        origins: &std::collections::BTreeSet<wes_core::flow::DatasetReadOrigin>,
    ) -> Result<(), StoreError> {
        if origins.is_empty() {
            Ok(())
        } else {
            Err(StoreError::DatasetUnavailable)
        }
    }
    /// Immutable conservative I/O/validation/reply reservation, not a process RSS promise.
    fn read_charge(&self) -> u64;
    fn protects_value(&self, _identity: &ValueHandle) -> Result<bool, StoreError> {
        Ok(false)
    }
    /// Bounded catalog metadata lookup; ordinary values have no dataset reference root.
    fn value_root(
        &self,
        _identity: &ValueHandle,
    ) -> Result<Option<DatasetRootRequest>, StoreError> {
        Ok(None)
    }
    /// Trusted retained value roots, not user-supplied Dataset identities.
    fn protect_workspace(
        &mut self,
        _generation: &str,
        _handles: &[ValueHandle],
    ) -> Result<(), StoreError> {
        Err(StoreError::DatasetUnavailable)
    }
    fn retire_workspace(&mut self, _generation: &str) -> Result<(), StoreError> {
        Err(StoreError::DatasetUnavailable)
    }
    fn inspect(&self, reference: &DatasetRef) -> Result<DatasetInfo, StoreError>;
    /// Read a committed extension within the same EventLog epoch or analysis attempt.
    /// A descriptor is not a control capability and cannot resume or attach work.
    fn head(&self, _reference: &DatasetRef) -> Result<DatasetInfo, StoreError> {
        Err(StoreError::DatasetUnavailable)
    }
    /// Capture an exact committed extension within the anchor's own attempt/epoch.
    /// Constraints identify evidence; the owned anchor supplies read authority.
    fn snapshot(
        &self,
        _reference: &DatasetRef,
        _selection: DatasetSnapshot,
    ) -> Result<DatasetInfo, StoreError> {
        Err(StoreError::DatasetUnavailable)
    }
    /// Process-local wakeups carry no dataset identities, data or execution authority.
    fn changes(&self) -> Option<tokio::sync::watch::Receiver<DatasetChanges>> {
        None
    }
    /// Resolve only a committed extension of an already authorized EventLog prefix.
    /// This port never creates a writer or starts a producer.
    fn eventlog_head(
        &self,
        _reference: &DatasetRef,
        _work: Option<&ReadWork>,
    ) -> Result<DatasetInfo, StoreError> {
        Err(StoreError::DatasetUnavailable)
    }
    /// Explicit bounded reachability walk used for retention admission, never every display read.
    fn retention_size(&self, reference: &DatasetRef) -> Result<u64, StoreError>;
    /// Deduplicate physical objects across all prefixes of one retained value.
    fn retention_size_many(&self, references: &[DatasetRef]) -> Result<u64, StoreError> {
        references.iter().try_fold(0u64, |total, reference| {
            total
                .checked_add(self.retention_size(reference)?)
                .ok_or(StoreError::Limit("retention bytes"))
        })
    }
    fn retention_preview(
        &self,
        _reference: &DatasetRef,
    ) -> Result<DatasetRetentionInventory, StoreError> {
        Err(StoreError::DatasetUnavailable)
    }
    fn page(&self, reference: &DatasetRef, request: PageRequest)
    -> Result<DatasetPage, StoreError>;
    /// Page the exceptional observations of this exact authorized parent prefix.
    /// This creates no descriptor, writer or producer and does not follow its head.
    fn coverage_page(
        &self,
        _reference: &DatasetRef,
        _request: PageRequest,
    ) -> Result<DatasetPage, StoreError> {
        Err(StoreError::DatasetUnavailable)
    }
    /// An exact-prefix root is committed before a retained descriptor can be acknowledged.
    /// An unconfirmed catalog outcome blocks this owner until explicit reconciliation.
    fn set_root(&mut self, request: DatasetRootRequest) -> Result<DatasetRootReceipt, StoreError>;
    fn root_covers(
        &self,
        identity: &ValueHandle,
        prefixes: &[DatasetRef],
        retention: Retention,
    ) -> Result<bool, StoreError>;
    fn create(&mut self, _request: DatasetCreate) -> Result<DatasetWriterAdmission, StoreError> {
        Err(StoreError::DatasetUnavailable)
    }
    fn append(&mut self, _request: DatasetAppend) -> Result<DatasetRef, StoreError> {
        Err(StoreError::DatasetUnavailable)
    }
    fn reconcile_store(&mut self) -> Result<crate::history::Persistence, StoreError> {
        Err(StoreError::DatasetUnavailable)
    }
    /// Must run behind every admitted local mutation, never beside a queued writer.
    fn reconcile_owned(
        &mut self,
        _owner: &DatasetWriteSelection,
    ) -> Result<DatasetReconciliation, StoreError> {
        Err(StoreError::DatasetUnavailable)
    }
    fn checkpoint(
        &self,
        _reference: &DatasetRef,
    ) -> Result<Option<AnalysisCheckpoint>, StoreError> {
        Err(StoreError::DatasetUnavailable)
    }
    fn resume(&mut self, _request: DatasetResume) -> Result<DatasetWriterAdmission, StoreError> {
        Err(StoreError::DatasetUnavailable)
    }
    fn analysis_status(&self, _reference: &DatasetRef) -> Result<AnalysisStatus, StoreError> {
        Err(StoreError::DatasetUnavailable)
    }
    /// The caller obtains this identity from an authorized workspace run, never a raw UI path.
    fn continuation(&self, _run: &str) -> Result<DatasetRef, StoreError> {
        Err(StoreError::DatasetUnavailable)
    }
    fn plan_delete(&mut self, _reference: &DatasetRef) -> Result<DatasetDeletePlan, StoreError> {
        Err(StoreError::DatasetUnavailable)
    }
    fn delete(
        &mut self,
        _token: &str,
        _references: bool,
        _protected: bool,
    ) -> Result<DatasetCleanup, StoreError> {
        Err(StoreError::DatasetUnavailable)
    }
    fn withdraw(&mut self, _reference: &DatasetRef) -> Result<Vec<String>, StoreError> {
        Err(StoreError::DatasetUnavailable)
    }
    fn collect(&mut self) -> Result<DatasetCleanup, StoreError> {
        Err(StoreError::DatasetUnavailable)
    }
    fn acknowledge_cleanup(&mut self, _captures: &[CapturedValue]) -> Result<(), StoreError> {
        Err(StoreError::DatasetUnavailable)
    }
}

#[cfg(test)]
mod read_work_tests {
    use super::*;
    use crate::work_budget::WorkCounter;
    #[test]
    fn immutable_read_refusal_preserves_atomic_cause_and_exact_prefix_demand() {
        let counter = WorkCounter::earned(1000, 800);
        counter.charge(20).unwrap();
        counter.prepaid(70).unwrap();
        let read = ReadWork::new(counter.clone());
        read.charge(30).unwrap();
        let refusal = read.charge(40).unwrap_err();
        assert_eq!(refusal.dimension(), ReadWorkDimension::Prepaid);
        assert_eq!(refusal.needed(), 70);
        assert_eq!(counter.used(), 50);
        let refusal = read.charge(900).unwrap_err();
        assert_eq!(refusal.dimension(), ReadWorkDimension::Allowance);
        let refusal = read.charge(1000).unwrap_err();
        assert_eq!(refusal.dimension(), ReadWorkDimension::Absolute);
        assert_eq!(counter.used(), 50);
        counter.prepaid(100).unwrap();
        read.charge(40).unwrap();
        assert_eq!(counter.used(), 90);
    }
}
