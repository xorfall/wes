use indexmap::{IndexMap, IndexSet};
use std::{collections::VecDeque, fmt, str::FromStr, sync::Arc};
use thiserror::Error;

/// Opaque identity, independent of user bindings and the numeric ID generator.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct NodeId(Arc<str>);
impl NodeId {
    pub fn new(value: impl AsRef<str>) -> Result<Self, GraphError> {
        let value = value.as_ref();
        // Use the language whitespace predicate without excluding non-breaking names.
        if value.chars().all(|ch|matches!(ch,'\u{9}'..='\u{d}'|'\u{1c}'..='\u{20}'|'\u{1680}'|'\u{2000}'..='\u{2006}'|'\u{2008}'..='\u{200a}'|'\u{2028}'|'\u{2029}'|'\u{205f}'|'\u{3000}')) {return Err(GraphError::BlankId);}
        Ok(Self(Arc::from(value)))
    }
    pub fn as_str(&self) -> &str {
        &self.0
    }
    fn generated_number(&self) -> Option<i64> {
        self.0.strip_prefix("id")?.parse().ok()
    }
}
impl fmt::Display for NodeId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}
impl FromStr for NodeId {
    type Err = GraphError;
    fn from_str(value: &str) -> Result<Self, Self::Err> {
        Self::new(value)
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum OutputPort {
    #[default]
    Data,
    Error,
    Cancel,
}
impl OutputPort {
    pub fn selector(self) -> &'static str {
        match self {
            Self::Data => "data",
            Self::Error => "error",
            Self::Cancel => "cancel",
        }
    }
    pub fn named(name: &str) -> Option<Self> {
        match name {
            "data" => Some(Self::Data),
            "error" => Some(Self::Error),
            "cancel" => Some(Self::Cancel),
            _ => None,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct OutputRef {
    pub node: NodeId,
    pub port: OutputPort,
}
impl OutputRef {
    pub fn data(node: NodeId) -> Self {
        Self {
            node,
            port: OutputPort::Data,
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum NodeState {
    #[default]
    Pending,
    Running,
    Ready,
    Stale,
    Failed,
    Cancelled,
    Skipped,
}

#[derive(Clone, Debug)]
pub struct Node<T> {
    id: NodeId,
    // Process-local definition identity: state/run changes preserve it; replacing work does not.
    definition: Arc<()>,
    payload: T,
    dependencies: IndexMap<NodeId, OutputPort>,
    state: NodeState,
}
impl<T> Node<T> {
    pub(crate) fn definition(&self) -> Arc<()> {
        self.definition.clone()
    }
    pub fn id(&self) -> &NodeId {
        &self.id
    }
    pub fn payload(&self) -> &T {
        &self.payload
    }
    pub fn state(&self) -> NodeState {
        self.state
    }
    pub fn dependencies(&self) -> &IndexMap<NodeId, OutputPort> {
        &self.dependencies
    }
}

#[derive(Clone, Debug, Error, PartialEq, Eq)]
pub enum GraphError {
    #[error("a node id must not be blank")]
    BlankId,
    #[error("no node called {0}")]
    Missing(NodeId),
    #[error("node {0} is already present")]
    Duplicate(NodeId),
    #[error("node identity space exhausted")]
    IdExhausted,
    #[error("one dependency cannot require mutually exclusive outputs of {0}")]
    ConflictingOutputs(NodeId),
}

/// Edges only point to existing nodes and never mutate. This enforces acyclicity at insertion.
/// Neither invalidation nor restoration invokes work. The runtime owns those decisions.
#[derive(Clone, Debug)]
pub struct DependencyGraph<T> {
    nodes: IndexMap<NodeId, Node<T>>,
    dependents: IndexMap<NodeId, IndexSet<NodeId>>,
    next_id: Option<i64>,
}
impl<T> Default for DependencyGraph<T> {
    fn default() -> Self {
        Self {
            nodes: IndexMap::new(),
            dependents: IndexMap::new(),
            next_id: Some(1000),
        }
    }
}
impl<T> DependencyGraph<T> {
    pub fn new() -> Self {
        Self::default()
    }
    pub fn node(&self, id: &NodeId) -> Option<&Node<T>> {
        self.nodes.get(id)
    }
    pub fn nodes(&self) -> impl ExactSizeIterator<Item = &Node<T>> {
        self.nodes.values()
    }
    pub fn len(&self) -> usize {
        self.nodes.len()
    }
    pub fn is_empty(&self) -> bool {
        self.nodes.is_empty()
    }
    pub fn add(
        &mut self,
        payload: T,
        dependencies: impl IntoIterator<Item = OutputRef>,
    ) -> Result<NodeId, GraphError> {
        let id = self.next_id_avoiding([])?;
        self.restore(id.clone(), payload, dependencies)?;
        Ok(id)
    }
    /// A non-mutating candidate for a staged workspace admission. The caller supplies names it
    /// owns (including new bindings); merely looking for a free ID never consumes one.
    pub fn next_id_avoiding<'a>(
        &self,
        reserved: impl IntoIterator<Item = &'a str>,
    ) -> Result<NodeId, GraphError> {
        let reserved = reserved.into_iter().collect::<IndexSet<_>>();
        let mut next = self.next_id.ok_or(GraphError::IdExhausted)?;
        loop {
            let candidate = format!("id{next}");
            if !reserved.contains(candidate.as_str()) {
                return NodeId::new(candidate);
            }
            next = next.checked_add(1).ok_or(GraphError::IdExhausted)?;
        }
    }
    /// Historical identities remain unavailable to fresh allocation even if their declarations
    /// can no longer be reconstructed. This creates no node, edge, binding or executable work.
    pub(crate) fn reserve_historical_ids(&mut self, ids: &[NodeId]) {
        for id in ids {
            if let (Some(next), Some(recorded)) = (self.next_id, id.generated_number())
                && recorded >= next
            {
                self.next_id = recorded.checked_add(1);
            }
        }
    }
    /// Validates everything before changing edges, payloads, or the identity sequence.
    pub fn restore(
        &mut self,
        id: NodeId,
        payload: T,
        dependencies: impl IntoIterator<Item = OutputRef>,
    ) -> Result<(), GraphError> {
        if self.nodes.contains_key(&id) {
            return Err(GraphError::Duplicate(id));
        }
        let mut selected = IndexMap::new();
        for dependency in dependencies {
            if !self.nodes.contains_key(&dependency.node) {
                return Err(GraphError::Missing(dependency.node));
            }
            if selected
                .insert(dependency.node.clone(), dependency.port)
                .is_some_and(|previous| previous != dependency.port)
            {
                return Err(GraphError::ConflictingOutputs(dependency.node));
            }
        }
        let next = match (self.next_id, id.generated_number()) {
            (Some(next), Some(restored)) if restored >= next => restored.checked_add(1),
            (next, _) => next,
        };
        for dependency in selected.keys() {
            self.dependents
                .get_mut(dependency)
                .expect("present dependency has reverse edges")
                .insert(id.clone());
        }
        self.dependents.insert(id.clone(), IndexSet::new());
        self.nodes.insert(
            id.clone(),
            Node {
                id,
                definition: Arc::new(()),
                payload,
                dependencies: selected,
                state: NodeState::Pending,
            },
        );
        self.next_id = next;
        Ok(())
    }
    pub fn set_state(&mut self, id: &NodeId, state: NodeState) -> Result<(), GraphError> {
        self.nodes
            .get_mut(id)
            .ok_or_else(|| GraphError::Missing(id.clone()))?
            .state = state;
        Ok(())
    }
    pub fn replace_payload(&mut self, id: &NodeId, payload: T) -> Result<T, GraphError> {
        let node = self
            .nodes
            .get_mut(id)
            .ok_or_else(|| GraphError::Missing(id.clone()))?;
        node.definition = Arc::new(());
        Ok(std::mem::replace(&mut node.payload, payload))
    }
    pub fn dependents_of(&self, id: &NodeId) -> impl Iterator<Item = &NodeId> {
        self.dependents.get(id).into_iter().flatten()
    }
    pub fn downstream(&self, id: &NodeId) -> Result<IndexSet<NodeId>, GraphError> {
        if !self.nodes.contains_key(id) {
            return Err(GraphError::Missing(id.clone()));
        }
        let mut reached = IndexSet::new();
        let mut queue = VecDeque::from([id.clone()]);
        while let Some(next) = queue.pop_front() {
            if reached.insert(next.clone()) {
                queue.extend(self.dependents_of(&next).cloned());
            }
        }
        Ok(reached)
    }
    pub fn mark_stale(&mut self, id: &NodeId) -> Result<IndexSet<NodeId>, GraphError> {
        let reached = self.downstream(id)?;
        for node in &reached {
            self.nodes
                .get_mut(node)
                .expect("reachable node exists")
                .state = NodeState::Stale;
        }
        Ok(reached)
    }
    pub fn remove(&mut self, id: &NodeId) -> Result<IndexSet<NodeId>, GraphError> {
        let removed = self.downstream(id)?;
        // Retain compacts once. Repeated ordered shift-removal makes long chains quadratic.
        self.nodes.retain(|id, _| !removed.contains(id));
        self.dependents.retain(|id, _| !removed.contains(id));
        for dependents in self.dependents.values_mut() {
            dependents.retain(|id| !removed.contains(id));
        }
        Ok(removed)
    }
    pub fn execution_order(&self) -> Vec<NodeId> {
        let mut remaining = self
            .nodes
            .iter()
            .map(|(id, node)| (id.clone(), node.dependencies.len()))
            .collect::<IndexMap<_, _>>();
        let mut ready = remaining
            .iter()
            .filter(|(_, count)| **count == 0)
            .map(|(id, _)| id.clone())
            .collect::<VecDeque<_>>();
        let mut order = Vec::with_capacity(self.nodes.len());
        while let Some(next) = ready.pop_front() {
            for dependent in self.dependents_of(&next) {
                let count = remaining
                    .get_mut(dependent)
                    .expect("reverse edge points to a node");
                *count -= 1;
                if *count == 0 {
                    ready.push_back(dependent.clone());
                }
            }
            order.push(next);
        }
        debug_assert_eq!(
            order.len(),
            self.nodes.len(),
            "construction preserves acyclicity"
        );
        order
    }
    pub fn clear(&mut self) {
        *self = Self::new();
    }
}
