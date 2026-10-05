//! Owned interactive execution, ephemeral bounded input and bounded live output.
//!
//! This port alone does not authorize a call, record history, retain a transcript or make a
//! runtime idle. Its embedding owner must keep the physical lease and join the returned task.
use crate::{
    driver::CancellationToken,
    providers::{Call, InvocationError, InvocationFuture},
    runtime::{Run, RuntimeCode},
};
use std::sync::{Arc, Mutex, Weak};
use tokio::{
    sync::{Notify, mpsc, watch},
    task::JoinHandle,
};
mod output;

pub fn input_bytes() -> usize {
    wes_budgets::get("conversation.input.bytes") as usize
}
pub const INPUT_MESSAGES: usize = 16;
pub fn output_bytes() -> usize {
    wes_budgets::get("conversation.output.bytes") as usize
}

/// Complete only after local execution and all entered I/O have ended, including cancellation.
/// Construction must not block. The endpoint is exclusive to this run; input is never source text.
pub trait InteractiveInvoker: Send + Sync + 'static {
    fn start(
        &self,
        call: Call,
        io: ConversationIo,
        cancellation: CancellationToken,
    ) -> InvocationFuture;
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Channel {
    Stdout,
    Stderr,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Phase {
    Opening,
    Open,
    Closing,
    Ended,
}
#[derive(Clone, Debug)]
pub struct Snapshot {
    pub run: Run,
    pub phase: Phase,
    pub output_revision: u64,
}
/// Loss counts refer to decoded UTF-8 bytes omitted from this live batch, not the final raw result.
pub struct Output {
    pub text: String,
    pub omitted_bytes: u64,
}
impl Output {
    pub fn is_empty(&self) -> bool {
        self.text.is_empty() && self.omitted_bytes == 0
    }
}
// Deliberately no Debug implementation: input can contain passwords and must not leak in errors.
pub enum Input {
    Bytes(Vec<u8>),
    Eof,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
pub enum ConversationError {
    #[error("the conversation no longer accepts this operation")]
    Closed,
    #[error("the conversation input queue is full")]
    Busy,
    #[error("the conversation byte budget or counter was exceeded")]
    Capacity,
    #[error("the conversation has already opened")]
    AlreadyOpen,
    #[error("the conversation metadata or limits are invalid")]
    Invalid,
}
struct State {
    output: output::Buffer,
    opened: bool,
    finished: bool,
    input_closed: bool,
    dirty: bool,
    problem: bool,
}
struct Shared {
    state: Mutex<State>,
    cancellation: CancellationToken,
    changed: Notify,
}
impl Shared {
    fn state(&self) -> std::sync::MutexGuard<'_, State> {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }
}
#[derive(Clone)]
pub struct OutputSink(Weak<Shared>);
impl OutputSink {
    pub fn opened(&self) -> Result<(), ConversationError> {
        let shared = self.0.upgrade().ok_or(ConversationError::Closed)?;
        let mut state = shared.state();
        if state.finished || shared.cancellation.is_cancelled() {
            return Err(ConversationError::Closed);
        }
        if state.opened {
            return Err(ConversationError::AlreadyOpen);
        }
        state.opened = true;
        shared.changed.notify_one();
        Ok(())
    }
    pub fn write(&self, channel: Channel, bytes: &[u8]) -> Result<(), ConversationError> {
        let shared = self.0.upgrade().ok_or(ConversationError::Closed)?;
        let mut state = shared.state();
        if state.finished || shared.cancellation.is_cancelled() {
            return Err(ConversationError::Closed);
        }
        if let Err(error) = state.output.write(channel, bytes) {
            state.problem = true;
            shared.cancellation.cancel();
            return Err(error);
        }
        state.dirty |= !bytes.is_empty();
        Ok(())
    }
}
pub struct ConversationIo {
    pub output: OutputSink,
    input: mpsc::Receiver<Input>,
}
impl ConversationIo {
    pub async fn receive(&mut self) -> Option<Input> {
        self.input.recv().await
    }
    /// Terminal handover has no piped input. Revoke admission without retaining queued secrets.
    pub fn close_input(&mut self) {
        if let Some(shared) = self.output.0.upgrade() {
            shared.state().input_closed = true;
        }
        self.input.close();
        while self.input.try_recv().is_ok() {}
    }
}
#[derive(Clone)]
pub struct ConversationHandle {
    shared: Arc<Shared>,
    input: mpsc::Sender<Input>,
    snapshots: watch::Receiver<Arc<Snapshot>>,
}
impl ConversationHandle {
    /// Acceptance into a bounded queue is not a claim that the child consumed the input.
    pub fn send(&self, bytes: &[u8]) -> Result<(), ConversationError> {
        if bytes.len() > input_bytes() {
            return Err(ConversationError::Capacity);
        }
        self.enqueue(Some(bytes))
    }
    pub fn eof(&self) -> Result<(), ConversationError> {
        self.enqueue(None)
    }
    fn enqueue(&self, bytes: Option<&[u8]>) -> Result<(), ConversationError> {
        let mut state = self.shared.state();
        if state.finished || state.input_closed || self.shared.cancellation.is_cancelled() {
            return Err(ConversationError::Closed);
        }
        // Acquire queue capacity before copying caller bytes. The same lock orders EOF and send.
        let permit = self.input.try_reserve().map_err(|error| match error {
            mpsc::error::TrySendError::Full(_) => ConversationError::Busy,
            mpsc::error::TrySendError::Closed(_) => ConversationError::Closed,
        })?;
        permit.send(match bytes {
            Some(bytes) => Input::Bytes(bytes.to_vec()),
            None => Input::Eof,
        });
        state.input_closed |= bytes.is_none();
        Ok(())
    }
    pub fn cancel(&self) {
        self.shared.cancellation.cancel();
    }
    pub fn snapshot(&self) -> Arc<Snapshot> {
        self.snapshots.borrow().clone()
    }
    pub fn subscribe(&self) -> watch::Receiver<Arc<Snapshot>> {
        self.snapshots.clone()
    }
    /// One embedding output owner drains batches; cloned handles do not duplicate transcripts.
    pub fn take_output(&self) -> Output {
        self.shared.state().output.take()
    }
}
#[must_use = "retain and join the conversation's physical lifetime"]
pub struct ConversationTask {
    cancellation: CancellationToken,
    task: Option<JoinHandle<Result<wes_core::Value, InvocationError>>>,
}
impl ConversationTask {
    pub async fn join(
        mut self,
    ) -> Result<Result<wes_core::Value, InvocationError>, tokio::task::JoinError> {
        self.task.take().expect("owned conversation task").await
    }
    pub async fn shutdown(
        mut self,
    ) -> Result<Result<wes_core::Value, InvocationError>, tokio::task::JoinError> {
        self.cancellation.cancel();
        self.task.take().expect("owned conversation task").await
    }
}
impl Drop for ConversationTask {
    fn drop(&mut self) {
        self.cancellation.cancel();
    }
}

/// Starts an explicitly authorized nonstreaming interaction. No implicit human-response deadline.
/// Concurrent conversations, final results and caller-held batches need separate embedding bounds.
pub fn spawn(
    call: Call,
    invoker: Arc<dyn InteractiveInvoker>,
    output_bytes: usize,
    parent: CancellationToken,
) -> Result<(ConversationHandle, ConversationTask), ConversationError> {
    if call.capability.streaming || output_bytes == 0 || output_bytes > 1024 * 1024 {
        return Err(ConversationError::Invalid);
    }
    let run = call.run.clone();
    let cancellation = parent.child_token();
    let shared = Arc::new(Shared {
        state: Mutex::new(State {
            output: output::Buffer::new(output_bytes),
            opened: false,
            finished: false,
            input_closed: false,
            dirty: false,
            problem: false,
        }),
        cancellation: cancellation.clone(),
        changed: Notify::new(),
    });
    let (input, receive) = mpsc::channel(INPUT_MESSAGES);
    let io = ConversationIo {
        output: OutputSink(Arc::downgrade(&shared)),
        input: receive,
    };
    let (publish, snapshots) = watch::channel(Arc::new(Snapshot {
        run: run.clone(),
        phase: Phase::Opening,
        output_revision: 0,
    }));
    let owner = shared.clone();
    let token = cancellation.clone();
    let task = tokio::spawn(async move {
        let provider_token = token.clone();
        let mut provider = tokio::spawn(async move {
            if provider_token.is_cancelled() {
                return Err(InvocationError::Cancelled);
            }
            invoker.start(call, io, provider_token).await
        });
        let period = std::time::Duration::from_millis(250);
        let mut cadence = tokio::time::interval_at(tokio::time::Instant::now() + period, period);
        cadence.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        let mut closing = false;
        loop {
            tokio::select! {
                biased;
                result = &mut provider => {
                    let mut state = owner.state();
                    state.finished = true;
                    state.problem |= state.output.finish().is_err();
                    let previous = publish.borrow().output_revision;
                    let revision = previous.checked_add(1).unwrap_or_else(|| { state.problem = true; previous });
                    publish.send_replace(Arc::new(Snapshot { run, phase: Phase::Ended, output_revision: revision }));
                    return if state.problem { Err(failed("The conversation output counter was exhausted.")) }
                        else if token.is_cancelled() { Err(InvocationError::Cancelled) }
                        else { match result {
                            Ok(Ok(value)) if state.opened => Ok(value),
                            Ok(Ok(_)) => Err(failed("The conversation ended without acknowledging its opening.")),
                            Ok(Err(error)) => Err(error),
                            Err(_) => Err(failed("The conversation provider terminated unexpectedly.")),
                        }};
                },
                () = token.cancelled(), if !closing => { closing = true; publish_state(&owner, &run, &publish, true); },
                () = owner.changed.notified() => { publish_state(&owner, &run, &publish, closing); },
                _ = cadence.tick() => { publish_state(&owner, &run, &publish, closing); },
            }
        }
    });
    Ok((
        ConversationHandle {
            shared,
            input,
            snapshots,
        },
        ConversationTask {
            cancellation,
            task: Some(task),
        },
    ))
}
fn failed(message: &str) -> InvocationError {
    InvocationError::Failed(RuntimeCode::ExecutionFailed.error(message, None))
}
fn publish_state(
    shared: &Shared,
    run: &Run,
    publish: &watch::Sender<Arc<Snapshot>>,
    closing: bool,
) {
    let mut state = shared.state();
    let previous = publish.borrow().clone();
    let phase = if closing {
        Phase::Closing
    } else if state.opened {
        Phase::Open
    } else {
        Phase::Opening
    };
    if !state.dirty && phase == previous.phase {
        return;
    }
    let Some(revision) = previous.output_revision.checked_add(1) else {
        state.problem = true;
        shared.cancellation.cancel();
        return;
    };
    state.dirty = false;
    publish.send_replace(Arc::new(Snapshot {
        run: run.clone(),
        phase,
        output_revision: revision,
    }));
}
