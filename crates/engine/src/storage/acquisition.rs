//! Finite, backpressured acquisition through the same scoped result owner.
use super::*;
use crate::providers::InvocationError;
use std::{future::Future, pin::Pin, sync::Arc};
use wes_core::{Provenance, Value};
pub const CHUNK_BYTES: usize = 65536;
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeafSelection {
    pub port: String,
    pub fields: Vec<String>,
    pub text: bool,
}
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SnapshotMember {
    pub name: String,
    pub handle: String,
    pub digest: String,
    pub selection: Vec<String>,
}
#[derive(Clone, Debug)]
pub struct SourceRange {
    pub bytes: Vec<u8>,
    pub extent: u64,
    pub text: bool,
}
pub enum AcquiredOutput {
    Value(Value),
    Stored {
        output: StoredOutput,
        hold: ValueLease,
    },
}
pub type AcquiredFuture =
    Pin<Box<dyn Future<Output = Result<AcquiredOutput, InvocationError>> + Send>>;
#[derive(Clone)]
pub struct AcquisitionContext {
    worker: StoreWorker,
    provenance: Provenance,
    finalizer: Option<SnapshotFinalizer>,
    decoder: Option<ChunkDecoder>,
}
/// A trusted adapter's bounded, nonexpanding transport decoder. It receives at most one
/// acquisition chunk and returns bytes for that same port. It cannot change placement or policy.
pub type ChunkDecoder =
    Arc<dyn Fn(&str, Vec<u8>) -> Result<Vec<u8>, InvocationError> + Send + Sync>;
pub type SnapshotFinalizer = Arc<
    dyn Fn(Value, Vec<LeafSelection>) -> Result<(Value, Vec<LeafSelection>), InvocationError>
        + Send
        + Sync,
