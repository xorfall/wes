//! Async execution resources, without ownership of graph or workspace declarations.
use super::conversations::{self, ConversationEvent};
use super::streaming::{self, Update};
use super::{CancellationToken, DriverError, ExecutionNotice, ExecutionReport, Executor};
use crate::runtime::{
    Deadline, Effect, Observation, Outcome, Run, RunTicket, Runtime, RuntimeCode,
};
use std::{
    collections::{BTreeMap, HashMap, HashSet},
    future::pending,
    sync::Arc,
    time::Duration,
};
use tokio::{
    sync::{Semaphore, broadcast, mpsc, oneshot},
    task::{Id, JoinSet},
    time::Instant,
};
use tracing::Instrument;

pub(crate) struct Enter<T> {
    pub(crate) ticket: RunTicket<T>,
    pub(crate) reply: oneshot::Sender<Result<Option<RunTicket<T>>, wes_core::ErrorValue>>,
    streaming: bool,
}
pub(crate) enum RuntimeInput<T> {
    Enter(Enter<T>),
    Completed(Run, ExecutionReport),
    Expired(Deadline),
    Stream(Update),
    Conversation(conversations::Update),
}
impl<T> RuntimeInput<T> {
    pub fn changes_snapshot(&self) -> bool {
        !matches!(
            self,
            Self::Conversation(conversations::Update::Output { .. })
        )
    }
    pub fn notices(&self) -> impl Iterator<Item = (&Run, &wes_core::ErrorValue)> {
        let completed = match self {
            Self::Completed(run, report) => Some((run, report)),
            _ => None,
        };
        completed
            .into_iter()
            .flat_map(|(run, report)| report.notices.iter().map(move |error| (run, error)))
    }
}
impl RuntimeInput<crate::tasks::BoundTask> {
    pub fn deferrable(&self, workspace: &crate::workspace::Workspace) -> bool {
        matches!(self, Self::Stream(update) if update.deferrable(workspace.runtime()))
    }
    pub fn apply_workspace(
        self,
        workspace: &mut crate::workspace::Workspace,
        now: Duration,
    ) -> Vec<Effect<crate::tasks::BoundTask>> {
        match self {
            Self::Enter(enter) => {
                let ticket = workspace.enter_ticket(enter.ticket);
                let _ = enter.reply.send(ticket);
                vec![]
            }
            Self::Completed(run, report) => workspace.complete_report(&run, report, now),
            Self::Expired(deadline) => workspace.expire(&deadline, now),
            Self::Stream(update) => workspace.stream_update(update, now),
            // The embedding I/O owner handles conversation capabilities before this pure path.
            Self::Conversation(update) => {
                update.reject();
                vec![]
            }
        }
    }
}
impl<T: Clone> RuntimeInput<T> {
    /// The caller still owns the single runtime. There is no shadow graph in the I/O driver.
    pub fn apply(self, runtime: &mut Runtime<T>, now: Duration) -> Vec<Effect<T>> {
        match self {
            Self::Enter(enter) => {
                let allowed = if enter.streaming {
                    runtime.enter_stream(&enter.ticket.run)
                } else {
                    runtime.enter(&enter.ticket.run)
                };
                let _ = enter.reply.send(Ok(allowed.then_some(enter.ticket)));
                vec![]
            }
            Self::Completed(run, report) => {
                if let Some(start) = report.stream_start {
                    runtime.set_stream_start(&run, start);
                }
                runtime.complete(&run, report.outcome, now)
            }
            Self::Expired(deadline) => runtime.expire(&deadline, now),
            Self::Stream(update) => update.apply(runtime, now),
            Self::Conversation(update) => {
                update.reject();
                vec![]
            }
        }
    }
}

