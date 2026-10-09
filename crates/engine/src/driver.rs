//! Asynchronous finite-execution driver. One actor owns runtime transitions; workers own I/O.
//! Live observations are bounded notifications, not the persistent journal.
use crate::{
    graph::{DependencyGraph, NodeId, OutputRef},
    runtime::*,
};
use indexmap::{IndexMap, IndexSet};
use std::{future::Future, num::NonZeroUsize, pin::Pin, sync::Arc, time::Duration};
use thiserror::Error;
use tokio::{
    sync::{broadcast, mpsc, oneshot},
    task::JoinHandle,
};
pub use tokio_util::sync::CancellationToken;
use wes_core::{ErrorValue, Value, capability::Typing};
pub(crate) mod conversations;
mod io;
pub mod lifetime;
pub mod progress;
pub use conversations::ConversationEvent;
mod capacity;
pub(crate) mod streaming;
pub use capacity::{
    CapacitySnapshot, DEFAULT_MAX_STREAMS, ExecutionCapacity, MAX_STREAMS, SlotUsage,
};
pub(crate) use io::{ExecutionIo, RuntimeInput};
pub(crate) mod waiting;
use waiting::{Waiter, Waiters};

pub type ExecutionFuture = Pin<Box<dyn Future<Output = ExecutionReport> + Send + 'static>>;
/// The completion future owns physical conversation cleanup and recovery acknowledgement. Retain
/// and await it even after cancellation; the handle alone is not an execution owner.
#[must_use = "retain and await the conversation completion"]
pub struct InteractiveExecution {
    pub handle: crate::conversations::ConversationHandle,
    pub completion: ExecutionFuture,
}
pub type InteractiveExecutionFuture =
    Pin<Box<dyn Future<Output = Result<InteractiveExecution, ExecutionReport>> + Send + 'static>>;
pub type StreamExecutionFuture = Pin<
    Box<
        dyn Future<
                Output = Result<
                    (crate::streams::StreamHandle, crate::streams::StreamTask),
                    ExecutionReport,
                >,
            > + Send
            + 'static,
    >,
>;

/// A resource reservation follows its result until coordinator admission. The
/// executor cannot release it while a completion still owns unadmitted output.
pub struct ExecutionHold {
    _owner: Box<dyn Send + 'static>,
}
impl ExecutionHold {
    pub(crate) fn retain(owner: impl Send + 'static) -> Self {
        Self {
            _owner: Box::new(owner),
        }
    }
}
impl std::fmt::Debug for ExecutionHold {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("ExecutionHold")
    }
}

/// Operational problems must not replace the provider's outcome after it has already run.
#[derive(Debug)]
pub struct ExecutionReport {
    pub outcome: Outcome,
    pub holds: Vec<ExecutionHold>,
    pub notices: Vec<ErrorValue>,
    /// Absolute start of a terminal stream window, published atomically with its outcome.
    pub stream_start: Option<u64>,
    /// Final status travels atomically with the accepted outcome; queue progress is lossy.
    pub progress: Option<progress::ExecutionProgress>,
}
impl From<Outcome> for ExecutionReport {
    fn from(outcome: Outcome) -> Self {
        Self {
            outcome,
            holds: vec![],
            notices: vec![],
            stream_start: None,
            progress: None,
        }
    }
}
#[derive(Clone, Debug)]
pub struct ExecutionNotice {
    pub run: Run,
    pub error: ErrorValue,
}