>;
impl AcquisitionContext {
    pub(crate) fn new(worker: StoreWorker, provenance: Provenance) -> Self {
        Self {
            worker,
            provenance,
            finalizer: None,
            decoder: None,
        }
    }
    pub fn with_finalizer(mut self, finalizer: SnapshotFinalizer) -> Self {
        self.finalizer = Some(finalizer);
        self
    }
    pub fn provenance(&self) -> &Provenance {
        &self.provenance
    }
    pub fn with_decoder(mut self, decoder: ChunkDecoder) -> Self {
        self.decoder = Some(decoder);
        self
    }
    pub async fn begin(&self, maximum: u64) -> Result<Acquisition, StoreError> {
        if self.provenance.policy().is_private()
            || self.provenance.policy().is_unknown()
            || (self.provenance.policy().is_confidential()
                && !self.worker.supports_confidential_values())
            || self.worker.placement != ValuePlacement::MemoryAndDisk
        {
            return Err(StoreError::Restricted);
        }
        let owner = Arc::new(());
        let witness = Arc::downgrade(&owner);
        let handle = self
            .worker
            .request(4096, move |store| store.begin_acquisition(maximum, witness))
            .await?
            .wait()
            .await?;
        Ok(Acquisition {
            worker: self.worker.clone(),
            handle,
            owner,
            provenance: self.provenance.clone(),
            finalizer: self.finalizer.clone(),
            decoder: self.decoder.clone(),
        })
    }
}
/// Its weak witness protects unpublished drafts without keeping an abandoned acquisition alive.
/// Explicit failure paths join abort; storage cleanup also collects abandoned drafts on admission.
pub struct Acquisition {
    worker: StoreWorker,
    handle: ValueHandle,
    owner: Arc<()>,
    provenance: Provenance,
    finalizer: Option<SnapshotFinalizer>,
    decoder: Option<ChunkDecoder>,
}
impl Acquisition {
    /// Decode transport bytes before storing them. The producer joins this operation before
    /// reading its next chunk, retaining the ordinary backpressure and physical acknowledgement.
    pub async fn append_input(&self, port: &str, bytes: Vec<u8>) -> Result<(), InvocationError> {
        let input_bytes = bytes.len();
        if input_bytes > CHUNK_BYTES || port.is_empty() || port.len() > 128 {
            return Err(acquisition_error(StoreError::Limit("acquisition chunk")));
        }
        let bytes = match &self.decoder {
            Some(decode) => decode(port, bytes)?,
            None => bytes,
        };
        if bytes.len() > input_bytes {
            return Err(acquisition_error(StoreError::Limit(
                "nonexpanding acquisition decoder",
            )));
        }
        self.append(port, bytes).await.map_err(acquisition_error)
    }
    pub async fn append(&self, port: &str, bytes: Vec<u8>) -> Result<(), StoreError> {
        if bytes.len() > CHUNK_BYTES || port.is_empty() || port.len() > 128 {
            return Err(StoreError::Limit("acquisition chunk"));
        }
        let handle = self.handle.clone();
        let port = port.to_owned();
        self.worker
            // A backend may protect a chunk using additional bounded encryption buffers.
            // Reserve the storage working allowance before any backend allocation or write.
            .request(self.worker.limits.bytes.get(), move |store| {
                store.append_acquisition(&handle, &port, &bytes)
            })
            .await?
            .wait()
            .await
    }
    pub async fn abort(self) -> Result<(), StoreError> {
        let handle = self.handle.clone();
        self.worker
            .request(4096, move |store| store.abort_acquisition(&handle))
            .await?
            .wait()
            .await
    }
    pub async fn finish(
        self,
        shell: Value,
        leaves: Vec<LeafSelection>,
    ) -> Result<AcquiredOutput, InvocationError> {
        let (shell, leaves) = match &self.finalizer {
            Some(f) => match f(shell, leaves) {
                Ok(v) => v,
                Err(e) => {
                    let _ = self.abort().await;
                    return Err(e);
                }
            },
            None => (shell, leaves),
        };
        let mut data = shell.data().clone();
        for leaf in &leaves {
            clear_leaf(&mut data, &leaf.fields, leaf.text).map_err(acquisition_error)?;
        }
        let shell = Value::new(shell.shape().clone(), data, shell.provenance().clone())
            .map_err(|_| acquisition_error(StoreError::Conflict))?
            .with_metadata(shell.metadata().cloned());
        let shell = shell.with_provenance(shell.provenance().inheriting(&self.provenance));
        let handle = self.handle.clone();
        let result = self
            .worker
            .seal_owned_acquisition(handle.clone(), shell, leaves)
            .await
            .map(|(snapshot, hold, bytes)| AcquiredOutput::Stored {
                output: StoredOutput {
                    bytes,
                    handle,
                    kept: false,
                    retention: Retention::Temporary,
                    retained_persistence: crate::history::Persistence::Volatile,
                    requires_ownership_record: true,
                    snapshot: Some(snapshot),
                },
                hold,
            })
            .map_err(acquisition_error);
        // Keep the draft witness until the seal operation physically finishes, including refusal.
        drop(self.owner);
        let result = result?;
        if let AcquiredOutput::Stored { output, .. } = &result {
            if let Some(snapshot) = &output.snapshot {
                self.worker
                    .residency
                    .admit_snapshot(snapshot)
                    .map_err(acquisition_error)?;
            }
        }
        Ok(result)
    }
}
impl StoreWorker {
    pub(crate) async fn compose(
        &self,
        handle: ValueHandle,
        members: Vec<SnapshotMember>,
    ) -> Result<StoredSnapshot, StoreError> {
        let limit = self.limits.bytes.get();
        let snapshot = self
            .request(limit, move |store| store.compose(&handle, &members))
            .await?
            .wait()
            .await?;
        self.residency.admit_snapshot(&snapshot)?;
        Ok(snapshot)
    }
    pub(crate) async fn capture_stored_source(
        &self,
        snapshot: StoredSnapshot,
    ) -> Result<super::datasets::CapturedValue, StoreError> {
        let scope = self.scope.clone().ok_or(StoreError::ScopeUnavailable)?;
        let limit = self.limits.bytes.get();
        self.request(limit, move |store| {
            let lease = store
                .hold(&snapshot.handle)?
                .ok_or(StoreError::MissingValue)?;
            if store.digest(&snapshot.handle)?.as_ref() != Some(&snapshot.digest) {
                return Err(StoreError::Conflict);
            }
            scope.capture(snapshot.handle.clone(), lease)?;
            Ok(super::datasets::CapturedValue {
                handle: snapshot.handle.to_string(),
                digest: snapshot.digest.clone(),
                bytes: store
                    .size(&snapshot.handle)?
                    .ok_or(StoreError::MissingValue)?,
                selection: snapshot.selection.clone(),
            })
        })
        .await?
        .wait()
        .await
    }

