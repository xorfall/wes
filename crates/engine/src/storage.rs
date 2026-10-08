//! Value storage port. Concrete codecs, directories and durability mechanisms belong to adapters.
use std::{fmt, sync::Arc};
use thiserror::Error;
use uuid::Uuid;
use wes_core::Value;
pub mod datasets;
mod private;
mod worker;
pub use worker::{
    AutoKeep, PendingStore, PublicationPolicy, StoreDrain, StoreWorker, StoreWorkerLimits,
    StoreWorkerTask, StoredOutput, spawn_storage, spawn_store,
};

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ValueHandle(Arc<str>);
impl ValueHandle {
    /// Canonical UUIDs are the supported persisted store identities.
    /// A data-plane handle is never a caller-supplied path fragment or a platform device name.
    pub fn new(value: &str) -> Result<Self, StoreError> {
        let parsed = Uuid::parse_str(value).map_err(|_| StoreError::InvalidHandle)?;
        if parsed.hyphenated().to_string() != value {
            return Err(StoreError::InvalidHandle);
        }
        Ok(Self(Arc::from(value)))
    }
    pub fn fresh() -> Self {
        Self(Arc::from(Uuid::new_v4().to_string()))
    }
    pub fn as_str(&self) -> &str {
        &self.0
    }
}
impl fmt::Display for ValueHandle {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

#[derive(Debug, Error)]
pub enum StoreError {
    #[error("private values cannot be retained or exported")]
    Restricted,
    #[error("a value must be materialized before retention")]
    NonMaterialized,
    #[error("the newly stored value is unavailable")]
    MissingValue,
    #[error("the requested retained copy could not be created")]
    RetentionUnavailable,
    #[error("the storage worker is unavailable")]
    Closed,
    #[error("the storage operation exceeds its {0} budget")]
    Limit(&'static str),
    #[error("a value handle must be a canonical UUID")]
    InvalidHandle,
    #[error("the value store is already in use")]
    Locked,
    #[error("the store directory must be private to its owner")]
    InsecureDirectory,
    #[error("the nonempty directory is not an owned wes value store")]
    UnownedDirectory,
    #[error("stored values must be regular files, not links or special files")]
    NotRegular,
    #[error("the handle already contains different data")]
    Conflict,
    #[error("this storage owner has no dataset capability")]
    DatasetUnavailable,
    #[error("the dataset descriptor is unavailable or belongs to another home")]
    DatasetMissing,
    #[error(
        "the owned analysis has no exact latest local-write witness; an unknown suffix cannot be resumed"
    )]
    DatasetRecoveryUnknown,
    #[error(
        "a newer analysis attempt owns the latest checkpoint; select that owned continuation instead"
    )]
    DatasetNewerAttempt,
    #[error(
        "dataset storage needs explicit local reconciliation; stop its writers, then run :dataset reconcile"
    )]
    DatasetNeedsReconciliation,
    #[error("invalid source excerpt range: {0}")]
    SourceRange(&'static str),
    #[error("the dataset read has been withdrawn")]
    DatasetWithdrawn,
    #[error("committed dataset storage is corrupt")]
    DatasetCorrupt,
    #[error("dataset commit {transaction} has an unconfirmed outcome")]
    DatasetUnconfirmed { transaction: String },
    #[error(
        "dataset write {transaction} has an unconfirmed admission {admission}; no data mutation entered"
    )]
    DatasetAdmissionUnconfirmed {
        transaction: String,
        admission: String,
    },
    #[error("value storage failed during {operation}")]
    Backend {
        operation: &'static str,
        #[source]
        source: Box<dyn std::error::Error + Send + Sync>,
    },
    #[error("value {handle} was published, but storage completion could not be confirmed")]
    Published {
        handle: ValueHandle,
        #[source]
        source: Box<dyn std::error::Error + Send + Sync>,
    },
    #[error("value {handle} may have been removed; removal completion could not be confirmed")]
    Released {
        handle: ValueHandle,
        #[source]
        source: Box<dyn std::error::Error + Send + Sync>,
    },
}
impl StoreError {
    pub fn backend(
        operation: &'static str,
        source: impl std::error::Error + Send + Sync + 'static,
    ) -> Self {
        Self::Backend {
            operation,
            source: Box::new(source),
        }
    }
}
#[derive(Clone, Debug)]
pub struct LoadedValue {
    pub value: Value,
}

