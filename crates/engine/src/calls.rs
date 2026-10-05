//! Acknowledged command admission and run-specific call recovery. No graph ownership or I/O codec.
use crate::{
    graph::NodeId,
    history::{
        CallRecord, CommandRecord, HistoryCheckpoint, JournalEntry, Record, RecordError,
        RecoveryEntry,
    },
    recording::Recorder,
    runtime::Run,
};
use indexmap::IndexSet;
use std::sync::Arc;
use thiserror::Error;
use wes_core::Timestamp;

pub use crate::history::RequiredPersistence;

#[derive(Debug, Error)]
pub enum CallJournalError {
    #[error("command admission does not authorize this node in this journal")]
    Unadmitted,
    #[error("invalid command admission: {0}")]
    InvalidCommand(#[from] crate::history::InvalidRecord),
    #[error("history acknowledgement does not satisfy the session persistence policy")]
    InsufficientPersistence,
    #[error(transparent)]
    Recording(#[from] RecordError),
}

/// An opaque local receipt, not a serializable substitute for replay or provider authorization.
/// It retains only origin metadata, not a duplicate copy of the command source and type packages.
#[derive(Clone, Debug)]
pub struct AdmittedCommand(Arc<Admission>);
#[derive(Debug)]
struct Admission {
    journal: Arc<()>,
    cell: String,
    nodes: IndexSet<NodeId>,
    needs_accepted: bool,
}
impl AdmittedCommand {
    pub fn cell(&self) -> &str {
        &self.0.cell
    }
    pub fn contains(&self, node: &NodeId) -> bool {
        self.0.nodes.contains(node)
    }
}

/// Consumed after physical local exit. `produced` never asserts a remote transaction outcome.
pub struct Calling {
    journal: Arc<()>,
    run: Run,
}

#[derive(Clone)]
pub struct CallJournal {
    identity: Arc<()>,
    recorder: Arc<Recorder>,
    required: RequiredPersistence,
}
impl CallJournal {
    pub(crate) fn recording(&self) -> (Recorder, RequiredPersistence) {
        (self.recorder.as_ref().clone(), self.required)
    }
    pub(crate) fn weak_recorder(&self) -> std::sync::Weak<Recorder> {
        Arc::downgrade(&self.recorder)
    }

    pub fn new(recorder: Recorder, required: RequiredPersistence) -> Self {
        Self {
            identity: Arc::new(()),
            recorder: Arc::new(recorder),
            required,
        }
    }

    /// A coordinator serializes structural changes across this await. Dropping the future does
    /// not retract an already enqueued write; without the returned receipt no call is authorized.
    pub async fn admit(&self, command: CommandRecord) -> Result<AdmittedCommand, CallJournalError> {
        command.validate()?;
        let admission = AdmittedCommand(Arc::new(Admission {
            journal: self.identity.clone(),
            cell: command.cell.clone(),
            nodes: command.nodes.iter().cloned().collect(),
            needs_accepted: false,
        }));
        self.append(Record::Journal(JournalEntry::Command(command)))
            .await?;
        self.append(Record::Recovery(RecoveryEntry::Accepted {
            cell: admission.cell().to_owned(),
        }))
        .await?;
        Ok(admission)
    }

    /// Only the strict reconstruction coordinator may recover origin metadata from synchronized
    /// history. This is not fresh execution authority: nodes remain held until explicit action.
    pub(crate) fn recover(
        &self,
        command: &CommandRecord,
        checkpoint: HistoryCheckpoint,
        accepted: bool,
    ) -> Result<AdmittedCommand, CallJournalError> {
        command.validate()?;
        self.checkpoint(checkpoint)?;
        Ok(AdmittedCommand(Arc::new(Admission {
            journal: self.identity.clone(),
            cell: command.cell.clone(),
            nodes: command.nodes.iter().cloned().collect(),
            needs_accepted: !accepted,
        })))
    }
    pub(crate) fn checkpoint(&self, checkpoint: HistoryCheckpoint) -> Result<(), CallJournalError> {
        if !self.required.accepts(checkpoint.journal.persistence)
            || !self.required.accepts(checkpoint.recovery.persistence)
        {
            return Err(CallJournalError::InsufficientPersistence);
        }
        Ok(())
    }

    /// Input validation precedes this call. The executor must await its receipt, then check
    /// cancellation again before provider entry. Never drop an admitted append on cancellation.
    pub async fn calling(
        &self,
        admission: &AdmittedCommand,
        run: &Run,
        capability: String,
        safe: bool,
        at: Timestamp,
    ) -> Result<Calling, CallJournalError> {
        self.authorize(admission, run).await?;
        self.append(Record::Recovery(RecoveryEntry::Calling(CallRecord {
            node: run.node().clone(),
            run: run.id().clone(),
            cell: admission.cell().to_owned(),
            capability,
            safe,
            at,
        })))
        .await?;
        Ok(Calling {
            journal: self.identity.clone(),
            run: run.clone(),
        })
    }

    /// Validate command/journal ownership without recording a finite Calling/Called attempt.
    /// Open streams are closed after process loss, not ambiguous finite recovery calls. A restored
    /// receipt may still require an acknowledged Accepted before explicit provider entry.
    pub(crate) async fn authorize(
        &self,
        admission: &AdmittedCommand,
        run: &Run,
    ) -> Result<(), CallJournalError> {
        if !Arc::ptr_eq(&self.identity, &admission.0.journal) || !admission.contains(run.node()) {
            return Err(CallJournalError::Unadmitted);
        }
        if admission.0.needs_accepted {
            // No write occurs during reconstruction. A missing crash-tail Accepted is
            // acknowledged on explicit execution, before provider entry. Repetition is idempotent metadata.
            self.append(Record::Recovery(RecoveryEntry::Accepted {
                cell: admission.cell().to_owned(),
            }))
            .await?;
        }
        Ok(())
    }

    pub async fn called(&self, calling: Calling, produced: bool) -> Result<(), CallJournalError> {
        if !Arc::ptr_eq(&self.identity, &calling.journal) {
            return Err(CallJournalError::Unadmitted);
        }
        self.append(Record::Recovery(RecoveryEntry::Called {
            node: calling.run.node().clone(),
            run: calling.run.id().clone(),
            produced,
        }))
        .await
    }

    pub(crate) async fn record_trace(
        &self,
        record: crate::trace::TraceRecord,
    ) -> Result<(), CallJournalError> {
        self.append(Record::Journal(JournalEntry::Trace(record)))
            .await
    }
    async fn append(&self, record: Record) -> Result<(), CallJournalError> {
        let receipt = self.recorder.append(Arc::new(record)).await?;
        if !self.required.accepts(receipt.persistence) {
            return Err(CallJournalError::InsufficientPersistence);
        }
        Ok(())
    }
}
