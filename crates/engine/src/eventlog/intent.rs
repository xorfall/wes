//! A bounded, process-local reservation for one future source admission.
use super::{Handle, Limits, Prepared};
use crate::{driver::CancellationToken, graph::NodeId, storage::StoreWorker};
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, Ordering},
};
use tokio::sync::oneshot;
use wes_core::{contracts::ResolvedContractBundle, flow::FlowPolicy};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Phase {
    Prepared,
    Attaching,
    Attached,
    Discarded,
    Expired,
    Cancelled,
    Failed,
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
    fn fixture() -> (
        Arc<Intent>,
        oneshot::Receiver<Delivery>,
        Arc<()>,
        crate::storage::StoreWorkerTask,
    ) {
        let (worker, task) =
            crate::storage::spawn_store(NoValues, StoreWorkerLimits::default()).unwrap();
        let definition = Arc::new(());
        let registry = wes_core::contracts::ContractRegistry::new();
        let schema =
            ResolvedContractBundle::capture(registry.resolve("Int").unwrap(), Default::default())
                .unwrap();
        let (intent, receive) = Intent::prepare(
            NodeId::new("source").unwrap(),
            uuid::Uuid::new_v4().to_string(),
            definition.clone(),
            worker,
            schema,
            Default::default(),
            Limits::default(),
        );
        (intent, receive, definition, task)
    }
    async fn close(intent: Arc<Intent>, task: crate::storage::StoreWorkerTask) {
        intent.worker.shutdown().await.unwrap();
        task.join().await.unwrap();
    }
    #[tokio::test(start_paused = true)]
    async fn unused_setup_expiry_and_discard_release_only_process_local_reservations() {
        let (intent, receive, definition, task) = fixture();
        assert!(intent.reserved());
        assert_eq!(intent.discard().unwrap().phase, Phase::Discarded);
        assert!(!intent.reserved());
        assert!(intent.claim(&definition).unwrap().is_none());
        assert!(intent.discard().is_err());
        assert!(
            intent
                .receive(receive, &CancellationToken::new())
                .await
                .is_err()
        );
        assert_eq!(intent.snapshot().phase, Phase::Discarded);
        close(intent, task).await;
        let (intent, receive, definition, task) = fixture();
        tokio::time::advance(std::time::Duration::from_millis(
            wes_budgets::get("dataset.intent.ttl.ms") + 1,
        ))
        .await;
        assert!(intent.claim(&definition).is_err());
        assert_eq!(intent.snapshot().phase, Phase::Expired);
        assert!(
            intent
                .receive(receive, &CancellationToken::new())
                .await
                .is_err()
        );
        assert!(!intent.reserved());
        close(intent, task).await;
    }
    #[tokio::test]
    async fn replacement_and_competing_claims_cannot_reuse_a_prepared_source_capability() {
        let (intent, receive, _, task) = fixture();
        assert!(intent.claim(&Arc::new(())).is_err());
        assert_eq!(intent.snapshot().phase, Phase::Failed);
        assert!(
            intent
                .receive(receive, &CancellationToken::new())
                .await
                .is_err()
        );
        close(intent, task).await;
        let (intent, receive, definition, task) = fixture();
        let mut competitors = Vec::new();
        for _ in 0..8 {
            let intent = intent.clone();
            let definition = definition.clone();
            competitors.push(tokio::spawn(
                async move { intent.claim(&definition).unwrap() },
            ));
        }
        let mut accepted = Vec::new();
        for competitor in competitors {
            if let Some(claim) = competitor.await.unwrap() {
                accepted.push(claim);
            }
        }
        assert_eq!(accepted.len(), 1);
        assert!(intent.discard().is_err());
        accepted
            .pop()
            .unwrap()
            .complete(Err("Synthetic preparation refused".into()));
        assert!(
            intent
                .receive(receive, &CancellationToken::new())
                .await
                .is_err()
        );
        assert!(intent.claim(&definition).unwrap().is_none());
        close(intent, task).await;
    }
    #[tokio::test]
    async fn cancellation_after_claim_waits_for_the_entered_preparation_acknowledgement() {
        let (intent, receive, definition, task) = fixture();
        let claim = intent.claim(&definition).unwrap().unwrap();
        let token = CancellationToken::new();
        token.cancel();
        let waiting = {
            let intent = intent.clone();
            tokio::spawn(async move { intent.receive(receive, &token).await })
        };
        while !intent.cancelled() {
            tokio::task::yield_now().await;
        }
        assert!(!waiting.is_finished());
        assert!(intent.discard().is_err());
        claim.complete(Err("Entered preparation joined without dispatch".into()));
        assert!(waiting.await.unwrap().is_err());
        close(intent, task).await;
    }
    #[tokio::test]
    async fn dropped_source_admission_resolves_the_setup_without_claiming_a_join() {
        let (intent, receive, definition, task) = fixture();
        let claim = intent.claim(&definition).unwrap().unwrap();
        assert_eq!(intent.snapshot().phase, Phase::Attaching);
        drop(claim);
        assert_eq!(intent.snapshot().phase, Phase::Failed);
        let error = intent
            .receive(receive, &CancellationToken::new())
            .await
            .err()
            .unwrap();
        assert!(error.contains("without confirming"));
        intent.complete_as(Err("late completion".into()), Phase::Attached);
        assert_eq!(intent.snapshot().phase, Phase::Failed);
        close(intent, task).await;
    }
}
impl Phase {
    pub fn name(self) -> &'static str {
        match self {
            Self::Prepared => "prepared",
            Self::Attaching => "attaching",
            Self::Attached => "attached",
            Self::Discarded => "discarded",
            Self::Expired => "expired",
            Self::Cancelled => "cancelled",
            Self::Failed => "failed",
        }
    }
}
pub(crate) enum Admission {
    Attached(Prepared, Handle),
    /// The entered local writer is transferred solely to join its refused boundary.
    Refused {
        writer: Prepared,
        handle: Handle,
        reason: String,
    },
}
pub(crate) type Delivery = Result<Admission, String>;
/// One admission owns the setup until it explicitly transfers or refuses it.
/// Dropping an opening cannot leave its recording waiting for an acknowledgement.
pub(crate) struct Claim {
    intent: Arc<Intent>,
    pub required: bool,
}
impl std::ops::Deref for Claim {
    type Target = Intent;
    fn deref(&self) -> &Intent {
        &self.intent
    }
}
impl Claim {
    pub fn complete(self, delivery: Result<(Prepared, Handle), String>) {
        let phase = if delivery.is_ok() {
            Phase::Attached
        } else {
            Phase::Failed
        };
        self.intent.complete_as(
            delivery.map(|(writer, handle)| Admission::Attached(writer, handle)),
            phase,
        );
    }
    pub fn reject(self, (writer, handle): (Prepared, Handle), reason: String) {
        let _ = writer.reject_source();
        // Transfer the entered local writer to its joined lifetime owner, but
        // never describe a refused producer admission as successful attachment.
        self.intent.complete_as(
            Ok(Admission::Refused {
                writer,
                handle,
                reason,
            }),
            Phase::Failed,
        );
    }
}
impl Drop for Claim {
    fn drop(&mut self) {
        self.intent.complete_as(
            Err("Source admission ended without confirming recording preparation".into()),
            Phase::Failed,
        );
    }
}
struct State {
    phase: Phase,
    reply: Option<oneshot::Sender<Delivery>>,
}
pub(crate) struct Intent {
    pub source: NodeId,
    owner_run: String,
    definition: Arc<()>,
    pub worker: StoreWorker,
    pub schema: ResolvedContractBundle,
    pub policy: FlowPolicy,
    limits: Limits,
    expires: tokio::time::Instant,
    state: Mutex<State>,
    cancelled: AtomicBool,
}
#[derive(Clone, Debug)]
pub(crate) struct Snapshot {
    pub source: NodeId,
    pub phase: Phase,
    pub remaining_ms: u64,
}
impl Intent {
    pub fn prepare(
        source: NodeId,
        owner_run: String,
        definition: Arc<()>,
        worker: StoreWorker,
        schema: ResolvedContractBundle,
        policy: FlowPolicy,
        limits: Limits,
    ) -> (Arc<Self>, oneshot::Receiver<Delivery>) {
        let (reply, receive) = oneshot::channel();
        (
            Arc::new(Self {
                source,
                owner_run,
                definition,
                worker,
                schema,
                policy,
                limits,
                expires: tokio::time::Instant::now()
                    + std::time::Duration::from_millis(wes_budgets::get("dataset.intent.ttl.ms")),
                state: Mutex::new(State {
                    phase: Phase::Prepared,
                    reply: Some(reply),
                }),
                cancelled: AtomicBool::new(false),
            }),
            receive,
        )
    }
    pub fn snapshot(&self) -> Snapshot {
        let phase = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .phase;
        Snapshot {
            source: self.source.clone(),
            phase,
            remaining_ms: if phase == Phase::Prepared {
                self.expires
                    .saturating_duration_since(tokio::time::Instant::now())
                    .as_millis() as u64
            } else {
                0
            },
        }
    }
    pub fn reserved(&self) -> bool {
        matches!(self.snapshot().phase, Phase::Prepared | Phase::Attaching)
    }
    fn close_prepared(&self, phase: Phase) -> bool {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if state.phase != Phase::Prepared {
            return false;
        }
        state.phase = phase;
        if let Some(reply) = state.reply.take() {
            let _ = reply.send(Err(format!("Recording setup {}", phase.name())));
        }
        true
    }
    pub fn discard(&self) -> Result<Snapshot, String> {
        if !self.close_prepared(Phase::Discarded) {
            return Err("Only an unused prepared setup can be discarded; use Stop recording after attachment".into());
        }
        Ok(self.snapshot())
    }
    pub fn claim(self: &Arc<Self>, definition: &Arc<()>) -> Result<Option<Claim>, String> {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if state.phase != Phase::Prepared {
            return Ok(None);
        }
        if self.expires <= tokio::time::Instant::now() {
            state.phase = Phase::Expired;
            if let Some(reply) = state.reply.take() {
                let _ = reply.send(Err("Recording setup expired".into()));
            }
            return Err(
                "Recording setup expired before source admission; prepare again explicitly".into(),
            );
        }
        if !Arc::ptr_eq(&self.definition, definition) {
            state.phase = Phase::Failed;
            if let Some(reply) = state.reply.take() {
                let _ = reply.send(Err(
                    "Source definition changed before recording attachment".into()
                ));
            }
            return Err("Source definition changed before recording attachment".into());
        }
        state.phase = Phase::Attaching;
        Ok(Some(Claim {
            intent: self.clone(),
            required: false,
        }))
    }
    pub fn cancelled(&self) -> bool {
        self.cancelled.load(Ordering::Acquire)
    }
    pub(super) fn abandon(&self) {
        self.cancelled.store(true, Ordering::Release);
        self.close_prepared(Phase::Failed);
    }
    fn complete_as(&self, delivery: Delivery, phase: Phase) {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if state.phase != Phase::Attaching {
            return;
        }
        state.phase = phase;
        if let Some(reply) = state.reply.take() {
            let _ = reply.send(delivery);
        }
    }
    pub async fn receive(
        &self,
        mut receive: oneshot::Receiver<Delivery>,
        token: &CancellationToken,
    ) -> Delivery {
        tokio::select! {
            result = &mut receive => return result.unwrap_or_else(|_| Err("Recording admission ended without an acknowledgement".into())),
            _ = tokio::time::sleep_until(self.expires) => { self.close_prepared(Phase::Expired); },
            _ = token.cancelled() => {
                self.cancelled.store(true, Ordering::Release);
                self.close_prepared(Phase::Cancelled);
            },
        }
        // An already claimed preparation owns entered local I/O and must join it.
        receive
            .await
            .unwrap_or_else(|_| Err("Recording admission ended without an acknowledgement".into()))
    }
    pub async fn prepare_writer(
        &self,
        run: String,
        policy: &FlowPolicy,
    ) -> Result<(Prepared, Handle), String> {
        let policy = self.policy.join(policy);
        if self.cancelled() || self.worker.admit_dataset_policy(&policy).is_err() {
            return Err("Recording source admission was cancelled or is not exportable".into());
        }
        Prepared::prepare(
            self.worker.clone(),
            run,
            self.owner_run.clone(),
            self.schema.clone(),
            policy,
            self.limits,
            1,
        )
        .await
        .map_err(|e| e.to_string())
    }
}