/// Implementations own concrete work and must not detach work that can still have local effects.
/// In particular, await a started blocking worker even after cancellation; returning early would
/// falsely release its lease. Metadata and bound provider handles belong in the ticket payload.
pub trait Executor<T>: Send + Sync + 'static {
    fn execute(&self, ticket: RunTicket<T>, cancellation: CancellationToken) -> ExecutionFuture;
    fn execute_reporting(
        &self,
        ticket: RunTicket<T>,
        cancellation: CancellationToken,
        _progress: progress::Reporter,
    ) -> ExecutionFuture {
        self.execute(ticket, cancellation)
    }
    /// A lifetime may publish acknowledged values without completing or releasing its physical owner.
    fn execute_lifetime(
        &self,
        ticket: RunTicket<T>,
        cancellation: CancellationToken,
        progress: progress::Reporter,
        _values: lifetime::Reporter,
    ) -> ExecutionFuture {
        self.execute_reporting(ticket, cancellation, progress)
    }
    /// Resource classification must describe the captured payload, without performing I/O.
    fn streaming(&self, _payload: &T) -> bool {
        false
    }
    /// A joined non-stream lifetime reserves live capacity but does not occupy ordinary
    /// operation concurrency while waiting. It still returns one finite terminal receipt.
    fn lifetime(&self, _payload: &T) -> bool {
        false
    }
    fn interactive(&self, _payload: &T) -> bool {
        false
    }
    fn execute_interactive(
        &self,
        _ticket: RunTicket<T>,
        _cancellation: CancellationToken,
    ) -> InteractiveExecutionFuture {
        Box::pin(async {
            Err(Outcome::Failed(
                RuntimeCode::ExecutionFailed
                    .error("This executor cannot start conversations.", None),
            )
            .into())
        })
    }
    /// Return ownership of an authorized subscription. The driver joins its task on every path.
    fn execute_stream(
        &self,
        _ticket: RunTicket<T>,
        _cancellation: CancellationToken,
    ) -> StreamExecutionFuture {
        Box::pin(async {
            Err(Outcome::Failed(
                RuntimeCode::ExecutionFailed.error("This executor cannot open streams.", None),
            )
            .into())
        })
    }
}

#[derive(Clone, Debug)]
pub struct Snapshot<T> {
    pub progress: IndexMap<NodeId, progress::ExecutionProgress>,
    pub waiting_inputs: IndexMap<NodeId, Vec<crate::runtime::WaitingInput>>,
    pub stale_reasons: IndexMap<NodeId, crate::runtime::StaleReason>,
    pub input_updates: IndexSet<NodeId>,
    pub creation_inputs: IndexMap<NodeId, bool>,
    pub captured_inputs: IndexMap<NodeId, bool>,
    pub graph: DependencyGraph<T>,
    pub values: IndexMap<NodeId, Value>,
    pub evidence_values: IndexMap<NodeId, crate::runtime::EvidenceValue>,
    pub actual_typings: IndexMap<NodeId, Arc<Typing>>,
    pub errors: IndexMap<NodeId, ErrorValue>,
    pub runs: IndexMap<NodeId, RunId>,
    pub executing: Vec<NodeId>,
    pub streaming: Vec<NodeId>,
    pub closed: bool,
    pub idle: bool,
}

