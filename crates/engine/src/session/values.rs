//! Serial publication follows physical storage and retention receipts, never task completion order.
use super::{RecordingMode, SessionError};
use crate::{
    graph::{NodeId, NodeState},
    history::{
        AppendReceipt, JournalEntry, NoticeContext, Persistence, Record, RequiredPersistence,
        RetainedResult,
    },
    recording::Recorder,
    runtime::{Observation, RunId, RuntimeCode},
    storage::{
        AutoKeep, EvictionBatch, PublicationPolicy, StoreError, StoreWorker, StoredOutput,
        ValueHandle,
    },
    value_size::value_charge,
    workspace::Workspace,
};
use indexmap::IndexMap;
use std::{collections::VecDeque, num::NonZeroUsize, sync::Arc};
use tokio::{
    sync::{broadcast, oneshot},
    task::JoinSet,
};
use wes_core::{ErrorValue, Value};

fn max_pending() -> usize {
    wes_budgets::get("values.pending") as usize
}
fn max_bytes() -> u64 {
    wes_budgets::get("values.pending.bytes") as u64
}
fn max_outputs() -> usize {
    wes_budgets::get("values.outputs") as usize
}

#[derive(Clone)]
pub struct SessionStorage {
    pub worker: StoreWorker,
    pub auto_keep: AutoKeep,
}
/// Session preference, not a request to archive past outputs. Disabling preserves the threshold.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RetentionPolicy {
    pub automatic: bool,
    pub under: u64,
}
impl From<AutoKeep> for RetentionPolicy {
    fn from(policy: AutoKeep) -> Self {
        match policy {
            AutoKeep::Never => Self {
                automatic: false,
                under: wes_budgets::get("storage.keep.bytes"),
            },
            AutoKeep::UpToBytes(under) => Self {
                automatic: true,
                under,
            },
        }
    }
}
impl RetentionPolicy {
    fn finite(self) -> AutoKeep {
        if self.automatic {
            AutoKeep::UpToBytes(self.under)
        } else {
            AutoKeep::Never
        }
    }
}
#[derive(Clone, Debug)]
pub enum ValuePublication {
    Pending { run: RunId },
    Complete(PublishedValue),
    Recovered(RecoveredValue),
}
impl ValuePublication {
    pub fn run(&self) -> &RunId {
        match self {
            Self::Pending { run } => run,
            Self::Complete(value) => &value.run,
            Self::Recovered(value) => &value.run,
        }
    }
    pub fn handle(&self) -> Option<&ValueHandle> {
        match self {
            Self::Pending { .. } => None,
            Self::Complete(value) => value
                .stored
                .as_ref()
                .map(|stored| &stored.handle)
                .or(value.uncertain_handle.as_ref()),
            Self::Recovered(value) => Some(&value.handle),
        }
    }
}
/// Retained bytes read at startup, not a new publication or archive synchronization receipt.
#[derive(Clone, Debug)]
pub struct RecoveredValue {
    pub run: RunId,
    pub handle: ValueHandle,
    pub bytes: u64,
    pub retention: crate::storage::Retention,
    pub journal_checkpoint: AppendReceipt,
}
#[derive(Clone, Debug)]
pub enum PinBinding {
    Pending,
    Bound,
    Refused(String),
}
#[derive(Clone, Debug)]
pub struct PublishedValue {
    /// Binding has its own outcome; refusal never erases an acknowledged retained result.
    pub pin: Option<PinBinding>,
    pub run: RunId,
    pub stored: Option<StoredOutput>,
    /// Present only for a kept result whose history acknowledgement satisfies session policy.
    pub journal: Option<AppendReceipt>,
    pub problem: Option<ErrorValue>,
    /// Storage reported possible publication but did not acknowledge the complete operation.
    pub uncertain_handle: Option<ValueHandle>,
}
impl PublishedValue {
    pub fn durably_retained(&self) -> bool {
        self.problem.is_none()
            && self.stored.as_ref().is_some_and(|stored| {
                stored.kept && stored.retained_persistence != Persistence::Volatile
            })
            && self
                .journal
                .is_some_and(|receipt| receipt.persistence != Persistence::Volatile)
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StorageCommand {
    Keep,
    KeepAutomatic,
    Release,
}
#[derive(Clone, Debug)]
pub struct RetainedAcknowledgement {
    pub node: NodeId,
    pub run: RunId,
    pub receipt: AppendReceipt,
}
#[derive(Clone, Debug)]
pub struct StorageReceipt {
    pub handle: ValueHandle,
    pub command: StorageCommand,
    /// Acknowledged keep or at least one removed copy. False with no problem is a release no-op.
    pub affected: bool,
    pub stored: Option<StoredOutput>,
    /// Individual successful Result receipts survive a later failure in the same request.
    pub journal: Vec<RetainedAcknowledgement>,
    pub problem: Option<ErrorValue>,
    /// Relevant on failure; never a rollback or remote-outcome claim.
    pub may_have_applied: bool,
}
pub type StorageReply = Result<Arc<StorageReceipt>, SessionError>;
pub(super) struct StorageNotice {
    pub context: NoticeContext,
    pub error: ErrorValue,
}
pub(crate) struct KeptPin {
    pub(crate) node: NodeId,
    pub(crate) run: RunId,
    pub(crate) handle: ValueHandle,
    pub(crate) intent: crate::views::PinIntent,
}
pub(super) struct ValueEffects {
    pub pins: Vec<KeptPin>,
    pub effects: Vec<crate::runtime::Effect<crate::tasks::BoundTask>>,
    pub notices: Vec<StorageNotice>,
}
#[derive(Clone, Debug)]
pub struct ValueSnapshot {
    /// Current successes and explicitly stopped display observations. Graph state remains separate;
    /// refreshing/dropping a node invalidates this live projection.
    pub outputs: IndexMap<NodeId, ValuePublication>,
    pub pending: usize,
    pub failures: u64,
    pub policy: RetentionPolicy,
}
struct Publication {
    pin: Option<crate::views::PinIntent>,
    node: NodeId,
    run: RunId,
    value: Value,
    charge: u64,
    policy: PublicationPolicy,
    version: Arc<()>,
    streaming: bool,
    previous: Option<ValueHandle>,
}
struct Origin {
    live: Option<crate::history::LiveSnapshot>,
    node: NodeId,
    run: RunId,
}
struct StorageRequest {
    command: StorageCommand,
    handle: ValueHandle,
    origins: Vec<Origin>,
    reply: oneshot::Sender<StorageReply>,
    charge: u64,
}
// Payload requests remain capped at max_pending(). Cleanup jobs contain only existing UUID handles:
// at most max_outputs() detached live entries plus two handles per already admitted publication.
// They stay in this same queue so cleanup never jumps ahead of an accepted Keep/Release.
enum Waiting {
    Discard(Retired),
    Publish(Publication),
    Storage(StorageRequest),
}
struct Version {
    live: Option<crate::history::LiveSnapshot>,
    stopped: bool,
    private: bool,
    token: Arc<()>,
    streaming: bool,
}
struct Retired {
    handle: ValueHandle,
}
enum Completed {
    Discarded {
        retired: Retired,
        result: Result<bool, StoreError>,
    },
    Storage {
        request: StorageRequest,
        receipt: StorageReceipt,
        recording_failed: bool,
    },
    Published {
        pin: Option<crate::views::PinIntent>,
        node: NodeId,
        result: PublishedValue,
        charge: u64,
        recording_failed: bool,
        version: Arc<()>,
        streaming: bool,
        previous: Option<ValueHandle>,
    },
    Evicted(Result<EvictionBatch, StoreError>),
}
#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum StreamRetention {
    Ready,
    Pending,
    Failed,
}
pub(super) struct SessionValues {
    pins: IndexMap<NodeId, (RunId, crate::views::PinIntent, u64)>,
    storage: StoreWorker,
    recording: Option<(Recorder, RequiredPersistence)>,
    outputs: IndexMap<NodeId, ValuePublication>,
    versions: IndexMap<NodeId, Version>,
    queued: VecDeque<Waiting>,
    active: JoinSet<Completed>,
    bytes: u64,
    failures: u64,
    recording_failed: bool,
    evictions_due: bool,
    updates: broadcast::Sender<()>,
    policy: RetentionPolicy,
}
impl SessionValues {
    pub fn prepare_pin(
        &mut self,
        node: NodeId,
        run: RunId,
        intent: crate::views::PinIntent,
    ) -> Result<(), &'static str> {
        if self.recording.is_none() || self.recording_failed {
            return Err("Pin requires healthy durable workspace recording");
        }
        if intent.principal.is_none() {
            return Err("Pin execution authority is unavailable");
        }
        if self.outputs.len() + self.pins.len() >= max_outputs()
            && !self.outputs.contains_key(&node)
        {
            return Err("Pin result metadata capacity is full");
        }
        if self.pins.contains_key(&node)
            || self.pins.len() + self.queued.len() + self.active.len() >= max_pending()
        {
            return Err("Pin continuation capacity is full");
        }
        let charge = value_charge(&intent.value, max_bytes().saturating_sub(self.bytes))
            .ok_or("Pin input exceeds pending storage capacity")?;
        self.bytes += charge;
        self.pins.insert(node, (run, intent, charge));
        let _ = self.updates.send(());
        Ok(())
    }
    fn take_pin(&mut self, node: &NodeId) -> Option<(RunId, crate::views::PinIntent)> {
        self.pins.swap_remove(node).map(|(run, intent, charge)| {
            self.bytes -= charge;
            (run, intent)
        })
    }
    pub fn finish_pin(&mut self, pin: &KeptPin, result: Result<(), String>) {
        if let Some(ValuePublication::Complete(value)) = self.outputs.get_mut(&pin.node)
            && value.run == pin.run
            && value
                .stored
                .as_ref()
                .is_some_and(|stored| stored.handle == pin.handle)
        {
            value.pin = Some(match result {
                Ok(()) => PinBinding::Bound,
                Err(problem) => PinBinding::Refused(problem),
            });
        }
        let _ = self.updates.send(());
    }
    /// Capture selected identities at acceptance, then serialize with publications and their
    /// journal receipts. Caller disconnect does not retract an admitted storage mutation.
    pub fn request(
        &mut self,
        command: StorageCommand,
        handle: ValueHandle,
        reply: oneshot::Sender<StorageReply>,
    ) {
        if self.pins.len() + self.queued.len() + self.active.len() >= max_pending() {
            let _ = reply.send(Err(SessionError::Capacity));
            return;
        }
        let mut origins = vec![];
        let mut charge = 1024u64;
        for (node, output) in &self.outputs {
            if output.handle() != Some(&handle) {
                continue;
            }
            charge = charge
                .saturating_add(256)
                .saturating_add((node.as_str().len() as u64).saturating_mul(6))
                .saturating_add((output.run().as_str().len() as u64).saturating_mul(6));
            if charge > max_bytes().saturating_sub(self.bytes) {
                let _ = reply.send(Err(SessionError::Capacity));
                return;
            }
            origins.push(Origin {
                live: self.versions.get(node).and_then(|v| v.live.clone()),
                node: node.clone(),
                run: output.run().clone(),
            });
        }
        if command == StorageCommand::Keep && origins.is_empty() {
            let _ = reply.send(Err(SessionError::UnknownValue));
            return;
        }
        if charge > max_bytes().saturating_sub(self.bytes) {
            let _ = reply.send(Err(SessionError::Capacity));
            return;
        }
        self.bytes += charge;
        self.queued.push_back(Waiting::Storage(StorageRequest {
            command,
            handle,
            origins,
            reply,
            charge,
        }));
        self.pump();
        let _ = self.updates.send(());
    }
    pub fn restore(
        &mut self,
        node: NodeId,
        value: RecoveredValue,
    ) -> Result<(), super::SessionError> {
        if self.outputs.len() >= max_outputs() {
            return Err(super::SessionError::Capacity);
        }
        self.outputs
            .insert(node, ValuePublication::Recovered(value));
        Ok(())
    }
    pub fn new(storage: SessionStorage, mode: &RecordingMode) -> Self {
        Self {
            policy: storage.auto_keep.into(),
            storage: storage.worker,
            recording: match mode {
                RecordingMode::Ephemeral => None,
                RecordingMode::Required(journal) => Some(journal.recording()),
            },
            pins: IndexMap::new(),
            outputs: IndexMap::new(),
            versions: IndexMap::new(),
            queued: VecDeque::new(),
            active: JoinSet::new(),
            bytes: 0,
            failures: 0,
            recording_failed: false,
            evictions_due: false,
            updates: broadcast::channel(1).0,
        }
    }
    pub fn updates(&self) -> broadcast::WeakSender<()> {
        self.updates.downgrade()
    }
    pub fn set_policy(&mut self, policy: RetentionPolicy) {
        if self.policy != policy {
            self.policy = policy;
            let _ = self.updates.send(());
        }
    }
    /// Snapshot/clean-stop retention, after entered publications settle. Each acknowledged current
    /// stream window is kept once, independently of the finite automatic retention preference.
    /// Already failed publication/keep is not silently retried. Entered requests remain owned here.
    pub fn retain_stream_windows(&mut self) -> StreamRetention {
        if !self.is_idle() {
            return StreamRetention::Pending;
        }
        let mut handles = vec![];
        let mut failed = false;
        for (node, output) in &self.outputs {
            if !self
                .versions
                .get(node)
                .is_some_and(|version| version.streaming && !version.private && !version.stopped)
            {
                continue;
            }
            match output {
                ValuePublication::Complete(value) if value.problem.is_none() => {
                    match &value.stored {
                        Some(stored) if stored.kept => {}
                        Some(stored) => handles.push(stored.handle.clone()),
                        None => failed = true,
                    }
                }
                ValuePublication::Recovered(_) => {}
                _ => failed = true,
            }
        }
        if handles.is_empty() {
            return if failed {
                StreamRetention::Failed
            } else {
                StreamRetention::Ready
            };
        }
        for handle in handles.into_iter().take(max_pending()) {
            // There is no caller-owned receipt; this coordinator still joins the operation and
            // records any failure/notices before reporting idle or completing shutdown.
            let (reply, _) = oneshot::channel();
            self.request(StorageCommand::KeepAutomatic, handle, reply);
        }
        StreamRetention::Pending
    }
    pub fn retained_live(&self, snapshot: &crate::history::LiveSnapshot) -> bool {
        matches!(self.outputs.get(snapshot.observation.node()), Some(ValuePublication::Complete(value))
            if Some(&value.run) == snapshot.observation.run()
                && value.problem.is_none() && value.stored.as_ref().is_some_and(|s| s.kept))
    }
    pub fn snapshot(&self) -> ValueSnapshot {
        ValueSnapshot {
            outputs: self.outputs.clone(),
            pending: self.pins.len() + self.queued.len() + self.active.len(),
            failures: self.failures,
            policy: self.policy,
        }
    }
    pub fn is_idle(&self) -> bool {
        self.pins.is_empty() && self.active.is_empty() && self.queued.is_empty()
    }
    pub fn has_completion(&self) -> bool {
        !self.active.is_empty()
    }
    pub fn retire_private_outputs(&mut self) {
        let private: Vec<_> = self
            .versions
            .iter()
            .filter(|(_, v)| v.private)
            .map(|(n, _)| n.clone())
            .collect();
        for node in private {
            self.versions.swap_remove(&node);
            if let Some(output) = self.outputs.swap_remove(&node)
                && let Some(handle) = output.handle()
            {
                self.queued.push_back(Waiting::Discard(Retired {
                    handle: handle.clone(),
                }));
            }
        }
        self.pump();
    }
    pub fn recording_failed(&self) -> bool {
        self.recording_failed
    }
    pub fn retain_nodes(&mut self, workspace: &Workspace) {
        for (node, output) in &self.outputs {
            if workspace.runtime().graph().node(node).is_none()
                && self
                    .versions
                    .get(node)
                    .is_some_and(|version| version.streaming || version.private)
                && let Some(handle) = output.handle()
            {
                self.queued.push_back(Waiting::Discard(Retired {
                    handle: handle.clone(),
                }));
            }
        }
        let retired = self
            .pins
            .keys()
            .filter(|node| workspace.runtime().graph().node(node).is_none())
            .cloned()
            .collect::<Vec<_>>();
        for node in retired {
            self.take_pin(&node);
        }
        self.outputs
            .retain(|node, _| workspace.runtime().graph().node(node).is_some());
        self.versions
            .retain(|node, _| self.outputs.contains_key(node));
        self.pump();
        let _ = self.updates.send(());
    }
    #[cfg(test)]
    pub fn observe(&mut self, observation: &Observation, streaming: bool) -> Option<StorageNotice> {
        self.observe_with_retention(observation, streaming, !streaming, None)
    }
    pub fn observe_live(
        &mut self,
        observation: &Observation,
        streaming: bool,
        live: Option<crate::history::LiveSnapshot>,
    ) -> Option<StorageNotice> {
        self.observe_with_retention(observation, streaming, !streaming, live)
    }
    pub fn observe_interactive(&mut self, observation: &Observation) -> Option<StorageNotice> {
        // A final conversation value is neither an automatically kept finite value nor a stream
        // window selected for checkpoint/clean-stop retention. Explicit Keep remains available.
        self.observe_with_retention(observation, false, false, None)
    }
    fn observe_with_retention(
        &mut self,
        observation: &Observation,
        streaming: bool,
        automatic: bool,
        live: Option<crate::history::LiveSnapshot>,
    ) -> Option<StorageNotice> {
        let stopped = observation.stopped.is_some();
        // Select the display value for publication only. The original observation is still what
        // the execution log records; this does not promote Cancelled/Skipped to graph Ready.
        let snapshot;
        let observation = if let Some(last) = &observation.stopped {
            snapshot = Observation {
                revision: 0,
                stale_reason: None,
                delivery: None,
                node: observation.node.clone(),
                run: Some(last.run.clone()),
                state: NodeState::Ready,
                value: Some(last.value.clone()),
                error: None,
                stopped: None,
            };
            &snapshot
        } else {
            observation
        };
        let automatic = automatic && !stopped;
        if observation.state != NodeState::Ready {
            self.take_pin(&observation.node);
            let private = self
                .versions
                .swap_remove(&observation.node)
                .is_some_and(|v| v.private);
            if let Some(output) = self.outputs.swap_remove(&observation.node) {
                if (streaming || private)
                    && let Some(handle) = output.handle()
                {
                    self.queued.push_back(Waiting::Discard(Retired {
                        handle: handle.clone(),
                    }));
                    self.pump();
                }
                let _ = self.updates.send(());
            }
            return None;
        }
        let (Some(run), Some(value)) = (&observation.run, &observation.value) else {
            return None;
        };
        let pin = self
            .take_pin(&observation.node)
            .filter(|(captured, _)| captured == run)
            .map(|(_, intent)| intent);
        if stopped && let Some(version) = self.versions.get_mut(&observation.node) {
            version.stopped = true;
        }
        if (stopped || !streaming)
            && self
                .outputs
                .get(&observation.node)
                .is_some_and(|output| output.run() == run)
        {
            return None;
        }
        if self.outputs.len() + self.pins.len() >= max_outputs()
            && !self.outputs.contains_key(&observation.node)
        {
            self.failures = self.failures.saturating_add(1);
            let _ = self.updates.send(());
            return Some(StorageNotice {
                context: NoticeContext::Publication {
                    node: observation.node.clone(),
                    run: run.clone(),
                    handle: None,
                },
                error: RuntimeCode::RecordingFailed.error(
                    "Result metadata capacity is full; the execution result was not changed.",
                    None,
                ),
            });
        }
        // At most one not-yet-entered window per node. Carry the replaced handle across coalescing
        // so it is reclaimed only through the serialized storage workflow, never forgotten.
        let mut previous = streaming
            .then(|| {
                self.outputs
                    .get(&observation.node)
                    .and_then(ValuePublication::handle)
                    .cloned()
            })
            .flatten();
        if streaming && let Some(index) = self.queued.iter().position(|waiting| matches!(waiting,
            Waiting::Publish(publication) if publication.streaming && publication.node == observation.node)) {
            let Waiting::Publish(obsolete) = self.queued.remove(index).expect("queued publication") else { unreachable!() };
            self.bytes -= obsolete.charge;
            previous = previous.or(obsolete.previous);
        }
        let version = Arc::new(());
        self.versions.insert(
            observation.node.clone(),
            Version {
                live,
                stopped,
                private: value.provenance().policy().is_private(),
                token: version.clone(),
                streaming,
            },
        );
        let mut notice = None;
        let charge = value_charge(value, max_bytes().saturating_sub(self.bytes));
        if let Some(charge) = charge
            .filter(|_| self.pins.len() + self.queued.len() + self.active.len() < max_pending())
        {
            self.bytes += charge;
            self.outputs.insert(
                observation.node.clone(),
                ValuePublication::Pending { run: run.clone() },
            );
            self.queued.push_back(Waiting::Publish(Publication {
                node: observation.node.clone(),
                run: run.clone(),
                value: value.clone(),
                charge,
                policy: if pin.is_some() {
                    PublicationPolicy::Protected
                } else if automatic {
                    self.policy.finite().into()
                } else {
                    PublicationPolicy::Temporary
                },
                pin,
                version,
                streaming: streaming || value.provenance().policy().is_private(),
                previous,
            }));
            self.pump();
        } else {
            self.failures = self.failures.saturating_add(1);
            let mut result = failed(
                run.clone(),
                "Result storage capacity is full; the execution result was not changed.",
                None,
            );
            if pin.is_some() {
                result.pin = Some(PinBinding::Refused(
                    "Pin input could not be retained within storage capacity; view was unchanged"
                        .into(),
                ));
            }
            if let Some(handle) = previous {
                self.queued.push_back(Waiting::Discard(Retired { handle }));
            }
            self.pump();
            notice = Some(publication_notice(&observation.node, &result));
            self.outputs
                .insert(observation.node.clone(), ValuePublication::Complete(result));
        }
        let _ = self.updates.send(());
        notice
    }
    fn pump(&mut self) {
        if !self.active.is_empty() {
            return;
        }
        let store = self.storage.clone();
        if self.evictions_due {
            self.evictions_due = false;
            self.active.spawn(async move {
                Completed::Evicted(store.take_evicted(NonZeroUsize::new(1024).unwrap()).await)
            });
        } else if let Some(waiting) = self.queued.pop_front() {
            let recording = self.recording.clone();
            self.active.spawn(async move {
                let waiting = match waiting {
                    Waiting::Discard(retired) => {
                        let result = store.release_unkept(retired.handle.clone()).await;
                        return Completed::Discarded { retired, result };
                    }
                    Waiting::Storage(request) => return storage_request(store, recording, request).await,
                    Waiting::Publish(publication) => publication,
                };
                let Publication { node, run, value, charge, policy, version, streaming, previous, pin } = waiting;
                let mut recording_failed = false;
                let result = match store.publish(value, policy).await {
                    Ok(stored) => {
                        let mut result = PublishedValue { pin: pin.as_ref().map(|_|PinBinding::Pending), run: run.clone(), stored: Some(stored.clone()), journal: None, problem: None, uncertain_handle: None };
                        if stored.kept {
                            match acknowledge_retained(recording.as_ref(), &node, &run, &stored, None).await {
                                Ok(receipt) => result.journal = receipt,
                                Err(error) => {
                                    recording_failed = true;
                                    result.problem = Some(error);
                                }
                            }
                        } else if !streaming && let Some((recorder, required)) = &recording {
                            // Finite public Temporary bytes need ownership for later
                            // explicit work deletion, not a false retention receipt.
                            let owned = Record::Journal(JournalEntry::Payload { node: node.clone(), run: run.clone(), handle: stored.handle.clone() });
                            if !recorder.append(Arc::new(owned)).await.is_ok_and(|receipt| required.accepts(receipt.persistence)) {
                                recording_failed = true;
                                result.problem = Some(RuntimeCode::RecordingFailed.error("Result ownership was not acknowledged; cleanup evidence may be incomplete.", None));
                            }
                        }
                        result
                    }
                    Err(error) => {
                        let uncertain = match error { StoreError::Published { handle, .. } => Some(handle), _ => None };
                        failed(run, "Result storage could not be confirmed; the execution result was not changed.", uncertain)
                    }
                };
                Completed::Published { node, result, charge, recording_failed, version, streaming, previous, pin }
            });
        }
    }
    /// Applies only run/handle-authorized evictions. Late storage completion cannot resurrect a node.
    pub async fn completed(&mut self, workspace: &mut Workspace) -> ValueEffects {
        let mut effects = vec![];
        let mut notices = vec![];
        let mut pins = vec![];
        match self.active.join_next().await {
            Some(Ok(Completed::Discarded {
                retired,
                result: Err(error),
            })) => {
                self.failures = self.failures.saturating_add(1);
                notices.push(StorageNotice {
                    context: NoticeContext::Release {
                        handle: retired.handle,
                        may_have_applied: matches!(
                            error,
                            StoreError::Released { .. } | StoreError::Closed
                        ),
                    },
                    error: RuntimeCode::RecordingFailed.error(
                        "A superseded stream window could not be reclaimed; no retry was made.",
                        None,
                    ),
                });
            }
            Some(Ok(Completed::Discarded { result: Ok(_), .. })) => {}
            Some(Ok(Completed::Storage {
                request,
                receipt,
                recording_failed,
            })) => {
                self.bytes -= request.charge;
                self.recording_failed |= recording_failed;
                if let Some(error) = &receipt.problem {
                    self.failures = self.failures.saturating_add(1);
                    let context = match request.command {
                        StorageCommand::Keep | StorageCommand::KeepAutomatic => {
                            NoticeContext::Keep {
                                handle: request.handle.clone(),
                                may_have_applied: receipt.may_have_applied,
                            }
                        }
                        StorageCommand::Release => NoticeContext::Release {
                            handle: request.handle.clone(),
                            may_have_applied: receipt.may_have_applied,
                        },
                    };
                    notices.push(StorageNotice {
                        context,
                        error: error.clone(),
                    });
                }
                for origin in &request.origins {
                    let matches = self.outputs.get(&origin.node).is_some_and(|output| {
                        output.handle() == Some(&request.handle) && output.run() == &origin.run
                    }) && workspace.runtime().run_of(&origin.node)
                        == Some(&origin.run);
                    if !matches {
                        continue;
                    }
                    if request.command == StorageCommand::Release && receipt.problem.is_none() {
                        self.outputs.swap_remove(&origin.node);
                        self.versions.swap_remove(&origin.node);
                        effects.extend(workspace.forget_output(&origin.node, Some(&origin.run)));
                    } else {
                        self.outputs.insert(
                            origin.node.clone(),
                            ValuePublication::Complete(PublishedValue {
                                pin: None,
                                run: origin.run.clone(),
                                stored: receipt.stored.clone(),
                                journal: receipt
                                    .journal
                                    .iter()
                                    .find(|ack| ack.node == origin.node && ack.run == origin.run)
                                    .map(|ack| ack.receipt),
                                problem: receipt.problem.clone(),
                                uncertain_handle: receipt
                                    .stored
                                    .is_none()
                                    .then(|| request.handle.clone()),
                            }),
                        );
                    }
                }
                let _ = request.reply.send(Ok(Arc::new(receipt)));
                self.evictions_due = true;
            }
            Some(Ok(Completed::Published {
                pin,
                node,
                result,
                charge,
                recording_failed,
                version,
                streaming,
                previous,
            })) => {
                self.bytes -= charge;
                self.recording_failed |= recording_failed;
                if result.problem.is_some() {
                    self.failures = self.failures.saturating_add(1);
                    notices.push(publication_notice(&node, &result));
                }
                let current = self
                    .versions
                    .get(&node)
                    .is_some_and(|wanted| Arc::ptr_eq(&wanted.token, &version))
                    && self
                        .outputs
                        .get(&node)
                        .is_some_and(|output| output.run() == &result.run)
                    && (if self
                        .versions
                        .get(&node)
                        .is_some_and(|version| version.stopped)
                    {
                        workspace
                            .runtime()
                            .stopped_value(&node)
                            .map(|value| &value.run)
                            == Some(&result.run)
                    } else {
                        workspace.runtime().run_of(&node) == Some(&result.run)
                            && workspace
                                .runtime()
                                .graph()
                                .node(&node)
                                .is_some_and(|node| node.state() == NodeState::Ready)
                    });
                if streaming {
                    if let Some(handle) = previous {
                        self.queued.push_back(Waiting::Discard(Retired { handle }));
                    }
                    if !current
                        && let Some(handle) = result
                            .stored
                            .as_ref()
                            .map(|stored| &stored.handle)
                            .or(result.uncertain_handle.as_ref())
                    {
                        self.queued.push_back(Waiting::Discard(Retired {
                            handle: handle.clone(),
                        }));
                    }
                }
                let mut result = result;
                if let Some(intent) = pin {
                    if current
                        && result.durably_retained()
                        && result.stored.as_ref().is_some_and(|stored| {
                            stored.retention == crate::storage::Retention::Protected
                        })
                    {
                        pins.push(KeptPin {
                            node: node.clone(),
                            run: result.run.clone(),
                            handle: result
                                .stored
                                .as_ref()
                                .expect("retained storage")
                                .handle
                                .clone(),
                            intent,
                        });
                    } else {
                        result.pin = Some(PinBinding::Refused("Protected Pin retention or current run could not be confirmed; view binding was unchanged".into()));
                        if result.problem.is_none() {
                            notices.push(StorageNotice {context:NoticeContext::Publication {node:node.clone(),run:result.run.clone(),handle:result.stored.as_ref().map(|stored|stored.handle.clone())},error:RuntimeCode::RecordingFailed.error("Input storage completed, but protected Pin binding was not confirmed; no retry was made",None)});
                        }
                    }
                }
                if current {
                    self.outputs
                        .insert(node, ValuePublication::Complete(result));
                }
                self.evictions_due = true;
            }
            Some(Ok(Completed::Evicted(Ok(batch)))) => {
                let handles: std::collections::HashSet<_> = batch.handles.into_iter().collect();
                let evicted: Vec<_> = self
                    .outputs
                    .iter()
                    .filter_map(|(node, output)| {
                        output
                            .handle()
                            .filter(|handle| handles.contains(handle))
                            .map(|_| (node.clone(), output.run().clone()))
                    })
                    .collect();
                for (node, run) in evicted {
                    self.outputs.swap_remove(&node);
                    self.versions.swap_remove(&node);
                    effects.extend(workspace.forget_output(&node, Some(&run)));
                }
                self.evictions_due = batch.more;
            }
            Some(Ok(Completed::Evicted(Err(_)))) => {
                // The store preserves notices on failure. No tight retry loop; retry after the next
                // publication, with failure visible even if there is no later publication.
                self.failures = self.failures.saturating_add(1);
                notices.push(StorageNotice { context: NoticeContext::Eviction, error: RuntimeCode::RecordingFailed.error("Storage eviction notices could not be read; remaining notices were not discarded.", None) });
            }
            Some(Err(_)) => {
                self.failures = self.failures.saturating_add(1);
                self.recording_failed = self.recording.is_some();
                notices.push(StorageNotice { context: NoticeContext::StorageWorker, error: RuntimeCode::RecordingFailed.error("The result storage workflow terminated unexpectedly; completion is uncertain.", None) });
            }
            None => {}
        }
        self.pump();
        let _ = self.updates.send(());
        ValueEffects {
            effects,
            notices,
            pins,
        }
    }
}
fn publication_notice(node: &NodeId, result: &PublishedValue) -> StorageNotice {
    StorageNotice {
        context: NoticeContext::Publication {
            node: node.clone(),
            run: result.run.clone(),
            handle: result
                .stored
                .as_ref()
                .map(|stored| stored.handle.clone())
                .or_else(|| result.uncertain_handle.clone()),
        },
        error: result
            .problem
            .clone()
            .expect("publication failure has a problem"),
    }
}
fn failed(
    run: RunId,
    message: &'static str,
    uncertain_handle: Option<ValueHandle>,
) -> PublishedValue {
    PublishedValue {
        pin: None,
        run,
        stored: None,
        journal: None,
        problem: Some(RuntimeCode::RecordingFailed.error(message, None)),
        uncertain_handle,
    }
}

