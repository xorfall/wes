//! Read-only foreign-language suggestions. Implementations may inspect local metadata but must
//! never execute a provider/program, load a workspace, journal source or fetch a remote service.
use crate::driver::CancellationToken;
use thiserror::Error;
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Candidate {
    pub text: String,
    pub kind: String,
    pub detail: String,
}
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Suggestions {
    /// UTF-8 byte offset in the supplied decoded text. Transport converts explicitly to UTF-16.
    pub from: usize,
    pub items: Vec<Candidate>,
}
#[derive(Clone, Copy, Debug, Error, PartialEq, Eq)]
pub enum CompletionError {
    #[error("invalid completion text or caret")]
    Invalid,
    #[error("completion work exceeds its budget")]
    Capacity,
    #[error("completion was cancelled")]
    Cancelled,
}
pub trait Completer: Send + Sync {
    /// Called on an owned blocking worker; returning/dropping a client request must not detach it.
    fn complete(
        &self,
        written: &str,
        caret: usize,
        cancelled: &CancellationToken,
    ) -> Result<Suggestions, CompletionError>;
}

/// Ephemeral projection of explicitly acquired, current public inventory. No provider I/O,
/// archive reads or independent value lifetime. The newest declaration owns each registry;
/// invalidating it withdraws suggestions instead of falling back to an older inventory.
#[derive(Clone, Debug)]
pub struct ResourceCandidate {
    pub value: String,
    pub label: String,
    pub detail: String,
}
#[derive(Clone, Debug)]
pub struct ResourceSuggestions {
    pub environment: Option<String>,
    pub provider: String,
    pub registry: String,
    pub node: String,
    pub observed_at_ns: i64,
    pub items: Vec<ResourceCandidate>,
}
pub fn retained_resources(
    observation: &crate::session::SessionObservation,
) -> Vec<ResourceSuggestions> {
    use std::collections::BTreeMap;
    use wes_core::Data;
    let mut selected = BTreeMap::new();
    for node in observation.state.execution.graph.nodes() {
        let Some(call) = node.payload().call() else {
            continue;
        };
        let invocation = call.invocation();
        let Some(projection) = invocation.capability.resources.as_ref() else {
            continue;
        };
        let environment = call
            .environment()
            .map(|binding| binding.environment().name().to_owned());
        let key = (
            environment.clone(),
            invocation.provider.name().to_owned(),
            projection.registry.clone(),
        );
        selected.insert(key, (node, call, projection));
    }
    let mut result = vec![];
    let mut budget = 1024 * 1024usize;
    for ((environment, provider, registry), (node, call, projection)) in selected {
        if !call.environment_available() || node.state() != crate::graph::NodeState::Ready {
            continue;
        }
        if let Some(binding) = call.environment()
            && observation
                .environment_revisions
                .get(binding.environment().name())
                != Some(&binding.environment().revision())
        {
            continue;
        }
        let Some(value) = observation.state.execution.values.get(node.id()) else {
            continue;
        };
        if value.provenance().policy().is_confidential() || value.provenance().policy().is_unknown()
        {
            continue;
        }
        let Data::Record(fields) = value.data() else {
            continue;
        };
        let Some(Data::List(rows)) = fields.get(&projection.rows) else {
            continue;
        };
        let Some(Data::Int(observed_at_ns)) = fields.get(&projection.observed_at) else {
            continue;
        };
        let mut items = vec![];
        for row in rows.iter().take(1000) {
            let Data::Record(row) = row else { continue };
            let get = |name: &str| match row.get(name) {
                Some(Data::Text(s)) if s.len() <= 4096 && !s.chars().any(char::is_control) => {
                    Some(s.clone())
                }
                _ => None,
            };
            let (Some(value), Some(label), Some(detail)) = (
                get(&projection.key),
                get(&projection.label),
                get(&projection.detail),
            ) else {
                continue;
            };
            let charge = value.len() + label.len() + detail.len() + 128;
            if charge > budget {
                break;
            }
            budget -= charge;
            items.push(ResourceCandidate {
                value: value.to_string(),
                label: label.to_string(),
                detail: detail.to_string(),
            });
        }
        result.push(ResourceSuggestions {
            environment,
            provider,
            registry,
            node: node.id().as_str().into(),
            observed_at_ns: *observed_at_ns,
            items,
        });
        if result.len() >= 256 || budget == 0 {
            break;
        }
    }
    result
}
