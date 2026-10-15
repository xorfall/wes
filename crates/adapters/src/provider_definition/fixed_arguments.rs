//! Typed declaration constants are captured configuration, never caller-selected arguments.
use super::*;
use crate::codec::{self, Limits};
use indexmap::IndexMap;
use serde_json::Value as Json;
use wes_core::{Provenance, Shape, Value, contracts::boundary};

const MAX_BYTES: usize = 65536;
const MAX_NODES: usize = 10000;

pub(super) fn compile(
    literals: &Json,
    parameters: &[(String, Arc<Contract>, bool)],
) -> Result<IndexMap<String, Value>, DefinitionError> {
    let Some(fields) = literals.as_object() else {
        return Err(DefinitionError::new(
            "fixed_arguments requires a literal argument map",
        ));
    };
    if fields.len() > 256 {
        return Err(DefinitionError::new(
            "fixed_arguments exceeds its field limit",
        ));
    }
    if fields.is_empty() {
        return Ok(IndexMap::new());
    }
    let encoded = serde_json::to_vec(literals)
        .map_err(|_| DefinitionError::new("invalid fixed arguments"))?;
    // Bound aggregate work as well as every individually interpreted contract below.
    codec::decode_json_preserving(
        &encoded,
        Limits {
            bytes: MAX_BYTES,
            nodes: MAX_NODES,
        },
    )
    .map_err(|_| DefinitionError::new("fixed_arguments exceeds its decoding budget"))?;
    fields
        .iter()
        .map(|(name, literal)| {
            let (_, contract, _) = parameters
                .iter()
                .find(|(parameter, _, _)| parameter == name)
                .ok_or(DefinitionError::new(
                    "fixed_arguments names an undeclared parameter",
                ))?;
            let encoded = serde_json::to_vec(literal)
                .map_err(|_| DefinitionError::new("invalid fixed argument"))?;
            let data = codec::decode_json_for_contract(
                &encoded,
                Limits {
                    bytes: MAX_BYTES,
                    nodes: MAX_NODES,
                },
                contract,
            )
            .map_err(|_| {
                DefinitionError::new("fixed argument cannot be decoded under its contract")
            })?;
            let value = Value::new(Shape::Unknown, data, Provenance::default())
                .map_err(|_| DefinitionError::new("invalid fixed argument value"))?;
            let checked = boundary::checked_result(contract, &value, &|| false).map_err(|_| {
                DefinitionError::new("fixed argument violates its captured contract")
            })?;
            Ok((name.clone(), checked))
        })
        .collect()
}

impl Operation {
    pub(super) fn bind_arguments(
        &self,
        provided: &IndexMap<String, Value>,
    ) -> Result<IndexMap<String, Value>, &'static str> {
        if provided
            .keys()
            .any(|name| self.fixed_arguments.contains_key(name))
        {
            return Err(
                "A configured fixed argument cannot be supplied or replaced by a caller; no executable was launched.",
            );
        }
        if provided.keys().any(|name| {
            !self
                .parameters
                .iter()
                .any(|(parameter, _, _)| parameter == name)
        }) {
            return Err(
                "An undeclared provider argument was supplied; no executable was launched.",
            );
        }
        let mut arguments = self.fixed_arguments.clone();
        arguments.extend(
            provided
                .iter()
                .map(|(name, value)| (name.clone(), value.clone())),
        );
        Ok(arguments)
    }
}
