//! An owned stream lifetime and bounded, coalesced observation window.
//!
//! This port is distinct from finite invocation. The caller must authorize/validate a call before
//! spawning it, retain its task, and join shutdown. Generic runtime/provider integration is separate
//! from this port; constructing it alone is not durable command admission.
use crate::{
    driver::CancellationToken,
    providers::{Call, InvocationError},
    runtime::{Run, RuntimeCode},
};
use std::{
    future::Future,
    pin::Pin,
    sync::{Arc, Mutex, Weak},
    time::Duration,
};
use tokio::{
    sync::{Notify, watch},
    task::JoinHandle,
};
use wes_core::{ErrorValue, Provenance, Value};
pub(crate) mod archive;
pub(crate) const MAX_ARCHIVES: usize = 4;
pub(crate) mod delivery;
mod window;
pub use window::Limits;
use window::Window;

pub type StreamFuture = Pin<Box<dyn Future<Output = Result<(), InvocationError>> + Send + 'static>>;

/// Call `opened` when ready and push items through the supplied run-bound sink. Completion of the
/// returned future means physical local work has ended, including cancellation cleanup. The method
/// constructing that future must not block the async scheduler. No detached delivery tasks.
/// Synchronous delivery before `opened` is accepted into the already installed window.
pub trait StreamingInvoker: Send + Sync + 'static {
    fn subscribe(
        &self,
        call: Call,
        sink: StreamSink,
        cancellation: CancellationToken,
    ) -> StreamFuture;
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Phase {
    Opening,
    Open,
    /// Cancellation requested; the provider has not yet joined. Never a physical-close receipt.
    Closing,
    Ended,
    Failed(ErrorValue),
    Cancelled,
}
#[derive(Clone, Debug)]
pub struct Snapshot {
    pub run: Run,
    pub phase: Phase,
    pub window: Value,
    /// Oldest items omitted by the configured rolling-window bounds, not delivery acknowledgements.
    pub omitted: u64,
    /// Invalid provider items skipped before entering the rolling window.
    pub rejected: u64,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
pub enum StreamError {
    #[error("stream limits or metadata are invalid")]
    Invalid,
    #[error("stream item exceeds the window's logical value budget")]
    Capacity,
    #[error("the stream has already opened")]
    AlreadyOpen,
    #[error("stream backpressure exceeded its waiting budget")]
    Overloaded,
    #[error("the stream no longer accepts items")]
    Closed,
}
struct State {
    window: Window,
    phase: Phase,
    opened: bool,
    finished: bool,
    dirty: bool,
    problem: Option<ErrorValue>,
    rejected: u64,
    sequence: u64,
    archives: Vec<Arc<archive::Branch>>,
}
struct Shared {
    state: Mutex<State>,
    cancellation: CancellationToken,
    changed: Notify,
    delivery: Option<Arc<delivery::Sender>>,
    archive_ingress: Arc<delivery::Sender>,
    attribution: Provenance,
}
impl Shared {
    fn state(&self) -> std::sync::MutexGuard<'_, State> {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }
}

/// A weak capability for precisely one subscription. It never looks up a mutable node-name map.
/// Keeping a late callback alive cannot retain its old window or write into a replacement stream.
#[derive(Clone)]
pub struct StreamSink(Weak<Shared>);
impl StreamSink {
    pub fn opened(&self) -> Result<(), StreamError> {
        let shared = self.0.upgrade().ok_or(StreamError::Closed)?;
        let mut state = shared.state();
        if state.finished || shared.cancellation.is_cancelled() {
            return Err(StreamError::Closed);
        }
        if state.opened {
            return Err(StreamError::AlreadyOpen);
        }
        state.opened = true;
        state.phase = Phase::Open;
        state.dirty = true;
        shared.changed.notify_one();
        Ok(())
    }
    /// Record a malformed provider item without retaining raw payload/error text. The provider may
    /// continue with later items; this count is distinct from ordinary oldest-window eviction.
    pub fn reject_item(&self) -> Result<(), StreamError> {
        let shared = self.0.upgrade().ok_or(StreamError::Closed)?;
        let mut state = shared.state();
        if state.finished || shared.cancellation.is_cancelled() {
            return Err(StreamError::Closed);
        }
        let Some(count) = state.rejected.checked_add(1) else {
            state.problem = Some(RuntimeCode::ExecutionFailed.error(
                "The stream rejected-item counter was exhausted; local cancellation was requested.",
                None,
            ));
            shared.cancellation.cancel();
            return Err(StreamError::Capacity);
        };
        state.rejected = count;
        for branch in &state.archives {
            branch.close(archive::End::Rejected);
        }
        if shared.delivery.is_some() {
            state.problem = Some(RuntimeCode::ExecutionFailed.error("The ordered stream rejected a provider event; processing stopped without silent loss.", None));
            shared.cancellation.cancel();
        }
        state.dirty = true;
        Ok(())
    }
    /// Bounded synchronous value accounting; no provider or filesystem I/O. Overflow of the rolling
    /// window omits oldest items explicitly. An individually oversized item instead closes admission
    /// and requests cancellation; an ignored return value cannot hide that terminal problem.
    pub fn push(&self, value: Value) -> Result<(), StreamError> {
        let shared = self.0.upgrade().ok_or(StreamError::Closed)?;
        let value =
            value.with_provenance(value.provenance().clone().inheriting(&shared.attribution));
        let has_archive = !shared.state().archives.is_empty();
        let credit = shared
            .delivery
            .as_ref()
            .or(has_archive.then_some(&shared.archive_ingress))
            .map(|d| d.reserve(&value))
            .transpose();
        self.admit(&shared, value, credit)
    }
    /// Await bounded computational capacity; observation-only streams retain rolling-window behavior.
    pub async fn send(&self, value: Value) -> Result<(), StreamError> {
        let shared = self.0.upgrade().ok_or(StreamError::Closed)?;
        let value =
            value.with_provenance(value.provenance().clone().inheriting(&shared.attribution));
        let has_archive = !shared.state().archives.is_empty();
        let credit = match shared
            .delivery
            .as_ref()
            .or(has_archive.then_some(&shared.archive_ingress))
        {
            Some(delivery) => delivery
                .reserve_wait(&value, &shared.cancellation)
                .await
                .map(Some),
            None => Ok(None),
        };
        self.admit(&shared, value, credit)
    }
    /// For joined blocking parser workers only. Never call on an async runtime worker.
    pub fn blocking_send(&self, value: Value) -> Result<(), StreamError> {
        tokio::runtime::Handle::current().block_on(self.send(value))
    }
    fn admit(
        &self,
        shared: &Shared,
        value: Value,
        credit: Result<Option<delivery::Credit>, StreamError>,
    ) -> Result<(), StreamError> {
        let mut state = shared.state();
        if state.finished || shared.cancellation.is_cancelled() {
            return Err(StreamError::Closed);
        }
        let accepted = (|| {
            let credit = credit?;
            let sequence = state.sequence.checked_add(1).ok_or(StreamError::Capacity)?;
            state.window.push(value.clone())?;
            if let Some(credit) = credit.as_ref() {
                for branch in &state.archives {
                    branch.deliver(sequence, &value, credit);
                }
                state.archives.retain(|b| b.is_accepting());
            }
            if let (Some(delivery), Some(credit)) = (&shared.delivery, credit) {
                delivery
                    .channel
                    .try_send(delivery::Event {
                        sequence,
                        value,
                        credit,
                    })
                    .map_err(|_| StreamError::Closed)?;
            }
            state.sequence = sequence;
            state.dirty = true;
            Ok(())
        })();
        if let Err(error) = &accepted {
            state.problem = Some(RuntimeCode::StreamOverloaded.error(match error {
                StreamError::Overloaded => "Stream stopped: computational capacity remained unavailable for 5 seconds. No computational events were silently discarded; provider cancellation was requested.",
                _ => "Stream stopped: admission overflow exceeded its event or byte budget, or delivery closed. No computational events were silently discarded; provider cancellation was requested.",
            }, None));
            shared.cancellation.cancel();
        }
        accepted
    }
}

#[derive(Clone)]
pub struct StreamHandle {
    cancellation: CancellationToken,
    snapshots: watch::Receiver<Arc<Snapshot>>,
    events: Arc<Mutex<Option<tokio::sync::mpsc::Receiver<delivery::Event>>>>,
    owner: Weak<Shared>,
}
impl StreamHandle {
    pub(crate) fn cancellation_token(&self) -> CancellationToken {
        self.cancellation.clone()
    }
    /// Attach at the next admission boundary; rolling history cannot supply earlier events.
    pub(crate) fn attach_archive(&self, branch: Arc<archive::Branch>) -> Result<(), StreamError> {
        let owner = self.owner.upgrade().ok_or(StreamError::Closed)?;
        let mut state = owner.state();
        state.archives.retain(|branch| branch.is_accepting());
        if state.finished
            || owner.cancellation.is_cancelled()
            || state.archives.len() >= MAX_ARCHIVES
        {
            return Err(StreamError::Closed);
        }
        let next = state.sequence.checked_add(1).ok_or(StreamError::Capacity)?;
        branch.attach(next)?;
        state.archives.push(branch);
        Ok(())
    }
    pub(crate) fn take_events(&self) -> Option<tokio::sync::mpsc::Receiver<delivery::Event>> {
        self.events
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take()
    }
    pub fn cancel(&self) {
        self.cancellation.cancel();
    }
    pub fn snapshot(&self) -> Arc<Snapshot> {
        self.snapshots.borrow().clone()
    }
    /// Latest-state delivery, not a per-item event queue. Slow observers may miss intermediate windows.
    pub fn subscribe(&self) -> watch::Receiver<Arc<Snapshot>> {
        self.snapshots.clone()
    }
}
#[must_use = "retain and join the stream's physical lifetime"]
pub struct StreamTask {
    cancel_on_drop: bool,
    cancellation: CancellationToken,
    task: Option<JoinHandle<()>>,
}
impl StreamTask {
    pub(crate) fn joined(cancellation: CancellationToken, task: JoinHandle<()>) -> Self {
        Self {
            cancel_on_drop: true,
            cancellation,
            task: Some(task),
        }
    }
    pub(crate) fn after_join(mut self, cleanup: impl FnOnce() + Send + 'static) -> Self {
        let original = self.task.take().expect("owned source lifetime");
        let cancellation = self.cancellation.clone();
        self.cancel_on_drop = false;
        let task = tokio::spawn(async move {
            let result = original.await;
            cleanup();
            result.expect("source lifetime terminated unexpectedly");
        });
        Self::joined(cancellation, task)
    }
    pub async fn join(mut self) -> Result<(), tokio::task::JoinError> {
        self.task.take().expect("owned stream task").await
    }
    pub async fn shutdown(mut self) -> Result<(), tokio::task::JoinError> {
        self.cancellation.cancel();
        self.task.take().expect("owned stream task").await
    }
}
impl Drop for StreamTask {
    fn drop(&mut self) {
        // Losing the external join handle must not leave an unobserved subscription running forever.
        // Request cancellation; the lifetime task still joins the provider instead of aborting it.
        // Only awaited join/shutdown is a completion acknowledgement to the embedding owner.
        if self.cancel_on_drop {
            self.cancellation.cancel();
        }
    }
}

/// Requires an active Tokio runtime. Starts one explicitly authorized subscription.
/// Limits bound retained logical window data, not
/// caller/provider allocations, external snapshots or aggregate streams. The embedding owner must
/// separately bound concurrent subscriptions. Parent cancellation propagates only into this child.
pub fn spawn(
    call: Call,
    invoker: Arc<dyn StreamingInvoker>,
    attribution: Provenance,
    limits: Limits,
    parent: CancellationToken,
) -> Result<(StreamHandle, StreamTask), StreamError> {
    spawn_with_delivery(call, invoker, attribution, limits, parent, None)
}
pub(crate) fn spawn_with_delivery(
    call: Call,
    invoker: Arc<dyn StreamingInvoker>,
    attribution: Provenance,
    limits: Limits,
    parent: CancellationToken,
    budget: Option<delivery::Budget>,
) -> Result<(StreamHandle, StreamTask), StreamError> {
    spawn_with_archives(
        call,
        invoker,
        attribution,
        limits,
        parent,
        budget,
        None,
        vec![],
    )
}
pub(crate) fn spawn_with_archives(
    call: Call,
    invoker: Arc<dyn StreamingInvoker>,
    attribution: Provenance,
    limits: Limits,
    parent: CancellationToken,
    budget: Option<delivery::Budget>,
    archive_budget: Option<delivery::Budget>,
    archives: Vec<Arc<archive::Branch>>,
) -> Result<(StreamHandle, StreamTask), StreamError> {
    if !call.capability.streaming {
        return Err(StreamError::Invalid);
    }
    if archives.len() > MAX_ARCHIVES {
        return Err(StreamError::Invalid);
    }
    for branch in &archives {
        branch.attach(1)?;
    }
    let (archive_ingress, _) = delivery::Sender::new(
        archive_budget
            .or_else(|| budget.clone())
            .unwrap_or_default(),
    );
    let (delivery, events) = match budget {
        Some(budget) => {
            let (send, receive) = delivery::Sender::new(budget);
            (Some(send), Some(receive))
        }
        None => (None, None),
    };
    let window = Window::new(&call.capability.result, attribution.clone(), limits)?;
    let run = call.run.clone();
    let initial = Arc::new(Snapshot {
        run: run.clone(),
        phase: Phase::Opening,
        window: window.value(),
        omitted: 0,
        rejected: 0,
    });
    let cancellation = parent.child_token();
    let shared = Arc::new(Shared {
        state: Mutex::new(State {
            window,
            phase: Phase::Opening,
            opened: false,
            finished: false,
            dirty: false,
            problem: None,
            rejected: 0,
            sequence: 0,
            archives,
        }),
        cancellation: cancellation.clone(),
        changed: Notify::new(),
        delivery,
        archive_ingress,
        attribution,
    });
    let sink = StreamSink(Arc::downgrade(&shared));
    let owner = Arc::downgrade(&shared);
    let (publish, snapshots) = watch::channel(initial);
    let token = cancellation.clone();
    let task = tokio::spawn(async move {
        // Joining this inner task also catches synchronous subscribe panics. Panic payloads are
        // deliberately excluded from snapshots, since arbitrary provider text may contain secrets.
        let provider_token = token.clone();
        let mut provider = tokio::spawn(async move {
            if provider_token.is_cancelled() {
                return Err(InvocationError::Cancelled);
            }
            invoker.subscribe(call, sink, provider_token).await
        });
        let period = Duration::from_millis(250);
        let mut cadence = tokio::time::interval_at(tokio::time::Instant::now() + period, period);
        cadence.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        let mut closing = false;
        loop {
            tokio::select! {
                biased;
                result = &mut provider => {
                    {
                        let mut state = shared.state();
                        state.finished = true;
                        state.phase = if let Some(problem) = state.problem.take() {
                            Phase::Failed(problem)
                        } else if token.is_cancelled() {
                            Phase::Cancelled
                        } else { match result {
                            Ok(Ok(())) if state.opened => Phase::Ended,
                            Ok(Err(InvocationError::Cancelled)) => Phase::Cancelled,
                            Ok(Err(InvocationError::Failed(_))) if state.window.value().provenance().policy().is_confidential() => Phase::Failed(RuntimeCode::ExecutionFailed.error("Private stream failed; details withheld.", None)),
                            Ok(Err(InvocationError::Failed(error))) => Phase::Failed(error),
                            Ok(Ok(())) => Phase::Failed(RuntimeCode::ExecutionFailed.error("The stream ended without acknowledging its opening.", None)),
                            Err(_) => Phase::Failed(RuntimeCode::ExecutionFailed.error("The stream provider terminated unexpectedly.", None)),
                        }};
                        state.dirty = true;
                        let phase = state.phase.clone();
                        for branch in state.archives.drain(..) { branch.finish(&phase); }
                    }
                    publish_window(&shared, &run, &publish).await;
                    break;
                },
                () = token.cancelled(), if !closing => {
                    closing = true;
                    { let mut state = shared.state(); state.phase = Phase::Closing; state.dirty = true; }
                    publish_window(&shared, &run, &publish).await;
                },
                () = shared.changed.notified() => { publish_window(&shared, &run, &publish).await; },
                _ = cadence.tick() => {
                    if shared.state().opened { publish_window(&shared, &run, &publish).await; }
                },
            }
        }
    });
    Ok((
        StreamHandle {
            cancellation: cancellation.clone(),
            snapshots,
            events: Arc::new(Mutex::new(events)),
            owner,
        },
        StreamTask {
            cancel_on_drop: true,
            cancellation,
            task: Some(task),
        },
    ))
}
async fn publish_window(shared: &Arc<Shared>, run: &Run, publish: &watch::Sender<Arc<Snapshot>>) {
    if !shared.state().dirty {
        return;
    }
    let worker_state = shared.clone();
    let worker_run = run.clone();
    // Copy/attribute a bounded window on a joined worker. Physical publication construction is never
    // detached by cancellation, and the state lock keeps it coherent with concurrent callbacks.
    let snapshot = tokio::task::spawn_blocking(move || {
        let (window, phase, rejected) = {
            let mut state = worker_state.state();
            if !state.dirty {
                return None;
            }
            state.dirty = false;
            (state.window.clone(), state.phase.clone(), state.rejected)
        };
        // Clone only Arc-backed items under the lock. Deep observation materialization cannot
        // hold up event admission, and computation never reads this materialized list.
        let value = window.value();
        let phase = match phase {
            Phase::Failed(error) => Phase::Failed(error.with_policy(value.provenance().policy())),
            other => other,
        };
        let losses = window::loss_cautions(window.omitted(), rejected);
        let value = if losses.is_empty() {
            value
        } else {
            let provenance = value.provenance().clone().cautioned(losses);
            value.with_provenance(provenance)
        };
        Some(Arc::new(Snapshot {
            run: worker_run,
            phase,
            window: value,
            omitted: window.omitted(),
            rejected,
        }))
    })
    .await;
    match snapshot {
        Ok(Some(snapshot)) => {
            publish.send_replace(snapshot);
        }
        Ok(None) => {}
        Err(_) => {
            // A projection failure must not unwind past the still-owned provider task. Preserve the
            // last successfully built window, revoke delivery and continue through physical join.
            let error = RuntimeCode::ExecutionFailed.error(
                "The stream window could not be constructed; local cancellation was requested.",
                None,
            );
            let phase = {
                let mut state = shared.state();
                state.dirty = false;
                state.phase = if state.finished {
                    Phase::Failed(error)
                } else {
                    state.problem = Some(error);
                    Phase::Closing
                };
                state.phase.clone()
            };
            shared.cancellation.cancel();
            let previous = publish.borrow().clone();
            publish.send_replace(Arc::new(Snapshot {
                run: run.clone(),
                phase,
                window: previous.window.clone(),
                omitted: previous.omitted,
                rejected: previous.rejected,
            }));
        }
    }
}
