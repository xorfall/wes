//! Process-local ownership of exact source and recording lifetimes. Data identities grant no control.
use super::{Handle, Status};
use crate::{graph::NodeId, runtime::Run, streams::StreamHandle};
use std::{
    collections::{BTreeMap, BTreeSet},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
};
use tokio::sync::watch;

#[derive(Clone, Default)]
pub(crate) struct Owner(Arc<Mutex<State>>);
#[derive(Default)]
struct State {
    sources: BTreeMap<NodeId, (String, StreamHandle)>,
    openings: BTreeSet<NodeId>,
    recordings: BTreeMap<(NodeId, String), Arc<Recording>>,
}
/// Capacity for one admitted producer is reserved before any asynchronous writer preparation.
pub(crate) struct SourceReservation {
    owner: Owner,
    run: Run,
}
impl SourceReservation {
    pub fn install(self, handle: StreamHandle) {
        let mut state = self
            .owner
            .0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        state.openings.remove(self.run.node());
        state
            .sources
            .insert(self.run.node().clone(), (self.run.id().to_string(), handle));
    }
}
impl Drop for SourceReservation {
    fn drop(&mut self) {
        self.owner
            .0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .openings
            .remove(self.run.node());
    }
}
pub(crate) struct Recording {
    pub schema: wes_core::contracts::ResolvedContractBundle,
    pub policy: wes_core::flow::FlowPolicy,
    handle: Mutex<Option<Handle>>,
    stop: AtomicBool,
    done: watch::Sender<Option<Result<Status, String>>>,
    access: watch::Receiver<crate::storage::datasets::DatasetAccess>,
    intent: Mutex<Option<Arc<super::intent::Intent>>>,
}
pub(crate) struct RecordingLifetime(Arc<Recording>);
impl Drop for RecordingLifetime {
    fn drop(&mut self) {
        if self.0.done.borrow().is_none() {
            self.0.stop();
            if let Some(intent) = self
                .0
                .intent
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .as_ref()
            {
                intent.abandon();
            }
            self.0.finish(Err(
                "Recording lifetime ended without a physical join acknowledgement".into(),
            ));
        }
    }
}
impl Recording {
    pub fn lifetime(self: &Arc<Self>) -> RecordingLifetime {
        RecordingLifetime(self.clone())
    }
    pub fn control(&self) -> Option<crate::tasks::recording::RecordingControl> {
        let access = self.access.borrow();
        if access.closed
            || self.policy.is_private()
            || self.policy.is_unknown()
            || self
                .policy
                .dataset_reads()
                .iter()
                .any(|origin| access.withdrawn.contains(origin.dataset()))
        {
            return None;
        }
        let status = self.snapshot().ok();
        let setup = self.setup_snapshot();
        if status
            .as_ref()
            .is_some_and(|s| access.withdrawn.contains(s.reference.dataset()))
        {
            return None;
        }
        Some(crate::tasks::recording::RecordingControl {
            active: self.done.borrow().is_none(),
            status_available: status.is_some() || setup.is_some(),
            stop_available: self.done.borrow().is_none()
                && !self.stop.load(Ordering::Acquire)
                && setup.is_none(),
            discard_available: setup.is_some_and(|s| s.phase == super::intent::Phase::Prepared),
        })
    }
    pub fn setup_snapshot(&self) -> Option<super::intent::Snapshot> {
        let installed = self
            .handle
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .is_some();
        let setup = self
            .intent
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .as_ref()
            .map(|intent| intent.snapshot());
        setup.filter(|setup| {
            !installed
                || (setup.phase == super::intent::Phase::Failed && self.done.borrow().is_none())
        })
    }
    pub fn discard(&self) -> Result<super::intent::Snapshot, String> {
        self.intent
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .as_ref()
            .ok_or("Recording has no unused setup")?
            .discard()
    }

    pub fn install(&self, handle: Handle) {
        let mut current = self
            .handle
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if self.stop.load(Ordering::Acquire) {
            handle.stop();
        }
        *current = Some(handle);
    }
    pub fn stop(&self) {
        self.stop.store(true, Ordering::Release);
        if let Some(handle) = self
            .handle
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .as_ref()
        {
            handle.stop();
        }
    }
    pub fn snapshot(&self) -> Result<Status, String> {
        if let Some(done) = self.done.borrow().as_ref() {
            return done.clone();
        }
        self.handle
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .as_ref()
            .map(Handle::snapshot)
            .ok_or_else(|| "Recording preparation has not acknowledged a readable prefix".into())
    }
    pub fn finish(&self, status: Result<Status, String>) {
        self.done.send_replace(Some(status));
    }
    pub async fn joined(&self) -> Result<Status, String> {
        let mut done = self.done.subscribe();
        loop {
            if let Some(result) = done.borrow_and_update().clone() {
                return result;
            }
            if done.changed().await.is_err() {
                return Err("Recording lifetime has no join acknowledgement".into());
            }
        }
    }
}
impl Owner {
    pub fn retire_obsolete(&self, runtime: &crate::runtime::Runtime<crate::tasks::BoundTask>) {
        self.0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .recordings
            .retain(|(node, run), receipt| {
                receipt.done.borrow().is_none()
                    || runtime
                        .run_of(node)
                        .is_some_and(|current| current.as_str() == run)
            });
    }