/// Typed internal control messages, not the public command language or wire protocol.
pub enum Command<T> {
    Add {
        payload: T,
        dependencies: Vec<OutputRef>,
        traits: ExecutionTraits,
    },
    Start,
    Cancel(NodeId),
    Refresh(NodeId),
    Invalidate(NodeId),
    Drop(NodeId),
    Forget(NodeId),
    Timeout {
        node: Option<NodeId>,
        budget: Duration,
    },
    Policy {
        node: Option<NodeId>,
        policy: Policy,
    },
    Snapshot,
    Shutdown,
}
#[derive(Debug)]
pub enum Reply<T> {
    Added(NodeId),
    Changed {
        started: Vec<NodeId>,
        removed: Vec<NodeId>,
    },
    Snapshot(Box<Snapshot<T>>),
}
#[derive(Clone, Debug, Error, PartialEq, Eq)]
pub enum DriverError {
    #[error("source run changed")]
    SourceChanged,
    #[error("source ended before readiness")]
    SourceClosed,
    #[error("source readiness wait cancelled")]
    WaitCancelled,
    #[error(transparent)]
    Conversation(#[from] crate::conversations::ConversationError),
    #[error("too many pending execution waits")]
    WaitCapacity,
    #[error("the selected-output wait exceeds the supported budget")]
    InvalidWait,
    #[error("the concurrency limit exceeds the executor's supported range")]
    InvalidConcurrency,
    #[error("the stream limit must be from 1 to 1024")]
    InvalidStreamCapacity,
    #[error("a driver can only adopt a runtime with no active execution leases")]
    ActiveRuntime,
    #[error("the execution driver is no longer available")]
    Stopped,
    #[error(transparent)]
    Runtime(#[from] RuntimeError),
}

enum Request<T> {
    Command(Command<T>, oneshot::Sender<Result<Reply<T>, DriverError>>),
    Wait(Waiter),
    Input {
        node: NodeId,
        run: RunId,
        bytes: Option<Vec<u8>>,
        reply: oneshot::Sender<Result<(), DriverError>>,
    },
}
#[derive(Clone)]
pub struct DriverHandle<T> {
    requests: mpsc::Sender<Request<T>>,
    events: broadcast::WeakSender<Observation>,
    notices: broadcast::WeakSender<ExecutionNotice>,
    conversations: broadcast::WeakSender<Arc<ConversationEvent>>,
}
impl<T: Send + 'static> DriverHandle<T> {
    /// Ephemeral input for exactly the displayed run. Queue acceptance is not child consumption.
    pub async fn input(&self, node: NodeId, run: RunId, bytes: &[u8]) -> Result<(), DriverError> {
        self.conversation_input(node, run, Some(bytes)).await
    }
    pub async fn eof(&self, node: NodeId, run: RunId) -> Result<(), DriverError> {
        self.conversation_input(node, run, None).await
    }
    async fn conversation_input(
        &self,
        node: NodeId,
        run: RunId,
        bytes: Option<&[u8]>,
    ) -> Result<(), DriverError> {
        if bytes.is_some_and(|bytes| bytes.len() > crate::conversations::input_bytes()) {
            return Err(crate::conversations::ConversationError::Capacity.into());
        }
        let permit = self
            .requests
            .reserve()
            .await
            .map_err(|_| DriverError::Stopped)?;
        let (reply, receive) = oneshot::channel();
        permit.send(Request::Input {
            node,
            run,
            bytes: bytes.map(<[u8]>::to_vec),
            reply,
        });
        receive.await.map_err(|_| DriverError::Stopped)?
    }
    /// Bounded transient display. Lag is explicit; a snapshot cannot replay omitted private output.
    pub fn subscribe_conversations(
        &self,
    ) -> Result<broadcast::Receiver<Arc<ConversationEvent>>, DriverError> {
        Ok(self
            .conversations
            .upgrade()
            .ok_or(DriverError::Stopped)?
            .subscribe())
    }
    /// Once enqueued, dropping this future does not retract the command. Cancellation is explicit.
    pub async fn command(&self, command: Command<T>) -> Result<Reply<T>, DriverError> {
        let (reply, receive) = oneshot::channel();
        self.requests
            .send(Request::Command(command, reply))
            .await
            .map_err(|_| DriverError::Stopped)?;
        receive.await.map_err(|_| DriverError::Stopped)?
    }
    /// Open streams are idle; opening/closing and physically executing cancelled work are not.
    /// Use a caller-side timeout when needed.
    pub async fn wait_idle(&self) -> Result<(), DriverError> {
        let (reply, receive) = oneshot::channel();
        self.requests
            .send(Request::Wait(Waiter::Idle(reply)))
            .await
            .map_err(|_| DriverError::Stopped)?;
        receive.await.map_err(|_| DriverError::Stopped)?
    }
    /// Wait only for the exact currently executing stream's open acknowledgement. No source is started.
    pub async fn wait_source_ready(
        &self,
        node: NodeId,
        run: RunId,
        budget: Duration,
        caller: CancellationToken,
    ) -> Result<bool, DriverError> {
        if budget.is_zero() || budget > crate::streams::MAX_READY_WAIT {
            return Err(DriverError::InvalidWait);
        }
        let until = tokio::time::Instant::now() + budget;
        let (reply, receive) = oneshot::channel();
        tokio::select! { biased;
            () = caller.cancelled() => return Err(DriverError::WaitCancelled),
            _ = tokio::time::sleep_until(until) => return Ok(false),
            sent = self.requests.send(Request::Wait(Waiter::SourceReady { node, run, until, reply })) => sent.map_err(|_| DriverError::Stopped)?,
        }
        tokio::select! { biased;
            () = caller.cancelled() => Err(DriverError::WaitCancelled),
            result = receive => result.map_err(|_|DriverError::Stopped)?,
        }
    }
    /// Waits for data/error/cancel selections without starting work or blocking control handling.
    /// False means the settled selections are unavailable, or the wait expired. A wait timeout
    /// never cancels the underlying call. Dropping the future cancels only this wait registration.
    pub async fn wait_outputs(
        &self,
        selected: OutputSelection,
        budget: Duration,
    ) -> Result<bool, DriverError> {
        if selected.len() > waiting::max_selected_outputs() {
            return Err(DriverError::InvalidWait);
        }
        let until = tokio::time::Instant::now()
            .checked_add(budget)
            .ok_or(DriverError::InvalidWait)?;
        let (reply, receive) = oneshot::channel();
        self.requests
            .send(Request::Wait(Waiter::Outputs {
                selected,
                until,
                reply,
            }))
            .await
            .map_err(|_| DriverError::Stopped)?;
        receive.await.map_err(|_| DriverError::Stopped)?
    }
    /// Lag is explicit through broadcast::RecvError::Lagged. Resynchronize with a snapshot.
    pub fn subscribe(&self) -> Result<broadcast::Receiver<Observation>, DriverError> {
        Ok(self
            .events
            .upgrade()
            .ok_or(DriverError::Stopped)?
            .subscribe())
    }
    /// Operational notices include recording failures even for an obsolete run. This bounded
    /// live stream is not a persistent log; lag is explicit and must not be ignored by consumers.
    pub fn subscribe_notices(&self) -> Result<broadcast::Receiver<ExecutionNotice>, DriverError> {
        Ok(self
            .notices
            .upgrade()
            .ok_or(DriverError::Stopped)?
            .subscribe())
    }
}

/// Kept separate from cloneable control handles. Dropping this handle does not abort workers.
pub struct DriverTask {
    task: JoinHandle<()>,
}
impl DriverTask {
    pub async fn join(self) -> Result<(), tokio::task::JoinError> {
        self.task.await
    }
}

/// Requires an active Tokio runtime. Restored nodes remain held; startup does not call start().
pub fn spawn<T: Clone + Send + 'static>(
    runtime: Runtime<T>,
    executor: Arc<dyn Executor<T>>,
    max_concurrent: NonZeroUsize,
) -> Result<(DriverHandle<T>, DriverTask), DriverError> {
    spawn_with_capacity(runtime, executor, ExecutionCapacity::new(max_concurrent)?)
}

