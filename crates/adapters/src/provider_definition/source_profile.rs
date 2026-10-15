//! Static profile metadata; executable effects and condition semantics stay adapter-owned.
use super::*;
use indexmap::IndexMap;
use serde::Deserialize;
use serde_json::Value as Json;
use std::time::Duration;
use wes_core::{Data, Value};
use wes_engine::{
    providers::{
        FinalizationLimits,
        profiles::{ProfileDeclaration, ProfileOption, Restriction, SourceProfile},
    },
    streams::ControlContracts,
};
#[cfg(test)]
mod tests;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Declaration {
    name: String,
    kind: String,
    #[serde(default)]
    default: bool,
    stop: Json,
    #[serde(default = "ready_wait")]
    ready_wait_ms: u64,
    #[serde(default)]
    tail_ms: u64,
    #[serde(default = "control_wait")]
    control_ms: u64,
    #[serde(default = "control_wait")]
    grace_ms: u64,
    #[serde(default)]
    narrowing: IndexMap<String, Narrowing>,
}
fn ready_wait() -> u64 {
    10000
}
fn control_wait() -> u64 {
    5000
}
#[derive(Deserialize)]
#[serde(tag = "kind", rename_all = "kebab-case", deny_unknown_fields)]
enum Narrowing {
    Maximum {
        parameter: String,
        maximum_from: Vec<String>,
    },
    BackendCondition {
        parameter: String,
    },
}
fn maximum(fixed: &IndexMap<String, Value>, path: &[String]) -> Result<i64, DefinitionError> {
    if path.is_empty()
        || path.len() > 16
        || path
            .iter()
            .any(|part| part.is_empty() || part.len() > 1024 || part.chars().any(char::is_control))
        || path.iter().map(String::len).sum::<usize>() > 4096
    {
        return Err(DefinitionError::new(
            "profile maximum_from requires a bounded fixed field path",
        ));
    }
    let mut value = fixed
        .get(&path[0])
        .ok_or(DefinitionError::new(
            "profile maximum_from requires a fixed argument",
        ))?
        .data();
    for key in &path[1..] {
        let Data::Record(fields) = value else {
            return Err(DefinitionError::new(
                "profile maximum_from requires a present fixed record field",
            ));
        };
        value = fields
            .get(key)
            .ok_or(DefinitionError::new("profile maximum_from field is absent"))?;
    }
    match value {
        Data::Int(n) if *n > 0 => Ok(*n),
        _ => Err(DefinitionError::new(
            "profile maximum_from must select a positive fixed Int",
        )),
    }
}
pub(super) fn compile(
    doc: Declaration,
    parameters: &[(String, Arc<Contract>, bool)],
    fixed: &IndexMap<String, Value>,
    control: Option<&Control>,
    streaming: bool,
) -> Result<Arc<SourceProfile>, DefinitionError> {
    let control = control.filter(|_| streaming).ok_or(DefinitionError::new(
        "source_profile requires a streaming session with typed controls",
    ))?;
    if doc.narrowing.len() > 16
        || parameters
            .iter()
            .any(|(name, _, required)| *required && !fixed.contains_key(name))
    {
        return Err(DefinitionError::new(
            "source_profile requires fixed required arguments and at most 16 narrowing options",
        ));
    }
    let mut options = IndexMap::new();
    for (name, entry) in doc.narrowing {
        let (parameter, restriction) = match entry {
            Narrowing::Maximum {
                parameter,
                maximum_from,
            } => (
                parameter,
                Restriction::Maximum(maximum(fixed, &maximum_from)?),
            ),
            Narrowing::BackendCondition { parameter } => (parameter, Restriction::BackendCondition),
        };
        let (_, contract, required) = parameters
            .iter()
            .find(|(name, _, _)| name == &parameter)
            .ok_or(DefinitionError::new(
                "profile narrowing names an undeclared parameter",
            ))?;
        if *required || fixed.contains_key(&parameter) {
            return Err(DefinitionError::new(
                "profile narrowing requires a distinct optional caller parameter",
            ));
        }
        options.insert(
            name,
            ProfileOption {
                parameter,
                contract: contract.clone(),
                restriction,
            },
        );
    }
    if parameters.iter().any(|(name, _, _)| {
        !fixed.contains_key(name) && !options.values().any(|option| &option.parameter == name)
    }) {
        return Err(DefinitionError::new(
            "source_profile permits only fixed arguments and declared narrowing parameters",
        ));
    }
    let mut stop = super::fixed_arguments::compile(
        &serde_json::json!({"action":doc.stop}),
        &[("action".into(), control.input.clone(), true)],
    )?;
    SourceProfile::new(
        ProfileDeclaration {
            name: doc.name,
            kind: doc.kind,
            default: doc.default,
            stop: stop.shift_remove("action").expect("compiled stop literal"),
            ready_wait: Duration::from_millis(doc.ready_wait_ms),
            finalization: FinalizationLimits {
                tail: Duration::from_millis(doc.tail_ms),
                control: Duration::from_millis(doc.control_ms),
                graceful: Duration::from_millis(doc.grace_ms),
            },
            options,
        },
        ControlContracts {
            input: control.input.clone(),
            output: control.output.clone(),
        },
    )
    .map(Arc::new)
    .map_err(|_| DefinitionError::new("invalid source profile metadata, options, stop or bounds"))
}