    pub fn reserve_source(&self, run: &Run) -> Result<SourceReservation, String> {
        let mut state = self
            .0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if state.sources.len() + state.openings.len() >= crate::driver::MAX_STREAMS
            || state.sources.contains_key(run.node())
            || state.openings.contains(run.node())
        {
            return Err(
                "Source lifetime capacity is exhausted or an earlier run has not joined".into(),
            );
        }
        state.openings.insert(run.node().clone());
        Ok(SourceReservation {
            owner: self.clone(),
            run: run.clone(),
        })
    }
    pub fn remove_source(&self, run: &Run) {
        let mut state = self
            .0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if state
            .sources
            .get(run.node())
            .is_some_and(|(id, _)| id == run.id().as_str())
        {
            state.sources.remove(run.node());
        }
    }
    pub fn source(&self, node: &NodeId, run: &str) -> Result<StreamHandle, String> {
        self.0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .sources
            .get(node)
            .filter(|(id, _)| id == run)
            .map(|(_, handle)| handle.clone())
            .ok_or_else(|| {
                "The selected source run has no active owned subscription; reading never starts it"
                    .into()
            })
    }
    pub fn reserve(
        &self,
        run: &Run,
        schema: wes_core::contracts::ResolvedContractBundle,
        policy: wes_core::flow::FlowPolicy,
        access: watch::Receiver<crate::storage::datasets::DatasetAccess>,
    ) -> Result<Arc<Recording>, String> {
        let mut state = self
            .0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if state.recordings.len() >= wes_budgets::get("dataset.writer.receipts") as usize {
            return Err("Recording receipt capacity is exhausted; retire completed work before adding another recording".into());
        }
        let key = (run.node().clone(), run.id().to_string());
        if state.recordings.contains_key(&key) {
            return Err("Recording run was already admitted; it is never replayed".into());
        }
        let recording = Arc::new(Recording {
            schema,
            policy,
            handle: Mutex::new(None),
            stop: AtomicBool::new(false),
            done: watch::channel(None).0,
            access,
            intent: Mutex::new(None),
        });
        state.recordings.insert(key, recording.clone());
        Ok(recording)
    }
    pub fn reserve_start(
        &self,
        run: &Run,
        source: NodeId,
        definition: Arc<()>,
        schema: wes_core::contracts::ResolvedContractBundle,
        policy: wes_core::flow::FlowPolicy,
        worker: crate::storage::StoreWorker,
        limits: super::Limits,
    ) -> Result<
        (
            Arc<Recording>,
            Arc<super::intent::Intent>,
            tokio::sync::oneshot::Receiver<super::intent::Delivery>,
        ),
        String,
    > {
        let receipt = self.reserve(run, schema.clone(), policy.clone(), worker.dataset_access())?;
        let state = self
            .0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let pending = state
            .recordings
            .values()
            .filter(|receipt| {
                receipt
                    .intent
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .as_ref()
                    .is_some_and(|i| i.source == source && i.reserved())
            })
            .count();
        if pending >= crate::streams::MAX_ARCHIVES {
            let message = "Source recording reservation capacity is exhausted".to_owned();
            receipt.finish(Err(message.clone()));
            return Err(message);
        }
        let (intent, receive) = super::intent::Intent::prepare(
            source,
            run.id().to_string(),
            definition,
            worker,
            schema,
            policy,
            limits,
        );
        *receipt
            .intent
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(intent.clone());
        Ok((receipt, intent, receive))
    }
    pub fn claim_start(
        &self,
        source: &NodeId,
        definition: &Arc<()>,
        required: Option<&(NodeId, String)>,
    ) -> Result<Vec<super::intent::Claim>, String> {
        let state = self
            .0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let mut claims = Vec::new();
        if let Some(key) = required {
            let intent = state
                .recordings
                .get(key)
                .and_then(|receipt| {
                    receipt
                        .intent
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner)
                        .clone()
                })
                .filter(|intent| &intent.source == source)
                .ok_or_else(|| {
                    "Recording launch has no setup for this exact owned source run".to_owned()
                })?;
            // Claim under the intent lock itself. Discard/cancel never take the
            // Owner lock, so a separate phase check cannot authorize dispatch.
            let mut claim = intent.claim(definition)?.ok_or_else(|| "Recording launch requires a fresh unused setup for this exact owned run; reading or refreshing a consumed setup cannot replay a producer".to_owned())?;
            claim.required = true;
            claims.push(claim);
        }
        for (key, receipt) in &state.recordings {
            if required == Some(key) {
                continue;
            }
            let intent = receipt
                .intent
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .clone();
            if let Some(intent) = intent.filter(|i| &i.source == source) {
                match intent.claim(definition) {
                    Ok(Some(claim)) => claims.push(claim),
                    Ok(None) => {}
                    // An expired/replaced optional setup settles only itself.
                    // It owns no permission to refuse an unrelated producer.
                    Err(_) => {}
                }
            }
        }
        Ok(claims)
    }
    pub fn recording(&self, node: &NodeId, run: &str) -> Result<Arc<Recording>, String> {
        self.0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .recordings
            .get(&(node.clone(), run.to_owned()))
            .cloned()
            .ok_or_else(|| {
                "Recording has no process-local lifetime; restored results grant prefix reads only"
                    .into()
            })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::{LoadedValue, StoreError, StoreWorkerLimits, ValueHandle, ValueStore};
    struct NoValues;
    impl ValueStore for NoValues {
        fn store(&mut self, _: &wes_core::Value) -> Result<ValueHandle, StoreError> {
            Err(StoreError::Restricted)
        }
        fn read(&self, _: &ValueHandle) -> Result<Option<LoadedValue>, StoreError> {
            Ok(None)
        }
        fn encoded(&self, _: &ValueHandle) -> Result<Option<Vec<u8>>, StoreError> {
            Ok(None)
        }
        fn size(&self, _: &ValueHandle) -> Result<Option<u64>, StoreError> {
            Ok(None)
        }
        fn release(&mut self, _: &ValueHandle) -> Result<bool, StoreError> {
            Ok(false)
        }
    }
    fn setup(
        owner: &Owner,
        worker: &crate::storage::StoreWorker,
        name: &str,
        definition: Arc<()>,
    ) -> ((NodeId, String), Arc<super::super::intent::Intent>) {
        let registry = wes_core::contracts::ContractRegistry::new();
        let schema = wes_core::contracts::ResolvedContractBundle::capture(
            registry.resolve("Int").unwrap(),
            Default::default(),
        )
        .unwrap();
        let (intent, _receive) = super::super::intent::Intent::prepare(
            NodeId::new("source").unwrap(),
            uuid::Uuid::new_v4().to_string(),
            definition,
            worker.clone(),
            schema.clone(),
            Default::default(),
            super::super::Limits::default(),
        );
        let key = (NodeId::new(name).unwrap(), uuid::Uuid::new_v4().to_string());
        let receipt = Arc::new(Recording {
            schema,
            policy: Default::default(),
            handle: Mutex::new(None),
            stop: AtomicBool::new(false),
            done: watch::channel(None).0,
            access: worker.dataset_access(),
            intent: Mutex::new(Some(intent.clone())),
        });
        owner
            .0
            .lock()
            .unwrap()
            .recordings
            .insert(key.clone(), receipt);
        (key, intent)
    }
    #[tokio::test]
    async fn required_setup_claim_and_discard_have_a_single_admission_boundary() {
        let (worker, task) =
            crate::storage::spawn_store(NoValues, StoreWorkerLimits::default()).unwrap();
        let owner = Owner::default();
        let source = NodeId::new("source").unwrap();
        let definition = Arc::new(());
        let (key, intent) = setup(&owner, &worker, "discarded", definition.clone());
        intent.discard().unwrap();
        assert!(owner.claim_start(&source, &definition, Some(&key)).is_err());
        let (key, intent) = setup(&owner, &worker, "claimed", definition.clone());
        let claims = owner.claim_start(&source, &definition, Some(&key)).unwrap();
        assert_eq!(claims.len(), 1);
        assert!(claims[0].required);
        assert!(intent.discard().is_err());
        drop(claims);
        assert_eq!(intent.snapshot().phase, super::super::intent::Phase::Failed);
        worker.shutdown().await.unwrap();
        task.join().await.unwrap();
    }
    #[tokio::test(start_paused = true)]
    async fn stale_optional_setups_do_not_refuse_the_source_or_consume_a_fresh_setup() {
        let (worker, task) =
            crate::storage::spawn_store(NoValues, StoreWorkerLimits::default()).unwrap();
        let owner = Owner::default();
        let source = NodeId::new("source").unwrap();
        let definition = Arc::new(());
        let (_, expired) = setup(&owner, &worker, "a-expired", definition.clone());
        tokio::time::advance(std::time::Duration::from_millis(
            wes_budgets::get("dataset.intent.ttl.ms") + 1,
        ))
        .await;
        let (_, replaced) = setup(&owner, &worker, "b-replaced", Arc::new(()));
        let (key, valid) = setup(&owner, &worker, "c-valid", definition.clone());
        let claims = owner.claim_start(&source, &definition, Some(&key)).unwrap();
        assert_eq!(claims.len(), 1);
        assert_eq!(
            expired.snapshot().phase,
            super::super::intent::Phase::Expired
        );
        assert_eq!(
            replaced.snapshot().phase,
            super::super::intent::Phase::Failed
        );
        assert_eq!(
            valid.snapshot().phase,
            super::super::intent::Phase::Attaching
        );
        drop(claims);
        worker.shutdown().await.unwrap();
        task.join().await.unwrap();
    }
}
