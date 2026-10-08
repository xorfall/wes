//! Optional contracts over the existing domain value model.

pub mod boundary;
mod display;
mod expression;
pub use display::{ContractDisplay, EnumTone};
pub mod metadata;
mod model;
mod package;
mod registry;

pub use crate::ValidationIssue;
pub use expression::{Constructor, TYPE_CONSTRUCTORS, TypeConstructor, TypeExpression};
pub use model::{
    Contract, Field as ContractField, Kind as ContractKind, Limits as ContractLimits,
    ValidationCancelled,
};
pub(crate) use package::read_strict as read_strict_package;
pub use package::{Node as PackageNode, ScalarKind as PackageScalarKind, read as read_package};
pub use registry::ContractRegistry;

use thiserror::Error;

/// A declaration failure; execution adapters attach run/error identity at their boundary.
#[derive(Clone, Debug, Error, PartialEq, Eq)]
#[error("{code}: {message}")]
pub struct ContractError {
    pub code: &'static str,
    pub message: String,
}

impl ContractError {
    fn declaration(message: impl Into<String>) -> Self {
        Self {
            code: "TYP002",
            message: message.into(),
        }
    }
}

/// Parses a restricted package for preflight validation without installing definitions.
/// Registry resolution adds schema and subtype checks; this does not claim full validity.
pub fn preflight_package(source: &str) -> Result<(), ContractError> {
    package::read(source).map(|_| ())
}