/// Embed multiple drivers under the same admission and observation owner.
pub fn spawn_with_capacity<T: Clone + Send + 'static>(
    runtime: Runtime<T>,
    executor: Arc<dyn Executor<T>>,
    capacity: ExecutionCapacity,
) -> Result<(DriverHandle<T>, DriverTask), DriverError> {
    if !runtime.is_drained() {
        return Err(DriverError::ActiveRuntime);
    }
    if runtime.is_closed() {
        return Err(RuntimeError::Closed.into());
    }
    let (requests, receiver) = mpsc::channel(256);
    let io = ExecutionIo::with_capacity(executor, capacity)?;
    let handle = DriverHandle {
        requests,
        events: io.events(),
        notices: io.notices(),
        conversations: io.conversations(),
    };
    let actor = Actor {
        runtime,
        io,
        receiver,
        waiters: Waiters::default(),
        commands_open: true,
    };
    Ok((
        handle,
        DriverTask {
            task: tokio::spawn(actor.run()),
        },
    ))
}

struct Actor<T> {
    runtime: Runtime<T>,
    io: ExecutionIo<T>,
    receiver: mpsc::Receiver<Request<T>>,
    waiters: Waiters,
    commands_open: bool,
}
impl<T: Clone + Send + 'static> Actor<T> {
    async fn run(mut self) {
        loop {
            self.waiters.poll(&self.runtime, self.io.is_idle());
            if self.runtime.is_drained() && self.io.is_drained() && self.runtime.is_closed() {
                break;
            }
            tokio::select! {
                request = self.receiver.recv(), if self.commands_open => {
                    match request {
                        Some(Request::Command(command, reply)) => { let result = self.command(command); let _ = reply.send(result); }
                        Some(Request::Wait(waiter)) => self.waiters.insert(waiter),
                        Some(Request::Input { node, run, bytes, reply }) => {
                            let result = self.io.input(&self.runtime, &node, &run, bytes.as_deref());
                            let _ = reply.send(result);
                        },
                        None => { self.commands_open = false; let effects = self.runtime.close(); self.io.effects(effects); }
                    }
                },
                input = self.io.next() => {
                    self.io.announce_notices(&input);
                    let effects = self.io.apply_input(input, &mut self.runtime);
                    self.io.effects(effects);
                },
                () = waiting::until(self.waiters.next_deadline()) => {},
            }
        }
    }
    fn command(&mut self, command: Command<T>) -> Result<Reply<T>, DriverError> {
        if self.runtime.is_closed() && !matches!(command, Command::Snapshot | Command::Shutdown) {
            return Err(RuntimeError::Closed.into());
        }
        let now = self.io.now();
        let mut removed = vec![];
        let effects = match command {
            Command::Add {
                payload,
                dependencies,
                traits,
            } => {
                return Ok(Reply::Added(self.runtime.add(
                    payload,
                    dependencies,
                    traits,
                )?));
            }
            Command::Start => self.runtime.start(now),
            Command::Cancel(node) => self.runtime.cancel(&node, now),
            Command::Refresh(node) => self.runtime.refresh(&node, now)?,
            Command::Invalidate(node) => self.runtime.invalidate(&node, now)?,
            Command::Drop(node) => {
                let (nodes, effects) = self.runtime.drop_node(&node)?;
                removed = nodes.into_iter().collect();
                effects
            }
            Command::Forget(node) => self.runtime.forget(&node),
            Command::Timeout { node, budget } => match node {
                Some(node) => self.runtime.set_timeout(&node, budget)?,
                None => {
                    self.runtime.set_default_timeout(budget)?;
                    vec![]
                }
            },
            Command::Policy { node, policy } => {
                if let Some(node) = node {
                    self.runtime.set_policy(&node, policy)?;
                } else {
                    self.runtime.set_default_policy(policy);
                }
                vec![]
            }
            Command::Snapshot => return Ok(Reply::Snapshot(Box::new(self.snapshot()))),
            Command::Shutdown => self.runtime.close(),
        };
        let started = effects
            .iter()
            .filter_map(|effect| {
                if let Effect::Spawn(ticket) = effect {
                    Some(ticket.run.node().clone())
                } else {
                    None
                }
            })
            .collect();
        self.io.effects(effects);
        Ok(Reply::Changed { started, removed })
    }
    fn snapshot(&self) -> Snapshot<T> {
        Snapshot::capture(&self.runtime, self.io.is_idle())
    }
}
impl<T: Clone> Snapshot<T> {
    pub(crate) fn capture(runtime: &Runtime<T>, io_idle: bool) -> Self {
        let graph = runtime.graph().clone();
        let mut stale_reasons = IndexMap::new();
        let mut input_updates = IndexSet::new();
        let mut creation_inputs = IndexMap::new();
        let mut captured_inputs = IndexMap::new();
        let mut waiting_inputs = IndexMap::new();
        let mut values = IndexMap::new();
        let mut evidence_values = IndexMap::new();
        let mut progress = IndexMap::new();
        let mut actual_typings = IndexMap::new();
        let mut errors = IndexMap::new();
        let mut runs = IndexMap::new();
        let mut executing = vec![];
        let mut streaming = vec![];
        for node in graph.nodes() {
            if let Some(value) = runtime.execution_progress(node.id()) {
                progress.insert(node.id().clone(), value.clone());
            }
            if runtime.dependency_lifetime(node.id()) == Some(DependencyLifetime::Creation) {
                creation_inputs.insert(node.id().clone(), runtime.construction_complete(node.id()));
            }
            if runtime.dependency_lifetime(node.id()) == Some(DependencyLifetime::Captured) {
                captured_inputs.insert(node.id().clone(), runtime.inputs_captured(node.id()));
            }
            if runtime.input_update_pending(node.id()) {
                input_updates.insert(node.id().clone());
            }
            let waits = runtime.waiting_inputs(node.id());
            if !waits.is_empty() {
                waiting_inputs.insert(node.id().clone(), waits);
            }
            if let Some(reason) = runtime.stale_reason(node.id()) {
                stale_reasons.insert(node.id().clone(), reason);
            }
            if let Some(typing) = runtime.actual_typing(node.id()) {
                actual_typings.insert(node.id().clone(), typing.clone());
            }
            if let Some(value) = runtime.evidence_value(node.id()) {
                evidence_values.insert(node.id().clone(), value.clone());
            }
            if let Some(value) = runtime.value_of(node.id()) {
                values.insert(node.id().clone(), value.clone());
            }
            if let Some(error) = runtime.error_of(node.id()) {
                errors.insert(node.id().clone(), error.clone());
            }
            if let Some(run) = runtime.run_of(node.id()) {
                runs.insert(node.id().clone(), run.clone());
            }
            if runtime.is_executing(node.id()) {
                executing.push(node.id().clone());
            }
            if runtime.is_streaming(node.id()) {
                streaming.push(node.id().clone());
            }
        }
        Snapshot {
            progress,
            waiting_inputs,
            stale_reasons,
            input_updates,
            creation_inputs,
            captured_inputs,
            graph,
            values,
            evidence_values,
            actual_typings,
            errors,
            runs,
            executing,
            streaming,
            closed: runtime.is_closed(),
            idle: runtime.is_idle() && io_idle,
        }
    }
}