    pub(crate) async fn acknowledge_acquired(
        &self,
        output: StoredOutput,
    ) -> Result<StoredOutput, StoreError> {
        let limit = self.limits.bytes.get();
        self.request(limit, move |store| {
            let snapshot = output.snapshot.as_ref().ok_or(StoreError::Conflict)?;
            let _hold = store
                .hold(&output.handle)?
                .ok_or(StoreError::MissingValue)?;
            if store
                .describe(&output.handle, &snapshot.selection)?
                .as_ref()
                != Some(snapshot)
                || store.size(&output.handle)? != Some(output.bytes)
            {
                return Err(StoreError::Conflict);
            }
            Ok(output)
        })
        .await?
        .wait()
        .await
    }

    pub async fn describe(
        &self,
        handle: ValueHandle,
        fields: Vec<String>,
    ) -> Result<Option<StoredSnapshot>, StoreError> {
        let epoch = self.read_epoch();
        let limit = self.limits.bytes.get();
        let snapshot = self
            .request(limit, move |store| store.describe(&handle, &fields))
            .await?
            .wait()
            .await?;
        if let Some(snapshot) = &snapshot {
            self.check_read_epoch(&epoch, snapshot.provenance.policy())?;
            self.residency.admit_snapshot(snapshot)?;
        }
        Ok(snapshot)
    }
    /// Bounded whole-result inspection. Size admission precedes descriptor fallbacks that decode
    /// ordinary payloads; native segmented descriptors retain their metadata-only path.
    pub async fn describe_bounded(
        &self,
        handle: ValueHandle,
        maximum: u64,
    ) -> Result<Option<StoredSnapshot>, StoreError> {
        let epoch = self.read_epoch();
        let limit = self.limits.bytes.get();
        let maximum = maximum.min(u64::from(limit));
        let snapshot = self
            .request(limit, move |store| store.describe_bounded(&handle, maximum))
            .await?
            .wait()
            .await?;
        if let Some(snapshot) = &snapshot {
            self.check_read_epoch(&epoch, snapshot.provenance.policy())?;
            self.residency.admit_snapshot(snapshot)?;
        }
        Ok(snapshot)
    }
    pub async fn source_range(
        &self,
        snapshot: StoredSnapshot,
        from: u64,
        maximum: usize,
    ) -> Result<SourceRange, StoreError> {
        self.source_range_selected(snapshot, vec![], from, maximum)
            .await
    }
    /// Select a Text/Bytes leaf only after validating the exact captured root under its
    /// scoped owner. The selection cannot weaken root read authority or change identity.
    pub async fn source_range_selected(
        &self,
        snapshot: StoredSnapshot,
        fields: Vec<String>,
        from: u64,
        maximum: usize,
    ) -> Result<SourceRange, StoreError> {
        let epoch = self.read_epoch();
        let policy = snapshot.provenance.policy().clone();
        if maximum == 0 || maximum > CHUNK_BYTES {
            return Err(StoreError::Limit("source range"));
        }
        if fields.len() > 64
            || fields.iter().any(|s| s.is_empty())
            || fields.iter().fold(0usize, |n, s| n.saturating_add(s.len())) > 4096
        {
            return Err(StoreError::SourceRange("invalid static field selection"));
        }
        let limit = self.limits.bytes.get();
        let result = self
            .request(limit, move |store| {
                let _hold = store
                    .hold(&snapshot.handle)?
                    .ok_or(StoreError::MissingValue)?;
                if fields.is_empty() {
                    return store.source_range(&snapshot, from, maximum);
                }
                let current = store
                    .describe(&snapshot.handle, &snapshot.selection)?
                    .ok_or(StoreError::MissingValue)?;
                if current.handle != snapshot.handle
                    || current.digest != snapshot.digest
                    || current.selection != snapshot.selection
                    || current.shape != snapshot.shape
                    || current.provenance != snapshot.provenance
                    || current.metadata != snapshot.metadata
                {
                    return Err(StoreError::Conflict);
                }
                if crate::plan::field_shape(&snapshot.shape, &fields).is_none() {
                    return Err(StoreError::SourceRange("selected field is unavailable"));
                }
                let mut selection = snapshot.selection.clone();
                selection.extend(fields);
                let selected = store
                    .describe(&snapshot.handle, &selection)?
                    .ok_or(StoreError::MissingValue)?;
                if selected.handle != snapshot.handle
                    || selected.digest != snapshot.digest
                    || selected.selection != selection
                {
                    return Err(StoreError::Conflict);
                }
                store.source_range(&selected, from, maximum)
            })
            .await?
            .wait()
            .await?;
        self.check_read_epoch(&epoch, &policy)?;
        if self
            .dataset_access()
            .borrow()
            .blocks_dataset_policy(&policy)
        {
            return Err(StoreError::DatasetWithdrawn);
        }
        if self.provider_readers().blocks_policy(&policy) {
            return Err(StoreError::ProviderReadUnavailable);
        }
        if self.scope.as_ref().is_some_and(|scope| !scope.is_open()) {
            return Err(StoreError::ScopeUnavailable);
        }
        Ok(result)
    }
}

