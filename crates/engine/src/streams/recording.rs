//! A raw recording belongs to one subscription, not to its rolling observation window.
use super::*;
use crate::storage::{
    AcquiredOutput, Acquisition, CHUNK_BYTES, LeafSelection, StoreError, StoredOutput, ValueLease,
    acquisition_error,
};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use wes_core::{
    Data,
    contracts::{Contract, ContractField, ContractRegistry, boundary, metadata::ValueMetadata},
};

/// Held by the original source owner and exact native reads. It contains no external control port.
#[derive(Debug)]
pub(crate) struct RecordedOutput {
    pub output: StoredOutput,
    _hold: ValueLease,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RecordingEnd {
    Ended,
    Cancelled,
    Limit,
    Failed,
    Unconfirmed,
}
impl RecordingEnd {
    fn name(self) -> &'static str {
        match self {
            Self::Ended => "ended",
            Self::Cancelled => "cancelled",
            Self::Limit => "limit",
            Self::Failed => "failed",
            Self::Unconfirmed => "unconfirmed",
        }
    }
}

/// A trusted provider must join every append and its external lifetime before sealing.
/// The result remains inaccessible until the enclosing subscription has physically joined.
pub struct StreamRecording {
    acquisition: Arc<Acquisition>,
    sink: StreamSink,
    maximum: u64,
    accepted: Arc<AtomicU64>,
    appending: Arc<AtomicBool>,
    poisoned: Arc<AtomicBool>,
    contract: Arc<Contract>,
    fields: Vec<String>,
}
pub type RecordingAppend =
    Pin<Box<dyn Future<Output = Result<(), InvocationError>> + Send + 'static>>;

