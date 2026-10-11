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
}
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
        }
    }
    pub fn with_finalizer(mut self, finalizer: SnapshotFinalizer) -> Self {
        self.finalizer = Some(finalizer);
        self
    }
    pub fn provenance(&self) -> &Provenance {
        &self.provenance
    }
    pub async fn begin(&self, maximum: u64) -> Result<Acquisition, StoreError> {
        if self.provenance.policy().is_private()
            || self.provenance.policy().is_unknown()
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
}
impl Acquisition {
    pub async fn append(&self, port: &str, bytes: Vec<u8>) -> Result<(), StoreError> {
        if bytes.len() > CHUNK_BYTES || port.is_empty() || port.len() > 128 {
            return Err(StoreError::Limit("acquisition chunk"));
        }
        let handle = self.handle.clone();
        let port = port.to_owned();
        self.worker
            .request((bytes.len() + 4096) as u32, move |store| {
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
        let limit = self.worker.limits.bytes.get();
        let result = self
            .worker
            .request(limit, move |store| {
                let snapshot = store.seal_acquisition(&handle, &shell, &leaves)?;
                let hold = store.hold(&handle)?.ok_or(StoreError::MissingValue)?;
                Ok(AcquiredOutput::Stored {
                    output: StoredOutput {
                        bytes: store.size(&handle)?.ok_or(StoreError::MissingValue)?,
                        handle: handle.clone(),
                        kept: false,
                        retention: Retention::Temporary,
                        retained_persistence: crate::history::Persistence::Volatile,
                        requires_ownership_record: true,
                        snapshot: Some(snapshot),
                    },
                    hold,
                })
            })
            .await
            .map_err(acquisition_error)?
            .wait()
            .await
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
        let limit = self.limits.bytes.get();
        let snapshot = self
            .request(limit, move |store| store.describe(&handle, &fields))
            .await?
            .wait()
            .await?;
        if let Some(snapshot) = &snapshot {
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
        let limit = self.limits.bytes.get();
        let maximum = maximum.min(u64::from(limit));
        let snapshot = self
            .request(limit, move |store| store.describe_bounded(&handle, maximum))
            .await?
            .wait()
            .await?;
        if let Some(snapshot) = &snapshot {
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
        if maximum == 0 || maximum > CHUNK_BYTES {
            return Err(StoreError::Limit("source range"));
        }
        let limit = self.limits.bytes.get();
        self.request(limit, move |store| {
            let _hold = store
                .hold(&snapshot.handle)?
                .ok_or(StoreError::MissingValue)?;
            store.source_range(&snapshot, from, maximum)
        })
        .await?
        .wait()
        .await
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