fn clear_leaf(data: &mut wes_core::Data, path: &[String], text: bool) -> Result<(), StoreError> {
    if path.is_empty() {
        *data = if text {
            wes_core::Data::Text("".into())
        } else {
            wes_core::Data::Bytes(vec![].into())
        };
        return Ok(());
    }
    let wes_core::Data::Record(fields) = data else {
        return Err(StoreError::Conflict);
    };
    clear_leaf(
        fields.get_mut(&path[0]).ok_or(StoreError::Conflict)?,
        &path[1..],
        text,
    )
}
pub fn acquisition_error(error: StoreError) -> InvocationError {
    InvocationError::Failed(error.read_failure_kind().runtime_code().error(
        format!("Finite acquisition refused: {error}. Inspect acquisition limits and storage state before any new operation. No provider was replayed."),
        None,
    ))
}

#[cfg(test)]
mod admission_tests {
    use super::*;
    struct NoAcquisition;
    impl ValueStore for NoAcquisition {
        fn read_policy(
            &self,
            _: &ValueHandle,
        ) -> Result<Option<wes_core::flow::FlowPolicy>, StoreError> {
            panic!("before dispatch")
        }
        fn store(&mut self, _: &Value) -> Result<ValueHandle, StoreError> {
            panic!("before dispatch")
        }
        fn read(&self, _: &ValueHandle) -> Result<Option<LoadedValue>, StoreError> {
            panic!("before dispatch")
        }
        fn encoded(&self, _: &ValueHandle) -> Result<Option<Vec<u8>>, StoreError> {
            panic!("before dispatch")
        }
        fn size(&self, _: &ValueHandle) -> Result<Option<u64>, StoreError> {
            panic!("before dispatch")
        }
        fn release(&mut self, _: &ValueHandle) -> Result<bool, StoreError> {
            panic!("before dispatch")
        }
        fn begin_acquisition(
            &mut self,
            _: u64,
            _: std::sync::Weak<()>,
        ) -> Result<ValueHandle, StoreError> {
            panic!("unsupported protection reached acquisition dispatch")
        }
    }
    #[tokio::test]
    async fn unsupported_confidential_acquisition_refuses_before_backend_dispatch() {
        use wes_core::flow::{FlowPolicy, Residence};
        let (worker, task) = spawn_store(NoAcquisition, StoreWorkerLimits::default()).unwrap();
        let worker = worker.with_placement(ValuePlacement::MemoryAndDisk);
        for residence in [
            Residence::Memory,
            Residence::Temporary,
            Residence::Retainable,
        ] {
            let provenance =
                Provenance::default().with_policy(&FlowPolicy::default().confidential(residence));
            assert!(matches!(
                AcquisitionContext::new(worker.clone(), provenance)
                    .begin(1024)
                    .await,
                Err(StoreError::Restricted)
            ));
        }
        drop(worker);
        task.join().await.unwrap();
    }
}
