//! Named finite operations supplied by captured providers, independent of their visible aliases.
use super::*;
use std::collections::BTreeSet;
use wes_core::contracts::Contract;

#[cfg(test)]
mod tests;

#[derive(Clone, Debug)]
pub struct Argument {
    pub parameter: String,
    pub contract: Arc<Contract>,
    pub required: bool,
}
#[derive(Clone, Debug)]
pub struct FiniteProfile {
    name: String,
    kind: String,
    arguments: IndexMap<String, Argument>,
}
#[derive(Clone, Copy, Debug, Error, PartialEq, Eq)]
pub enum ProfileError {
    #[error(
        "Invalid finite profile declaration or captured parameter contracts; nothing was started."
    )]
    InvalidDeclaration,
    #[error("No bound finite profile matches this name and kind; nothing was started.")]
    NotFound,
    #[error(
        "Finite profile selection is ambiguous; use distinct names within each kind. Nothing was started."
    )]
    Ambiguous,
    #[error(
        "Finite profile arguments are missing, unknown, invalid or exceed their metadata budget; nothing was started."
    )]
    InvalidArgument,
    #[error("Finite profile lookup exceeded its metadata budget; nothing was started.")]
    Limit,
}
fn identifier(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 64
        && value
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, b'_' | b'-' | b'.'))
}
impl FiniteProfile {
    pub fn new(
        name: String,
        kind: String,
        arguments: IndexMap<String, Argument>,
    ) -> Result<Self, ProfileError> {
        let mut parameters = BTreeSet::new();
        if !identifier(&name)
            || !identifier(&kind)
            || arguments.len() > 32
            || arguments.iter().any(|(key, arg)| {
                !identifier(key)
                    || !identifier(&arg.parameter)
                    || !parameters.insert(&arg.parameter)
            })
        {
            return Err(ProfileError::InvalidDeclaration);
        }
        Ok(Self {
            name,
            kind,
            arguments,
        })
    }
    pub fn name(&self) -> &str {
        &self.name
    }
    pub fn kind(&self) -> &str {
        &self.kind
    }
    pub fn arguments(&self) -> &IndexMap<String, Argument> {
        &self.arguments
    }
    fn matches(&self, capability: &Capability) -> bool {
        !capability.streaming
            && capability.parameters.len() == self.arguments.len()
            && self.arguments.values().all(|a| {
                capability
                    .parameter(&a.parameter)
                    .is_some_and(|p| p.required == a.required && p.shape == a.contract.shape())
            })
    }
}
impl Providers {
    /// Pure lookup and binding. No executable, file read or mutable provider-name inference.
    pub fn resolve_finite_profile(
        &self,
        name: &str,
        kind: &str,
        inputs: &IndexMap<String, Input>,
    ) -> Result<BoundCall, ProfileError> {
        if !identifier(name) || !identifier(kind) || inputs.len() > 32 {
            return Err(ProfileError::InvalidArgument);
        }
        let mut selected = None;
        let mut work = 0usize;
        for provider in self.providers.values() {
            if matches!(provider.scope, ExecutionScope::Unbound) {
                continue;
            }
            for capability in provider.description.capabilities() {
                work += 1;
                if work > 4096 {
                    return Err(ProfileError::Limit);
                }
                let Some(profile) = provider.invoker.finite_profile(capability) else {
                    continue;
                };
                if profile.name != name || profile.kind != kind {
                    continue;
                }
                if !profile.matches(capability) {
                    return Err(ProfileError::InvalidDeclaration);
                }
                if selected.is_some() {
                    return Err(ProfileError::Ambiguous);
                }
                selected = Some((provider, capability, profile));
            }
        }
        let (provider, capability, profile) = selected.ok_or(ProfileError::NotFound)?;
        if profile
            .arguments
            .iter()
            .any(|(key, a)| a.required && !inputs.contains_key(key))
        {
            return Err(ProfileError::InvalidArgument);
        }
        let mut arguments = IndexMap::new();
        let mut guards = IndexMap::new();
        let mut remaining = 128 * 1024;
        for (key, input) in inputs {
            let arg = profile
                .arguments
                .get(key)
                .ok_or(ProfileError::InvalidArgument)?;
            let input = match input {
                Input::Literal(value) => {
                    let charge = crate::value_size::value_charge(value, remaining)
                        .ok_or(ProfileError::InvalidArgument)?;
                    remaining = remaining
                        .checked_sub(charge)
                        .ok_or(ProfileError::InvalidArgument)?;
                    Input::Literal(
                        boundary::literal(
                            &arg.parameter,
                            std::slice::from_ref(&arg.contract),
                            &arg.contract.shape(),
                            value,
                        )
                        .map_err(|_| ProfileError::InvalidArgument)?,
                    )
                }
                input => input.clone(),
            };
            arguments.insert(arg.parameter.clone(), input);
            guards.insert(arg.parameter.clone(), vec![arg.contract.clone()]);
        }
        self.bind_finite(Invocation {
            trace_profile: None,
            provider: provider.description.clone(),
            capability: capability.clone(),
            inputs: arguments,
            cautions: Default::default(),
            interactive: false,
            guards,
        })
        .map_err(|_| ProfileError::InvalidDeclaration)
    }
}
impl BoundCall {
    /// Local execution location, not provider alias, endpoint or working directory.
    pub fn is_local_target(&self) -> bool {
        match &self.provider.scope {
            ExecutionScope::Local(_) => true,
            ExecutionScope::Environment { binding, .. } => matches!(
                binding.import().target().kind(),
                wes_core::environments::TargetKind::Local
            ),
            ExecutionScope::Unbound => false,
        }
    }
}