async fn storage_request(
    store: StoreWorker,
    recording: Option<(Recorder, RequiredPersistence)>,
    request: StorageRequest,
) -> Completed {
    let mut receipt = StorageReceipt {
        handle: request.handle.clone(),
        command: request.command,
        affected: false,
        stored: None,
        journal: vec![],
        problem: None,
        may_have_applied: false,
    };
    let mut recording_failed = false;
    match request.command {
        StorageCommand::Release => match store.release(request.handle.clone()).await {
            Ok(removed) => receipt.affected = removed,
            Err(error) => {
                receipt.may_have_applied =
                    matches!(error, StoreError::Released { .. } | StoreError::Closed);
                receipt.problem = Some(RuntimeCode::RecordingFailed.error(
                    "Result release could not be confirmed; stored copies may have changed.",
                    None,
                ));
            }
        },
        StorageCommand::Keep | StorageCommand::KeepAutomatic => match store
            .retain_as(
                request.handle.clone(),
                if request.command == StorageCommand::Keep {
                    crate::storage::Retention::Protected
                } else {
                    crate::storage::Retention::Automatic
                },
            )
            .await
        {
            Err(error) => {
                receipt.may_have_applied =
                    matches!(error, StoreError::Published { .. } | StoreError::Closed);
                receipt.problem = Some(RuntimeCode::RecordingFailed.error(
                    "Result retention could not be confirmed; no automatic retry was made.",
                    None,
                ));
            }
            Ok(stored) => {
                receipt.affected = true;
                receipt.stored = Some(stored.clone());
                for origin in &request.origins {
                    match acknowledge_retained(
                        recording.as_ref(),
                        &origin.node,
                        &origin.run,
                        &stored,
                        origin.live.as_ref(),
                    )
                    .await
                    {
                        Ok(Some(ack)) => receipt.journal.push(RetainedAcknowledgement {
                            node: origin.node.clone(),
                            run: origin.run.clone(),
                            receipt: ack,
                        }),
                        Ok(None) => {}
                        Err(error) => {
                            recording_failed = true;
                            receipt.may_have_applied = true;
                            receipt.problem = Some(error);
                            break;
                        }
                    }
                }
            }
        },
    }
    Completed::Storage {
        request,
        receipt,
        recording_failed,
    }
}

