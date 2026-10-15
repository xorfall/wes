//! Frozen source declarations and exact-target selection. No discovery or execution.
use super::*;
use crate::{
    streams::{ControlContracts, MAX_READY_WAIT},
    tasks::BoundCompanion,
};
use std::{collections::BTreeSet, time::Duration};
use wes_core::{Data, Primitive, contracts::ContractKind};

#[cfg(test)]
mod tests;

#[derive(Clone, Debug)]
pub enum Restriction {
    /// An optional positive Int cannot exceed its captured fixed configuration.
    Maximum(i64),
    /// An additional Text condition. The backend must validate its language and
    /// combine it with the immutable base before effects; this is no subset proof.
    BackendCondition,
}
#[derive(Clone, Debug)]
pub struct ProfileOption {
    pub parameter: String,
    pub contract: Arc<wes_core::contracts::Contract>,
    pub restriction: Restriction,
}
#[derive(Clone)]
pub struct ProfileDeclaration {
    pub name: String,
    pub kind: String,
    pub default: bool,
    pub stop: Value,
    pub ready_wait: Duration,
    pub finalization: FinalizationLimits,
    pub options: IndexMap<String, ProfileOption>,
}
#[derive(Clone)]
pub struct SourceProfile {
    declaration: ProfileDeclaration,
    controls: ControlContracts,
}
impl fmt::Debug for SourceProfile {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SourceProfile")
            .field("name", &self.name())
            .field("kind", &self.kind())
            .finish_non_exhaustive()
    }
}
#[derive(Clone, Copy, Debug, Error, PartialEq, Eq)]
pub enum ProfileError {
    #[error("Source profile declaration or typed stop is invalid; nothing was started.")]
    InvalidDeclaration,
    #[error(
        "No explicit source profile matches this selection and execution target; nothing was started."
    )]
    NotFound,
    #[error(
        "Source profile selection is ambiguous. Configure distinct names and one default per kind and target; nothing was started."
    )]
    Ambiguous,
    #[error(
        "The captured business definition is unbound or no longer current in this registry; nothing was started."
    )]
    Changed,
    #[error(
        "Source profile options are unknown, invalid or exceed their captured ceilings; nothing was started."
    )]
    InvalidOption,
    #[error(
        "Source profile and business work have different execution targets; nothing was started."
    )]
    TargetMismatch,
    #[error("Source profile lookup exceeded its metadata budget; nothing was started.")]
    Limit,
}
fn name(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 64
        && value.trim() == value
        && !value.chars().any(char::is_control)
}
fn identifier(value: &str) -> bool {
    name(value)
        && value
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, b'_' | b'-' | b'.'))
}
impl SourceProfile {
    pub fn new(
        mut declaration: ProfileDeclaration,
        controls: ControlContracts,
    ) -> Result<Self, ProfileError> {
        if !name(&declaration.name)
            || !identifier(&declaration.kind)
            || declaration.ready_wait.is_zero()
            || declaration.ready_wait > MAX_READY_WAIT
            || !declaration.finalization.valid()
            || declaration.options.len() > 16
            || !declaration.stop.data().is_inline()
            || crate::value_size::value_charge(&declaration.stop, 65536).is_none()
        {
            return Err(ProfileError::InvalidDeclaration);
        }
        let mut parameters = BTreeSet::new();
        for (key, option) in &declaration.options {
            let compatible = match option.restriction {
                Restriction::Maximum(maximum) => {
                    maximum > 0
                        && matches!(option.contract.kind(), ContractKind::Scalar(Primitive::Int))
                }
                Restriction::BackendCondition => matches!(
                    option.contract.kind(),
                    ContractKind::Scalar(Primitive::Text)
                ),
            };
            if !identifier(key)
                || !identifier(&option.parameter)
                || !parameters.insert(&option.parameter)
                || !compatible
            {
                return Err(ProfileError::InvalidDeclaration);
            }
        }
        declaration.stop = boundary::literal(
            "stop",
            std::slice::from_ref(&controls.input),
            &controls.input.shape(),
            &declaration.stop,
        )
        .map_err(|_| ProfileError::InvalidDeclaration)?;
        Ok(Self {
            declaration,
            controls,
        })
    }
    pub fn name(&self) -> &str {
        &self.declaration.name
    }
    pub fn kind(&self) -> &str {
        &self.declaration.kind
    }
    pub fn is_default(&self) -> bool {
        self.declaration.default
    }
    pub fn options(&self) -> &IndexMap<String, ProfileOption> {
        &self.declaration.options
    }
    fn check_option(option: &ProfileOption, value: &Value) -> Result<Value, ProfileError> {
        if crate::value_size::value_charge(value, 65536).is_none() {
            return Err(ProfileError::InvalidOption);
        }
        let value = boundary::literal(
            &option.parameter,
            std::slice::from_ref(&option.contract),
            &option.contract.shape(),
            value,
        )
        .map_err(|_| ProfileError::InvalidOption)?;
        let valid = match (&option.restriction, value.data()) {
            (Restriction::Maximum(maximum), Data::Int(n)) => *n > 0 && n <= maximum,
            (Restriction::BackendCondition, Data::Text(text)) => {
                !text.trim().is_empty() && text.len() <= 4096 && !text.chars().any(char::is_control)
            }
            _ => false,
        };
        if !valid {
            return Err(ProfileError::InvalidOption);
        }
        Ok(value)
    }
    /// Also used at the direct adapter boundary; a caller cannot bypass numeric
    /// ceilings by avoiding named-profile resolution.
    pub fn validate_arguments(
        &self,
        provided: &IndexMap<String, Value>,
    ) -> Result<(), ProfileError> {
        let mut remaining = 65536u64;
        for option in self.declaration.options.values() {
            if let Some(value) = provided.get(&option.parameter) {
                let charge = crate::value_size::value_charge(value, remaining)
                    .ok_or(ProfileError::InvalidOption)?;
                remaining = remaining
                    .checked_sub(charge)
                    .ok_or(ProfileError::InvalidOption)?;
                Self::check_option(option, value)?;
            }
        }
        Ok(())
    }
    fn inputs(
        &self,
        options: &IndexMap<String, Value>,
    ) -> Result<IndexMap<String, Input>, ProfileError> {
        if options.len() > 16 {
            return Err(ProfileError::InvalidOption);
        }
        let mut remaining = 65536u64;
        options
            .iter()
            .map(|(key, value)| {
                let charge = crate::value_size::value_charge(value, remaining)
                    .ok_or(ProfileError::InvalidOption)?;
                remaining = remaining
                    .checked_sub(charge)
                    .ok_or(ProfileError::InvalidOption)?;
                let option = self
                    .declaration
                    .options
                    .get(key)
                    .ok_or(ProfileError::InvalidOption)?;
                Ok((
                    option.parameter.clone(),
                    Input::Literal(Self::check_option(option, value)?),
                ))
            })
            .collect()
    }
    fn controls_match(&self, controls: &ControlContracts) -> bool {
        self.controls.input.digest() == controls.input.digest()
            && self.controls.output.digest() == controls.output.digest()
    }
}

