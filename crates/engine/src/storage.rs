//! Value storage port. Concrete codecs, directories and durability mechanisms belong to adapters.
use std::{fmt, sync::Arc};
use thiserror::Error;
use uuid::Uuid;
use wes_core::Value;
mod acquisition;
mod analysis_attempt;
mod analysis_budget;
mod analysis_coverage;
pub mod datasets;
mod private;
mod read_failure;
mod residency;
mod scope;
mod snapshot;
pub use acquisition::{
    AcquiredFuture, AcquiredOutput, Acquisition, AcquisitionContext, CHUNK_BYTES, LeafSelection,
    SnapshotFinalizer, SnapshotMember, SourceRange, acquisition_error,
};
pub use read_failure::ReadFailureKind;
pub use residency::ResidentMemory;
pub use scope::{DatasetLifetime, TransientScope, TransientScopeWitness};
pub use snapshot::{SnapshotDescriptor, StoredSnapshot};
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
    #[error("the output policy does not permit this storage or export operation")]
    Restricted,
    #[error(
        "provider-origin read permission is unavailable; explicitly revalidate its captured scope"
    )]
    ProviderReadUnavailable,
    #[error(
        "provider-origin authority changed during this read; obtain a fresh observation after explicit revalidation"
    )]
    ProviderReadChanged,
    #[error("a value must be materialized before retention")]
    NonMaterialized,
    #[error("the newly stored value is unavailable")]
    MissingValue,
    #[error("the requested retained copy could not be created")]
    RetentionUnavailable,
    #[error("the storage worker is unavailable")]
    Closed,
    #[error("transient data requires its active scope or an acknowledged retained copy")]
    ScopeUnavailable,
    #[error(transparent)]
    ReadWork(#[from] datasets::ReadWorkRefusal),
    #[error("a dataset row exceeds its logical charge limit ({limit})")]
    DatasetRowCharge { limit: u64 },
    #[error("a dataset row exceeds its stored payload page-byte limit ({limit})")]
    DatasetRowBytes { limit: u64 },
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
    #[error("stored result validation failed")]
    Corrupt,
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
    pub memory_count: u64,
    pub memory_bytes: u64,
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

/// Placement of a new payload, independent of retention and data authority.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ValuePlacement {
    #[default]
    ManagedFile,
    Memory,
    /// Explicit permission to spill cold memory copies into bounded temporary storage.
    MemoryAndDisk,
}

/// A bounded reader/retention hold. Dropping the hold requires no storage I/O.
#[derive(Clone)]
#[must_use = "retain the hold for the admitted read or retention operation"]
pub struct ValueLease {
    _owner: Arc<dyn Send + Sync>,
}
impl ValueLease {
    pub fn retain(owner: impl Send + Sync + 'static) -> Self {
        Self {
            _owner: Arc::new(owner),
        }
    }
}
impl fmt::Debug for ValueLease {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ValueLease").finish_non_exhaustive()
    }
}