impl StreamSink {
    /// Reserve bounded protected raw storage before dispatching the external producer.
    /// The context is supplied by the native owner; the provider cannot choose policy or placement.
    pub async fn begin_recording(
        &self,
        maximum: u64,
        contract: Arc<Contract>,
        fields: Vec<String>,
    ) -> Result<StreamRecording, InvocationError> {
        if maximum == 0 || maximum > wes_budgets::get("storage.acquisition.bytes") {
            return Err(acquisition_error(StoreError::Limit(
                "recording byte ceiling",
            )));
        }
        if fields.is_empty()
            || fields.len() > 64
            || fields.iter().any(|s| s.is_empty() || s.len() > 1024)
            || fields.iter().map(String::len).sum::<usize>() > 4096
        {
            return Err(acquisition_error(StoreError::Conflict));
        }
        let mut leaf = contract.as_ref();
        for field in &fields {
            if !leaf.constraints().enumeration.is_empty() {
                return Err(acquisition_error(StoreError::Conflict));
            }
            let wes_core::contracts::ContractKind::Record(record) = leaf.kind() else {
                return Err(acquisition_error(StoreError::Conflict));
            };
            leaf = &record
                .get(field)
                .filter(|field| !field.optional)
                .ok_or_else(|| acquisition_error(StoreError::Conflict))?
                .contract;
        }
        let constraints = leaf.constraints();
        if !matches!(
            leaf.kind(),
            wes_core::contracts::ContractKind::Scalar(wes_core::Primitive::Bytes)
        ) || !constraints.enumeration.is_empty()
            || !constraints.patterns.is_empty()
            || constraints.min.is_some()
            || constraints.max.is_some()
            || constraints.min_length.is_some()
            || constraints.max_length.is_some()
            || constraints.min_items.is_some()
            || constraints.max_items.is_some()
        {
            return Err(acquisition_error(StoreError::Conflict));
        }
        let owner = self.0.upgrade().ok_or(InvocationError::Cancelled)?;
        let context = owner
            .recording_context
            .as_ref()
            .ok_or_else(|| acquisition_error(StoreError::Restricted))?
            .clone();
        {
            let mut state = owner.state();
            if state.connection_epoch != self.1
                || state.finished
                || state.opened
                || state.recording_started
                || owner.cancellation.is_cancelled()
            {
                return Err(InvocationError::Cancelled);
            }
            state.recording_started = true;
            state.recording_preparing = true;
        }
        let acquisition = context.begin(maximum).await;
        let closed = {
            let state = owner.state();
            state.connection_epoch != self.1 || state.finished || owner.cancellation.is_cancelled()
        };
        if closed {
            if let Ok(acquisition) = acquisition {
                let _ = acquisition.abort().await;
            }
            return Err(InvocationError::Cancelled);
        }
        let acquisition = acquisition.map_err(acquisition_error)?;
        if let Err(error) = acquisition.append("raw", vec![]).await {
            let _ = acquisition.abort().await;
            return Err(acquisition_error(error));
        }
        let closed = {
            let mut state = owner.state();
            state.recording_preparing = false;
            let closed = state.connection_epoch != self.1
                || state.finished
                || owner.cancellation.is_cancelled();
            if !closed {
                state.recording_reserved = true;
            }
            closed
        };
        if closed {
            let _ = acquisition.abort().await;
            return Err(InvocationError::Cancelled);
        }
        Ok(StreamRecording {
            acquisition: Arc::new(acquisition),
            sink: self.clone(),
            maximum,
            accepted: Arc::new(AtomicU64::new(0)),
            appending: Arc::new(AtomicBool::new(false)),
            poisoned: Arc::new(AtomicBool::new(false)),
            contract,
            fields,
        })
    }
}
impl StreamRecording {
    pub fn accepted_bytes(&self) -> u64 {
        self.accepted.load(Ordering::Acquire)
    }
    /// Retain and await this operation on cancellation as well as success. One append at a time.
    /// Credit describes physical storage acknowledgement, never OS/network loss or traffic coverage.
    pub fn append(&self, bytes: Vec<u8>) -> RecordingAppend {
        let acquisition = self.acquisition.clone();
        let sink = self.sink.clone();
        let accepted = self.accepted.clone();
        let appending = self.appending.clone();
        let maximum = self.maximum;
        let poisoned = self.poisoned.clone();
        Box::pin(async move {
            let owner = sink.0.upgrade().ok_or(InvocationError::Cancelled)?;
            {
                let state = owner.state();
                if state.connection_epoch != sink.1
                    || state.finished
                    || owner.cancellation.is_cancelled()
                {
                    return Err(InvocationError::Cancelled);
                }
            }
            if bytes.is_empty()
                || bytes.len() > CHUNK_BYTES
                || accepted
                    .load(Ordering::Acquire)
                    .saturating_add(bytes.len() as u64)
                    > maximum
            {
                return Err(acquisition_error(StoreError::Limit("recording raw chunk")));
            }
            if appending.swap(true, Ordering::AcqRel) {
                return Err(acquisition_error(StoreError::Conflict));
            }
            struct Admission(Arc<AtomicBool>);
            impl Drop for Admission {
                fn drop(&mut self) {
                    self.0.store(false, Ordering::Release);
                }
            }
            let _admission = Admission(appending);
            let length = bytes.len() as u64;
            if let Err(error) = acquisition.append("raw", bytes).await {
                poisoned.store(true, Ordering::Release);
                return Err(acquisition_error(error));
            }
            accepted.fetch_add(length, Ordering::AcqRel);
            Ok(())
        })
    }
    pub async fn abort(self) -> Result<(), InvocationError> {
        let acquisition = Arc::try_unwrap(self.acquisition)
            .map_err(|_| acquisition_error(StoreError::Conflict))?;
        acquisition.abort().await.map_err(acquisition_error)
    }
    pub async fn finish(self, shell: Value, end: RecordingEnd) -> Result<(), InvocationError> {
        if self.poisoned.load(Ordering::Acquire) {
            self.abort().await?;
            return Err(acquisition_error(StoreError::Conflict));
        }
        let owner = self.sink.0.upgrade().ok_or(InvocationError::Cancelled)?;
        {
            let state = owner.state();
            if state.connection_epoch != self.sink.1
                || state.finished
                || owner.recorded.borrow().is_some()
                || (end == RecordingEnd::Ended && !state.opened)
            {
                return Err(acquisition_error(StoreError::Conflict));
            }
        }
        let acquisition = Arc::try_unwrap(self.acquisition)
            .map_err(|_| acquisition_error(StoreError::Conflict))?;
        let checked = boundary::checked_result(&self.contract, &shell, &|| false);
        let shell = match checked {
            Ok(shell) => shell,
            Err(_) => {
                let _ = acquisition.abort().await;
                return Err(acquisition_error(StoreError::Conflict));
            }
        };
        let mut leaf = shell.data();
        for field in &self.fields {
            let Data::Record(record) = leaf else {
                let _ = acquisition.abort().await;
                return Err(acquisition_error(StoreError::Conflict));
            };
            let Some(selected) = record.get(field) else {
                let _ = acquisition.abort().await;
                return Err(acquisition_error(StoreError::Conflict));
            };
            leaf = selected;
        }
        if !matches!(leaf, Data::Bytes(bytes) if bytes.is_empty()) {
            let _ = acquisition.abort().await;
            return Err(acquisition_error(StoreError::Conflict));
        }
        let registry = ContractRegistry::new();
        let text = registry.resolve("Text").expect("native text");
        let boolean = registry.resolve("Bool").expect("native bool");
        let contract = Contract::record(
            "SourceRecording",
            [
                ("source".into(), text.clone()),
                ("run".into(), text.clone()),
                ("acceptedBytes".into(), text.clone()),
                ("termination".into(), text),
                ("complete".into(), boolean),
                ("value".into(), self.contract),
            ]
            .into_iter()
            .map(|(name, contract)| {
                (
                    name,
                    ContractField {
                        contract,
                        optional: false,
                    },
                )
            })
            .collect(),
        )
        .map_err(|_| acquisition_error(StoreError::Conflict))?;
        let mut provenance = shell.provenance().inheriting(&owner.attribution);
        if end != RecordingEnd::Ended {
            provenance = provenance.cautioned(["Source recording contains only the physically acknowledged raw prefix; transport coverage is incomplete.".into()]);
        }
        let value = Value::new(
            contract.shape(),
            Data::Record(
                [
                    (
                        "source".into(),
                        Data::Text(owner.run.node().to_string().into()),
                    ),
                    ("run".into(), Data::Text(owner.run.id().to_string().into())),
                    (
                        "acceptedBytes".into(),
                        Data::Text(self.accepted.load(Ordering::Acquire).to_string().into()),
                    ),
                    ("complete".into(), Data::Bool(end == RecordingEnd::Ended)),
                    ("termination".into(), Data::Text(end.name().into())),
                    ("value".into(), shell.data().clone()),
                ]
                .into(),
            ),
            provenance,
        )
        .map_err(|_| acquisition_error(StoreError::Conflict))?
        .with_metadata(Some(ValueMetadata::capture(&contract)));
        let fields = std::iter::once("value".into()).chain(self.fields).collect();
        let output = acquisition
            .finish(
                value,
                vec![LeafSelection {
                    port: "raw".into(),
                    fields,
                    text: false,
                }],
            )
            .await?;
        let AcquiredOutput::Stored { output, hold } = output else {
            return Err(acquisition_error(StoreError::Conflict));
        };
        owner.recorded.send_replace(Some(Arc::new(RecordedOutput {
            output,
            _hold: hold,
        })));
        Ok(())
    }
}
impl StreamHandle {
    pub(crate) fn recorded(&self) -> Option<Arc<RecordedOutput>> {
        matches!(
            self.snapshot().phase,
            Phase::Ended | Phase::Failed(_) | Phase::Cancelled
        )
        .then(|| self.recorded.borrow().clone())
        .flatten()
    }
}

#[cfg(test)]
mod tests;
