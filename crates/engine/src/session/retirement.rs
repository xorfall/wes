//! Work retirement is distinct from graph dropping and payload release.
//! Planning is bounded by the checkpoint image and never executes providers.
use super::{Actor, Control, SessionCheckpoint, SessionError, SessionHandle, SessionObservation};
use crate::{
    driver::CancellationToken,
    graph::{NodeId, NodeState},
    history::{
        HistoryCapture, HistoryCaptureLimits, HistoryImage, JournalEntry, Record, RecoveryEntry,
        RetiredWork,
    },
    storage::ValueHandle,
    workspace::{ReplayWorkspace, Workspace},
};
use std::collections::{BTreeMap, BTreeSet};
use tokio::sync::oneshot;

#[derive(Debug, thiserror::Error)]
pub enum RetirementError {
    #[error("The selected work is not available in this workspace.")]
    Missing,
    #[error("Wait for execution, streams and workspace operations to settle before deleting work.")]
    Busy,
    #[error("Some affected work has not finished. No work was deleted.")]
    Blocked(Vec<RetirementBlocker>),
    #[error(
        "This work has an unresolved external-call outcome. Its recovery evidence cannot be deleted."
    )]
    Uncertain,
    #[error(
        "Deletion would change surviving definitions, bindings or captured configuration. No work was deleted. This version supports independently removable node declarations and submission history, not configuration-changing command groups."
    )]
    Reconstruction,
    #[error("The deletion plan exceeds the bounded history or identity budget.")]
    Capacity,
}

/// Authoritative lifecycle evidence, independent of presentation/serialization.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RetirementBlocker {
    pub node: Option<NodeId>,
    pub cells: Vec<String>,
    pub state: String,
    pub reason: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RetirementPlan {
    pub cells: BTreeSet<String>,
    pub nodes: BTreeSet<NodeId>,
    pub payloads: BTreeSet<ValueHandle>,
    pub dependents: BTreeSet<String>,
}
impl RetirementPlan {
    pub fn prepare(
        image: &HistoryImage,
        observation: &SessionObservation,
        cell: &str,
    ) -> Result<Self, RetirementError> {
        let plan = Self::closure(image.journal(), observation, cell)?;
        plan.require_completed(observation)?;
        let state = &observation.state;
        if !state.execution.idle
            || state.admission_pending
            || !state.execution.streaming.is_empty()
            || !state.conversations.is_empty()
        {
            return Err(RetirementError::Busy);
        }
        plan.finish(image, observation)
    }

    /// Early refusal before acquiring a global checkpoint. The complete captured
    /// history is checked again under the checkpoint; this is not delete authority.
    pub fn preflight(observation: &SessionObservation, cell: &str) -> Result<(), RetirementError> {
        Self::closure(&[], observation, cell)?.require_completed(observation)
    }

    fn closure(
        journal: &[JournalEntry],
        observation: &SessionObservation,
        cell: &str,
    ) -> Result<Self, RetirementError> {
        let state = &observation.state;
        let selected = observation
            .cells
            .iter()
            .find(|c| c.input.cell() == cell)
            .ok_or(RetirementError::Missing)?;
        let roots = observation.work_roots();
        let origin = roots.get(selected.input.cell()).copied().unwrap_or(cell);
        let mut plan = Self {
            cells: BTreeSet::from([origin.to_owned(), cell.to_owned()]),
            nodes: BTreeSet::new(),
            payloads: BTreeSet::new(),
            dependents: BTreeSet::new(),
        };
        let mut work = 0usize;
        let mut expanded = BTreeSet::new();
        loop {
            let before = (plan.cells.len(), plan.nodes.len());
            for c in &observation.cells {
                work += 1;
                if work > 1_000_000 {
                    return Err(RetirementError::Capacity);
                }
                let nodes = c
                    .reply
                    .as_ref()
                    .and_then(|r| r.as_ref().ok())
                    .map(|r| r.nodes.as_slice())
                    .unwrap_or(&[]);
                if plan.cells.contains(c.input.cell())
                    || roots
                        .get(c.input.cell())
                        .is_some_and(|r| plan.cells.contains(*r))
                    || nodes.iter().any(|n| plan.nodes.contains(n))
                {
                    plan.cells.insert(c.input.cell().into());
                    plan.nodes.extend(nodes.iter().cloned());
                }
            }
            for entry in journal {
                if let JournalEntry::Command(c) = entry {
                    if plan.cells.contains(&c.cell)
                        || c.nodes.iter().any(|n| plan.nodes.contains(n))
                    {
                        c.validate().map_err(|_| RetirementError::Reconstruction)?;
                        plan.cells.insert(c.cell.clone());
                        plan.nodes.extend(c.nodes.iter().cloned());
                    }
                }
            }
            for node in plan.nodes.clone() {
                if !expanded.contains(&node) && state.execution.graph.node(&node).is_some() {
                    let downstream = state
                        .execution
                        .graph
                        .downstream(&node)
                        .map_err(|_| RetirementError::Reconstruction)?;
                    work = work.saturating_add(downstream.len());
                    if work > 1_000_000 {
                        return Err(RetirementError::Capacity);
                    }
                    expanded.extend(downstream.iter().cloned());
                    plan.nodes.extend(downstream);
                }
            }
            if before == (plan.cells.len(), plan.nodes.len()) {
                break;
            }
        }
        for c in &observation.cells {
            if plan.cells.contains(c.input.cell())
                && c.input.cell() != origin
                && roots.get(c.input.cell()).is_none_or(|r| *r != origin)
            {
                plan.dependents.insert(c.input.cell().into());
            }
        }
        if plan.nodes.len() > 10_000 {
            return Err(RetirementError::Capacity);
        }
        Ok(plan)
    }

