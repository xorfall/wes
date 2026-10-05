use crate::graph::{DependencyGraph, NodeId, OutputPort, OutputRef};
use indexmap::{IndexMap, IndexSet};
use thiserror::Error;

#[derive(Clone, Debug, Error, PartialEq, Eq)]
pub enum BindingError {
    #[error("a binding name may contain only letters, digits and underscores: {0}")]
    InvalidName(String),
    #[error("'{0}' is already a node's own id")]
    ShadowsId(String),
    #[error("no node called {0}")]
    MissingNode(NodeId),
    #[error("distinct outputs need distinct binding names")]
    DuplicateName,
}

/// User names are separate from node identity and can select any output channel.
#[derive(Clone, Debug, Default)]
pub struct Bindings {
    names: IndexMap<String, OutputRef>,
}
impl Bindings {
    pub fn new() -> Self {
        Self::default()
    }
    pub fn names(&self) -> &IndexMap<String, OutputRef> {
        &self.names
    }
    pub fn validate_names<'a, T>(
        names: impl IntoIterator<Item = &'a str>,
        graph: &DependencyGraph<T>,
    ) -> Result<(), BindingError> {
        let mut seen = IndexSet::new();
        for name in names {
            if !wes_language::binding_name(name) {
                return Err(BindingError::InvalidName(name.into()));
            }
            if !seen.insert(name) {
                return Err(BindingError::DuplicateName);
            }
            if NodeId::new(name)
                .ok()
                .is_some_and(|id| graph.node(&id).is_some())
            {
                return Err(BindingError::ShadowsId(name.into()));
            }
        }
        Ok(())
    }
    pub fn bind<T>(
        &mut self,
        name: impl Into<String>,
        output: OutputRef,
        graph: &DependencyGraph<T>,
    ) -> Result<(), BindingError> {
        let name = name.into();
        Self::validate_names([name.as_str()], graph)?;
        if graph.node(&output.node).is_none() {
            return Err(BindingError::MissingNode(output.node));
        }
        self.names.insert(name, output);
        Ok(())
    }
    pub fn resolve<T>(&self, written: &str, graph: &DependencyGraph<T>) -> Option<OutputRef> {
        Self::resolve_names(&self.names, written, graph)
    }
    /// Resolve an observed binding table with exactly the same rules as live bindings.
    pub fn resolve_names<T>(
        names: &IndexMap<String, OutputRef>,
        written: &str,
        graph: &DependencyGraph<T>,
    ) -> Option<OutputRef> {
        let (name, selected) = match written.split_once("::") {
            Some((name, port)) => (name, Some(OutputPort::named(port)?)),
            None => (written, None),
        };
        let mut output = names
            .get(name)
            .cloned()
            .or_else(|| Some(OutputRef::data(NodeId::new(name).ok()?)))?;
        if let Some(port) = selected {
            output.port = port;
        }
        graph.node(&output.node)?;
        Some(output)
    }
    pub fn names_of<'a>(&'a self, node: &'a NodeId) -> impl Iterator<Item = &'a str> {
        self.names
            .iter()
            .filter(move |(_, output)| &output.node == node)
            .map(|(name, _)| name.as_str())
    }
    pub fn unbind(&mut self, name: &str) -> Option<OutputRef> {
        self.names.shift_remove(name)
    }
    pub fn retain_nodes<T>(&mut self, graph: &DependencyGraph<T>) {
        self.names
            .retain(|_, output| graph.node(&output.node).is_some());
    }
    pub fn clear(&mut self) {
        self.names.clear();
    }
}
