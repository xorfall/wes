//! Single-owner finite execution state machine. It performs no provider, clock, or storage I/O.
//! A driver serializes transitions and consumes their ordered effects outside the state lock.
use crate::graph::{DependencyGraph, GraphError, NodeId, NodeState, OutputPort, OutputRef};
use indexmap::{IndexMap, IndexSet};
use std::{collections::VecDeque, fmt, sync::Arc, time::Duration};
use thiserror::Error;
use uuid::Uuid;
use wes_core::{ErrorId, ErrorValue, Value, capability::Typing};
mod ordered;
mod pipeline;
mod stale;
use ordered::OrderedPipeline;
pub(crate) use pipeline::PipelineStep;
pub use stale::StaleReason;

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct RunId(Arc<str>);
impl RunId {
    pub fn new(value: impl AsRef<str>) -> Result<Self, RuntimeError> {
        // Shared blank validation also admits compound calculation-child identities.
        NodeId::new(value.as_ref()).map_err(|_| RuntimeError::BlankRunId)?;
        Ok(Self(Arc::from(value.as_ref())))
    }
    fn fresh() -> Self {
        Self(Arc::from(Uuid::new_v4().to_string()))
    }
    pub fn as_str(&self) -> &str {
        &self.0
    }
}
impl fmt::Display for RunId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Run {
    node: NodeId,
    id: RunId,
}
impl Run {
    /// A sequential calculation attempt belongs to the admitted parent node and carries its run.
    pub(crate) fn calculation_child(&self, ordinal: u64) -> Self {
        Self {
            node: self.node.clone(),
            id: RunId::new(format!("{}:calc:{ordinal}", self.id)).expect("nonblank child identity"),
        }
    }

    pub fn node(&self) -> &NodeId {
        &self.node
    }
    pub fn id(&self) -> &RunId {
        &self.id
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Policy {
    #[default]
    Automatic,
    Manual,
    Reactive,
}
impl Policy {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Automatic => "automatic",
            Self::Manual => "manual",
            Self::Reactive => "reactive",
        }
    }
}

/// Facts supplied by the bound executor, not guessed from command spelling.
#[derive(Clone, Copy, Debug)]
pub struct ExecutionTraits {
    /// Verified absence of external operations. SAFE/repeatable alone never implies this.
    pub pure: bool,
    pub repeatable: bool,
    pub bounded: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum OutputState {
    Pending,
    Available(Value),
    Closed,
}

/// Stable output references for a wait, deduplicated without merging mutually exclusive channels.
#[derive(Clone, Debug, Default)]
pub struct OutputSelection(IndexMap<NodeId, OutputPort>);
impl OutputSelection {
    pub fn new(outputs: impl IntoIterator<Item = OutputRef>) -> Result<Self, GraphError> {
        let mut selected = IndexMap::new();
        for output in outputs {
            if let Some(previous) = selected.insert(output.node.clone(), output.port)
                && previous != output.port
            {
                return Err(GraphError::ConflictingOutputs(output.node));
            }
        }
        Ok(Self(selected))
    }
    pub fn outputs(&self) -> impl ExactSizeIterator<Item = OutputRef> + '_ {
        self.0.iter().map(|(node, port)| OutputRef {
            node: node.clone(),
            port: *port,
        })
    }
    pub fn len(&self) -> usize {
        self.0.len()
    }
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

#[derive(Clone, Debug)]
pub enum Outcome {
    /// Deliberate filtering: closes this delivery without selecting error or cancel.
    Skipped,
    Produced(Value),
    Failed(ErrorValue),
    Cancelled(ErrorValue),
}
impl Outcome {
    pub(crate) fn with_policy(self, policy: &wes_core::flow::FlowPolicy) -> Self {
        match self {
            Self::Skipped => Self::Skipped,
            Self::Produced(value) => {
                let provenance = value.provenance().clone().with_policy(policy);
                Self::Produced(value.with_provenance(provenance))
            }
            Self::Failed(error) => Self::Failed(error.with_policy(policy)),
            Self::Cancelled(error) => Self::Cancelled(error.with_policy(policy)),
        }
    }
}

/// Input edges order construction once or continuously govern result currency.
/// Both remain structural ownership edges after successful construction.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum DependencyLifetime {
    #[default]
    Continuous,
    Creation,
}
impl DependencyLifetime {
    pub fn name(self) -> &'static str {
        match self {
            Self::Continuous => "continuous",
            Self::Creation => "creation",
        }
    }
}

/// A display-only last success. It is never an available graph input.
#[derive(Clone, Debug)]
pub struct StoppedValue {
    pub value: Value,
    pub run: RunId,
    pub source: NodeId,
}

/// A snapshot at the transition, never a deferred lookup of mutable state.
#[derive(Clone, Debug)]
pub struct Observation {
    /// Monotonic per-node acknowledged value revision, separate from run identity.
    pub revision: u64,
    pub stale_reason: Option<StaleReason>,
    pub delivery: Option<(NodeId, RunId, u64)>,
    pub stopped: Option<StoppedValue>,
    pub node: NodeId,
    pub run: Option<RunId>,
    pub state: NodeState,
    pub value: Option<Value>,
    pub error: Option<ErrorValue>,
}

/// A read-only explanation of an unsatisfied selected port, tied to the producer run.
#[derive(Clone, Debug)]
pub struct WaitingInput {
    pub source: NodeId,
    pub port: OutputPort,
    pub closed: bool,
    pub run: Option<RunId>,
}
impl WaitingInput {
    pub fn port_name(&self) -> &'static str {
        match self.port {
            OutputPort::Data => "data",
            OutputPort::Error => "error",
            OutputPort::Cancel => "cancel",
        }
    }
    pub fn message(&self) -> String {
        self.message_with_name(None)
    }
    pub fn message_with_name(&self, name: Option<&str>) -> String {
        let label = name.unwrap_or(self.source.as_str());
        if self.closed {
            format!(
                "{} output was not produced by ${} in this run",
                self.port_name(),
                label
            )
        } else {
            format!("Waiting for {} output from ${}", self.port_name(), label)
        }
    }
}

#[derive(Clone, Debug)]
pub struct RunTicket<T> {
    pub run: Run,
    pub payload: T,
    pub inputs: IndexMap<NodeId, Value>,
}

/// Timer identity includes both run and replacement revision. Time is monotonic and driver-owned.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Deadline {
    run: Run,
    revision: u64,
    started: Duration,
    budget: Duration,
}
impl Deadline {
    pub(crate) fn due(&self) -> Duration {
        self.started.saturating_add(self.budget)
    }
    pub fn run(&self) -> &Run {
        &self.run
    }
    pub fn remaining(&self, now: Duration) -> Duration {
        self.budget.saturating_sub(now.saturating_sub(self.started))
    }
}

#[derive(Clone, Debug)]
pub enum Effect<T> {
    Observe(Observation),
    Spawn(RunTicket<T>),
    Cancel(Run),
    Watch(Deadline),
    /// Transfer an entered subscription into the live pool, preserving explicit deadlines.
    StreamReady {
        run: Run,
        deadline: Option<Deadline>,
    },
    StreamClosing(Run),
}

#[derive(Clone, Debug)]
pub enum RestoredState {
    Ready(Value),
    Stale,
    StaleBecause(StaleReason),
    Failed(ErrorValue),
    Cancelled(ErrorValue),
    Skipped,
}

#[derive(Clone, Debug, Error, PartialEq, Eq)]
pub enum RuntimeError {
    #[error("node {0} is not held for restoration")]
    NotHeld(NodeId),
    #[error(
        "node {0} already completed construction; declare a new instance instead of refreshing it"
    )]
    ConstructionComplete(NodeId),
    #[error(transparent)]
    Graph(#[from] GraphError),
    #[error("a run id must not be blank")]
    BlankRunId,
    #[error("the runtime is closed")]
    Closed,
    #[error("timeout must be positive and fit signed 64-bit nanoseconds")]
    InvalidTimeout,
    #[error("timeout revision space exhausted")]
    RevisionExhausted,
    #[error("node {0} still has an execution lease")]
    Busy(NodeId),
    #[error("node {0} is not a finite standalone downstream refresh target")]
    UnsupportedDownstream(NodeId),
    #[error("node {node} needs available external input {}.{}", input.node, input.port.selector())]
    UnavailableDownstreamInput { node: NodeId, input: OutputRef },
}