    fn require_completed(&self, observation: &SessionObservation) -> Result<(), RetirementError> {
        let execution = &observation.state.execution;
        let owners = |node: &NodeId| {
            observation
                .cells
                .iter()
                .filter(|c| {
                    c.reply
                        .as_ref()
                        .and_then(|r| r.as_ref().ok())
                        .is_some_and(|r| r.nodes.contains(node))
                })
                .map(|c| c.input.cell().to_owned())
                .collect::<Vec<_>>()
        };
        let mut blockers = Vec::new();
        for id in &self.nodes {
            let Some(node) = execution.graph.node(id) else {
                continue;
            };
            let state = node.state();
            let reason = if execution.streaming.contains(id)
                || observation.state.conversations.contains_key(id)
            {
                Some("The stream or conversation is still open.")
            } else if execution.executing.contains(id) {
                Some("Execution has not physically exited yet.")
            } else if !matches!(
                state,
                NodeState::Ready | NodeState::Failed | NodeState::Cancelled | NodeState::Skipped
            ) {
                Some("This work has not completed its current execution lifecycle.")
            } else {
                None
            };
            if let Some(reason) = reason {
                blockers.push(RetirementBlocker {
                    node: Some(id.clone()),
                    cells: owners(id),
                    state: format!("{state:?}"),
                    reason: reason.into(),
                });
            }
        }
        for cell in &observation.cells {
            if self.cells.contains(cell.input.cell()) && cell.reply.is_none() {
                blockers.push(RetirementBlocker {
                    node: None,
                    cells: vec![cell.input.cell().into()],
                    state: "Preparing".into(),
                    reason: "Submission preparation or admission has not finished.".into(),
                });
            }
        }
        if blockers.is_empty() {
            Ok(())
        } else {
            Err(RetirementError::Blocked(blockers))
        }
    }

    fn finish(
        mut self,
        image: &HistoryImage,
        observation: &SessionObservation,
    ) -> Result<Self, RetirementError> {
        let plan = &mut self;
        if crate::history::unresolved_calls(image.recovery())
            .iter()
            .any(|c| plan.nodes.contains(&c.node) || plan.cells.contains(&c.cell))
        {
            return Err(RetirementError::Uncertain);
        }
        for entry in image.journal() {
            if let JournalEntry::Payload { node, handle, .. }
            | JournalEntry::ProtectedRun { node, handle, .. } = entry
                && plan.nodes.contains(node)
            {
                plan.payloads.insert(handle.clone());
            }
            if let JournalEntry::Noticed(n) = entry
                && n.context().node().is_some_and(|n| plan.nodes.contains(n))
                && let Some(handle) = n.context().handle()
            {
                plan.payloads.insert(handle.clone());
            }
            if let Some(r) = entry.retained_result()
                && plan.nodes.contains(&r.node)
            {
                plan.payloads.insert(r.handle.clone());
            }
        }
        if let Some(values) = &observation.values {
            for (node, output) in &values.outputs {
                if plan.nodes.contains(node)
                    && let Some(handle) = output.handle()
                {
                    plan.payloads.insert(handle.clone());
                }
            }
        }
        if plan.nodes.len() > 10_000 || plan.payloads.len() > 10_000 {
            return Err(RetirementError::Capacity);
        }
        Ok(self)
    }

