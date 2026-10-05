//! Ordered event scheduling with branch-local failure and end-to-end ingress credits.
use super::*;
#[derive(Default)]
pub(super) struct OrderedPipeline {
    pub stages: Vec<NodeId>,
    pub completed: IndexSet<NodeId>,
    pub skipped: IndexSet<NodeId>,
    disabled: IndexSet<Vec<usize>>,
    pub deliveries: IndexMap<NodeId, u64>,
    queue: VecDeque<crate::streams::delivery::Event>,
    current_credit: Option<crate::streams::delivery::Credit>,
    pub current: Option<Value>,
    seen: u64,
    pub stopped: bool,
}
impl OrderedPipeline {
    pub fn reset(&mut self) {
        self.queue.clear();
        self.disabled.clear();
        self.deliveries.clear();
        self.completed.clear();
        self.skipped.clear();
        self.current = None;
        self.current_credit = None;
        self.seen = 0;
        self.stopped = false;
    }
}
impl<T: Clone> Runtime<T> {
    pub(crate) fn is_event_stage(&self, node: &NodeId) -> bool {
        self.entries
            .get(node)
            .is_some_and(|entry| entry.ordered_root.is_some())
    }

    pub(super) fn retire_event_credit(&mut self, root: &NodeId) {
        let Some(group) = self.ordered.get_mut(root) else {
            return;
        };
        if let Some(credit) = group.current_credit.take() {
            let runs: Vec<_> = group
                .stages
                .iter()
                .filter_map(|n| {
                    self.leases
                        .get(n)
                        .filter(|l| l.entered)
                        .map(|l| l.run.clone())
                })
                .collect();
            if !runs.is_empty() {
                self.retiring_events.push((runs, credit));
            }
        }
    }
    pub(crate) fn install_ordered_stage(&mut self, node: &NodeId, root: &NodeId) {
        self.entries
            .get_mut(node)
            .expect("admitted stage")
            .ordered_root = Some(root.clone());
        self.ordered
            .entry(root.clone())
            .or_default()
            .stages
            .push(node.clone());
    }
    /// The current admitted event's identity, captured only under the session actor.
    pub(crate) fn ordered_delivery(&self, node: &NodeId) -> Option<(NodeId, RunId, u64)> {
        let root = self.entries.get(node)?.ordered_root.as_ref()?;
        let group = self.ordered.get(root)?;
        if group.stopped || group.current.is_none() {
            return None;
        }
        Some((
            root.clone(),
            self.run_of(root)?.clone(),
            *group.deliveries.get(node)?,
        ))
    }
    pub fn ordered_root(&self, node: &NodeId) -> Option<NodeId> {
        if self.ordered.contains_key(node) {
            Some(node.clone())
        } else {
            self.entries.get(node)?.ordered_root.clone()
        }
    }
    pub(super) fn ordered_output(&self, consumer: &NodeId, reference: &OutputRef) -> OutputState {
        if self
            .entries
            .get(consumer)
            .and_then(|e| e.ordered_root.as_ref())
            == Some(&reference.node)
        {
            return self
                .ordered
                .get(&reference.node)
                .and_then(|g| g.current.clone())
                .map(OutputState::Available)
                .unwrap_or(OutputState::Pending);
        }
        if let Some(root) = self
            .entries
            .get(consumer)
            .and_then(|e| e.ordered_root.as_ref())
        {
            if self
                .ordered
                .get(root)
                .is_some_and(|g| g.current.is_some() && g.skipped.contains(&reference.node))
            {
                return OutputState::Closed;
            }
        }
        self.output(reference)
    }
    pub(crate) fn ingest_event(
        &mut self,
        run: &Run,
        event: crate::streams::delivery::Event,
        now: Duration,
    ) -> Option<Vec<Effect<T>>> {
        if !self.is_active(run) {
            return None;
        }
        let group = self.ordered.get_mut(run.node())?;
        if group.stopped {
            return None;
        }
        if group.seen.checked_add(1) != Some(event.sequence) {
            return Some(
                self.stop_ordered(
                    run.node(),
                    RuntimeCode::ExecutionFailed
                        .error(
                            "The ordered event sequence contains a gap or regression.",
                            None,
                        )
                        .with_policy(event.value.provenance().policy()),
                    now,
                ),
            );
        }
        // Admission already owns count, per-stream bytes and aggregate byte credits. The same
        // credit moves through this queue and the current event until the complete suffix joins.
        group.seen = event.sequence;
        group.queue.push_back(event);
        Some(self.advance_ordered(run.node(), now))
    }
    pub(super) fn advance_ordered(&mut self, root: &NodeId, now: Duration) -> Vec<Effect<T>> {
        let Some(group) = self.ordered.get(root) else {
            return vec![];
        };
        if group.stopped || group.stages.iter().any(|n| self.leases.contains_key(n)) {
            return vec![];
        }
        if group.current.is_some() {
            if group.stages.iter().any(|n| {
                self.graph.node(n).is_some_and(|n| {
                    matches!(
                        n.state(),
                        NodeState::Pending | NodeState::Stale | NodeState::Running
                    )
                })
            }) {
                return vec![];
            }
            let failed: Vec<_> = group
                .stages
                .iter()
                .filter(|n| {
                    self.graph.node(n).is_some_and(|n| {
                        matches!(n.state(), NodeState::Failed | NodeState::Cancelled)
                    })
                })
                .map(|n| self.entries[n].pipeline.branch.clone())
                .collect();
            if failed.iter().any(Vec::is_empty) {
                return self.stop_ordered(root, RuntimeCode::ExecutionFailed.error("A common stream pipeline stage failed or was cancelled. Later events were not executed.", None), now);
            }
            let finished: Vec<_> = group
                .stages
                .iter()
                .filter(|n| {
                    self.entries[*n].pipeline.limit.is_some_and(|limit| {
                        group
                            .deliveries
                            .get(*n)
                            .is_some_and(|count| *count >= limit)
                    })
                })
                .map(|n| self.entries[n].pipeline.branch.clone())
                .collect();
            let group = self.ordered.get_mut(root).unwrap();
            group.disabled.extend(failed);
            group.disabled.extend(finished);
            // A source opened by this declaration is owned by its event plan. Stop its local
            // subscription when every leaf consumer has ended, without cancelling ready results.
            let has_consumer = group.stages.iter().any(|n| {
                !group
                    .disabled
                    .iter()
                    .any(|path| self.entries[n].pipeline.branch.starts_with(path))
                    && !self
                        .graph
                        .dependents_of(n)
                        .any(|child| group.stages.contains(child))
            });
            if !has_consumer {
                self.preserve_stopped_stream(root);
                return self.stop_ordered(
                    root,
                    RuntimeCode::Cancelled.error(
                        "All event consumers completed or stopped; the owned source was closed.",
                        None,
                    ),
                    now,
                );
            }
        }
        let group = self.ordered.get_mut(root).unwrap();
        group.current = None;
        group.current_credit = None;
        let Some(event) = group.queue.pop_front() else {
            let empty = group.seen == 0;
            let stages = group.stages.clone();
            let mut effects = vec![];
            if empty && self.entries.get(root).is_some_and(|e| e.active.is_none()) {
                for stage in stages {
                    if let Some(entry) = self.entries.get_mut(&stage) {
                        entry.value = None;
                        entry.error = None;
                        entry.explicitly_requested = false;
                    }
                    self.private_charges.shift_remove(&stage);
                    self.transition(&stage, NodeState::Skipped, &mut effects);
                }
            }
            return effects;
        };
        group.current = Some(event.value);
        group.current_credit = Some(event.credit);
        group.completed.clear();
        group.skipped.clear();
        let stages: Vec<_> = group
            .stages
            .iter()
            .filter(|n| {
                !group
                    .disabled
                    .iter()
                    .any(|path| self.entries[*n].pipeline.branch.starts_with(path))
            })
            .cloned()
            .collect();
        let mut effects = vec![];
        let other_groups: IndexSet<_> = stages
            .iter()
            .filter_map(|n| self.graph.downstream(n).ok())
            .flatten()
            .filter_map(|n| self.ordered_root(&n))
            .filter(|other| other != root)
            .collect();
        for other in other_groups {
            effects.extend(self.stop_ordered(
                &other,
                RuntimeCode::Cancelled.error(
                    "The stream pipeline stopped because an upstream event changed its input.",
                    None,
                ),
                now,
            ));
        }
        // Invalidate the whole suffix before any first-stage dispatch can observe old stage values.
        for stage in &stages {
            if let Ok((_, e)) = self.mark_stale(
                stage,
                super::StaleReason::StreamUpdated,
                super::StaleReason::StreamUpdated,
            ) {
                effects.extend(e);
            }
        }
        for stage in &stages {
            if let Some(e) = self.entries.get_mut(stage) {
                e.explicitly_requested = true;
                e.restored = false;
            }
        }
        effects.extend(self.schedule(stages, false, now));
        effects
    }
    pub(super) fn stop_ordered_branch(
        &mut self,
        root: &NodeId,
        node: &NodeId,
        error: ErrorValue,
        now: Duration,
    ) -> Vec<Effect<T>> {
        let path = self.entries[node].pipeline.branch.clone();
        let group = self.ordered.get_mut(root).expect("ordered branch");
        group.disabled.insert(path.clone());
        let nodes: Vec<_> = group
            .stages
            .iter()
            .filter(|n| self.entries[*n].pipeline.branch.starts_with(&path))
            .cloned()
            .collect();
        let candidates = group.stages.clone();
        let mut effects = self.stop_ordered_members(&nodes, &error);
        effects.extend(self.schedule(candidates, false, now));
        effects.extend(self.advance_ordered(root, now));
        effects
    }
    fn stop_ordered_members(&mut self, nodes: &[NodeId], error: &ErrorValue) -> Vec<Effect<T>> {
        let mut effects = vec![];
        for node in nodes {
            let active = self.leases.contains_key(node);
            let waiting = self.graph.node(node).is_some_and(|n| {
                matches!(
                    n.state(),
                    NodeState::Pending | NodeState::Stale | NodeState::Running
                )
            });
            if !active && !waiting {
                continue;
            }
            if let Some(run) = self.revoke(node) {
                effects.push(Effect::Cancel(run));
            }
            if let Some(entry) = self.entries.get_mut(node) {
                entry.explicitly_requested = false;
                entry.refresh_pending = false;
                let policy = entry
                    .value
                    .as_ref()
                    .map(|v| v.provenance().policy().clone())
                    .unwrap_or_default();
                entry.error = Some(error.clone().with_policy(&policy));
                entry.value = None;
                self.private_charges.shift_remove(node);
                self.transition(
                    node,
                    if error.code() == RuntimeCode::Cancelled.as_str() {
                        NodeState::Cancelled
                    } else {
                        NodeState::Failed
                    },
                    &mut effects,
                );
            }
        }
        effects
    }
    pub(super) fn stop_ordered(
        &mut self,
        root: &NodeId,
        error: ErrorValue,
        now: Duration,
    ) -> Vec<Effect<T>> {
        let Some(group) = self.ordered.get(root) else {
            return vec![];
        };
        if group.stopped {
            return vec![];
        }
        self.retire_event_credit(root);
        let group = self.ordered.get_mut(root).expect("ordered group");
        group.reset();
        group.stopped = true;
        let stages = group.stages.clone();
        let mut effects = vec![];
        effects.extend(
            self.stop_ordered_members(
                &std::iter::once(root.clone())
                    .chain(stages.iter().cloned())
                    .collect::<Vec<_>>(),
                &error,
            ),
        );
        let outside: Vec<_> = self
            .graph
            .downstream(root)
            .unwrap_or_default()
            .into_iter()
            .filter(|n| n != root && !stages.contains(n))
            .collect();
        effects.extend(self.schedule(outside, false, now));
        effects
    }
}