/// Read-only recovery evidence. Presence is not a fresh archive synchronization receipt.
#[derive(Clone, Debug)]
pub struct RecoveredOutput {
    pub loaded: LoadedValue,
    pub bytes: u64,
    pub retention: Retention,
}

/// Why bytes are retained, not merely whether an archive copy exists. Unknown is
/// missing evidence, never inferred intent. No class authorizes automatic deletion.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Retention {
    Temporary,
    Automatic,
    Protected,
    #[default]
    Unknown,
}
impl Retention {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Temporary => "temporary",
            Self::Automatic => "automatic",
            Self::Protected => "protected",
            Self::Unknown => "unknown",
        }
    }
}
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct RetentionUsage {
    pub classes: Vec<(Retention, u64, u64)>,
    pub live_bytes: u64,
    pub archive_bytes: u64,
    pub private_count: u64,
    pub private_bytes: u64,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct EvictionBatch {
    pub handles: Vec<ValueHandle>,
    /// More candidates remain, even when all candidates in this batch had retained archive copies.
    pub more: bool,
}

/// Synchronous I/O port, called on storage workers rather than the runtime state owner.
pub trait ValueStore: Send {
    /// Minimum persistence established by a successful `keep`. Unspecified/memory stores MUST
    /// remain volatile; a journal receipt cannot make their retained bytes durable.
    fn retained_persistence(&self) -> crate::history::Persistence {
        crate::history::Persistence::Volatile
    }
    fn store(&mut self, value: &Value) -> Result<ValueHandle, StoreError>;
    fn read(&self, handle: &ValueHandle) -> Result<Option<LoadedValue>, StoreError>;
    fn encoded(&self, handle: &ValueHandle) -> Result<Option<Vec<u8>>, StoreError>;
    fn size(&self, handle: &ValueHandle) -> Result<Option<u64>, StoreError>;
    /// Automatic retention must account for referenced data, not just the descriptor encoding.
    fn retention_size(&self, handle: &ValueHandle) -> Result<Option<u64>, StoreError> {
        self.size(handle)
    }
    fn automatic_retention_allowed(&self, _handle: &ValueHandle) -> Result<bool, StoreError> {
        Ok(true)
    }
    fn release(&mut self, handle: &ValueHandle) -> Result<bool, StoreError>;
    fn keep(&mut self, _handle: &ValueHandle) -> Result<bool, StoreError> {
        Ok(false)
    }
    fn is_kept(&self, _handle: &ValueHandle) -> Result<bool, StoreError> {
        Ok(false)
    }
    fn keep_with_reason(
        &mut self,
        handle: &ValueHandle,
        _reason: Retention,
    ) -> Result<bool, StoreError> {
        self.keep(handle)
    }
    fn retention(&self, handle: &ValueHandle) -> Result<Retention, StoreError> {
        Ok(if self.is_kept(handle)? {
            Retention::Unknown
        } else {
            Retention::Temporary
        })
    }
    fn retention_usage(&self) -> Result<RetentionUsage, StoreError> {
        Err(StoreError::Limit("retention inventory unavailable"))
    }
    /// Examine at most `maximum` candidates and return at most that many handles. Unexamined
    /// notifications stay pending; failure must not consume the attempted batch. This boundary
    /// allows bounded delivery without discarding notifications after an oversized destructive read.
    fn take_evicted(&mut self, _maximum: usize) -> Result<EvictionBatch, StoreError> {
        Ok(EvictionBatch::default())
    }
}