    /// Replay both histories without execution or authority restoration. Selected
    /// command groups must contain only node declarations. Surviving dependencies
    /// and bindings must be identical; shadowed-name resurrection is refused.
    pub async fn validate(
        &self,
        image: &HistoryImage,
        original: Workspace,
        candidate: Workspace,
    ) -> Result<(), RetirementError> {
        let fail = |_| RetirementError::Reconstruction;
        let mut original = ReplayWorkspace::validation(original).map_err(fail)?;
        let mut candidate = ReplayWorkspace::validation(candidate).map_err(fail)?;
        let mut seen = BTreeSet::new();
        let mut environments = BTreeSet::new();
        for entry in image.journal() {
            match entry {
                JournalEntry::Retired(r) => {
                    original.reserve_retired(r).map_err(fail)?;
                    candidate.reserve_retired(r).map_err(fail)?;
                }
                JournalEntry::Environments(r) => {
                    if !environments.insert(r.id()) {
                        continue;
                    }
                    original
                        .restore_environments(r)
                        .await
                        .map_err(|_| RetirementError::Reconstruction)?;
                    candidate
                        .restore_environments(r)
                        .await
                        .map_err(|_| RetirementError::Reconstruction)?;
                }
                JournalEntry::Command(c) => {
                    c.validate().map_err(|_| RetirementError::Reconstruction)?;
                    if !seen.insert(c.cell.clone()) {
                        continue;
                    }
                    let prepared = original
                        .prepare(c, CancellationToken::default())
                        .await
                        .map_err(|_| RetirementError::Reconstruction)?;
                    if self.cells.contains(&c.cell)
                        && (prepared.batch.nodes().count() != prepared.batch.len()
                            || prepared.batch.is_empty())
                    {
                        return Err(RetirementError::Reconstruction);
                    }
                    original.apply(prepared).map_err(fail)?;
                    if !self.cells.contains(&c.cell) {
                        let prepared = candidate
                            .prepare(c, CancellationToken::default())
                            .await
                            .map_err(|_| RetirementError::Reconstruction)?;
                        candidate.apply(prepared).map_err(fail)?;
                        for node in &c.nodes {
                            let left = original.workspace().runtime().graph().node(node);
                            let right = candidate.workspace().runtime().graph().node(node);
                            if left.map(|n| n.dependencies()) != right.map(|n| n.dependencies()) {
                                return Err(RetirementError::Reconstruction);
                            }
                        }
                    }
                }
                _ => {}
            }
        }
        let expected: BTreeMap<_, _> = original
            .workspace()
            .bindings()
            .names()
            .iter()
            .filter(|(_, r)| !self.nodes.contains(&r.node))
            .collect();
        let actual: BTreeMap<_, _> = candidate.workspace().bindings().names().iter().collect();
        let expected_nodes: BTreeSet<_> = original
            .workspace()
            .runtime()
            .graph()
            .nodes()
            .filter(|n| !self.nodes.contains(n.id()))
            .map(|n| n.id())
            .collect();
        let actual_nodes: BTreeSet<_> = candidate
            .workspace()
            .runtime()
            .graph()
            .nodes()
            .map(|n| n.id())
            .collect();
        if expected != actual || expected_nodes != actual_nodes {
            return Err(RetirementError::Reconstruction);
        }
        Ok(())
    }

    pub fn compact(
        &self,
        image: &HistoryImage,
        protected: Vec<ValueHandle>,
    ) -> Result<HistoryImage, RetirementError> {
        let mut capture = HistoryCapture::new(HistoryCaptureLimits::default());
        let mut push = |record| capture.push(record).map_err(|_| RetirementError::Capacity);
        let retired = RetiredWork {
            nodes: self.nodes.iter().cloned().collect(),
            payloads: self.payloads.iter().cloned().collect(),
            protected,
        };
        retired.validate().map_err(|_| RetirementError::Capacity)?;
        push(Record::Journal(JournalEntry::Retired(retired)))?;
        let orders: BTreeMap<_, _> = image
            .journal()
            .iter()
            .filter_map(|e| match e {
                JournalEntry::Submitted(s) if !self.cells.contains(&s.cell) => {
                    Some((s.order, s.cell.clone()))
                }
                _ => None,
            })
            .collect();
        let orders: BTreeMap<_, _> = orders
            .into_values()
            .enumerate()
            .map(|(i, c)| (c, i as u64))
            .collect();
        let surviving_handles: BTreeSet<_> = image
            .journal()
            .iter()
            .filter_map(|entry| match entry {
                JournalEntry::Payload { node, handle, .. }
                | JournalEntry::ProtectedRun { node, handle, .. }
                    if !self.nodes.contains(node) =>
                {
                    Some(handle)
                }
                JournalEntry::Snapshot(s) if !self.nodes.contains(s.observation.node()) => {
                    s.result.as_ref().map(|r| &r.handle)
                }
                JournalEntry::Result(r) if !self.nodes.contains(&r.node) => Some(&r.handle),
                _ => None,
            })
            .collect();
        for entry in image.journal() {
            let removed = match entry {
                JournalEntry::Payload { node, .. } | JournalEntry::ProtectedRun { node, .. } => {
                    self.nodes.contains(node)
                }
                JournalEntry::Submitted(s) => self.cells.contains(&s.cell),
                JournalEntry::Command(c) => self.cells.contains(&c.cell),
                JournalEntry::Diagnosed(d) => self.cells.contains(d.cell()),
                JournalEntry::Observed(o) => self.nodes.contains(o.node()),
                JournalEntry::Snapshot(s) => self.nodes.contains(s.observation.node()),
                JournalEntry::Result(r) => self.nodes.contains(&r.node),
                JournalEntry::Trace(t) => self.nodes.contains(&t.node),
                JournalEntry::Noticed(n) => {
                    n.context().node().is_some_and(|n| self.nodes.contains(n))
                        || n.context().handle().is_some_and(|h| {
                            self.payloads.contains(h) && !surviving_handles.contains(h)
                        })
                }
                _ => false,
            };
            if !removed {
                let mut entry = entry.clone();
                if let JournalEntry::Views(record) = &mut entry {
                    record
                        .retire(&self.nodes)
                        .map_err(|_| RetirementError::Reconstruction)?;
                }
                if let JournalEntry::Submitted(s) = &mut entry {
                    s.order = orders[&s.cell];
                }
                push(Record::Journal(entry))?;
            }
        }
        let mut accepted = BTreeSet::new();
        for entry in image.recovery() {
            let removed = match entry {
                RecoveryEntry::Accepted { cell } => {
                    accepted.insert(cell.clone());
                    false
                }
                RecoveryEntry::Calling(c) => self.nodes.contains(&c.node),
                RecoveryEntry::Called { node, .. } => self.nodes.contains(node),
            };
            if !removed {
                push(Record::Recovery(entry.clone()))?;
            }
        }
        for cell in self.cells.difference(&accepted) {
            push(Record::Recovery(RecoveryEntry::Accepted {
                cell: cell.clone(),
            }))?;
        }
        // This is a candidate, not a durable receipt. Adapters return actual receipts after seed.
        Ok(capture.finish(image.checkpoint()))
    }
}