/// Automatic and explicit retention share the same archive-before-history policy gate.
async fn acknowledge_retained(
    recording: Option<&(Recorder, RequiredPersistence)>,
    node: &NodeId,
    run: &RunId,
    stored: &StoredOutput,
    live: Option<&crate::history::LiveSnapshot>,
) -> Result<Option<AppendReceipt>, ErrorValue> {
    let Some((recorder, required)) = recording else {
        return Ok(None);
    };
    if !required.accepts(stored.retained_persistence) {
        return Err(RuntimeCode::RecordingFailed.error(
            "The retained value does not satisfy the required persistence policy.",
            None,
        ));
    }
    let result = RetainedResult {
        retention: stored.retention,
        node: node.clone(),
        run: run.clone(),
        handle: stored.handle.clone(),
    };
    let entry = if let Some(snapshot) = live {
        let mut snapshot = snapshot.clone();
        snapshot.result = Some(result);
        snapshot.validate().map_err(|_| {
            RuntimeCode::RecordingFailed.error("Invalid live retention identity.", None)
        })?;
        JournalEntry::Snapshot(snapshot)
    } else {
        JournalEntry::Result(result)
    };
    let record = Record::Journal(entry);
    match recorder.append(Arc::new(record)).await {
        Ok(receipt) if required.accepts(receipt.persistence) => Ok(Some(receipt)),
        _ => Err(RuntimeCode::RecordingFailed.error(
            "The retained result history was not acknowledged; recovery may be incomplete.",
            None,
        )),
    }
}

#[cfg(test)]
mod tests;