#[derive(Clone, Debug)]
struct Attempt {
    flow: wes_core::flow::FlowPolicy,
    run: Run,
    started: Duration,
    budget: Option<Duration>,
    revision: u64,
}
impl Attempt {
    fn deadline(&self) -> Option<Deadline> {
        self.budget.map(|budget| Deadline {
            run: self.run.clone(),
            revision: self.revision,
            started: self.started,
            budget,
        })
    }
}
#[derive(Clone, Debug)]
struct Entry {
    // Independent of graph presentation state: only admitted execution acknowledges this pause.
    automatic_pause: Option<StaleReason>,
    stale_reason: Option<StaleReason>,
    value: Option<Value>,
    value_run: Option<RunId>,
    display_revision: u64,
    stream_counts: Option<(u64, u64, usize)>,
    stopped: Option<StoppedValue>,
    actual_typing: Option<Arc<Typing>>,
    error: Option<ErrorValue>,
    last_run: Option<RunId>,
    stream_start: Option<u64>,
    active: Option<Attempt>,
    policy: Option<Policy>,
    timeout: Option<Duration>,
    traits: ExecutionTraits,
    activation: Option<OutputRef>,
    dependency_lifetime: DependencyLifetime,
    creation_complete: bool,
    restored: bool,
    refresh_pending: bool,
    // Coalesces observation windows while a finite pure calculation owns its captured inputs.
    input_update_pending: bool,
    explicitly_requested: bool,
    ordered_root: Option<NodeId>,
    last_delivery: Option<(NodeId, RunId, u64)>,
    pipeline: PipelineStep,
}
impl Entry {
    fn automatic_paused(&self) -> bool {
        self.automatic_pause.is_some()
    }
    fn mark_stale_reason(&mut self, reason: StaleReason) {
        match reason {
            StaleReason::DefinitionChanged | StaleReason::DependencyChanged => {
                self.automatic_pause = Some(reason)
            }
            StaleReason::RefreshRequested => self.automatic_pause = None,
            _ => {}
        }
        self.stale_reason = self.automatic_pause.or(Some(reason));
    }
    fn restore_state(&mut self, state: RestoredState, run: Option<RunId>) -> NodeState {
        self.restored = true;
        self.value_run = run.clone();
        self.stopped = None;
        self.last_run = run;
        self.value = None;
        self.error = None;
        self.actual_typing = None;
        self.stale_reason = None;
        self.automatic_pause = None;
        match state {
            RestoredState::Ready(value) => {
                self.actual_typing = Some(typing(&value));
                self.creation_complete = self.dependency_lifetime == DependencyLifetime::Creation;
                self.value = Some(value);
                NodeState::Ready
            }
            RestoredState::Stale => {
                self.stale_reason = Some(StaleReason::Unknown);
                self.automatic_pause = self.stale_reason;
                NodeState::Stale
            }
            RestoredState::StaleBecause(reason) => {
                self.stale_reason = Some(reason);
                self.automatic_pause = Some(reason);
                NodeState::Stale
            }
            RestoredState::Failed(error) => {
                self.error = Some(error);
                NodeState::Failed
            }
            RestoredState::Cancelled(error) => {
                self.error = Some(error);
                NodeState::Cancelled
            }
            RestoredState::Skipped => NodeState::Skipped,
        }
    }
    fn new(traits: ExecutionTraits) -> Self {
        Self {
            automatic_pause: None,
            stale_reason: None,
            value: None,
            value_run: None,
            display_revision: 0,
            stream_counts: None,
            stopped: None,
            actual_typing: None,
            error: None,
            last_run: None,
            stream_start: None,
            active: None,
            policy: None,
            timeout: None,
            traits,
            activation: None,
            dependency_lifetime: DependencyLifetime::Continuous,
            creation_complete: false,
            restored: false,
            refresh_pending: false,
            input_update_pending: false,
            explicitly_requested: false,
            ordered_root: None,
            last_delivery: None,
            pipeline: Default::default(),
        }
    }
}
#[derive(Clone, Debug)]
struct Lease {
    run: Run,
    entered: bool,
    kind: WorkKind,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum WorkKind {
    Finite,
    Opening,
    Open,
    Closing,
}

/// Not Clone: duplicating a live runtime would duplicate publication authority.
/// Graph mutations stay here so callers cannot invalidate the lease/output invariants.
pub struct Runtime<T> {
    graph: DependencyGraph<T>,
    entries: IndexMap<NodeId, Entry>,
    leases: IndexMap<NodeId, Lease>,
    // At most 1024 charged private values; summing this bounded map avoids walking the graph.
    private_charges: IndexMap<NodeId, u64>,
    ordered: IndexMap<NodeId, OrderedPipeline>,
    stream_budget: crate::streams::delivery::Budget,
    retiring_events: Vec<(Vec<Run>, crate::streams::delivery::Credit)>,
    default_policy: Policy,
    default_timeout: Duration,
    closed: bool,
}
impl<T: Clone> Default for Runtime<T> {
    fn default() -> Self {
        Self::new()
    }
}
impl<T: Clone> Runtime<T> {
    pub fn new() -> Self {
        Self {
            graph: DependencyGraph::new(),
            entries: IndexMap::new(),
            leases: IndexMap::new(),
            private_charges: IndexMap::new(),
            ordered: IndexMap::new(),
            stream_budget: Default::default(),
            retiring_events: vec![],
            default_policy: Policy::Automatic,
            default_timeout: Duration::from_secs(900),
            closed: false,
        }
    }
    pub(crate) fn stream_delivery_budget(
        &self,
        node: &NodeId,
    ) -> Option<crate::streams::delivery::Budget> {
        self.ordered
            .contains_key(node)
            .then(|| self.stream_budget.clone())
    }
    pub fn graph(&self) -> &DependencyGraph<T> {
        &self.graph
    }
    fn admit_value(&mut self, node: &NodeId, value: &Value) -> Result<(), ErrorValue> {
        if !value.provenance().policy().is_private() {
            self.private_charges.shift_remove(node);
            return Ok(());
        }
        let used: u64 = self
            .private_charges
            .iter()
            .filter(|(id, _)| *id != node)
            .map(|(_, n)| *n)
            .sum();
        let charge =
            crate::value_size::value_charge(value, (64 * 1024 * 1024u64).saturating_sub(used));
        if (self.private_charges.len() < 1024 || self.private_charges.contains_key(node))
            && let Some(charge) = charge
        {
            self.private_charges.insert(node.clone(), charge);
            Ok(())
        } else {
            Err(RuntimeCode::InputFailed
                .error(
                    "Private output memory budget exceeded; no output published.",
                    None,
                )
                .with_policy(value.provenance().policy()))
        }
    }
    fn admit_restored(&mut self, node: &NodeId, state: RestoredState) -> RestoredState {
        if let RestoredState::Ready(value) = &state {
            if let Err(error) = self.admit_value(node, value) {
                self.private_charges.shift_remove(node);
                return RestoredState::Failed(error);
            }
        } else {
            self.private_charges.shift_remove(node);
        }
        state
    }
    pub(crate) fn reserve_historical_ids(&mut self, ids: &[NodeId]) {
        self.graph.reserve_historical_ids(ids);
    }
    pub fn is_closed(&self) -> bool {
        self.closed
    }
    pub fn is_idle(&self) -> bool {
        self.stream_budget.is_idle()
            && self
                .leases
                .values()
                .all(|lease| lease.kind == WorkKind::Open && self.is_active(&lease.run))
    }
    /// No outstanding physical execution, including open or cancelled subscriptions.
    pub fn is_drained(&self) -> bool {
        self.leases.is_empty()
    }
    pub fn is_streaming(&self, node: &NodeId) -> bool {
        self.leases
            .get(node)
            .is_some_and(|lease| lease.kind == WorkKind::Open && self.is_active(&lease.run))
    }
    pub fn is_executing(&self, node: &NodeId) -> bool {
        self.leases.get(node).is_some_and(|lease| lease.entered)
    }
    pub(crate) fn has_lease(&self, node: &NodeId) -> bool {
        self.leases.contains_key(node)
    }
    /// Ephemeral input/output uses the same current-run authority as durable result publication.
    pub(crate) fn accepts_live_io(&self, run: &Run) -> bool {
        !self.closed
            && self.is_active(run)
            && self
                .leases
                .get(run.node())
                .is_some_and(|lease| lease.entered && lease.run == *run)
    }
    pub fn run_of(&self, node: &NodeId) -> Option<&RunId> {
        self.entries.get(node)?.last_run.as_ref()
    }
    /// Revision of acknowledged values, independent of an ordered stage's next attempt.
    pub fn display_revision(&self, node: &NodeId) -> u64 {
        self.entries
            .get(node)
            .map_or(0, |entry| entry.display_revision)
    }
    pub fn value_run(&self, node: &NodeId) -> Option<&RunId> {
        self.entries.get(node)?.value_run.as_ref()
    }
    /// Effective preference, separate from the executor's eligibility and stale reason.
    pub fn policy_of(&self, node: &NodeId) -> Option<Policy> {
        self.entries
            .get(node)
            .map(|entry| entry.policy.unwrap_or(self.default_policy))
    }
    pub fn stream_counts(&self, node: &NodeId) -> Option<(u64, u64, usize)> {
        self.entries.get(node)?.stream_counts
    }
    pub(crate) fn set_stream_counts(
        &mut self,
        run: &Run,
        omitted: u64,
        rejected: u64,
        items: usize,
    ) {
        if self.is_active(run)
            && let Some(entry) = self.entries.get_mut(run.node())
        {
            entry.stream_counts = Some((omitted, rejected, items));
        }
    }
    pub fn stream_phase(&self, node: &NodeId) -> &'static str {
        use NodeState::*;
        let Some(entry) = self.entries.get(node) else {
            return "failed";
        };
        if let Some(lease) = self.leases.get(node) {
            return match lease.kind {
                WorkKind::Closing => "closing",
                WorkKind::Open if self.is_active(&lease.run) => "open",
                _ if entry.active.is_some() => "opening",
                _ => "closing",
            };
        }
        match self.graph.node(node).map(|node| node.state()) {
            Some(Failed | Skipped) => "failed",
            Some(Cancelled) => "stopped",
            Some(Ready) => "ended",
            _ => "opening",
        }
    }
    pub fn value_of(&self, node: &NodeId) -> Option<&Value> {
        self.entries.get(node)?.value.as_ref()
    }
    pub fn stopped_value(&self, node: &NodeId) -> Option<&StoppedValue> {
        self.entries.get(node)?.stopped.as_ref()
    }
    /// Select once at cancellation, before the data branch is invalidated. Value clones share
    /// immutable payloads; no growing history or extra successful-result cache is introduced.
    fn preserve_stopped_stream(&mut self, node: &NodeId) {
        if !self.leases.get(node).is_some_and(|lease| {
            matches!(lease.kind, WorkKind::Open | WorkKind::Closing) && self.is_active(&lease.run)
        }) {
            return;
        }
        let Ok(affected) = self.currency_downstream(node) else {
            return;
        };
        for id in affected {
            let entry = self.entries.get_mut(&id).expect("downstream entry");
            if let (Some(value), Some(run)) = (&entry.value, &entry.value_run)
                && !value.provenance().policy().is_private()
                && !value.provenance().policy().is_unknown()
                && value.data().is_materialized()
            {
                entry.stopped = Some(StoppedValue {
                    value: value.clone(),
                    run: run.clone(),
                    source: node.clone(),
                });
            }
        }
    }
    pub(crate) fn set_stream_start(&mut self, run: &Run, start: u64) {
        if self.is_active(run)
            && let Some(e) = self.entries.get_mut(run.node())
        {
            e.stream_start = Some(start);
        }
    }
    /// Last accepted successful typing, retained even if its payload bytes are evicted. A failed,
    /// cancelled or obsolete worker cannot refine this metadata outside its publication authority.
    pub fn actual_typing(&self, node: &NodeId) -> Option<&Arc<Typing>> {
        self.entries.get(node)?.actual_typing.as_ref()
    }
    pub fn error_of(&self, node: &NodeId) -> Option<&ErrorValue> {
        self.entries.get(node)?.error.as_ref()
    }
    fn open(&self) -> Result<(), RuntimeError> {
        if self.closed {
            Err(RuntimeError::Closed)
        } else {
            Ok(())
        }
    }
    pub fn add(
        &mut self,
        payload: T,
        dependencies: impl IntoIterator<Item = OutputRef>,
        traits: ExecutionTraits,
    ) -> Result<NodeId, RuntimeError> {
        self.open()?;
        let node = self.graph.next_id_avoiding([])?;
        self.add_at(node.clone(), payload, dependencies, traits)?;
        Ok(node)
    }
    /// Admit a preselected/journalled identity without restoring historical state or starting work.
    /// This is distinct from restore: the new node is pending, but old physical leases still forbid
    /// reusing a dropped node's identity until its worker really exits.
    pub fn add_at(
        &mut self,
        node: NodeId,
        payload: T,
        dependencies: impl IntoIterator<Item = OutputRef>,
        traits: ExecutionTraits,
    ) -> Result<(), RuntimeError> {
        self.open()?;
        if self.leases.contains_key(&node) {
            return Err(RuntimeError::Busy(node));
        }
        self.graph.restore(node.clone(), payload, dependencies)?;
        self.entries.insert(node, Entry::new(traits));
        Ok(())
    }
    /// Restored declarations are held, including handlers. No effects or automatic starts occur.
    pub fn restore(
        &mut self,
        node: NodeId,
        payload: T,
        dependencies: impl IntoIterator<Item = OutputRef>,
        traits: ExecutionTraits,
        state: RestoredState,
        run: Option<RunId>,
    ) -> Result<(), RuntimeError> {
        self.open()?;
        if self.leases.contains_key(&node) {
            return Err(RuntimeError::Busy(node));
        }
        self.graph.restore(node.clone(), payload, dependencies)?;
        let mut entry = Entry::new(traits);
        let state = self.admit_restored(&node, state);
        let state = entry.restore_state(state, run);
        self.entries.insert(node.clone(), entry);
        self.graph.set_state(&node, state)?;
        Ok(())
    }
    pub(crate) fn install_dependency_lifetime(
        &mut self,
        node: &NodeId,
        lifetime: DependencyLifetime,
    ) {
        let entry = self.entries.get_mut(node).expect("admitted declaration");
        assert!(entry.active.is_none() && entry.last_run.is_none());
        entry.dependency_lifetime = lifetime;
        entry.creation_complete = lifetime == DependencyLifetime::Creation
            && self
                .graph
                .node(node)
                .is_some_and(|node| node.state() == NodeState::Ready);
    }
    pub fn dependency_lifetime(&self, node: &NodeId) -> Option<DependencyLifetime> {
        self.entries
            .get(node)
            .map(|entry| entry.dependency_lifetime)
    }
    pub fn construction_complete(&self, node: &NodeId) -> bool {
        self.entries
            .get(node)
            .is_some_and(|entry| entry.creation_complete)
    }
    /// Currency propagation differs from structural ownership/control reachability.
    fn currency_downstream(&self, root: &NodeId) -> Result<IndexSet<NodeId>, GraphError> {
        if self.graph.node(root).is_none() {
            return Err(GraphError::Missing(root.clone()));
        }
        let mut reached = IndexSet::new();
        let mut queue = std::collections::VecDeque::from([root.clone()]);
        while let Some(next) = queue.pop_front() {
            if reached.insert(next.clone()) {
                queue.extend(
                    self.graph
                        .dependents_of(&next)
                        .filter(|id| !self.construction_complete(id))
                        .cloned(),
                );
            }
        }
        Ok(reached)
    }
    fn check_construction_refresh(&self, node: &NodeId) -> Result<(), RuntimeError> {
        if self.construction_complete(node) {
            return Err(RuntimeError::ConstructionComplete(node.clone()));
        }
        Ok(())
    }
    /// Install a prepared output-selection condition before admitting any execution. The
    /// selected output is already a dependency: pending waits, closed skips, available
    /// enables ordinary dependency evaluation. Other graph edges keep their error semantics.
    /// Source replay installs the same condition before historical state is hydrated.
    pub(crate) fn install_activation(&mut self, node: &NodeId, output: Option<OutputRef>) {
        let graph_node = self.graph.node(node).expect("admitted declaration");
        let entry = self.entries.get_mut(node).expect("admitted entry");
        assert!(entry.active.is_none() && entry.last_run.is_none());
        if let Some(output) = &output {
            assert_eq!(
                graph_node.dependencies().get(&output.node),
                Some(&output.port)
            );
        }
        entry.activation = output;
    }
    pub fn set_default_policy(&mut self, policy: Policy) {
        self.default_policy = policy;
    }
    pub(crate) fn restore_held_state(
        &mut self,
        node: &NodeId,
        state: RestoredState,
        run: Option<RunId>,
    ) -> Result<(), RuntimeError> {
        self.open()?;
        self.require_held(node)?;
        let state = self.admit_restored(node, state);
        let state = self
            .entries
            .get_mut(node)
            .expect("held entry")
            .restore_state(state, run);
        self.graph.set_state(node, state)?;
        Ok(())
    }
    fn require_held(&self, node: &NodeId) -> Result<(), RuntimeError> {
        if self.leases.contains_key(node)
            || self
                .entries
                .get(node)
                .is_none_or(|entry| !entry.restored || entry.active.is_some())
        {
            return Err(RuntimeError::NotHeld(node.clone()));
        }
        Ok(())
    }
    pub(crate) fn replace_held_payload(
        &mut self,
        node: &NodeId,
        payload: T,
        traits: ExecutionTraits,
    ) -> Result<(), RuntimeError> {
        self.open()?;
        self.require_held(node)?;
        self.graph.replace_payload(node, payload)?;
        self.entries.get_mut(node).expect("held entry").traits = traits;
        Ok(())
    }
    pub fn set_policy(&mut self, node: &NodeId, policy: Policy) -> Result<(), RuntimeError> {
        self.entry_mut(node)?.policy = Some(policy);
        Ok(())
    }
    pub fn set_default_timeout(&mut self, budget: Duration) -> Result<(), RuntimeError> {
        valid_timeout(budget)?;
        self.default_timeout = budget;
        Ok(())
    }
    /// Replacements retain the start instant, not "now + budget".
    pub fn set_timeout(
        &mut self,
        node: &NodeId,
        budget: Duration,
    ) -> Result<Vec<Effect<T>>, RuntimeError> {
        self.open()?;
        valid_timeout(budget)?;
        let entry = self.entry_mut(node)?;
        let revision = entry
            .active
            .as_ref()
            .map(|attempt| {
                attempt
                    .revision
                    .checked_add(1)
                    .ok_or(RuntimeError::RevisionExhausted)
            })
            .transpose()?;
        entry.timeout = Some(budget);
        if let Some(attempt) = &mut entry.active {
            attempt.revision = revision.expect("active revision checked");
            attempt.budget = Some(budget);
            Ok(vec![Effect::Watch(
                attempt.deadline().expect("explicit timeout"),
            )])
        } else {
            Ok(vec![])
        }
    }
    /// Preflight all staged replacements before a declaration batch mutates any live state.
    /// Missing nodes may be new declarations in that batch. Resetting an attempt can only reduce
    /// this conservative bound; no deadline revision is allowed to wrap.
    pub(crate) fn validate_timeout_updates(
        &self,
        node: &NodeId,
        count: u64,
    ) -> Result<(), RuntimeError> {
        if let Some(attempt) = self
            .entries
            .get(node)
            .and_then(|entry| entry.active.as_ref())
        {
            attempt
                .revision
                .checked_add(count)
                .ok_or(RuntimeError::RevisionExhausted)?;
        }
        Ok(())
    }
    fn entry_mut(&mut self, node: &NodeId) -> Result<&mut Entry, RuntimeError> {
        self.entries
            .get_mut(node)
            .ok_or_else(|| GraphError::Missing(node.clone()).into())
    }
    pub fn output(&self, reference: &OutputRef) -> OutputState {
        let Some(node) = self.graph.node(&reference.node) else {
            return OutputState::Closed;
        };
        let entry = &self.entries[&reference.node];
        match (node.state(), reference.port) {
            (NodeState::Pending | NodeState::Running | NodeState::Stale, _) => OutputState::Pending,
            (NodeState::Ready, OutputPort::Data) => {
                OutputState::Available(entry.value.clone().expect("ready holds a value"))
            }
            (NodeState::Failed, OutputPort::Error) => OutputState::Available(
                entry
                    .error
                    .as_ref()
                    .expect("failure holds error")
                    .to_value(),
            ),
            (NodeState::Cancelled, OutputPort::Cancel) => OutputState::Available(
                entry
                    .error
                    .as_ref()
                    .expect("cancellation holds reason")
                    .to_cancellation_value(),
            ),
            _ => OutputState::Closed,
        }
    }
    pub fn waiting_inputs(&self, consumer: &NodeId) -> Vec<WaitingInput> {
        let Some(node) = self.graph.node(consumer) else {
            return vec![];
        };
        if !matches!(node.state(), NodeState::Pending | NodeState::Skipped) {
            return vec![];
        }
        node.dependencies()
            .iter()
            .filter_map(|(source, port)| {
                let status = self.ordered_output(
                    consumer,
                    &OutputRef {
                        node: source.clone(),
                        port: *port,
                    },
                );
                let closed = match status {
                    OutputState::Available(_) => return None,
                    OutputState::Closed => true,
                    OutputState::Pending => false,
                };
                Some(WaitingInput {
                    source: source.clone(),
                    port: *port,
                    closed,
                    run: self.run_of(source).cloned(),
                })
            })
            .take(8)
            .collect()
    }
    pub fn has_settled(&self, node: &NodeId) -> bool {
        self.graph
            .node(node)
            .is_none_or(|node| !matches!(node.state(), NodeState::Pending | NodeState::Running))
    }
    /// Mirrors selected-output waiting: all selected nodes settle first; stale/removed nodes
    /// count as settled but have no available output. This neither starts work nor pins values.
    pub fn selected_outputs(&self, selected: &OutputSelection) -> Option<bool> {
        if selected.0.keys().any(|node| !self.has_settled(node)) {
            return None;
        }
        Some(
            selected
                .outputs()
                .all(|output| matches!(self.output(&output), OutputState::Available(_))),
        )
    }
    pub fn start(&mut self, now: Duration) -> Vec<Effect<T>> {
        let candidates = self.graph.nodes().map(|node| node.id().clone()).collect();
        self.schedule(candidates, false, now)
    }
    /// Must be called immediately before entering executor code, through the same state owner.
    /// An obsolete queued ticket can never enter, even if its OS task was not cancelled in time.
    pub fn enter(&mut self, run: &Run) -> bool {
        if self.closed || !self.is_active(run) {
            return false;
        }
        let Some(lease) = self.leases.get_mut(&run.node) else {
            return false;
        };
        if lease.run != *run || lease.entered {
            return false;
        }
        lease.entered = true;
        true
    }
    /// Stream designation is captured at the same guarded entry as the physical lease.
    pub fn enter_stream(&mut self, run: &Run) -> bool {
        if !self.enter(run) {
            return false;
        }
        self.leases.get_mut(run.node()).expect("entered lease").kind = WorkKind::Opening;
        if let Some(group) = self.ordered.get_mut(run.node()) {
            group.reset();
        }
        true
    }
    /// Only an entered, still-current stream can publish. Its physical lease remains owned until
    /// complete(), even though an open stream no longer makes ordinary idle waits block forever.
    pub fn stream_window(
        &mut self,
        run: &Run,
        value: Value,
        now: Duration,
    ) -> Option<Vec<Effect<T>>> {
        if self.closed || !self.is_active(run) {
            return None;
        }
        let lease = self.leases.get_mut(run.node())?;
        if lease.run != *run
            || !lease.entered
            || !matches!(lease.kind, WorkKind::Opening | WorkKind::Open)
        {
            return None;
        }
        let opening = lease.kind == WorkKind::Opening;
        if let Err(error) = self.admit_value(run.node(), &value) {
            return Some(self.cancel_with(run.node(), error, now));
        }
        let lease = self.leases.get_mut(run.node()).expect("validated lease");
        lease.kind = WorkKind::Open;
        let entry = self.entries.get_mut(run.node()).expect("active entry");
        entry.value = Some(value.clone());
        entry.value_run = Some(run.id.clone());
        entry.stopped = None;
        entry.actual_typing = Some(typing(&value));
        entry.error = None;
        let mut effects = vec![];
        if opening {
            let attempt = entry.active.as_mut().expect("active stream");
            // The finite default protects opening, not a successfully established subscription.
            if entry.timeout.is_none() {
                attempt.budget = None;
            }
            effects.push(Effect::StreamReady {
                run: run.clone(),
                deadline: attempt.deadline(),
            });
        }
        self.transition(run.node(), NodeState::Ready, &mut effects);
        if !opening {
            effects.extend(self.stream_dependents(run.node(), now));
        } else {
            effects.extend(self.schedule(
                self.graph.dependents_of(run.node()).cloned().collect(),
                false,
                now,
            ));
        }
        effects.extend(self.advance_ordered(run.node(), now));
        Some(effects)
    }
    fn stream_dependents(&mut self, node: &NodeId, now: Duration) -> Vec<Effect<T>> {
        let mut effects = vec![];
        let committed_window = matches!(
            self.output(&OutputRef::data(node.clone())),
            OutputState::Available(_)
        );
        let mut downstream = self.currency_downstream(node).expect("active node");
        downstream.shift_remove(node);
        // Group suffixes are invalidated once per consumed event, never per observation window.
        if let Some(group) = self.ordered.get(node) {
            for stage in &group.stages {
                if let Ok(branch) = self.graph.downstream(stage) {
                    for id in branch {
                        downstream.shift_remove(&id);
                    }
                }
            }
        }
        for node in &downstream {
            // A dependent that has never had all its inputs still deserves its first run.
            if self
                .graph
                .node(node)
                .is_some_and(|node| node.state() == NodeState::Pending)
            {
                continue;
            }
            let entry = &self.entries[node];
            let coalesce = committed_window
                && entry.policy.unwrap_or(self.default_policy) == Policy::Automatic
                && entry.traits.pure
                && entry.traits.bounded
                && entry.traits.repeatable
                && entry.active.is_some()
                && entry.ordered_root.is_none()
                && self
                    .leases
                    .get(node)
                    .is_some_and(|lease| lease.kind == WorkKind::Finite);
            if coalesce {
                self.entries
                    .get_mut(node)
                    .expect("downstream entry")
                    .input_update_pending = true;
                continue;
            }
            if let Some(run) = self.revoke(node) {
                effects.push(Effect::Cancel(run));
            }
            let entry = self.entries.get_mut(node).expect("downstream entry");
            let first_selection =
                entry.last_run.is_none() && !entry.restored && !entry.automatic_paused();
            entry.mark_stale_reason(StaleReason::StreamUpdated);
            entry.restored = false;
            entry.error = None;
            self.transition(
                node,
                if first_selection {
                    NodeState::Pending
                } else {
                    NodeState::Stale
                },
                &mut effects,
            );
        }
        effects.extend(self.schedule(downstream.into_iter().collect(), false, now));
        effects
    }
    pub fn stream_closing(&mut self, run: &Run) -> Option<Vec<Effect<T>>> {
        if self.closed || !self.is_active(run) {
            return None;
        }
        let lease = self.leases.get_mut(run.node())?;
        if lease.run != *run || !matches!(lease.kind, WorkKind::Opening | WorkKind::Open) {
            return None;
        }
        lease.kind = WorkKind::Closing;
        Some(vec![Effect::StreamClosing(run.clone())])
    }
    fn is_active(&self, run: &Run) -> bool {
        self.entries
            .get(&run.node)
            .and_then(|entry| entry.active.as_ref())
            .is_some_and(|attempt| attempt.run == *run)
    }
    /// Called only after executor exit (or failed dispatch), not merely after requesting cancellation.
    pub fn complete(&mut self, run: &Run, outcome: Outcome, now: Duration) -> Vec<Effect<T>> {
        if !self
            .leases
            .get(&run.node)
            .is_some_and(|lease| lease.run == *run)
        {
            return vec![];
        }
        let stream = self
            .leases
            .get(run.node())
            .is_some_and(|lease| lease.kind != WorkKind::Finite);
        self.leases.shift_remove(&run.node);
        self.retiring_events.retain(|(runs, _)| {
            runs.iter().any(|r| {
                self.leases
                    .get(r.node())
                    .is_some_and(|lease| lease.run == *r)
            })
        });
        if self.closed {
            return vec![];
        }
        if !self.is_active(run) {
            let forced = self
                .entries
                .get_mut(run.node())
                .is_some_and(|entry| std::mem::take(&mut entry.refresh_pending));
            let mut candidates = self.pipeline_successors(run.node());
            candidates.push(run.node.clone());
            let root = self
                .entries
                .get(run.node())
                .and_then(|e| e.ordered_root.clone());
            if let Some(group) = root.as_ref().and_then(|root| self.ordered.get(root)) {
                candidates.extend(group.stages.clone());
            }
            let mut effects = self.schedule(candidates, forced, now);
            if let Some(root) = root {
                effects.extend(self.advance_ordered(&root, now));
            }
            return effects;
        }
        let outcome = match outcome {
            Outcome::Produced(value) => match self.admit_value(run.node(), &value) {
                Ok(()) => Outcome::Produced(value),
                Err(error) => Outcome::Failed(error),
            },
            other => other,
        };
        if !matches!(outcome, Outcome::Produced(_)) {
            self.private_charges.shift_remove(run.node());
        }
        let changed_window = stream
            && self.value_of(run.node()).is_some_and(|old| match &outcome {
                Outcome::Produced(value) => old != value,
                Outcome::Failed(_) | Outcome::Cancelled(_) | Outcome::Skipped => true,
            });
        let succeeded = matches!(outcome, Outcome::Produced(_));
        let entry = self.entries.get_mut(&run.node).expect("active node exists");
        let inputs_changed = std::mem::take(&mut entry.input_update_pending) && succeeded;
        entry.active = None;
        let state = match outcome {
            Outcome::Skipped => {
                entry.value = None;
                entry.error = None;
                NodeState::Skipped
            }
            Outcome::Produced(value) => {
                entry.actual_typing = Some(typing(&value));
                entry.creation_complete = entry.dependency_lifetime == DependencyLifetime::Creation;
                entry.value_run = Some(run.id.clone());
                entry.stopped = None;
                entry.value = Some(value);
                entry.error = None;
                NodeState::Ready
            }
            Outcome::Failed(error) => {
                entry.value = None;
                entry.error = Some(error);
                NodeState::Failed
            }
            Outcome::Cancelled(reason) => {
                entry.value = None;
                entry.error = Some(reason);
                NodeState::Cancelled
            }
        };
        let mut effects = vec![];
        if inputs_changed {
            // Keep the completed sample for display, but never deliver it as a current
            // input. At most one new attempt captures the latest committed inputs.
            self.entries
                .get_mut(run.node())
                .expect("active entry")
                .mark_stale_reason(StaleReason::InputBehind);
            self.transition(run.node(), NodeState::Stale, &mut effects);
            effects.extend(self.schedule(vec![run.node.clone()], false, now));
            return effects;
        }
        self.transition(&run.node, state, &mut effects);
        if changed_window {
            effects.extend(self.stream_dependents(run.node(), now));
        } else {
            effects.extend(self.schedule(self.pipeline_successors(&run.node), false, now));
        }
        if let Some(root) = self
            .entries
            .get(run.node())
            .and_then(|e| e.ordered_root.clone())
        {
            if let Some(group) = self.ordered.get_mut(&root) {
                if state == NodeState::Ready {
                    group.completed.insert(run.node().clone());
                }
                if state == NodeState::Skipped {
                    group.skipped.insert(run.node().clone());
                }
            }
            effects.extend(self.advance_ordered(&root, now));
        } else if self.ordered.contains_key(run.node()) {
            if state != NodeState::Ready {
                effects.extend(self.stop_ordered(
                    run.node(),
                    RuntimeCode::ExecutionFailed.error(
                        "The stream pipeline source stopped before all events completed.",
                        None,
                    ),
                    now,
                ));
            } else {
                effects.extend(self.advance_ordered(run.node(), now));
            }
        }
        effects
    }
    pub fn cancel(&mut self, node: &NodeId, now: Duration) -> Vec<Effect<T>> {
        self.cancel_with(
            node,
            RuntimeCode::Cancelled.error("The local operation was cancelled.", None),
            now,
        )
    }
    pub fn expire(&mut self, deadline: &Deadline, now: Duration) -> Vec<Effect<T>> {
        let current = self
            .entries
            .get(&deadline.run.node)
            .and_then(|entry| entry.active.as_ref())
            .and_then(Attempt::deadline);
        if current.as_ref() != Some(deadline) || !deadline.remaining(now).is_zero() {
            return vec![];
        }
        self.cancel_with(
            &deadline.run.node,
            RuntimeCode::TimedOut.error(
                "The local operation was cancelled because its timeout expired.",
                None,
            ),
            now,
        )
    }
    fn cancel_with(&mut self, node: &NodeId, error: ErrorValue, now: Duration) -> Vec<Effect<T>> {
        if let Some(root) = self.ordered_root(node) {
            if self
                .entries
                .get(node)
                .is_some_and(|e| !e.pipeline.branch.is_empty())
            {
                return self.stop_ordered_branch(&root, node, error, now);
            }
            self.preserve_stopped_stream(&root);
            return self.stop_ordered(&root, error, now);
        }
        self.preserve_stopped_stream(node);
        let waiting_request = self.entries.get_mut(node).is_some_and(|entry| {
            let waiting = entry.explicitly_requested && entry.active.is_none();
            entry.refresh_pending = false;
            entry.explicitly_requested = false;
            waiting
        });
        if self.closed {
            return vec![];
        }
        if self
            .entries
            .get(node)
            .is_none_or(|entry| entry.active.is_none())
        {
            if !waiting_request {
                return vec![];
            }
            // A queued explicit request is cancellable before it has a run/lease.
            // Terminal state also prevents ordinary Reactive scheduling from reviving it.
            self.private_charges.shift_remove(node);
            let entry = self.entries.get_mut(node).expect("waiting request");
            let error = entry.value.as_ref().map_or(error.clone(), |value| {
                error.with_policy(value.provenance().policy())
            });
            entry.restored = false;
            entry.value = None;
            entry.error = Some(error);
            let mut effects = vec![];
            self.transition(node, NodeState::Cancelled, &mut effects);
            effects.extend(self.schedule(self.pipeline_successors(node), false, now));
            return effects;
        }
        let stream_window = self
            .leases
            .get(node)
            .is_some_and(|lease| lease.kind != WorkKind::Finite)
            && self.value_of(node).is_some();
        let error = self.entries[node]
            .active
            .as_ref()
            .map_or(error.clone(), |attempt| error.with_policy(&attempt.flow));
        let mut effects = vec![];
        let run = self.revoke(node).expect("active attempt");
        self.private_charges.shift_remove(node);
        let entry = self.entries.get_mut(node).expect("active node");
        let error = entry.value.as_ref().map_or(error.clone(), |value| {
            error.with_policy(value.provenance().policy())
        });
        entry.value = None;
        entry.error = Some(error);
        self.transition(node, NodeState::Cancelled, &mut effects);
        effects.push(Effect::Cancel(run));
        if stream_window {
            effects.extend(self.stream_dependents(node, now));
        } else {
            effects.extend(self.schedule(self.pipeline_successors(node), false, now));
        }
        effects
    }
    /// Revoke publication first. An unentered ticket has no physical execution to wait for.
    fn revoke(&mut self, node: &NodeId) -> Option<Run> {
        let entry = self.entries.get_mut(node)?;
        entry.input_update_pending = false;
        let run = entry.active.take()?.run;
        if self
            .leases
            .get(node)
            .is_some_and(|lease| lease.run == run && !lease.entered)
        {
            self.leases.shift_remove(node);
        }
        Some(run)
    }
    fn mark_stale(
        &mut self,
        node: &NodeId,
        reason: StaleReason,
        downstream_reason: StaleReason,
    ) -> Result<(IndexSet<NodeId>, Vec<Effect<T>>), RuntimeError> {
        let stale = self.currency_downstream(node)?;
        let effects = self.mark_selected_stale(&stale, node, reason, downstream_reason);
        Ok((stale, effects))
    }
    fn mark_selected_stale(
        &mut self,
        selected: &IndexSet<NodeId>,
        root: &NodeId,
        reason: StaleReason,
        downstream_reason: StaleReason,
    ) -> Vec<Effect<T>> {
        let mut effects = vec![];
        for marked in selected {
            if let Some(run) = self.revoke(marked) {
                effects.push(Effect::Cancel(run));
            }
            let entry = self.entries.get_mut(marked).expect("graph entry");
            entry.refresh_pending = false;
            entry.explicitly_requested = false;
            entry.stopped = None;
            entry.restored = false;
            entry.error = None;
            let next_reason = if marked == root {
                reason
            } else {
                downstream_reason
            };
            entry.input_update_pending = false;
            entry.mark_stale_reason(next_reason);
            self.transition(marked, NodeState::Stale, &mut effects);
        }
        effects
    }