impl SessionHandle {
    /// Admission check for user inspection outside the command mailbox. Reads
    /// admitted before the retirement fence may finish; new reads are refused.
    /// Internal observation/checkpoint validation deliberately remains available.
    pub async fn check_retirement_access(&self) -> Result<(), SessionError> {
        let (reply, receive) = oneshot::channel();
        self.controls
            .send(Control::CheckRetirementAccess(reply))
            .await
            .map_err(|_| SessionError::Stopped)?;
        receive.await.map_err(|_| SessionError::Stopped)?
    }
    /// Convert this acknowledged pause to a retirement fence: newly received
    /// declarations are refused, while ordinary checkpoints retain queue/resume.
    pub async fn fence_retirement_at_checkpoint(
        &self,
        checkpoint: &SessionCheckpoint,
    ) -> Result<(), SessionError> {
        let (reply, receive) = oneshot::channel();
        self.controls
            .send(Control::FenceRetirement(checkpoint.authority(), reply))
            .await
            .map_err(|_| SessionError::Stopped)?;
        receive.await.map_err(|_| SessionError::Stopped)?
    }
    /// Application owner only: publish and replace the recorder sink first, with
    /// this same checkpoint still held. A failed completion must close the app.
    pub async fn retire_at_checkpoint(
        &self,
        plan: RetirementPlan,
        image: std::sync::Arc<HistoryImage>,
        checkpoint: &SessionCheckpoint,
    ) -> Result<(), SessionError> {
        let (reply, receive) = oneshot::channel();
        self.controls
            .send(Control::Retire(plan, image, checkpoint.authority(), reply))
            .await
            .map_err(|_| SessionError::Stopped)?;
        receive.await.map_err(|_| SessionError::Stopped)?
    }
}
impl Actor {
    pub(super) fn retire_work(
        &mut self,
        plan: RetirementPlan,
        image: std::sync::Arc<HistoryImage>,
    ) -> Result<(), SessionError> {
        if self.workspace.runtime().is_closed() {
            return Err(SessionError::Stopped);
        }
        for node in &plan.nodes {
            if self.workspace.runtime().graph().node(node).is_some() {
                let (_, effects) = self
                    .workspace
                    .drop_node(node)
                    .map_err(|_| SessionError::Commit)?;
                self.effects(effects);
            }
        }
        self.cells.retire(&plan.cells);
        self.log.retain_nodes(&self.workspace);
        if let Some(values) = &mut self.values {
            values.retain_nodes(&self.workspace);
        }
        self.workspace.traces.retire(&plan.nodes);
        if let Some(report) = &mut self.restoration {
            let report = std::sync::Arc::make_mut(report);
            report.problems.retain(|p| !plan.nodes.contains(&p.node));
            report
                .unconfirmed_changes
                .retain(|n| !plan.nodes.contains(n));
            report.interrupted.retain(|c| !plan.nodes.contains(&c.node));
        }
        self.log.replace_history(&image);
        Ok(())
    }
}
