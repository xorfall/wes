//! Process-local typed commands tied to one admitted source principal and connection.
use super::*;
use crate::environments::InvocationAuthority;
use std::sync::atomic::{AtomicU8, Ordering};
use tokio::sync::{OwnedSemaphorePermit, Semaphore, mpsc, oneshot};
use wes_core::contracts::Contract;

pub type ControlFuture = Pin<Box<dyn Future<Output = ControlOutcome> + Send + 'static>>;
/// Protocol confirmation and settlement are adapter semantics. Never infer success from a send.
pub enum ControlOutcome {
    Confirmed(Value),
    Refused,
    Unknown,
}
pub trait LiveController: Send + Sync + 'static {
    /// Mandatory current permission check, both at enqueue and dispatch. Bounded, synchronous,
    /// effect-free; must not call back into this source. Acquisition is not a write grant.
    fn authorize(&self, caller: &InvocationAuthority, action: &Value) -> bool;
    /// Construct without effects; the future owns physical dispatch and cleanup through completion.
    fn invoke(&self, action: Value, cancellation: CancellationToken) -> ControlFuture;
}
#[derive(Clone, Copy, Debug)]
pub struct ControlLimits {
    pub capacity: usize,
    pub bytes: u64,
}
impl Default for ControlLimits {
    fn default() -> Self {
        Self {
            capacity: 16,
            bytes: 64 * 1024,
        }
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
pub enum ControlError {
    #[error("invalid typed live control input or budget")]
    Invalid,
    #[error("live control capacity exhausted")]
    Capacity,
    #[error("live control authority refused")]
    Refused,
    #[error("live control run or connection changed")]
    Changed,
    #[error("live control expired before dispatch")]
    Expired,
    #[error("live control cancelled before dispatch")]
    Cancelled,
    #[error("live control outcome is unknown; do not replay automatically")]
    Unknown,
}
struct ControlShared {
    source: Weak<Shared>,
    run: Run,
    epoch: uuid::Uuid,
    principal: InvocationAuthority,
    adapter: Arc<dyn LiveController>,
    input: Arc<Contract>,
    output: Arc<Contract>,
    limits: ControlLimits,
    cancellation: CancellationToken,
    credits: Arc<Semaphore>,
    send: mpsc::Sender<Request>,
}
struct Request {
    action: Value,
    caller: InvocationAuthority,
    until: tokio::time::Instant,
    status: Arc<AtomicU8>,
    cancellation: CancellationToken,
    reply: oneshot::Sender<Result<Value, ControlError>>,
    _credit: OwnedSemaphorePermit,
}
/// Dropping a caller cannot allow its queued action to escape later. A dispatched action remains
/// owned by the worker and is joined even when this guard cancels its request token.
struct Abandon {
    status: Arc<AtomicU8>,
    cancellation: CancellationToken,
}
impl Drop for Abandon {
    fn drop(&mut self) {
        let _ = self
            .status
            .compare_exchange(0, 2, Ordering::AcqRel, Ordering::Acquire);
        self.cancellation.cancel();
    }
}
impl ControlShared {
    fn principal_allows(&self, caller: &InvocationAuthority) -> bool {
        caller.is_admitted() && (caller.is_local_user() || caller == &self.principal)
    }
    fn validate(&self, value: &Value, contract: &Contract) -> bool {
        value.data().is_inline()
            && crate::value_size::value_charge(value, self.limits.bytes).is_some()
            && contract
                .issues_with_cancel(value.data(), &|| self.cancellation.is_cancelled())
                .is_ok_and(|issues| issues.is_empty())
    }
    fn authority(
        &self,
        caller: &InvocationAuthority,
        action: &Value,
    ) -> Result<Arc<Shared>, ControlError> {
        if !self.principal_allows(caller) {
            return Err(ControlError::Refused);
        }
        let source = self.source.upgrade().ok_or(ControlError::Changed)?;
        {
            let state = source.state();
            if state.connection_epoch != self.epoch
                || state.phase != Phase::Open
                || state.finished
                || source.cancellation.is_cancelled()
                || self.cancellation.is_cancelled()
            {
                return Err(ControlError::Changed);
            }
            if !self.adapter.authorize(caller, action) {
                return Err(ControlError::Refused);
            }
        }
        Ok(source)
    }
}
#[derive(Clone)]
pub struct LiveControls {
    owner: Weak<ControlShared>,
    run: Run,
    epoch: uuid::Uuid,
    caller: InvocationAuthority,
}
impl LiveControls {
    /// One bounded action, no retries. Timeout/cancellation after dispatch reports Unknown while
    /// the owner keeps the physical future and credit until it actually joins.
    pub async fn invoke(
        &self,
        action: Value,
        budget: Duration,
        caller: CancellationToken,
    ) -> Result<Value, ControlError> {
        if budget.is_zero() || budget > MAX_READY_WAIT {
            return Err(ControlError::Invalid);
        }
        let owner = self.owner.upgrade().ok_or(ControlError::Changed)?;
        if owner.run != self.run || owner.epoch != self.epoch {
            return Err(ControlError::Changed);
        }
        if owner.cancellation.is_cancelled() {
            return Err(ControlError::Changed);
        }
        if !owner.validate(&action, &owner.input) {
            return Err(ControlError::Invalid);
        }
        let source = owner.authority(&self.caller, &action)?;
        let action = action
            .with_shape(owner.input.shape())
            .map_err(|_| ControlError::Invalid)?;
        let action =
            action.with_provenance(action.provenance().clone().inheriting(&source.attribution));
        drop(source);
        if !owner.validate(&action, &owner.input) {
            return Err(ControlError::Invalid);
        }
        if caller.is_cancelled() {
            return Err(ControlError::Cancelled);
        }
        let credit = owner
            .credits
            .clone()
            .try_acquire_owned()
            .map_err(|_| ControlError::Capacity)?;
        let until = tokio::time::Instant::now() + budget;
        let status = Arc::new(AtomicU8::new(0));
        let cancellation = owner.cancellation.child_token();
        let _abandon = Abandon {
            status: status.clone(),
            cancellation: cancellation.clone(),
        };
        let (reply, receive) = oneshot::channel();
        owner
            .send
            .try_send(Request {
                action,
                caller: self.caller.clone(),
                until,
                status: status.clone(),
                cancellation,
                reply,
                _credit: credit,
            })
            .map_err(|_| ControlError::Capacity)?;
        let uncertain = |before| {
            if status
                .compare_exchange(0, 2, Ordering::AcqRel, Ordering::Acquire)
                .is_ok()
                || status.load(Ordering::Acquire) == 2
            {
                before
            } else {
                ControlError::Unknown
            }
        };
        tokio::select! { biased;
            result=receive => result.unwrap_or_else(|_|Err(uncertain(ControlError::Refused))),
            ()=caller.cancelled()=>Err(uncertain(ControlError::Cancelled)),
            ()=owner.cancellation.cancelled()=>Err(uncertain(ControlError::Changed)),
            _=tokio::time::sleep_until(until)=>Err(uncertain(ControlError::Expired)),
        }
    }
}
pub(super) struct Owner {
    shared: Arc<ControlShared>,
    done: watch::Receiver<bool>,
    task: Option<JoinHandle<()>>,
}
impl Owner {
    pub(super) fn revoke(&self) {
        self.shared.cancellation.cancel();
    }
    pub(super) fn completion(&self) -> watch::Receiver<bool> {
        self.done.clone()
    }
    pub(super) async fn join(mut self) -> Result<(), tokio::task::JoinError> {
        self.revoke();
        self.task.take().expect("owned live control worker").await
    }
}
impl Drop for Owner {
    fn drop(&mut self) {
        self.revoke();
    }
}
async fn worker(owner: Arc<ControlShared>, mut receive: mpsc::Receiver<Request>) {
    loop {
        let request = tokio::select! {biased; ()=owner.cancellation.cancelled()=>break, request=receive.recv()=>match request {Some(r)=>r,None=>break}};
        let refused = (|| {
            if request.cancellation.is_cancelled() || request.reply.is_closed() {
                return Err(ControlError::Cancelled);
            }
            if tokio::time::Instant::now() >= request.until {
                return Err(ControlError::Expired);
            }
            let source = owner.authority(&request.caller, &request.action)?;
            let state = source.state();
            if state.connection_epoch != owner.epoch
                || state.phase != Phase::Open
                || source.cancellation.is_cancelled()
                || owner.cancellation.is_cancelled()
            {
                return Err(ControlError::Changed);
            }
            // Serialize the dispatch decision with connection closure. No protocol I/O under lock.
            request
                .status
                .compare_exchange(0, 1, Ordering::AcqRel, Ordering::Acquire)
                .map_err(|_| ControlError::Cancelled)?;
            Ok(())
        })();
        if let Err(error) = refused {
            let _ = request.reply.send(Err(error));
            continue;
        }
        let action = request.action;
        let provenance = action.provenance().clone();
        // Await physical work even if the request, connection or source is cancelled. No abort/replay.
        let outcome = owner
            .adapter
            .invoke(action, request.cancellation.clone())
            .await;
        let result = match outcome {
            ControlOutcome::Confirmed(value) => {
                let value =
                    value.with_provenance(value.provenance().clone().inheriting(&provenance));
                if owner.validate(&value, &owner.output) {
                    value
                        .with_shape(owner.output.shape())
                        .map_err(|_| ControlError::Unknown)
                } else {
                    Err(ControlError::Unknown)
                }
            }
            ControlOutcome::Refused => Err(ControlError::Refused),
            ControlOutcome::Unknown => Err(ControlError::Unknown),
        };
        let _ = request.reply.send(result);
    }
    while let Ok(request) = receive.try_recv() {
        let _ = request.reply.send(Err(ControlError::Changed));
    }
}
impl StreamSink {
    /// Install only after the adapter has acknowledged the connection. The source owner joins this
    /// worker before ending or reconnecting. Contracts describe the permitted protocol operations.
    pub fn install_controls(
        &self,
        input: Arc<Contract>,
        output: Arc<Contract>,
        limits: ControlLimits,
        adapter: Arc<dyn LiveController>,
    ) -> Result<(), ControlError> {
        if limits.capacity == 0
            || limits.capacity > 64
            || limits.bytes == 0
            || limits.bytes > 1024 * 1024
        {
            return Err(ControlError::Invalid);
        }
        let source = self.0.upgrade().ok_or(ControlError::Changed)?;
        let mut state = source.state();
        if state.connection_epoch != self.1
            || state.phase != Phase::Open
            || state.finished
            || source.cancellation.is_cancelled()
            || !source.principal.is_admitted()
        {
            return Err(ControlError::Refused);
        }
        if state.controls.is_some() {
            return Err(ControlError::Refused);
        }
        let (send, receive) = mpsc::channel(limits.capacity);
        let shared = Arc::new(ControlShared {
            source: Arc::downgrade(&source),
            run: source.run.clone(),
            epoch: self.1,
            principal: source.principal.clone(),
            adapter,
            input,
            output,
            limits,
            cancellation: source.cancellation.child_token(),
            credits: Arc::new(Semaphore::new(limits.capacity)),
            send,
        });
        let (done, completion) = watch::channel(false);
        let owner = shared.clone();
        let task = tokio::spawn(async move {
            worker(owner, receive).await;
            let _ = done.send(true);
        });
        state.controls = Some(Owner {
            shared,
            done: completion,
            task: Some(task),
        });
        Ok(())
    }
}
impl StreamHandle {
    pub fn live_controls(
        &self,
        expected: &Run,
        caller: InvocationAuthority,
    ) -> Result<LiveControls, ControlError> {
        let source = self.owner.upgrade().ok_or(ControlError::Changed)?;
        let state = source.state();
        let owner = state.controls.as_ref().ok_or(ControlError::Refused)?;
        if &owner.shared.run != expected
            || state.phase != Phase::Open
            || source.cancellation.is_cancelled()
            || !owner.shared.principal_allows(&caller)
        {
            return Err(ControlError::Refused);
        }
        Ok(LiveControls {
            owner: Arc::downgrade(&owner.shared),
            run: expected.clone(),
            epoch: state.connection_epoch,
            caller,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        runtime::{Effect, ExecutionTraits, Runtime},
        source::SourceInput,
    };
    use std::sync::atomic::{AtomicBool, AtomicUsize};
    use wes_core::{
        Data, Primitive, Shape,
        capability::{Capability, Safety},
        contracts::ContractRegistry,
    };
    fn value(n: i64) -> Value {
        Value::new(
            Shape::Primitive(Primitive::Int),
            Data::Int(n),
            Provenance::default(),
        )
        .unwrap()
    }
    fn actor(name: &str) -> InvocationAuthority {
        InvocationAuthority::from_source(
            &SourceInput::new("test".into(), "source".into())
                .unwrap()
                .with_client(name.into())
                .unwrap()
                .cooperative(),
        )
    }
    fn run() -> Run {
        let mut runtime = Runtime::new();
        runtime
            .add(
                (),
                [],
                ExecutionTraits {
                    pure: false,
                    repeatable: false,
                    bounded: false,
                },
            )
            .unwrap();
        runtime
            .start(Duration::ZERO)
            .into_iter()
            .find_map(|e| {
                if let Effect::Spawn(ticket) = e {
                    Some(ticket.run)
                } else {
                    None
                }
            })
            .unwrap()
    }
    struct Pending {
        action: Value,
        token: CancellationToken,
        finish: oneshot::Sender<ControlOutcome>,
    }
    struct Controller {
        gate: Arc<AtomicBool>,
        calls: Arc<AtomicUsize>,
        entered: mpsc::UnboundedSender<Pending>,
    }
    impl LiveController for Controller {
        fn authorize(&self, _: &InvocationAuthority, _: &Value) -> bool {
            self.gate.load(Ordering::Acquire)
        }
        fn invoke(&self, action: Value, token: CancellationToken) -> ControlFuture {
            let entered = self.entered.clone();
            let calls = self.calls.clone();
            Box::pin(async move {
                calls.fetch_add(1, Ordering::AcqRel);
                let (finish, receive) = oneshot::channel();
                entered
                    .send(Pending {
                        action,
                        token,
                        finish,
                    })
                    .unwrap();
                receive.await.unwrap_or(ControlOutcome::Unknown)
            })
        }
    }
    struct Source(mpsc::UnboundedSender<(StreamSink, oneshot::Sender<()>)>);
    impl StreamingInvoker for Source {
        fn subscribe(&self, _: Call, sink: StreamSink, _: CancellationToken) -> StreamFuture {
            sink.opened().unwrap();
            let (finish, receive) = oneshot::channel();
            self.0.send((sink, finish)).unwrap();
            Box::pin(async move {
                let _ = receive.await;
                Ok(())
            })
        }
    }
    struct Fixture {
        handle: StreamHandle,
        task: StreamTask,
        sink: StreamSink,
        finish: oneshot::Sender<()>,
        controls: LiveControls,
        gate: Arc<AtomicBool>,
        calls: Arc<AtomicUsize>,
        entered: mpsc::UnboundedReceiver<Pending>,
        run: Run,
        adapter: Arc<Controller>,
    }
    async fn fixture(capacity: usize) -> Fixture {
        let run = run();
        let caller = actor("source-owner");
        let (entered, mut receive) = mpsc::unbounded_channel();
        let mut capability =
            Capability::new(["observe"], Shape::Primitive(Primitive::Int), Safety::Safe);
        capability.streaming = true;
        let call = Call {
            run: run.clone(),
            authority: caller.clone(),
            capability: Arc::new(capability),
            arguments: Default::default(),
        };
        let (handle, task) = spawn(
            call,
            Arc::new(Source(entered)),
            Provenance::default(),
            Limits::default(),
            CancellationToken::new(),
        )
        .unwrap();
        let (sink, finish) = receive.recv().await.unwrap();
        let gate = Arc::new(AtomicBool::new(true));
        let calls = Arc::new(AtomicUsize::new(0));
        let (entered, receive) = mpsc::unbounded_channel();
        let adapter = Arc::new(Controller {
            gate: gate.clone(),
            calls: calls.clone(),
            entered,
        });
        let contract = ContractRegistry::new().resolve("Int").unwrap();
        sink.install_controls(
            contract.clone(),
            contract,
            ControlLimits {
                capacity,
                bytes: 4096,
            },
            adapter.clone(),
        )
        .unwrap();
        let controls = handle.live_controls(&run, caller).unwrap();
        Fixture {
            handle,
            task,
            sink,
            finish,
            controls,
            gate,
            calls,
            entered: receive,
            run,
            adapter,
        }
    }
    async fn queued(controls: &LiveControls, count: usize) {
        for _ in 0..100 {
            if controls
                .owner
                .upgrade()
                .unwrap()
                .credits
                .available_permits()
                == count
            {
                return;
            }
            tokio::task::yield_now().await;
        }
        panic!("request was not enqueued");
    }
    #[tokio::test]
    async fn contracts_capacity_principal_and_current_permission_are_checked_without_dispatch() {
        let mut f = fixture(2).await;
        assert!(matches!(
            f.handle
                .live_controls(&f.run, InvocationAuthority::default()),
            Err(ControlError::Refused)
        ));
        assert!(matches!(
            f.handle.live_controls(&f.run, actor("other-owner")),
            Err(ControlError::Refused)
        ));
        assert!(matches!(
            f.handle.live_controls(&run(), actor("source-owner")),
            Err(ControlError::Refused)
        ));
        let bad = Value::new(
            Shape::Primitive(Primitive::Text),
            Data::Text("bad".into()),
            Provenance::default(),
        )
        .unwrap();
        assert_eq!(
            f.controls
                .invoke(bad, Duration::from_secs(2), CancellationToken::new())
                .await,
            Err(ControlError::Invalid)
        );
        let large = value(1).with_provenance(Provenance::default().cautioned(["x".repeat(8192)]));
        assert_eq!(
            f.controls
                .invoke(large, Duration::from_secs(2), CancellationToken::new())
                .await,
            Err(ControlError::Invalid)
        );
        let secret = value(1).with_provenance(
            Provenance::default().with_policy(&wes_core::flow::FlowPolicy::default().private()),
        );
        let first = tokio::spawn({
            let control = f.controls.clone();
            async move {
                control
                    .invoke(secret, Duration::from_secs(10), CancellationToken::new())
                    .await
            }
        });
        let pending = f.entered.recv().await.unwrap();
        assert!(pending.action.provenance().policy().is_private());
        let second = tokio::spawn({
            let control = f.controls.clone();
            async move {
                control
                    .invoke(value(2), Duration::from_secs(10), CancellationToken::new())
                    .await
            }
        });
        queued(&f.controls, 0).await;
        assert_eq!(
            f.controls
                .invoke(value(3), Duration::from_secs(2), CancellationToken::new())
                .await,
            Err(ControlError::Capacity)
        );
        f.gate.store(false, Ordering::Release);
        pending
            .finish
            .send(ControlOutcome::Confirmed(value(1)))
            .ok()
            .unwrap();
        assert!(
            first
                .await
                .unwrap()
                .unwrap()
                .provenance()
                .policy()
                .is_private()
        );
        assert_eq!(second.await.unwrap(), Err(ControlError::Refused));
        assert_eq!(f.calls.load(Ordering::Acquire), 1);
        f.finish.send(()).unwrap();
        f.task.join().await.unwrap();
    }
    #[tokio::test(start_paused = true)]
    async fn dispatched_timeout_is_unknown_and_abandoned_queue_never_escapes_or_releases_physical_credit()
     {
        let mut f = fixture(2).await;
        let first = tokio::spawn({
            let control = f.controls.clone();
            async move {
                control
                    .invoke(value(1), Duration::from_secs(1), CancellationToken::new())
                    .await
            }
        });
        let pending = f.entered.recv().await.unwrap();
        let second = tokio::spawn({
            let control = f.controls.clone();
            async move {
                control
                    .invoke(value(2), Duration::from_secs(10), CancellationToken::new())
                    .await
            }
        });
        queued(&f.controls, 0).await;
        second.abort();
        let _ = second.await;
        tokio::time::advance(Duration::from_secs(1)).await;
        assert_eq!(first.await.unwrap(), Err(ControlError::Unknown));
        assert!(pending.token.is_cancelled());
        assert_eq!(
            f.controls
                .invoke(value(3), Duration::from_secs(2), CancellationToken::new())
                .await,
            Err(ControlError::Capacity)
        );
        f.finish.send(()).unwrap();
        let joined = tokio::spawn(f.task.join());
        tokio::task::yield_now().await;
        assert!(!joined.is_finished());
        pending
            .finish
            .send(ControlOutcome::Confirmed(value(1)))
            .ok()
            .unwrap();
        joined.await.unwrap().unwrap();
        assert_eq!(f.calls.load(Ordering::Acquire), 1);
        assert!(f.entered.try_recv().is_err());
    }
    #[tokio::test(start_paused = true)]
    async fn queued_deadline_is_a_known_refusal_and_live_user_still_needs_current_permission() {
        let mut f = fixture(2).await;
        let user = InvocationAuthority::from_source(
            &SourceInput::new("user".into(), "source".into()).unwrap(),
        );
        let local = f.handle.live_controls(&f.run, user).unwrap();
        let first = tokio::spawn({
            let control = local.clone();
            async move {
                control
                    .invoke(value(1), Duration::from_secs(60), CancellationToken::new())
                    .await
            }
        });
        let pending = f.entered.recv().await.unwrap();
        let second = tokio::spawn({
            let control = f.controls.clone();
            async move {
                control
                    .invoke(value(2), Duration::from_secs(1), CancellationToken::new())
                    .await
            }
        });
        queued(&f.controls, 0).await;
        tokio::time::advance(Duration::from_secs(1)).await;
        assert_eq!(second.await.unwrap(), Err(ControlError::Expired));
        pending
            .finish
            .send(ControlOutcome::Confirmed(value(1)))
            .ok()
            .unwrap();
        first.await.unwrap().unwrap();
        queued(&f.controls, 2).await;
        f.gate.store(false, Ordering::Release);
        assert_eq!(
            local
                .invoke(value(3), Duration::from_secs(2), CancellationToken::new())
                .await,
            Err(ControlError::Refused)
        );
        assert_eq!(f.calls.load(Ordering::Acquire), 1);
        f.finish.send(()).unwrap();
        f.task.join().await.unwrap();
    }
    #[tokio::test]
    async fn reconnect_joins_old_controls_and_revokes_callbacks_and_ready_receipts() {
        let mut f = fixture(1).await;
        let archive = archive::Branch::prepare().unwrap();
        f.handle.attach_archive(archive.branch.clone()).unwrap();
        let receipt = f
            .handle
            .wait_ready(&f.run, Duration::from_secs(2), CancellationToken::new())
            .await
            .unwrap();
        let first = tokio::spawn({
            let control = f.controls.clone();
            async move {
                control
                    .invoke(value(1), Duration::from_secs(10), CancellationToken::new())
                    .await
            }
        });
        let pending = f.entered.recv().await.unwrap();
        let reconnect = tokio::spawn({
            let sink = f.sink.clone();
            async move { sink.reopening().await }
        });
        pending.token.cancelled().await;
        assert_eq!(
            archive.status.borrow().end,
            Some(archive::End::SourceFailed)
        );
        assert!(!archive.branch.is_accepting());
        assert!(!reconnect.is_finished());
        assert_eq!(first.await.unwrap(), Err(ControlError::Unknown));
        assert_eq!(
            receipt.admit(&f.run, || panic!("stale receipt")),
            Err::<(), _>(ReadyError::Changed)
        );
        assert_eq!(f.sink.push(value(99)), Err(StreamError::Closed));
        assert_eq!(f.sink.opened(), Err(StreamError::Closed));
        assert_eq!(f.sink.reject_item(), Err(StreamError::Closed));
        pending
            .finish
            .send(ControlOutcome::Confirmed(value(1)))
            .ok()
            .unwrap();
        let next = reconnect.await.unwrap().unwrap();
        next.opened().unwrap();
        let ready = f
            .handle
            .wait_ready(&f.run, Duration::from_secs(2), CancellationToken::new())
            .await
            .unwrap();
        assert_eq!(ready.admit(&f.run, || 7), Ok(7));
        let contract = ContractRegistry::new().resolve("Int").unwrap();
        next.install_controls(
            contract.clone(),
            contract,
            ControlLimits::default(),
            f.adapter,
        )
        .unwrap();
        assert!(
            f.controls
                .invoke(value(2), Duration::from_secs(2), CancellationToken::new())
                .await
                .is_err()
        );
        let mut updates = f.handle.subscribe();
        while updates.borrow_and_update().phase != Phase::Open {
            updates.changed().await.unwrap();
        }
        assert!(
            f.handle
                .snapshot()
                .window
                .provenance()
                .cautions()
                .iter()
                .any(|c| c.contains("Connection gap"))
        );
        assert_eq!(f.calls.load(Ordering::Acquire), 1);
        f.finish.send(()).unwrap();
        f.task.join().await.unwrap();
    }
    #[tokio::test]
    async fn malformed_confirmation_after_dispatch_is_unknown_and_is_never_replayed() {
        let mut f = fixture(1).await;
        let call = tokio::spawn({
            let control = f.controls.clone();
            async move {
                control
                    .invoke(value(1), Duration::from_secs(2), CancellationToken::new())
                    .await
            }
        });
        let pending = f.entered.recv().await.unwrap();
        let bad = Value::new(
            Shape::Primitive(Primitive::Text),
            Data::Text("wrong".into()),
            Provenance::default(),
        )
        .unwrap();
        pending
            .finish
            .send(ControlOutcome::Confirmed(bad))
            .ok()
            .unwrap();
        assert_eq!(call.await.unwrap(), Err(ControlError::Unknown));
        assert_eq!(f.calls.load(Ordering::Acquire), 1);
        f.finish.send(()).unwrap();
        f.task.join().await.unwrap();
    }
}
