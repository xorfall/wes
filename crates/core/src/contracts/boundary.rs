use super::{Contract, ContractError, ValidationCancelled};
use crate::{Data, Shape, ValidationIssue, Value, literals};
use std::sync::Arc;
use thiserror::Error;

#[derive(Clone, Debug, Error, PartialEq, Eq)]
pub enum BoundaryError {
    #[error(transparent)]
    Declaration(#[from] ContractError),
    #[error("argument '{argument}' does not satisfy {contract}")]
    Invalid {
        argument: String,
        contract: String,
        issues: Vec<ValidationIssue>,
    },
    #[error(transparent)]
    Cancelled(#[from] ValidationCancelled),
}

/// The most specific supplied shape assignable to the destination; no implicit widening.
pub fn shape(contracts: &[Arc<Contract>], expected: &Shape) -> Result<Shape, ContractError> {
    let mut best = None;
    for contract in contracts {
        let candidate = contract.shape();
        if candidate.is_assignable_to(expected)
            && best
                .as_ref()
                .is_none_or(|best| candidate.is_assignable_to(best))
        {
            best = Some(candidate);
        }
    }
    best.ok_or_else(|| ContractError {
        code: "TYP009",
        message: format!("argument contracts do not supply {expected}"),
    })
}

/// Only written literals may be re-read contextually. Runtime values use require unchanged.
pub fn literal(
    argument: &str,
    contracts: &[Arc<Contract>],
    expected: &Shape,
    value: &Value,
) -> Result<Value, BoundaryError> {
    let refined = shape(contracts, expected)?;
    let mut value = value.clone();
    if let Data::Text(text) = value.data()
        && value.shape() != &refined
        && let Some(data) = literals::read(text, &refined)
    {
        value = Value::new(refined.clone(), data, value.provenance().clone())
            .expect("contextual literal matches selected shape");
    }
    require(argument, contracts, expected, &value, &|| false)
}

/// A mandatory argument boundary. It never retypes the producer or converts a referenced value.
pub fn require(
    argument: &str,
    contracts: &[Arc<Contract>],
    expected: &Shape,
    value: &Value,
    cancelled: &dyn Fn() -> bool,
) -> Result<Value, BoundaryError> {
    if !value.shape().is_assignable_to(&Shape::Unknown) {
        return Err(ContractError {
            code: "TYP009",
            message: "Data contracts cannot accept or retype management values.".into(),
        }
        .into());
    }
    let refined = shape(contracts, expected)?;
    for contract in contracts {
        let mut issues = contract.issues_with_cancel(value.data(), cancelled)?;
        if !issues.is_empty() {
            let prefix = format!(
                "/arguments/{}",
                argument.replace('~', "~0").replace('/', "~1")
            );
            for issue in &mut issues {
                issue.path = prefix.clone() + &issue.path;
            }
            return Err(BoundaryError::Invalid {
                argument: argument.into(),
                contract: contract.name().into(),
                issues,
            });
        }
    }
    if refined == Shape::Unknown {
        Ok(value.clone())
    } else {
        Ok(value
            .with_shape(refined)
            .expect("deep contract validation guarantees selected shallow shape"))
    }
}

/// Validate a producer result and capture the immutable contract that accepted it.
/// Argument checks deliberately use `require` instead.
pub fn checked_result(
    contract: &Arc<Contract>,
    value: &Value,
    cancelled: &dyn Fn() -> bool,
) -> Result<Value, BoundaryError> {
    let checked = require(
        "return",
        std::slice::from_ref(contract),
        &Shape::Unknown,
        value,
        cancelled,
    )?;
    Ok(checked.with_metadata(Some(super::metadata::ValueMetadata::capture(contract))))
}
