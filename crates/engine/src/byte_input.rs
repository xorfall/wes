//! Native original-byte ownership. Public values cannot manufacture this input port.
use crate::{
    driver::CancellationToken,
    graph::{OutputPort, OutputRef},
    provider_reads::Readers,
    providers::InvocationError,
    runtime::OutputState,
    storage::{CHUNK_BYTES, SourceRange, StoreError, StoreWorker, StoredSnapshot, ValueLease},
    workspace::Workspace,
};
use wes_core::{Data, Provenance, Value};

#[derive(Clone, Debug)]
pub struct ByteInputSpec {
    pub parameter: String,
    pub maximum: u64,
}
#[derive(Clone, Debug)]
pub(crate) enum Source {
    Resident(Value),
    Stored(StoredSnapshot),
}
#[derive(Clone, Debug)]
pub(crate) struct CapturedSource {
    pub source: Source,
    pub fields: Vec<String>,
    pub node: String,
    pub run: Option<String>,
    pub revision: u64,
    pub provenance: Provenance,
    readers: Readers,
    epoch: String,
}
impl CapturedSource {
    pub(crate) fn capture(
        workspace: &Workspace,
        output: &OutputRef,
        fields: Vec<String>,
    ) -> Result<Self, StoreError> {
        let (mut source, run, revision, provenance) = match workspace.runtime().output(output) {
            OutputState::Available(value) => {
                let provenance = value.provenance().clone();
                let run = if output.port == OutputPort::Data {
                    workspace.runtime().value_run(&output.node)
                } else {
                    workspace.runtime().run_of(&output.node)
                }
                .map(ToString::to_string);
                (
                    Source::Resident(value),
                    run,
                    workspace.runtime().display_revision(&output.node),
                    provenance,
                )
            }
            OutputState::Stored(captured) => {
                let provenance = captured.snapshot.provenance.clone();
                (
                    Source::Stored(captured.snapshot),
                    captured.run.map(|r| r.to_string()),
                    captured.revision,
                    provenance,
                )
            }
            _ if output.port == OutputPort::Data => {
                let evidence = workspace
                    .runtime()
                    .evidence_value(&output.node)
                    .ok_or(StoreError::MissingValue)?;
                let provenance = evidence
                    .value
                    .provenance()
                    .clone()
                    .with_fact("wes.observation.state", "stopped")
                    .with_fact("wes.observation.run", evidence.run.as_str())
                    .with_fact("wes.observation.source", evidence.source.as_str());
                (
                    Source::Resident(evidence.value.clone()),
                    Some(evidence.run.to_string()),
                    workspace.runtime().display_revision(&output.node),
                    provenance,
                )
            }
            _ => return Err(StoreError::MissingValue),
        };
        if workspace.dataset_access.blocks_policy(provenance.policy()) {
            return Err(StoreError::ProviderReadUnavailable);
        }
        // Materializing a result must not discard its native backing identity. Only the
        // acknowledged runtime owner can supply it; matching provenance on a copy cannot.
        if output.port == OutputPort::Data
            && matches!(source, Source::Resident(_))
            && let Some(stored) = workspace.runtime().stored_output(&output.node)
            && stored.run.as_ref().map(ToString::to_string) == run
            && stored.snapshot.provenance == provenance
        {
            source = Source::Stored(stored.snapshot);
        }
        let readers = workspace.dataset_access.providers.clone();
        let epoch = readers.epoch();
        Ok(Self {
            source,
            fields,
            node: output.node.to_string(),
            run,
            revision,
            provenance,
            readers,
            epoch,
        })
    }
    pub(crate) fn literal(workspace: &Workspace, value: Value) -> Result<Self, StoreError> {
        if workspace
            .dataset_access
            .blocks_policy(value.provenance().policy())
        {
            return Err(StoreError::ProviderReadUnavailable);
        }
        let readers = workspace.dataset_access.providers.clone();
        Ok(Self {
            provenance: value.provenance().clone(),
            source: Source::Resident(value),
            fields: vec![],
            node: String::new(),
            run: None,
            revision: 0,
            epoch: readers.epoch(),
            readers,
        })
    }
    pub(crate) fn check(&self) -> Result<(), StoreError> {
        let policy = self.provenance.policy();
        if !policy.provider_reads().is_empty() && self.epoch != self.readers.epoch() {
            return Err(StoreError::ProviderReadChanged);
        }
        if self.readers.blocks_policy(policy) {
            return Err(StoreError::ProviderReadUnavailable);
        }
        Ok(())
    }
    pub(crate) async fn publish_derived(
        &self,
        worker: &StoreWorker,
        value: Value,
    ) -> Result<crate::storage::AcquiredOutput, InvocationError> {
        self.check().map_err(refusal)?;
        let Source::Stored(snapshot) = &self.source else {
            return Ok(crate::storage::AcquiredOutput::Value(value));
        };
        if value.provenance().policy().is_private() {
            return Ok(crate::storage::AcquiredOutput::Value(value));
        }
        let mut selection = snapshot.selection.clone();
        selection.extend(self.fields.clone());
        let provenance = value
            .provenance()
            .clone()
            .with_fact("wes.byteInput.source", self.node.clone())
            .with_fact("wes.byteInput.run", self.run.clone().unwrap_or_default())
            .with_fact("wes.byteInput.revision", self.revision.to_string())
            .with_fact("wes.byteInput.handle", snapshot.handle.to_string())
            .with_fact("wes.byteInput.digest", snapshot.digest.clone())
            .with_fact(
                "wes.byteInput.selection",
                serde_json::to_string(&selection).map_err(|_| refusal(StoreError::Corrupt))?,
            );
        let value = value.with_provenance(provenance.clone());
        let worker = worker.for_derivation().map_err(refusal)?;
        let context = crate::storage::AcquisitionContext::new(worker, provenance)
            .with_dependencies(vec![crate::storage::SnapshotDependency {
                name: "input".into(),
                selection: selection.clone(),
                handle: snapshot.handle.to_string(),
                digest: snapshot.digest.clone(),
            }]);
        let acquisition = context.begin(1).await.map_err(refusal)?;
        let output = acquisition.finish(value, vec![]).await?;
        self.check().map_err(refusal)?;
        Ok(output)
    }
    pub(crate) async fn range(
        &self,
        storage: Option<&StoreWorker>,
        from: u64,
        limit: usize,
    ) -> Result<SourceRange, StoreError> {
        self.check()?;
        if limit == 0 || limit > CHUNK_BYTES {
            return Err(StoreError::Limit("source range"));
        }
        let result = match &self.source {
            Source::Resident(value) => resident_range(value, &self.fields, from, limit),
            Source::Stored(snapshot) => match storage {
                Some(worker) => {
                    worker
                        .source_range_selected(snapshot.clone(), self.fields.clone(), from, limit)
                        .await
                }
                None => Err(StoreError::Closed),
            },
        };
        self.check()?;
        result
    }
    pub(crate) async fn open(
        self,
        storage: Option<StoreWorker>,
        maximum: u64,
    ) -> Result<OwnedByteInput, InvocationError> {
        self.check().map_err(refusal)?;
        let holds = match &self.source {
            Source::Stored(snapshot) => storage
                .as_ref()
                .ok_or(StoreError::Closed)
                .map_err(refusal)?
                .hold_snapshots(vec![snapshot.clone()])
                .await
                .map_err(refusal)?,
            _ => vec![],
        };
        let first = self
            .range(storage.as_ref(), 0, CHUNK_BYTES)
            .await
            .map_err(refusal)?;
        if first.text {
            return Err(refusal(StoreError::SourceRange(
                "byte input requires Bytes",
            )));
        }
        if maximum == 0 || first.extent > maximum {
            return Err(refusal(StoreError::Limit("declared byte input")));
        }
        if first.bytes.len() as u64 > first.extent || (first.bytes.is_empty() && first.extent != 0)
        {
            return Err(refusal(StoreError::Corrupt));
        }
        Ok(OwnedByteInput {
            extent: first.extent,
            first: Some(first.bytes),
            offset: 0,
            source: self,
            storage,
            _holds: holds,
        })
    }
    /// Independent finite-execution guard, including withdrawal while the child stops reading.
    pub(crate) async fn guard(self, token: CancellationToken) {
        let mut changes = self.readers.changes();
        loop {
            self.readers.expire();
            if self.check().is_err() {
                token.cancel();
                return;
            }
            let deadline = self.readers.deadline();
            tokio::select! {
                _ = token.cancelled() => return,
                changed = changes.changed() => if changed.is_err() {token.cancel(); return;},
                _ = async {match deadline {
                    Some(at) => tokio::time::sleep_until(tokio::time::Instant::from_std(at)).await,
                    None => std::future::pending().await,
                }} => {},
            }
        }
    }
}
pub(crate) fn resident_range(
    value: &Value,
    fields: &[String],
    from: u64,
    maximum: usize,
) -> Result<SourceRange, StoreError> {
    let mut data = value.data();
    for field in fields {
        let Data::Record(record) = data else {
            return Err(StoreError::SourceRange("select: requires record fields"));
        };
        data = record
            .get(field)
            .ok_or(StoreError::SourceRange("selected field is unavailable"))?;
    }
    let (bytes, text) = match data {
        Data::Bytes(bytes) => (bytes.as_ref(), false),
        Data::Text(text) => (text.as_bytes(), true),
        _ => {
            return Err(StoreError::SourceRange(
                "selected source must be Text or Bytes",
            ));
        }
    };
    let from = usize::try_from(from)
        .ok()
        .filter(|n| *n <= bytes.len())
        .ok_or(StoreError::SourceRange(
            "from: exceeds the original source extent",
        ))?;
    Ok(SourceRange {
        bytes: bytes[from..from.saturating_add(maximum).min(bytes.len())].to_vec(),
        extent: bytes.len() as u64,
        text,
    })
}
fn refusal(error: StoreError) -> InvocationError {
    InvocationError::Failed(
        error.read_error("Original byte input refused; no producer was replayed"),
    )
}
/// Trusted adapter port, bounded by its captured native source and declared ceiling.
/// It is neither serializable nor constructible from a caller-supplied handle or policy.
pub struct OwnedByteInput {
    source: CapturedSource,
    storage: Option<StoreWorker>,
    extent: u64,
    offset: u64,
    first: Option<Vec<u8>>,
    _holds: Vec<ValueLease>,
}
impl OwnedByteInput {
    pub(crate) fn retain_owner(&self) -> Vec<ValueLease> {
        self._holds.clone()
    }
    pub fn provenance(&self) -> &Provenance {
        &self.source.provenance
    }
    pub fn extent(&self) -> u64 {
        self.extent
    }
    pub fn check(&self) -> Result<(), InvocationError> {
        self.source.check().map_err(refusal)
    }
    pub async fn next(&mut self) -> Result<Option<Vec<u8>>, InvocationError> {
        self.check()?;
        let bytes = if let Some(bytes) = self.first.take() {
            bytes
        } else {
            if self.offset == self.extent {
                return Ok(None);
            }
            let range = self
                .source
                .range(self.storage.as_ref(), self.offset, CHUNK_BYTES)
                .await
                .map_err(refusal)?;
            if range.text || range.extent != self.extent {
                return Err(refusal(StoreError::Conflict));
            }
            range.bytes
        };
        if bytes.len() > CHUNK_BYTES
            || self
                .offset
                .checked_add(bytes.len() as u64)
                .is_none_or(|next| next > self.extent)
        {
            return Err(refusal(StoreError::Corrupt));
        }
        if bytes.is_empty() {
            return if self.offset == self.extent {
                Ok(None)
            } else {
                Err(refusal(StoreError::Corrupt))
            };
        }
        self.offset += bytes.len() as u64;
        self.check()?;
        Ok(Some(bytes))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        credentials::material::ChangeObservers,
        provider_reads::{Changes, ReadAuthority},
        providers::LocalScope,
    };
    use std::sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    };
    use wes_core::{
        Primitive, Shape,
        flow::{FlowPolicy, ProviderReadOrigin},
    };
    struct Reader {
        realm: String,
        revision: AtomicU64,
        observers: ChangeObservers,
    }
    impl ReadAuthority for Reader {
        fn identity(&self) -> String {
            self.realm.clone()
        }
        fn realm(&self) -> Option<String> {
            Some(self.realm.clone())
        }
        fn closed(&self) -> bool {
            false
        }
        fn revision(&self) -> String {
            self.revision.load(Ordering::Acquire).to_string()
        }
        fn allows(&self, _: &ProviderReadOrigin) -> bool {
            self.revision.load(Ordering::Acquire) % 2 == 0
        }
        fn observe(&self, observer: std::sync::Weak<Changes>) -> Result<(), &'static str> {
            self.observers
                .register(observer)
                .map_err(|_| "observer unavailable")
        }
    }
    #[tokio::test]
    async fn revoke_regrant_cannot_revive_a_captured_input_and_withdrawal_wakes_its_guard() {
        let workspace = Workspace::local(LocalScope::new("synthetic-input").unwrap());
        let reader = Arc::new(Reader {
            realm: uuid::Uuid::new_v4().to_string(),
            revision: AtomicU64::new(0),
            observers: Default::default(),
        });
        workspace
            .dataset_access
            .providers
            .register(reader.clone())
            .unwrap();
        let origin =
            ProviderReadOrigin::new(reader.realm.clone(), "a".repeat(64), "b".repeat(64)).unwrap();
        let value = Value::new(
            Shape::Primitive(Primitive::Bytes),
            Data::Bytes(vec![83; CHUNK_BYTES + 10].into()),
            Provenance::default().with_policy(&FlowPolicy::default().with_provider_read(origin)),
        )
        .unwrap();
        let source = CapturedSource::literal(&workspace, value).unwrap();
        let token = CancellationToken::new();
        let guard = tokio::spawn(source.clone().guard(token.clone()));
        let mut input = source.open(None, CHUNK_BYTES as u64 + 10).await.unwrap();
        assert_eq!(input.next().await.unwrap().unwrap().len(), CHUNK_BYTES);
        reader.revision.store(1, Ordering::Release);
        reader.observers.changed();
        reader.revision.store(2, Ordering::Release);
        reader.observers.changed();
        assert!(input.next().await.is_err());
        tokio::time::timeout(std::time::Duration::from_secs(1), token.cancelled())
            .await
            .unwrap();
        guard.await.unwrap();
    }
    #[tokio::test]
    async fn preflight_rejects_text_and_oversized_input_without_consumption() {
        let workspace = Workspace::local(LocalScope::new("synthetic-input").unwrap());
        for (shape, data, max) in [
            (
                Shape::Primitive(Primitive::Text),
                Data::Text("synthetic".into()),
                100,
            ),
            (
                Shape::Primitive(Primitive::Bytes),
                Data::Bytes(vec![0; 100].into()),
                99,
            ),
        ] {
            let source = CapturedSource::literal(
                &workspace,
                Value::new(shape, data, Default::default()).unwrap(),
            )
            .unwrap();
            assert!(source.open(None, max).await.is_err());
        }
        let source = CapturedSource::literal(
            &workspace,
            Value::new(
                Shape::Primitive(Primitive::Bytes),
                Data::Bytes(vec![].into()),
                Default::default(),
            )
            .unwrap(),
        )
        .unwrap();
        assert!(
            source
                .open(None, 1)
                .await
                .unwrap()
                .next()
                .await
                .unwrap()
                .is_none()
        );
    }
}