pub(crate) struct ExecutionIo<T> {
    executor: Arc<dyn Executor<T>>,
    events: broadcast::Sender<Observation>,
    notices: broadcast::Sender<ExecutionNotice>,
    enters: mpsc::Sender<Enter<T>>,
    enter_receiver: mpsc::Receiver<Enter<T>>,
    workers: JoinSet<Option<ExecutionReport>>,
    task_runs: HashMap<Id, Run>,
    tokens: HashMap<Run, CancellationToken>,
    permits: super::capacity::Pool,
    stream_permits: super::capacity::Pool,
    stream_updates: mpsc::Sender<Update>,
    stream_receiver: mpsc::Receiver<Update>,
    open_streams: HashSet<Run>,
    conversation_permits: Arc<Semaphore>,
    conversation_updates: mpsc::Sender<conversations::Update>,
    conversation_receiver: mpsc::Receiver<conversations::Update>,
    conversations: HashMap<Run, crate::conversations::ConversationHandle>,
    conversation_events: broadcast::Sender<Arc<ConversationEvent>>,
    deadlines: BTreeMap<(Duration, Run), Deadline>,
    due: HashMap<Run, Duration>,
    origin: Instant,
}
impl<T: Send + 'static> ExecutionIo<T> {
    pub fn with_capacity(
        executor: Arc<dyn Executor<T>>,
        capacity: super::ExecutionCapacity,
    ) -> Result<Self, DriverError> {
        let (events, _) = broadcast::channel(1024);
        let (notices, _) = broadcast::channel(1024);
        let (enters, enter_receiver) = mpsc::channel(256);
        let (stream_updates, stream_receiver) = mpsc::channel(32);
        let (conversation_updates, conversation_receiver) = mpsc::channel(32);
        let (conversation_events, _) = broadcast::channel(32);
        Ok(Self {
            executor,
            events,
            notices,
            enters,
            enter_receiver,
            workers: JoinSet::new(),
            task_runs: HashMap::new(),
            tokens: HashMap::new(),
            permits: capacity.calls,
            stream_permits: capacity.streams,
            stream_updates,
            stream_receiver,
            open_streams: HashSet::new(),
            conversation_permits: capacity.conversations,
            conversation_updates,
            conversation_receiver,
            conversations: HashMap::new(),
            conversation_events,
            deadlines: BTreeMap::new(),
            due: HashMap::new(),
            origin: Instant::now(),
        })
    }
    pub fn events(&self) -> broadcast::WeakSender<Observation> {
        self.events.downgrade()
    }
    pub fn notices(&self) -> broadcast::WeakSender<ExecutionNotice> {
        self.notices.downgrade()
    }
    pub fn conversations(&self) -> broadcast::WeakSender<Arc<ConversationEvent>> {
        self.conversation_events.downgrade()
    }
    pub fn input(
        &self,
        runtime: &Runtime<T>,
        node: &crate::graph::NodeId,
        run: &crate::runtime::RunId,
        bytes: Option<&[u8]>,
    ) -> Result<(), DriverError>
    where
        T: Clone,
    {
        let (_, handle) = self
            .conversations
            .iter()
            .find(|(active, _)| {
                active.node() == node && active.id() == run && runtime.accepts_live_io(active)
            })
            .ok_or(crate::conversations::ConversationError::Closed)?;
        match bytes {
            Some(bytes) => handle.send(bytes),
            None => handle.eof(),
        }
        .map_err(Into::into)
    }
    pub fn apply_input(
        &mut self,
        input: RuntimeInput<T>,
        runtime: &mut Runtime<T>,
    ) -> Vec<Effect<T>>
    where
        T: Clone,
    {
        match input {
            RuntimeInput::Conversation(update) => {
                self.conversation_update(update, runtime);
                vec![]
            }
            input => input.apply(runtime, self.now()),
        }
    }
    pub(crate) fn conversation_update(
        &mut self,
        update: conversations::Update,
        runtime: &Runtime<T>,
    ) where
        T: Clone,
    {
        match update {
            conversations::Update::Started { run, handle, reply } => {
                let accepted = runtime.accepts_live_io(&run)
                    && handle.snapshot().run == run
                    && !self.conversations.contains_key(&run);
                if accepted {
                    self.conversations.insert(run.clone(), handle);
                    let _ = self
                        .conversation_events
                        .send(Arc::new(ConversationEvent::Started(run)));
                }
                let _ = reply.send(accepted);
            }
            conversations::Update::Output { run, batch, reply } => {
                let accepted =
                    runtime.accepts_live_io(&run) && self.conversations.contains_key(&run);
                if accepted {
                    let _ = self
                        .conversation_events
                        .send(Arc::new(ConversationEvent::Output {
                            run,
                            batch: Arc::new(batch),
                        }));
                }
                let _ = reply.send(accepted);
            }
        }
    }
    pub fn active_conversations(
        &self,
        runtime: &Runtime<T>,
    ) -> indexmap::IndexMap<crate::graph::NodeId, crate::runtime::RunId>
    where
        T: Clone,
    {
        self.conversations
            .keys()
            .filter(|run| runtime.accepts_live_io(run))
            .map(|run| (run.node().clone(), run.id().clone()))
            .collect()
    }
    /// The session captures directly into its owned Log before issuing these lossy wakeups.
    pub fn announce_notices(&self, input: &RuntimeInput<T>) {
        for (run, error) in input.notices() {
            let _ = self.notices.send(ExecutionNotice {
                run: run.clone(),
                error: error.clone(),
            });
        }
    }
    pub fn now(&self) -> Duration {
        self.origin.elapsed()
    }
    pub fn is_idle(&self) -> bool {
        self.workers.len() == self.open_streams.len()
    }
    pub fn is_drained(&self) -> bool {
        self.workers.is_empty()
    }
    /// Cancellation-safe under an outer select: channels/task joins retain unread work. Monotonic
    /// deadlines are removed only when their guarded input is returned to the state owner.
    pub async fn next(&mut self) -> RuntimeInput<T> {
        loop {
            let delay = self
                .deadlines
                .first_key_value()
                .map(|((due, _), _)| due.saturating_sub(self.now()));
            tokio::select! {
                Some(enter) = self.enter_receiver.recv() => return RuntimeInput::Enter(enter),
                Some(update) = self.stream_receiver.recv() => return RuntimeInput::Stream(update),
                Some(update) = self.conversation_receiver.recv() => return RuntimeInput::Conversation(update),
                Some(completion) = self.workers.join_next_with_id(), if !self.workers.is_empty() => {
                    let (task, outcome) = match completion {
                        Ok((task, outcome)) => (task, outcome),
                        Err(error) => (error.id(), Some(Outcome::Failed(RuntimeCode::ExecutionFailed.error("The executor terminated unexpectedly.", None)).into())),
                    };
                    let run = self.task_runs.remove(&task).expect("each spawned worker has a run");
                    self.open_streams.remove(&run);
                    if self.conversations.remove(&run).is_some() {
                        let _ = self.conversation_events.send(Arc::new(ConversationEvent::Ended(run.clone())));
                    }
                    self.tokens.remove(&run); self.remove_deadline(&run);
                    let report = outcome.unwrap_or_else(|| Outcome::Failed(RuntimeCode::ExecutionFailed.error("Execution did not enter its worker.", None)).into());
                    return RuntimeInput::Completed(run, report);
                },
                () = wait_deadline(delay) => {
                    if self.deadlines.first_key_value().is_some_and(|((due,_),_)| *due <= self.now()) {
                        let ((_,run), deadline) = self.deadlines.pop_first().expect("due entry");
                        self.due.remove(&run);
                        return RuntimeInput::Expired(deadline);
                    }
                }
            }
        }
    }
    pub fn effects(&mut self, effects: Vec<Effect<T>>) {
        for effect in effects {
            match effect {
                Effect::Observe(observation) => {
                    let _ = self.events.send(observation);
                }
                Effect::Watch(deadline) => {
                    self.remove_deadline(deadline.run());
                    let due = deadline.due();
                    self.due.insert(deadline.run().clone(), due);
                    self.deadlines
                        .insert((due, deadline.run().clone()), deadline);
                }
                Effect::Cancel(run) => {
                    self.open_streams.remove(&run);
                    self.remove_deadline(&run);
                    if let Some(token) = self.tokens.get(&run) {
                        token.cancel();
                    }
                }
                Effect::Spawn(ticket) => self.dispatch(ticket),
                Effect::StreamReady { run, deadline } => {
                    self.open_streams.insert(run.clone());
                    self.remove_deadline(&run);
                    if let Some(deadline) = deadline {
                        self.due.insert(run.clone(), deadline.due());
                        self.deadlines.insert((deadline.due(), run), deadline);
                    }
                }
                Effect::StreamClosing(run) => {
                    self.open_streams.remove(&run);
                }
            }
        }
    }
    fn remove_deadline(&mut self, run: &Run) {
        if let Some(due) = self.due.remove(run) {
            self.deadlines.remove(&(due, run.clone()));
        }
    }
    fn dispatch(&mut self, ticket: RunTicket<T>) {
        let run = ticket.run.clone();
        let token = CancellationToken::new();
        self.tokens.insert(run.clone(), token.clone());
        let permits = self.permits.clone();
        let enters = self.enters.clone();
        let executor = self.executor.clone();
        let stream_permits = self.stream_permits.clone();
        let updates = self.stream_updates.clone();
        let conversation_permits = self.conversation_permits.clone();
        let conversation_updates = self.conversation_updates.clone();
        let execution = crate::diagnostics::Operation::start("execution");
        let queued = execution.child("queue");
        let task = self.workers.spawn(async move {
            let report = async {
            let streaming = executor.streaming(&ticket.payload);
            let interactive = executor.interactive(&ticket.payload);
            if streaming && interactive { return Some(Outcome::Failed(RuntimeCode::ExecutionFailed.error("A call cannot select both stream and conversation execution.", None)).into()); }
            let _conversation_permit = if interactive { match conversation_permits.try_acquire_owned() {
                Ok(permit) => Some(permit),
                Err(_) => return Some(Outcome::Failed(RuntimeCode::ExecutionFailed.error("The conversation capacity is exhausted; the provider was not entered.", None)).into()),
            }} else { None };
            // Reserve a separate lifetime slot BEFORE provider entry, without waiting behind an
            // unlimited set of open subscriptions. It is retained through physical cleanup.
            let _stream_permit = if streaming { match stream_permits.try_acquire_owned() {
                Ok(permit) => Some(permit),
                Err(_) => return Some(Outcome::Failed(RuntimeCode::ExecutionFailed.error("The stream capacity is exhausted; the provider was not entered.", None)).into()),
            }} else { None };
            let permit = tokio::select! {
                biased;
                () = token.cancelled() => return None,
                permit = permits.acquire_owned() => permit.expect("driver never closes permit pool"),
            };
            let (reply, receive) = oneshot::channel();
            if enters.send(Enter { ticket, reply, streaming }).await.is_err() {
                return None;
            }
            let ticket = match receive.await.ok()? {
                Ok(Some(ticket)) => ticket,
                Ok(None) => return None,
                Err(error) => return Some(Outcome::Failed(error).into()),
            };
            if token.is_cancelled() { return None; }
            queued.finish("ok");
            if streaming {
                Some(streaming::execute(executor, ticket, token, permit, updates).await)
            } else if interactive {
                // Human interaction retains ordinary concurrency as well as its bounded physical
                // conversation slot. Neither is released by cancellation or an output update.
                let report = conversations::execute(executor, ticket, token, conversation_updates).await;
                drop(permit);
                Some(report)
            } else { Some(executor.execute(ticket, token).await) }
            }.instrument(execution.span()).await;
            execution.finish(match report.as_ref().map(|r: &ExecutionReport| &r.outcome) { Some(Outcome::Produced(_)) => "ok", Some(Outcome::Skipped) => "skipped", Some(Outcome::Failed(_)) => "error", Some(Outcome::Cancelled(_)) | None => "cancelled" });
            report
        });
        self.task_runs.insert(task.id(), run);
    }
}
async fn wait_deadline(delay: Option<Duration>) {
    match delay {
        // Avoid platform Instant overflow for long budgets; waking early does not expire them.
        Some(delay) => tokio::time::sleep(delay.min(Duration::from_secs(86_400))).await,
        None => pending().await,
    }
}
