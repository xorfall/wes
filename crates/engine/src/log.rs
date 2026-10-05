//! Observation/diagnostic logging never changes execution outcomes. This receipt boundary owns no
//! graph, UI visibility, history cache or retry policy; the session must retain and join its attempts.
use crate::{
    history::{
        AppendReceipt, DiagnosticRecord, ExecutionRecord, InvalidRecord, JournalEntry,
        NoticeContext, NoticeRecord, Persistence, Record, RecordError, RequiredPersistence,
    },
    recording::{PendingAppend, Recorder, Rejection},
    runtime::{Observation, RuntimeCode},
};
use std::sync::Arc;
use uuid::Uuid;
use wes_core::{ErrorValue, Timestamp};
use wes_language::Diagnostic;

#[derive(Clone, Debug)]
pub enum LogStatus {
    Pending,
    /// Explicitly memory-only logging, not a failed disk write.
    Memory,
    Acknowledged(AppendReceipt),
    /// The containing historical prefix was synchronized at startup; not a new event append.
    Recovered(AppendReceipt),
    Unconfirmed {
        /// Separate from any error in the observed node; recording failure must not replace it.
        error: ErrorValue,
        may_have_appended: bool,
        acknowledgement: Option<AppendReceipt>,
    },
}
#[derive(Clone, Debug)]
pub struct Logged {
    /// Only observations, diagnostics and operational notices are constructed by this boundary.
    entry: Arc<Record>,
    status: LogStatus,
}
impl Logged {
    pub(crate) fn recover(entry: JournalEntry, checkpoint: AppendReceipt) -> Option<Self> {
        matches!(
            entry,
            JournalEntry::Observed(_)
                | JournalEntry::Snapshot(_)
                | JournalEntry::Diagnosed(_)
                | JournalEntry::Noticed(_)
                | JournalEntry::Submitted(_)
        )
        .then(|| Self {
            entry: Arc::new(Record::Journal(entry)),
            status: LogStatus::Recovered(checkpoint),
        })
    }
    pub(crate) fn charge(&self) -> u64 {
        crate::recording::record_charge(&self.entry).saturating_add(1024)
    }
    pub fn entry(&self) -> &JournalEntry {
        let Record::Journal(entry) = self.entry.as_ref() else {
            unreachable!("log entries belong to workspace history")
        };
        entry
    }
    pub fn status(&self) -> &LogStatus {
        &self.status
    }
    pub fn id(&self) -> &str {
        match self.entry() {
            JournalEntry::Submitted(record) => &record.id,
            JournalEntry::Views(record) => &record.id,
            JournalEntry::Observed(record) => record.id(),
            JournalEntry::Snapshot(snapshot) => snapshot.observation.id(),
            JournalEntry::Diagnosed(record) => record.id(),
            JournalEntry::Noticed(record) => record.id(),
            _ => unreachable!("log entries are observations, diagnostics or operational notices"),
        }
    }
    pub fn durable(&self) -> bool {
        matches!(self.status, LogStatus::Acknowledged(receipt) | LogStatus::Recovered(receipt) if receipt.persistence != Persistence::Volatile)
    }
}
pub struct PendingLog {
    initial: Logged,
    receipt: Option<PendingAppend>,
    required: RequiredPersistence,
}
/// Captured before write admission so the owner can reserve receipt-retention capacity first.
pub struct PreparedLog(Arc<Record>);
impl PreparedLog {
    pub(crate) fn views(record: crate::views::ViewRecord) -> Result<Self, InvalidRecord> {
        record.validate()?;
        Ok(Self(Arc::new(Record::Journal(JournalEntry::Views(record)))))
    }