    pub fn invalidate(
        &mut self,
        node: &NodeId,
        now: Duration,
    ) -> Result<Vec<Effect<T>>, RuntimeError> {
        self.open()?;
        let roots: IndexSet<_> = self
            .graph
            .downstream(node)?
            .iter()
            .filter_map(|n| self.ordered_root(n))
            .collect();
        let mut effects = vec![];
        for root in roots {
            effects.extend(self.stop_ordered(&root,RuntimeCode::Cancelled.error("The stream pipeline stopped because an input or definition changed; queued events were not executed.",None),now));
        }
        let (stale, more) = self.mark_stale(
            node,
            StaleReason::DefinitionChanged,
            StaleReason::DependencyChanged,
        )?;
        effects.extend(more);
        effects.extend(self.schedule(stale.into_iter().collect(), false, now));
        Ok(effects)
    }
    pub fn refresh(
        &mut self,
        node: &NodeId,
        now: Duration,
    ) -> Result<Vec<Effect<T>>, RuntimeError> {
        self.open()?;
        self.check_construction_refresh(node)?;
        if self
            .entries
            .get(node)
            .is_some_and(|e| e.ordered_root.is_some())
        {
            return Err(RuntimeError::Busy(node.clone()));
        }
        if self.leases.contains_key(node) {
            if self
                .entries
                .get(node)
                .is_some_and(|entry| entry.refresh_pending)
            {
                return Ok(vec![]);
            }
            if self.is_streaming(node) {
                let (_, effects) = self.mark_stale(
                    node,
                    StaleReason::RefreshRequested,
                    StaleReason::DependencyRefreshed,
                )?;
                self.entries
                    .get_mut(node)
                    .expect("stream entry")
                    .refresh_pending = true;
                return Ok(effects);
            }
            return Err(RuntimeError::Busy(node.clone()));
        }
        let (_, mut effects) = self.mark_stale(
            node,
            StaleReason::RefreshRequested,
            StaleReason::DependencyRefreshed,
        )?;
        effects.extend(self.schedule(vec![node.clone()], true, now));
        Ok(effects)
    }
    /// Atomically admit one explicit finite refresh for the root and its current
    /// dependent closure. This does not change policy, executor traits or identity.
    /// Readiness, selected output ports and activation still govern execution.
    pub fn refresh_downstream(
        &mut self,
        root: &NodeId,
        now: Duration,
    ) -> Result<Vec<Effect<T>>, RuntimeError> {
        self.open()?;
        self.check_construction_refresh(root)?;
        let selected = self.currency_downstream(root)?;
        // No state/effect mutation before every selected node and external edge passes.
        for node in &selected {
            if self.leases.contains_key(node) {
                return Err(RuntimeError::Busy(node.clone()));
            }
            if self.ordered_root(node).is_some() || !self.entries[node].traits.bounded {
                return Err(RuntimeError::UnsupportedDownstream(node.clone()));
            }
            for (dependency, port) in self
                .graph
                .node(node)
                .expect("selected graph node")
                .dependencies()
            {
                if selected.contains(dependency) {
                    continue;
                }
                let input = OutputRef {
                    node: dependency.clone(),
                    port: *port,
                };
                if !matches!(self.output(&input), OutputState::Available(_)) {
                    return Err(RuntimeError::UnavailableDownstreamInput {
                        node: node.clone(),
                        input,
                    });
                }
            }
        }
        let mut effects = self.mark_selected_stale(
            &selected,
            root,
            StaleReason::RefreshRequested,
            StaleReason::DependencyRefreshed,
        );
        for node in &selected {
            self.entries
                .get_mut(node)
                .expect("preflighted node")
                .explicitly_requested = true;
        }
        effects.extend(self.schedule(selected.into_iter().collect(), false, now));
        Ok(effects)
    }
    /// Preflight the whole group before revoking any publication. Explicit intent survives
    /// dependency waiting, including manual/unsafe stages, but is consumed on start/closure.
    pub fn refresh_group(
        &mut self,
        nodes: &[NodeId],
        now: Duration,
    ) -> Result<Vec<Effect<T>>, RuntimeError> {
        self.open()?;
        let mut affected = IndexSet::new();
        for node in nodes {
            self.check_construction_refresh(node)?;
            affected.extend(self.currency_downstream(node)?);
        }
        for node in &affected {
            if self.leases.contains_key(node) {
                return Err(RuntimeError::Busy(node.clone()));
            }
        }
        let mut effects = vec![];
        for node in nodes {
            effects.extend(
                self.mark_stale(
                    node,
                    StaleReason::RefreshRequested,
                    StaleReason::DependencyRefreshed,
                )?
                .1,
            );
        }
        for node in nodes {
            let entry = self.entries.get_mut(node).expect("preflighted group");
            entry.explicitly_requested = true;
            entry.restored = false;
        }
        effects.extend(self.schedule(nodes.to_vec(), false, now));
        Ok(effects)
    }
    /// Cancel the group's waiting stages before scheduling any dependent work. Finished
    /// stages retain their result; physical worker exit remains separately joined.
    pub fn cancel_group(
        &mut self,
        nodes: &[NodeId],
        now: Duration,
    ) -> Result<Vec<Effect<T>>, RuntimeError> {
        self.open()?;
        for node in nodes {
            self.graph.downstream(node)?;
        }
        let mut effects = vec![];
        for node in nodes {
            self.preserve_stopped_stream(node);
        }
        for node in nodes {
            if self.ordered_root(node).is_some() {
                effects.extend(self.cancel_with(
                    node,
                    RuntimeCode::Cancelled.error("The selected stream work was cancelled.", None),
                    now,
                ));
            }
        }
        let mut dependents = IndexSet::new();
        let mut streams = vec![];
        for node in nodes {
            let state = self.graph.node(node).expect("preflighted group").state();
            let streaming = self
                .leases
                .get(node)
                .is_some_and(|lease| lease.kind != WorkKind::Finite && self.is_active(&lease.run));
            if !matches!(
                state,
                NodeState::Pending | NodeState::Stale | NodeState::Running
            ) && !streaming
            {
                continue;
            }
            if streaming {
                streams.push(node.clone());
            }
            let policy = self.entries[node]
                .active
                .as_ref()
                .map(|a| a.flow.clone())
                .unwrap_or_default()
                .join(
                    &self.entries[node]
                        .value
                        .as_ref()
                        .map(|v| v.provenance().policy().clone())
                        .unwrap_or_default(),
                );
            if let Some(run) = self.revoke(node) {
                effects.push(Effect::Cancel(run));
            }
            self.private_charges.shift_remove(node);
            let entry = self.entries.get_mut(node).expect("preflighted group");
            entry.explicitly_requested = false;
            entry.refresh_pending = false;
            entry.restored = false;
            entry.value = None;
            entry.error = Some(
                RuntimeCode::Cancelled
                    .error("The work group was cancelled locally.", None)
                    .with_policy(&policy),
            );
            self.transition(node, NodeState::Cancelled, &mut effects);
            dependents.extend(self.graph.dependents_of(node).cloned());
        }
        for stream in streams {
            effects.extend(self.stream_dependents(&stream, now));
        }
        effects.extend(self.schedule(dependents.into_iter().collect(), false, now));
        Ok(effects)
    }
    /// Preserve dependency identity; replacing inputs must have been checked by workspace planning.
    pub fn replace_payload(
        &mut self,
        node: &NodeId,
        payload: T,
        traits: ExecutionTraits,
        now: Duration,
    ) -> Result<Vec<Effect<T>>, RuntimeError> {
        self.open()?;
        self.graph.replace_payload(node, payload)?;
        self.entries.get_mut(node).expect("graph entry").traits = traits;
        self.invalidate(node, now)
    }
    /// Structural replay has no live cancellation or observation transitions.
    pub(crate) fn drop_held_node(
        &mut self,
        node: &NodeId,
    ) -> Result<IndexSet<NodeId>, RuntimeError> {
        self.open()?;
        let removed = self.graph.downstream(node)?;
        for id in &removed {
            self.require_held(id)?;
        }
        self.remove_structure(node, &removed)?;
        Ok(removed)
    }
    pub fn drop_node(
        &mut self,
        node: &NodeId,
    ) -> Result<(IndexSet<NodeId>, Vec<Effect<T>>), RuntimeError> {
        self.open()?;
        let removed = self.graph.downstream(node)?;
        let roots: IndexSet<_> = removed
            .iter()
            .filter_map(|n| self.ordered_root(n))
            .collect();
        let mut effects = vec![];
        for root in roots {
            effects.extend(self.stop_ordered(
                &root,
                RuntimeCode::Cancelled.error(
                    "The stream pipeline stopped because a stage was removed.",
                    None,
                ),
                Duration::ZERO,
            ));
        }
        for node in &removed {
            if let Some(run) = self.revoke(node) {
                effects.push(Effect::Cancel(run));
            }
        }
        self.remove_structure(node, &removed)?;
        // Entered leases deliberately outlive removal. Late exits cannot resurrect nodes.
        Ok((removed, effects))
    }
    fn remove_structure(
        &mut self,
        node: &NodeId,
        removed: &IndexSet<NodeId>,
    ) -> Result<(), RuntimeError> {
        self.graph.remove(node)?;
        self.ordered.retain(|root, group| {
            group.stages.retain(|s| !removed.contains(s));
            !removed.contains(root) && !group.stages.is_empty()
        });
        self.entries.retain(|id, _| !removed.contains(id));
        self.private_charges.retain(|id, _| !removed.contains(id));
        Ok(())
    }
    pub fn forget(&mut self, node: &NodeId) -> Vec<Effect<T>> {
        self.private_charges.shift_remove(node);
        // Eviction never revokes an active run; stale historical bytes may still be evicted.
        let Some(entry) = self.entries.get_mut(node) else {
            return vec![];
        };
        let stopped = entry.stopped.take().is_some();
        if entry.value.take().is_none() && !stopped {
            return vec![];
        }
        let mut effects = vec![];
        if stopped {
            let state = self.graph.node(node).expect("entry node").state();
            self.transition(node, state, &mut effects);
        }
        if self
            .graph
            .node(node)
            .is_some_and(|node| node.state() == NodeState::Ready)
        {
            let entry = self.entries.get_mut(node).expect("entry");
            entry.automatic_pause = Some(StaleReason::ResultEvicted);
            entry.stale_reason = entry.automatic_pause;
            self.transition(node, NodeState::Stale, &mut effects);
        }
        effects
    }
    /// No terminal branch handlers are launched during shutdown.
    pub fn close(&mut self) -> Vec<Effect<T>> {
        if self.closed {
            return vec![];
        }
        self.closed = true;
        for root in self.ordered.keys().cloned().collect::<Vec<_>>() {
            self.retire_event_credit(&root);
        }
        self.ordered.clear();
        for entry in self.entries.values_mut() {
            entry.explicitly_requested = false;
            entry.refresh_pending = false;
        }
        let nodes = self.entries.keys().cloned().collect::<Vec<_>>();
        nodes
            .into_iter()
            .filter_map(|node| self.revoke(&node).map(Effect::Cancel))
            .collect()
    }
    pub fn input_update_pending(&self, node: &NodeId) -> bool {
        self.entries
            .get(node)
            .is_some_and(|entry| entry.input_update_pending)
    }
    pub fn stale_reason(&self, node: &NodeId) -> Option<StaleReason> {
        (self.graph.node(node)?.state() == NodeState::Stale).then(|| {
            self.entries[node]
                .automatic_pause
                .or(self.entries[node].stale_reason)
                .unwrap_or(StaleReason::Unknown)
        })
    }
    fn transition(&mut self, node: &NodeId, state: NodeState, effects: &mut Vec<Effect<T>>) {
        let entry = self.entries.get_mut(node).expect("transition node exists");
        if state == NodeState::Ready {
            entry.display_revision = entry.display_revision.saturating_add(1);
        }
        if state != NodeState::Stale {
            entry.stale_reason = None;
        } else {
            entry.stale_reason.get_or_insert(StaleReason::Unknown);
        }
        self.graph
            .set_state(node, state)
            .expect("transition node exists");
        let entry = &self.entries[node];
        effects.push(Effect::Observe(Observation {
            revision: entry.display_revision,
            stale_reason: entry.stale_reason,
            delivery: entry.last_delivery.clone(),
            stopped: entry.stopped.clone(),
            node: node.clone(),
            run: entry.last_run.clone(),
            state,
            value: (state == NodeState::Ready)
                .then(|| entry.value.clone())
                .flatten(),
            error: matches!(state, NodeState::Failed | NodeState::Cancelled)
                .then(|| entry.error.clone())
                .flatten(),
        }));
    }
    fn schedule(&mut self, candidates: Vec<NodeId>, forced: bool, now: Duration) -> Vec<Effect<T>> {
        if self.closed {
            return vec![];
        }
        let mut queue = VecDeque::from(candidates);
        let mut effects = vec![];
        while let Some(id) = queue.pop_front() {
            let Some(node) = self.graph.node(&id) else {
                continue;
            };
            let entry = &self.entries[&id];
            if !self.pipeline_predecessor_finished(&id) {
                continue;
            }
            if let Some(root) = &entry.ordered_root {
                if self
                    .ordered
                    .get(root)
                    .is_none_or(|g| g.stopped || g.current.is_none())
                {
                    continue;
                }
            }
            if !forced
                && !entry.explicitly_requested
                && (entry.restored
                    || (entry.policy.unwrap_or(self.default_policy) == Policy::Automatic
                        && entry.automatic_paused()))
            {
                continue;
            }
            if matches!(node.state(), NodeState::Pending | NodeState::Stale) {
                if let Some(output) = &entry.activation {
                    match self.ordered_output(&id, output) {
                        OutputState::Pending => continue,
                        OutputState::Closed => {
                            self.skip_pipeline_delivery(&id, &mut effects);
                            queue.extend(self.pipeline_successors(&id));
                            continue;
                        }
                        OutputState::Available(_) => {}
                    }
                }
                let closed = node
                    .dependencies()
                    .iter()
                    .filter(|(node, port)| {
                        self.ordered_output(
                            &id,
                            &OutputRef {
                                node: (*node).clone(),
                                port: **port,
                            },
                        ) == OutputState::Closed
                    })
                    .map(|(node, port)| (node.clone(), *port))
                    .collect::<Vec<_>>();
                if !closed.is_empty() {
                    let unselected = closed.iter().any(|(node, port)| {
                        *port != OutputPort::Data
                            || self
                                .graph
                                .node(node)
                                .is_none_or(|node| node.state() != NodeState::Failed)
                    });
                    let error = if unselected {
                        None
                    } else {
                        let cause = self.entries[&closed[0].0]
                            .error
                            .as_ref()
                            .expect("failed dependency has error");
                        Some(
                            RuntimeCode::InputFailed
                                .error(
                                    format!("an input failed: {}", cause.message()),
                                    Some(cause.id().clone()),
                                )
                                .with_policy(cause.policy()),
                        )
                    };
                    self.private_charges.shift_remove(&id);
                    let entry = self.entries.get_mut(&id).expect("graph entry");
                    entry.explicitly_requested = false;
                    entry.value = None;
                    entry.error = error;
                    self.transition(
                        &id,
                        if unselected {
                            NodeState::Skipped
                        } else {
                            NodeState::Failed
                        },
                        &mut effects,
                    );
                    queue.extend(self.pipeline_successors(&id));
                    continue;
                }
            }
            let may_start = forced
                || entry.explicitly_requested
                || node.state() == NodeState::Pending
                || (node.state() == NodeState::Stale
                    && entry.traits.repeatable
                    && match entry.policy.unwrap_or(self.default_policy) {
                        Policy::Manual => false,
                        Policy::Reactive => true,
                        Policy::Automatic => {
                            entry.traits.pure
                                && entry.traits.bounded
                                && matches!(
                                    entry.stale_reason,
                                    Some(
                                        StaleReason::DependencyRefreshed
                                            | StaleReason::StreamUpdated
                                            | StaleReason::InputBehind
                                    )
                                )
                        }
                    });
            if !may_start || self.leases.contains_key(&id) || node.state() == NodeState::Running {
                continue;
            }
            let inputs = node
                .dependencies()
                .iter()
                .map(|(node, port)| {
                    match self.ordered_output(
                        &id,
                        &OutputRef {
                            node: node.clone(),
                            port: *port,
                        },
                    ) {
                        OutputState::Available(value) => Some((node.clone(), value)),
                        _ => None,
                    }
                })
                .collect::<Option<IndexMap<_, _>>>();
            let Some(inputs) = inputs else {
                continue;
            };
            let payload = node.payload().clone();
            let entry = self.entries.get_mut(&id).expect("graph entry");
            let run = Run {
                node: id.clone(),
                id: RunId::fresh(),
            };
            let attempt = Attempt {
                flow: inputs
                    .values()
                    .fold(wes_core::flow::FlowPolicy::default(), |policy, value| {
                        policy.join(value.provenance().policy())
                    }),
                run: run.clone(),
                started: now,
                budget: entry
                    .timeout
                    .or_else(|| entry.traits.bounded.then_some(self.default_timeout)),
                revision: 0,
            };
            let deadline = attempt.deadline();
            entry.explicitly_requested = false;
            entry.automatic_pause = None;
            entry.active = Some(attempt);
            entry.stopped = None;
            entry.last_run = Some(run.id.clone());
            entry.stream_start = None;
            entry.stream_counts = None;
            entry.error = None;
            entry.restored = false;
            self.leases.insert(
                id.clone(),
                Lease {
                    run: run.clone(),
                    entered: false,
                    kind: WorkKind::Finite,
                },
            );
            if let Some(deadline) = deadline {
                effects.push(Effect::Watch(deadline));
            }
            if let Some(root) = self.entries[&id].ordered_root.clone() {
                let count = self
                    .ordered
                    .get_mut(&root)
                    .expect("installed ordered group")
                    .deliveries
                    .entry(id.clone())
                    .or_default();
                *count += 1;
                let sequence = *count;
                let epoch = self.run_of(&root).expect("active ordered source").clone();
                self.entries.get_mut(&id).expect("stage").last_delivery =
                    Some((root, epoch, sequence));
            }
            self.transition(&id, NodeState::Running, &mut effects);
            effects.push(Effect::Spawn(RunTicket {
                run,
                payload,
                inputs,
            }));
        }
        effects
    }
}