/// Synchronous I/O port, called on storage workers rather than the runtime state owner.
pub trait ValueStore: Send {
    /// Whole-result policy before typed payload decoding or selected reads.
    /// Implement from bounded immutable metadata or already resident policy;
    /// never fall back to reading/decoding the domain body. None means absent.
    fn read_policy(
        &self,
        handle: &ValueHandle,
    ) -> Result<Option<wes_core::flow::FlowPolicy>, StoreError>;
    /// Compose already retained immutable members without decoding their payloads.
    fn compose(
        &mut self,
        _handle: &ValueHandle,
        _members: &[SnapshotMember],
    ) -> Result<StoredSnapshot, StoreError> {
        Err(StoreError::Limit("stored composition unavailable"))
    }
    fn begin_acquisition(
        &mut self,
        _maximum: u64,
        _owner: std::sync::Weak<()>,
    ) -> Result<ValueHandle, StoreError> {
        Err(StoreError::Limit("bounded acquisition unavailable"))
    }
    fn append_acquisition(
        &mut self,
        _handle: &ValueHandle,
        _port: &str,
        _bytes: &[u8],
    ) -> Result<(), StoreError> {
        Err(StoreError::Limit("bounded acquisition unavailable"))
    }
    fn seal_acquisition(
        &mut self,
        _handle: &ValueHandle,
        _shell: &Value,
        _leaves: &[LeafSelection],
    ) -> Result<StoredSnapshot, StoreError> {
        Err(StoreError::Limit("bounded acquisition unavailable"))
    }
    fn abort_acquisition(&mut self, _handle: &ValueHandle) -> Result<(), StoreError> {
        Err(StoreError::Limit("bounded acquisition unavailable"))
    }
    fn describe(
        &self,
        handle: &ValueHandle,
        fields: &[String],
    ) -> Result<Option<StoredSnapshot>, StoreError> {
        let Some(loaded) = self.read(handle)? else {
            return Ok(None);
        };
        let value =
            crate::plan::project_value(&loaded.value, fields).map_err(|_| StoreError::Conflict)?;
        let Some(digest) = self.digest(handle)? else {
            return Ok(None);
        };
        let mut snapshot = StoredSnapshot::capture(handle.clone(), digest, &value)
            .ok_or(StoreError::Limit("snapshot metadata"))?;
        snapshot = snapshot.with_selection(fields.to_vec());
        Ok(Some(snapshot))
    }
    /// Inspect a whole result with a conservative stored-representation byte bound. Descriptor
    /// fallback can decode ordinary encodings, so size refusal must precede that fallback.
    fn describe_bounded(
        &self,
        handle: &ValueHandle,
        maximum: u64,
    ) -> Result<Option<StoredSnapshot>, StoreError> {
        if maximum == 0 {
            return Err(StoreError::Limit("snapshot inspection bytes"));
        }
        let Some(bytes) = self.size(handle)? else {
            return Ok(None);
        };
        if bytes > maximum {
            return Err(StoreError::Limit("snapshot inspection bytes"));
        }
        self.describe(handle, &[])
    }
    /// Immutable physical members that must be held with a composed result. This grants no read
    /// authority; the shared owner validates every member's current policy and Dataset roots.
    fn dependencies(&self, _handle: &ValueHandle) -> Result<Vec<ValueHandle>, StoreError> {
        Ok(vec![])
    }
    fn read_selected(
        &self,
        handle: &ValueHandle,
        fields: &[String],
        limit: u64,
    ) -> Result<Option<LoadedValue>, StoreError> {
        let Some(loaded) = self.read(handle)? else {
            return Ok(None);
        };
        let value =
            crate::plan::project_value(&loaded.value, fields).map_err(|_| StoreError::Conflict)?;
        crate::value_size::value_charge(&value, limit)
            .ok_or(StoreError::Limit("selected result"))?;
        Ok(Some(LoadedValue { value }))
    }
    fn source_range(
        &self,
        snapshot: &StoredSnapshot,
        from: u64,
        maximum: usize,
    ) -> Result<SourceRange, StoreError> {
        if maximum == 0 || maximum > CHUNK_BYTES {
            return Err(StoreError::Limit("source range"));
        }
        if self.digest(&snapshot.handle)?.as_ref() != Some(&snapshot.digest) {
            return Err(StoreError::Conflict);
        }
        let loaded = self
            .read_selected(
                &snapshot.handle,
                &snapshot.selection,
                wes_budgets::get("storage.bytes"),
            )?
            .ok_or(StoreError::MissingValue)?;
        if !snapshot.matches(&loaded.value) {
            return Err(StoreError::Conflict);
        }
        let (bytes, text) = match loaded.value.data() {
            wes_core::Data::Text(s) => (s.as_bytes(), true),
            wes_core::Data::Bytes(b) => (b.as_ref(), false),
            _ => return Err(StoreError::NonMaterialized),
        };
        let extent = bytes.len() as u64;
        let start = usize::try_from(from)
            .ok()
            .filter(|n| *n <= bytes.len())
            .ok_or(StoreError::Conflict)?;
        Ok(SourceRange {
            bytes: bytes[start..start.saturating_add(maximum).min(bytes.len())].to_vec(),
            extent,
            text,
        })
    }
    /// Authenticated local storage; does not grant retention or export.
    fn supports_confidential(&self) -> bool {
        false
    }
    /// Minimum persistence established by a successful `keep`. Unspecified/memory stores MUST
    /// remain volatile; a journal receipt cannot make their retained bytes durable.
    fn retained_persistence(&self) -> crate::history::Persistence {
        crate::history::Persistence::Volatile
    }
    fn store(&mut self, value: &Value) -> Result<ValueHandle, StoreError>;
    /// Unsupported placement refuses before publication; it must never fall back to disk.
    fn store_with_placement(
        &mut self,
        value: &Value,
        placement: ValuePlacement,
    ) -> Result<ValueHandle, StoreError> {
        match placement {
            ValuePlacement::ManagedFile => self.store(value),
            ValuePlacement::Memory | ValuePlacement::MemoryAndDisk => {
                Err(StoreError::Limit("memory placement unavailable"))
            }
        }
    }
    /// Whether this handle owns a temporary file requiring durable cleanup evidence.
    fn has_temporary_file(&self, _handle: &ValueHandle) -> Result<bool, StoreError> {
        Ok(false)
    }
    /// Temporary payload or dependency ownership requiring work cleanup evidence.
    /// A memory descriptor can still own physical Dataset objects.
    fn requires_ownership_record(&self, handle: &ValueHandle) -> Result<bool, StoreError> {
        self.has_temporary_file(handle)
    }
    fn hold(&mut self, _handle: &ValueHandle) -> Result<Option<ValueLease>, StoreError> {
        Ok(None)
    }
    fn read(&self, handle: &ValueHandle) -> Result<Option<LoadedValue>, StoreError>;
    fn encoded(&self, handle: &ValueHandle) -> Result<Option<Vec<u8>>, StoreError>;
    /// Immutable snapshot digest. Large adapters hash with bounded buffers rather than exporting
    /// the entire encoding. The digest includes any checksummed payload dependencies.
    fn digest(&self, handle: &ValueHandle) -> Result<Option<String>, StoreError> {
        use sha2::{Digest, Sha256};
        Ok(self
            .encoded(handle)?
            .map(|bytes| format!("sha256:{:x}", Sha256::digest(bytes))))
    }
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
