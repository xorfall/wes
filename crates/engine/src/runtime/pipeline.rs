//! Written execution order is a completion barrier, not a successful-value dependency.
//! A failed branch releases its successor only after physical work has joined.
use super::*;

#[derive(Clone, Debug, Default)]
pub(crate) struct PipelineStep {
    pub after: Option<NodeId>,
    pub branch: Vec<usize>,
    pub stateful: bool,
    pub limit: Option<u64>,
}

impl<T: Clone> Runtime<T> {
    pub(crate) fn pipeline_groups(&self) -> IndexMap<NodeId, Vec<NodeId>> {
        self.ordered
            .iter()
            .map(|(root, group)| (root.clone(), group.stages.clone()))
            .collect()
    }
    /// Definition changes/removal stop every ordered group touched by their data closure.
    pub(crate) fn invalidation_scope(&self, node: &NodeId) -> Vec<NodeId> {
        let mut affected = self.graph.downstream(node).unwrap_or_default();
        let roots: IndexSet<_> = affected
            .iter()
            .filter_map(|id| self.ordered_root(id))
            .collect();
        for root in roots {
            affected.insert(root.clone());
            affected.extend(self.ordered[&root].stages.iter().cloned());
        }
        affected.into_iter().collect()
    }
    pub(crate) fn cancellation_scope(&self, node: &NodeId) -> Vec<NodeId> {
        let Some(root) = self.ordered_root(node) else {
            return vec![node.clone()];
        };
        let path = &self.entries[node].pipeline.branch;
        let group = &self.ordered[&root];
        if path.is_empty() {
            return std::iter::once(root)
                .chain(group.stages.iter().cloned())
                .collect();
        }
        group
            .stages
            .iter()
            .filter(|id| self.entries[*id].pipeline.branch.starts_with(path))
            .cloned()
            .collect()
    }
    pub(crate) fn install_pipeline_step(&mut self, node: &NodeId, step: PipelineStep) {
        self.entries
            .get_mut(node)
            .expect("admitted pipeline node")
            .pipeline = step;
    }
    pub(super) fn skip_pipeline_delivery(&mut self, node: &NodeId, effects: &mut Vec<Effect<T>>) {
        let entry = self.entries.get_mut(node).expect("pipeline entry");
        entry.explicitly_requested = false;
        entry.error = None;
        if let Some(group) = entry
            .ordered_root
            .as_ref()
            .and_then(|root| self.ordered.get_mut(root))
        {
            group.skipped.insert(node.clone());
        }
        if entry.pipeline.stateful {
            // No accepted input means no state transition. Keep the last committed checkpoint
            // readable, while closing this event's selected output for ordered dependents.
            if entry.value.is_some() {
                self.transition(node, NodeState::Ready, effects);
                return;
            }
        }
        entry.value = None;
        self.private_charges.shift_remove(node);
        self.transition(node, NodeState::Skipped, effects);
    }
    pub(super) fn pipeline_predecessor_finished(&self, node: &NodeId) -> bool {
        let mut previous = self.entries[node].pipeline.after.as_ref();
        while let Some(id) = previous {
            if self.leases.contains_key(id)
                || self.graph.node(id).is_some_and(|n| {
                    matches!(
                        n.state(),
                        NodeState::Pending | NodeState::Stale | NodeState::Running
                    )
                })
            {
                return false;
            }
            previous = self
                .entries
                .get(id)
                .and_then(|entry| entry.pipeline.after.as_ref());
        }
        true
    }

    pub(super) fn pipeline_successors(&self, node: &NodeId) -> Vec<NodeId> {
        self.graph
            .dependents_of(node)
            .cloned()
            .chain(
                self.entries
                    .iter()
                    .filter(|(_, e)| e.pipeline.after.as_ref() == Some(node))
                    .map(|(id, _)| id.clone()),
            )
            .collect::<IndexSet<_>>()
            .into_iter()
            .collect()
    }
}
