//! Canonical finite operation arguments captured with the provider definition.
use super::*;
use indexmap::IndexMap;
use serde::Deserialize;
use wes_core::{Value, contracts::Contract};
use wes_engine::providers::finite_profiles::{Argument, FiniteProfile};

#[cfg(test)]
mod tests;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Declaration {
    name: String,
    kind: String,
    arguments: IndexMap<String, String>,
}
pub(super) fn compile(
    doc: Declaration,
    parameters: &[(String, Arc<Contract>, bool)],
    fixed: &IndexMap<String, Value>,
    streaming: bool,
) -> Result<Arc<FiniteProfile>, DefinitionError> {
    if streaming
        || doc.arguments.len()
            != parameters
                .iter()
                .filter(|(name, _, _)| !fixed.contains_key(name))
                .count()
    {
        return Err(DefinitionError::new(
            "finite_profile requires a finite operation and a complete map of caller parameters",
        ));
    }
    let arguments = doc
        .arguments
        .into_iter()
        .map(|(key, parameter)| {
            let (_, contract, required) = parameters
                .iter()
                .find(|(name, _, _)| name == &parameter && !fixed.contains_key(name))
                .ok_or(DefinitionError::new(
                    "finite_profile maps a missing or fixed parameter",
                ))?;
            Ok((
                key,
                Argument {
                    parameter,
                    contract: contract.clone(),
                    required: *required,
                },
            ))
        })
        .collect::<Result<_, DefinitionError>>()?;
    FiniteProfile::new(doc.name, doc.kind, arguments)
        .map(Arc::new)
        .map_err(|_| DefinitionError::new("invalid finite_profile name, kind or argument mapping"))
}