pub(crate) fn valid_timeout(budget: Duration) -> Result<(), RuntimeError> {
    if budget.is_zero() || budget.as_nanos() > i64::MAX as u128 {
        Err(RuntimeError::InvalidTimeout)
    } else {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn activation_waits_before_other_errors_and_preserves_ordinary_dependency_failures() {
        for gate_succeeds in [false, true] {
            let mut runtime = Runtime::new();
            let traits = ExecutionTraits {
                pure: false,
                repeatable: true,
                bounded: true,
            };
            let broken = runtime.add((), [], traits).unwrap();
            let gate = runtime.add((), [], traits).unwrap();
            let reference = |node: &NodeId| OutputRef {
                node: node.clone(),
                port: OutputPort::Data,
            };
            let ordinary = runtime.add((), [reference(&broken)], traits).unwrap();
            let piped = runtime
                .add((), [reference(&broken), reference(&gate)], traits)
                .unwrap();
            runtime.install_activation(&piped, Some(reference(&gate)));
            let effects = runtime.start(Duration::ZERO);
            let runs = effects
                .into_iter()
                .filter_map(|effect| match effect {
                    Effect::Spawn(ticket) => Some((ticket.run.node.clone(), ticket.run)),
                    _ => None,
                })
                .collect::<IndexMap<_, _>>();
            assert_eq!(runs.len(), 2);
            for run in runs.values() {
                assert!(runtime.enter(run));
            }
            runtime.complete(
                &runs[&broken],
                Outcome::Failed(RuntimeCode::InputFailed.error("fixture", None)),
                Duration::ZERO,
            );
            assert_eq!(
                runtime.graph.node(&ordinary).unwrap().state(),
                NodeState::Failed
            );
            assert_eq!(
                runtime.graph.node(&piped).unwrap().state(),
                NodeState::Pending
            );
            let outcome = if gate_succeeds {
                Outcome::Produced(
                    Value::new(
                        wes_core::Shape::Primitive(wes_core::Primitive::Int),
                        wes_core::Data::Int(1),
                        Default::default(),
                    )
                    .unwrap(),
                )
            } else {
                Outcome::Failed(RuntimeCode::InputFailed.error("gate fixture", None))
            };
            let effects = runtime.complete(&runs[&gate], outcome, Duration::ZERO);
            assert!(
                effects
                    .iter()
                    .all(|effect| !matches!(effect, Effect::Spawn(_)))
            );
            assert_eq!(
                runtime.graph.node(&piped).unwrap().state(),
                if gate_succeeds {
                    NodeState::Failed
                } else {
                    NodeState::Skipped
                }
            );
            assert_eq!(runtime.error_of(&piped).is_some(), gate_succeeds);
        }
    }

    #[test]
    fn private_runtime_budget_rejects_before_publication_and_releases_on_forget() {
        use wes_core::{Data, Primitive, Provenance, Shape, flow::FlowPolicy};
        let mut runtime = Runtime::new();
        let traits = ExecutionTraits {
            pure: false,
            repeatable: true,
            bounded: true,
        };
        let value = Value::new(
            Shape::Primitive(Primitive::Bytes),
            Data::Bytes(vec![0; 17 * 1024 * 1024].into()),
            Provenance::default().with_policy(&FlowPolicy::default().private()),
        )
        .unwrap();
        let a = NodeId::new("private-a").unwrap();
        runtime
            .restore(
                a.clone(),
                (),
                [],
                traits,
                RestoredState::Ready(value.clone()),
                None,
            )
            .unwrap();
        assert!(runtime.value_of(&a).is_some());
        let b = NodeId::new("private-b").unwrap();
        runtime
            .restore(
                b.clone(),
                (),
                [],
                traits,
                RestoredState::Ready(value.clone()),
                None,
            )
            .unwrap();
        assert!(runtime.value_of(&b).is_none());
        assert!(
            runtime.entries[&b]
                .error
                .as_ref()
                .unwrap()
                .policy()
                .is_private()
        );
        runtime.forget(&a);
        runtime
            .restore_held_state(&b, RestoredState::Ready(value), None)
            .unwrap();
        assert!(runtime.value_of(&b).is_some());
        runtime.drop_node(&b).unwrap();
        assert!(runtime.private_charges.is_empty());
    }

    #[test]
    fn timeout_batch_preflight_checks_aggregate_revision_without_mutating_the_attempt() {
        let mut runtime = Runtime::new();
        let node = runtime
            .add(
                (),
                [],
                ExecutionTraits {
                    pure: false,
                    repeatable: true,
                    bounded: true,
                },
            )
            .unwrap();
        assert!(runtime.validate_timeout_updates(&node, u64::MAX).is_ok());
        runtime.start(Duration::ZERO);
        runtime
            .entries
            .get_mut(&node)
            .unwrap()
            .active
            .as_mut()
            .unwrap()
            .revision = u64::MAX - 1;
        assert!(runtime.validate_timeout_updates(&node, 1).is_ok());
        assert!(matches!(
            runtime.validate_timeout_updates(&node, 2),
            Err(RuntimeError::RevisionExhausted)
        ));
        assert_eq!(
            runtime.entries[&node].active.as_ref().unwrap().revision,
            u64::MAX - 1
        );
        runtime.set_timeout(&node, Duration::from_secs(2)).unwrap();
        assert!(matches!(
            runtime.validate_timeout_updates(&node, 1),
            Err(RuntimeError::RevisionExhausted)
        ));
        assert!(
            runtime
                .validate_timeout_updates(&NodeId::new("future-node").unwrap(), 2)
                .is_ok()
        );
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RuntimeCode {
    ExecutionFailed,
    TimedOut,
    Cancelled,
    InputFailed,
    RecordingFailed,
    StreamOverloaded,
}
fn typing(value: &Value) -> Arc<Typing> {
    Arc::new(Typing {
        shape: value.shape().clone(),
        provenance: value.provenance().clone(),
    })
}
impl RuntimeCode {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::ExecutionFailed => "RUN001",
            Self::TimedOut => "RUN002",
            Self::Cancelled => "RUN003",
            Self::InputFailed => "RUN004",
            Self::RecordingFailed => "RUN005",
            Self::StreamOverloaded => "RUN006",
        }
    }
    pub fn error(self, message: impl Into<String>, cause: Option<ErrorId>) -> ErrorValue {
        ErrorValue::new(
            ErrorId::new(Uuid::new_v4().to_string()).expect("UUID is nonblank"),
            self.as_str(),
            message,
            vec![],
            cause,
        )
        .expect("runtime error fields are valid")
    }
}

#[cfg(test)]
mod downstream_tests {
    use super::*;
    const FINITE: ExecutionTraits = ExecutionTraits {
        pure: false,
        repeatable: true,
        bounded: true,
    };

    #[test]
    fn downstream_rejects_ordered_roots_and_members_before_mutation() {
        let mut runtime = Runtime::new();
        let root = runtime.add((), [], FINITE).unwrap();
        let stage = runtime
            .add((), [OutputRef::data(root.clone())], FINITE)
            .unwrap();
        runtime.install_ordered_stage(&stage, &root);
        for requested in [&root, &stage] {
            assert_eq!(
                runtime
                    .refresh_downstream(requested, Duration::ZERO)
                    .unwrap_err(),
                RuntimeError::UnsupportedDownstream(requested.clone())
            );
            for node in [&root, &stage] {
                assert_eq!(
                    runtime.graph.node(node).unwrap().state(),
                    NodeState::Pending
                );
                assert!(!runtime.entries[node].explicitly_requested);
                assert!(runtime.entries[node].last_run.is_none());
            }
        }
    }

    #[test]
    fn downstream_preflight_includes_external_activation_and_preserves_request_metadata() {
        let mut runtime = Runtime::new();
        let gate = runtime.add((), [], FINITE).unwrap();
        let root = runtime.add((), [], FINITE).unwrap();
        let input = OutputRef::data(gate.clone());
        let child = runtime
            .add((), [OutputRef::data(root.clone()), input.clone()], FINITE)
            .unwrap();
        runtime.install_activation(&child, Some(input.clone()));
        assert_eq!(
            runtime
                .refresh_downstream(&root, Duration::ZERO)
                .unwrap_err(),
            RuntimeError::UnavailableDownstreamInput {
                node: child.clone(),
                input
            }
        );
        assert_eq!(
            runtime.graph.node(&root).unwrap().state(),
            NodeState::Pending
        );
        assert!(
            runtime
                .entries
                .values()
                .all(|entry| !entry.explicitly_requested)
        );
    }

    #[test]
    fn downstream_preserves_policies_traits_definitions_and_consumes_intent_on_close() {
        let mut runtime = Runtime::new();
        runtime.set_default_policy(Policy::Reactive);
        let root = runtime.add((), [], FINITE).unwrap();
        let child = runtime
            .add(
                (),
                [OutputRef::data(root.clone())],
                ExecutionTraits {
                    pure: false,
                    repeatable: false,
                    bounded: true,
                },
            )
            .unwrap();
        runtime.set_policy(&child, Policy::Manual).unwrap();
        runtime
            .set_timeout(&child, Duration::from_secs(17))
            .unwrap();
        runtime.refresh_downstream(&root, Duration::ZERO).unwrap();
        assert!(!runtime.entries[&root].explicitly_requested); // consumed when root starts
        assert!(runtime.entries[&child].explicitly_requested);
        assert_eq!(runtime.entries[&child].policy, Some(Policy::Manual));
        assert!(!runtime.entries[&child].traits.repeatable);
        assert!(runtime.entries[&child].traits.bounded);
        assert_eq!(
            runtime.entries[&child].timeout,
            Some(Duration::from_secs(17))
        );
        assert_eq!(runtime.default_policy, Policy::Reactive);
        runtime.close();
        assert!(
            runtime
                .entries
                .values()
                .all(|entry| !entry.explicitly_requested && !entry.refresh_pending)
        );
        assert_eq!(
            runtime
                .refresh_downstream(&root, Duration::ZERO)
                .unwrap_err(),
            RuntimeError::Closed
        );
        assert!(runtime.start(Duration::ZERO).is_empty());
    }
}

#[cfg(test)]
mod creation_tests {
    use super::*;
    use wes_core::{Data, Primitive, Provenance, Shape};
    const FINITE: ExecutionTraits = ExecutionTraits {
        pure: true,
        repeatable: true,
        bounded: true,
    };
    fn value(number: i64) -> Value {
        Value::new(
            Shape::Primitive(Primitive::Int),
            Data::Int(number.into()),
            Provenance::default(),
        )
        .unwrap()
    }
    fn spawns(effects: Vec<Effect<()>>) -> Vec<RunTicket<()>> {
        effects
            .into_iter()
            .filter_map(|effect| match effect {
                Effect::Spawn(ticket) => Some(ticket),
                _ => None,
            })
            .collect()
    }
    fn finish(
        runtime: &mut Runtime<()>,
        ticket: RunTicket<()>,
        outcome: Outcome,
    ) -> Vec<RunTicket<()>> {
        assert!(runtime.enter(&ticket.run));
        spawns(runtime.complete(&ticket.run, outcome, Duration::ZERO))
    }
    #[test]
    fn creation_gate_preserves_structural_ownership_and_other_currency_paths() {
        let mut runtime = Runtime::new();
        let root = runtime.add((), [], FINITE).unwrap();
        let created = runtime
            .add((), [OutputRef::data(root.clone())], FINITE)
            .unwrap();
        runtime.install_dependency_lifetime(&created, DependencyLifetime::Creation);
        runtime.install_activation(&created, Some(OutputRef::data(root.clone())));
        let derived = runtime
            .add(
                (),
                [
                    OutputRef::data(root.clone()),
                    OutputRef::data(created.clone()),
                ],
                FINITE,
            )
            .unwrap();
        let first = spawns(runtime.start(Duration::ZERO)).pop().unwrap();
        assert_eq!(first.run.node(), &root);
        let mut work = finish(&mut runtime, first, Outcome::Produced(value(1)));
        let constructor = work.pop().unwrap();
        assert_eq!(constructor.run.node(), &created);
        let creation_run = constructor.run.id().clone();
        let mut work = finish(&mut runtime, constructor, Outcome::Produced(value(100)));
        let consumer = work.pop().unwrap();
        assert_eq!(consumer.run.node(), &derived);
        finish(&mut runtime, consumer, Outcome::Produced(value(101)));
        let descendants = runtime.graph.downstream(&root).unwrap();
        assert!(descendants.contains(&created) && descendants.contains(&derived));
        let mut work = spawns(runtime.refresh_downstream(&root, Duration::ZERO).unwrap());
        assert_eq!(
            runtime.graph.node(&created).unwrap().state(),
            NodeState::Ready
        );
        assert_eq!(runtime.run_of(&created), Some(&creation_run));
        let refreshed = work.pop().unwrap();
        assert_eq!(refreshed.run.node(), &root);
        let mut work = finish(&mut runtime, refreshed, Outcome::Produced(value(2)));
        let consumer = work.pop().unwrap();
        assert_eq!(consumer.run.node(), &derived);
        assert_eq!(consumer.inputs[&created].data(), &Data::Int(100));
        finish(&mut runtime, consumer, Outcome::Produced(value(102)));
        assert!(runtime.refresh(&created, Duration::ZERO).is_err());
        assert!(
            runtime
                .refresh_group(&[root.clone(), created.clone()], Duration::ZERO)
                .is_err()
        );
        assert_eq!(runtime.graph.node(&root).unwrap().state(), NodeState::Ready);
        assert_eq!(runtime.run_of(&created), Some(&creation_run));
        assert!(runtime.drop_node(&root).unwrap().0.contains(&created));
    }
    #[test]
    fn stream_windows_do_not_invalidate_a_completed_creation_consumer() {
        let mut runtime = Runtime::new();
        let root = runtime.add((), [], FINITE).unwrap();
        let created = runtime
            .add((), [OutputRef::data(root.clone())], FINITE)
            .unwrap();
        runtime.install_dependency_lifetime(&created, DependencyLifetime::Creation);
        runtime.install_activation(&created, Some(OutputRef::data(root.clone())));
        let first = spawns(runtime.start(Duration::ZERO)).pop().unwrap();
        assert!(runtime.enter_stream(&first.run));
        let mut work = spawns(
            runtime
                .stream_window(&first.run, value(1), Duration::ZERO)
                .unwrap(),
        );
        let constructor = work.pop().unwrap();
        let construction_run = constructor.run.id().clone();
        assert!(finish(&mut runtime, constructor, Outcome::Produced(value(100))).is_empty());
        assert!(
            spawns(
                runtime
                    .stream_window(&first.run, value(2), Duration::ZERO)
                    .unwrap()
            )
            .is_empty()
        );
        assert_eq!(
            runtime.graph.node(&created).unwrap().state(),
            NodeState::Ready
        );
        assert_eq!(runtime.run_of(&created), Some(&construction_run));
        assert!(runtime.construction_complete(&created));
        runtime.cancel(&root, Duration::ZERO);
        assert!(runtime.stopped_value(&root).is_some());
        assert!(runtime.stopped_value(&created).is_none());
        assert_eq!(
            runtime.graph.node(&created).unwrap().state(),
            NodeState::Ready
        );
        assert_eq!(runtime.run_of(&created), Some(&construction_run));
    }
    #[test]
    fn failed_initial_input_does_not_consume_the_gate_and_restored_success_does() {
        let mut runtime = Runtime::new();
        let root = runtime.add((), [], FINITE).unwrap();
        let created = runtime
            .add((), [OutputRef::data(root.clone())], FINITE)
            .unwrap();
        runtime.install_dependency_lifetime(&created, DependencyLifetime::Creation);
        runtime.install_activation(&created, Some(OutputRef::data(root.clone())));
        let first = spawns(runtime.start(Duration::ZERO)).pop().unwrap();
        assert!(
            finish(
                &mut runtime,
                first,
                Outcome::Failed(RuntimeCode::ExecutionFailed.error("synthetic", None))
            )
            .is_empty()
        );
        assert_eq!(
            runtime.graph.node(&created).unwrap().state(),
            NodeState::Skipped
        );
        assert!(!runtime.construction_complete(&created));
        let mut restored = Runtime::new();
        let root = NodeId::new("root").unwrap();
        let created = NodeId::new("created").unwrap();
        restored
            .restore(
                root.clone(),
                (),
                [],
                FINITE,
                RestoredState::Ready(value(1)),
                None,
            )
            .unwrap();
        restored
            .restore(
                created.clone(),
                (),
                [OutputRef::data(root.clone())],
                FINITE,
                RestoredState::Stale,
                None,
            )
            .unwrap();
        restored.install_dependency_lifetime(&created, DependencyLifetime::Creation);
        restored
            .restore_held_state(&created, RestoredState::Ready(value(100)), None)
            .unwrap();
        assert!(restored.construction_complete(&created));
        assert!(restored.start(Duration::ZERO).is_empty());
        restored.refresh(&root, Duration::ZERO).unwrap();
        assert_eq!(
            restored.graph.node(&created).unwrap().state(),
            NodeState::Ready
        );
        assert!(restored.refresh(&created, Duration::ZERO).is_err());
    }
}