    pub(crate) fn observation_record(record: ExecutionRecord) -> Result<Self, InvalidRecord> {
        Ok(Self(Arc::new(Record::Journal(JournalEntry::Observed(
            record,
        )))))
    }
    pub(crate) fn snapshot(snapshot: crate::history::LiveSnapshot) -> Result<Self, InvalidRecord> {
        snapshot.validate()?;
        Ok(Self(Arc::new(Record::Journal(JournalEntry::Snapshot(
            snapshot,
        )))))
    }
    pub(crate) fn submission(
        record: crate::history::SubmissionRecord,
    ) -> Result<Self, InvalidRecord> {
        record.input()?;
        Ok(Self(Arc::new(Record::Journal(JournalEntry::Submitted(
            record,
        )))))
    }
    pub fn notice(
        at: Timestamp,
        context: NoticeContext,
        error: ErrorValue,
    ) -> Result<Self, InvalidRecord> {
        Ok(Self(Arc::new(Record::Journal(JournalEntry::Noticed(
            NoticeRecord::new(Uuid::new_v4().to_string(), at, context, error)?,
        )))))
    }
    pub fn observation(at: Timestamp, observation: &Observation) -> Result<Self, InvalidRecord> {
        let record = ExecutionRecord::capture(Uuid::new_v4().to_string(), at, observation)?;
        Ok(Self(Arc::new(Record::Journal(JournalEntry::Observed(
            record,
        )))))
    }
    pub fn diagnostic(
        at: Timestamp,
        cell: String,
        source: String,
        diagnostic: Diagnostic,
    ) -> Result<Self, InvalidRecord> {
        Self::diagnostic_shared(at, cell, source.into(), diagnostic)
    }
    pub(crate) fn diagnostic_shared(
        at: Timestamp,
        cell: String,
        source: Arc<str>,
        diagnostic: Diagnostic,
    ) -> Result<Self, InvalidRecord> {
        let record = DiagnosticRecord::with_shared_source(
            Uuid::new_v4().to_string(),
            at,
            cell,
            source,
            diagnostic,
        )?;
        Ok(Self(Arc::new(Record::Journal(JournalEntry::Diagnosed(
            record,
        )))))
    }
    pub(crate) fn charge(&self) -> u64 {
        crate::recording::record_charge(&self.0).saturating_add(1024)
    }
    pub(crate) fn refuse(self) -> Logged {
        Logged {
            entry: self.0,
            status: unconfirmed(
                "Execution history could not be saved: pending receipt capacity is full.",
                false,
                None,
            ),
        }
    }
}
impl PendingLog {
    pub fn initial(&self) -> &Logged {
        &self.initial
    }
    /// Cancelling this wait does not cancel an admitted append. The session owner must join it
    /// through disconnect/shutdown to publish the final acknowledgement or recording error.
    pub async fn wait(mut self) -> Logged {
        if let Some(receipt) = self.receipt {
            self.initial.status = match receipt.wait().await {
                Ok(acknowledgement) if self.required.accepts(acknowledgement.persistence) => {
                    LogStatus::Acknowledged(acknowledgement)
                }
                Ok(acknowledgement) => unconfirmed(
                    "The history acknowledgement does not satisfy the required persistence policy.",
                    true,
                    Some(acknowledgement),
                ),
                Err(error) => recording_failure(error),
            };
        }
        self.initial
    }
}
#[derive(Clone)]
pub struct LogRecorder {
    recorder: Option<Recorder>,
    required: RequiredPersistence,
}
impl LogRecorder {
    pub(crate) fn is_recorded(&self) -> bool {
        self.recorder.is_some()
    }
    pub fn memory() -> Self {
        Self {
            recorder: None,
            required: RequiredPersistence::Volatile,
        }
    }
    pub fn recorded(recorder: Recorder, required: RequiredPersistence) -> Self {
        Self {
            recorder: Some(recorder),
            required,
        }
    }
    pub fn observation(
        &self,
        at: Timestamp,
        observation: &Observation,
    ) -> Result<PendingLog, InvalidRecord> {
        Ok(self.record(PreparedLog::observation(at, observation)?))
    }
    pub fn diagnostic(
        &self,
        at: Timestamp,
        cell: String,
        source: String,
        diagnostic: Diagnostic,
    ) -> Result<PendingLog, InvalidRecord> {
        Ok(self.record(PreparedLog::diagnostic(at, cell, source, diagnostic)?))
    }
    pub fn record(&self, prepared: PreparedLog) -> PendingLog {
        let entry = prepared.0;
        let mut receipt = None;
        let status = match &self.recorder {
            None => LogStatus::Memory,
            Some(recorder) => match recorder.try_enqueue(entry.clone()) {
                Ok(pending) => {
                    receipt = Some(pending);
                    LogStatus::Pending
                }
                Err(rejected) => unconfirmed(
                    match rejected.reason {
                        Rejection::Full => {
                            "Execution history could not be saved: the recording queue is full."
                        }
                        Rejection::TooLarge => {
                            "Execution history could not be saved: the record exceeds the recording budget."
                        }
                        Rejection::Closed => {
                            "Execution history could not be saved: the recording writer is unavailable."
                        }
                    },
                    false,
                    None,
                ),
            },
        };
        PendingLog {
            initial: Logged { entry, status },
            receipt,
            required: self.required,
        }
    }
}
fn unconfirmed(
    message: &'static str,
    may_have_appended: bool,
    acknowledgement: Option<AppendReceipt>,
) -> LogStatus {
    LogStatus::Unconfirmed {
        error: RuntimeCode::RecordingFailed.error(message, None),
        may_have_appended,
        acknowledgement,
    }
}
fn recording_failure(error: RecordError) -> LogStatus {
    let (message, may_have_appended) = match error {
        RecordError::RequestUnsupported
        | RecordError::RequestConflict
        | RecordError::RequestContext
        | RecordError::PageUnsupported
        | RecordError::InvalidCursor
        | RecordError::ReadBusy => (
            "The history backend refused an unsupported or invalid history operation.",
            false,
        ),
        RecordError::CaptureUnsupported => (
            "The history backend does not support synchronized capture.",
            false,
        ),
        RecordError::Closed => (
            "The recording writer stopped before acknowledging execution history.",
            true,
        ),
        RecordError::Poisoned => (
            "Execution history could not be saved: the writer requires recovery before further writes.",
            false,
        ),
        RecordError::Limit(_) => (
            "Execution history could not be saved: the record exceeds a recording limit.",
            false,
        ),
        RecordError::Backend {
            may_have_appended, ..
        } => (
            "Execution history could not be saved because recording failed.",
            may_have_appended,
        ),
    };
    unconfirmed(message, may_have_appended, None)
}