#[derive(Clone, Debug)]
pub enum ProfileSelector {
    Named(String),
    Default(String),
}
/// A captured native definition, not a live source or reconstructed authority.
pub struct ResolvedProfile {
    profile: Arc<SourceProfile>,
    source: BoundCall,
    limits: FinalizationLimits,
    records_bytes: bool,
}
impl fmt::Debug for ResolvedProfile {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ResolvedProfile")
            .field("profile", &self.profile)
            .field("source", &self.source)
            .finish_non_exhaustive()
    }
}
impl ResolvedProfile {
    pub fn name(&self) -> &str {
        self.profile.name()
    }
    pub fn kind(&self) -> &str {
        self.profile.kind()
    }
    pub fn records_bytes(&self) -> bool {
        self.records_bytes
    }
    pub fn source(&self) -> &BoundCall {
        &self.source
    }
    pub fn into_source(self) -> BoundCall {
        self.source
    }
    pub fn into_companion(self, call: BoundCall) -> Result<BoundCompanion, ErrorValue> {
        if !same_target(&self.source.provider.scope, &call.provider.scope) {
            return Err(
                RuntimeCode::ExecutionFailed.error(ProfileError::TargetMismatch.to_string(), None)
            );
        }
        BoundCompanion::new(
            call,
            self.source,
            self.profile.declaration.stop.clone(),
            self.profile.declaration.ready_wait,
            self.limits,
        )
    }
}
/// This is observation location, not a provider alias, endpoint URL, cwd or
/// process argument. Local declarations all execute on this native host. Remote
/// kinds retain exact captured daemon/container or SSH transport configuration.
fn same_target(a: &ExecutionScope, b: &ExecutionScope) -> bool {
    use wes_core::environments::TargetKind;
    fn target(scope: &ExecutionScope) -> Option<Option<&TargetKind>> {
        match scope {
            ExecutionScope::Unbound => None,
            ExecutionScope::Local(_) => Some(None),
            ExecutionScope::Environment { binding, .. } => match binding.import().target().kind() {
                TargetKind::Local => Some(None),
                kind => Some(Some(kind)),
            },
        }
    }
    match (target(a), target(b)) {
        (Some(a), Some(b)) => a == b,
        _ => false,
    }
}
impl Providers {
    /// Lookup is pure and fail-closed. A default requires actual business target
    /// context; an independent named start uses its own captured target.
    pub fn resolve_source_profile(
        &self,
        selector: &ProfileSelector,
        business: Option<&BoundCall>,
        options: &IndexMap<String, Value>,
        tail: Option<Duration>,
    ) -> Result<ResolvedProfile, ProfileError> {
        if let Some(call) = business {
            if !self
                .providers
                .get(call.provider.description.name())
                .is_some_and(|current| Arc::ptr_eq(current, &call.provider))
                || matches!(call.provider.scope, ExecutionScope::Unbound)
            {
                return Err(ProfileError::Changed);
            }
        } else if matches!(selector, ProfileSelector::Default(_)) {
            return Err(ProfileError::NotFound);
        }
        let mut selected = None;
        let mut work = 0usize;
        for provider in self.providers.values() {
            let Some(streams) = &provider.streams else {
                continue;
            };
            for capability in provider.description.capabilities() {
                work += 1;
                if work > 4096 {
                    return Err(ProfileError::Limit);
                }
                if !capability.streaming {
                    continue;
                }
                let Some(profile) = streams.source_profile(capability) else {
                    continue;
                };
                let matches = match selector {
                    ProfileSelector::Named(name) => profile.name() == name,
                    ProfileSelector::Default(kind) => {
                        profile.is_default() && profile.kind() == kind
                    }
                };
                if !matches
                    || matches!(provider.scope, ExecutionScope::Unbound)
                    || business
                        .is_some_and(|call| !same_target(&provider.scope, &call.provider.scope))
                {
                    continue;
                }
                let valid_parameters = capability.parameters.iter().all(|parameter| {
                    !parameter.required
                        && profile.options().values().any(|option| {
                            option.parameter == parameter.name
                                && option.contract.shape() == parameter.shape
                        })
                }) && profile.options().values().all(|option| {
                    capability
                        .parameter(&option.parameter)
                        .is_some_and(|parameter| {
                            !parameter.required && parameter.shape == option.contract.shape()
                        })
                });
                if !valid_parameters
                    || !streams
                        .control_contracts(capability)
                        .is_some_and(|controls| profile.controls_match(&controls))
                {
                    return Err(ProfileError::InvalidDeclaration);
                }
                if selected.is_some() {
                    return Err(ProfileError::Ambiguous);
                }
                selected = Some((provider, capability, streams, profile));
            }
        }
        let Some((provider, capability, streams, profile)) = selected else {
            return Err(ProfileError::NotFound);
        };
        let mut limits = profile.declaration.finalization;
        if let Some(tail) = tail {
            if tail > limits.tail {
                return Err(ProfileError::InvalidOption);
            }
            limits.tail = tail;
        }
        let invocation = Invocation {
            provider: provider.description.clone(),
            capability: capability.clone(),
            inputs: profile.inputs(options)?,
            cautions: Default::default(),
            interactive: false,
            trace_profile: None,
            guards: Default::default(),
        };
        let source = self
            .bind_stream(invocation)
            .map_err(|_| ProfileError::Changed)?;
        let records_bytes = streams.records_bytes(capability);
        Ok(ResolvedProfile {
            source,
            profile,
            limits,
            records_bytes,
        })
    }
}
