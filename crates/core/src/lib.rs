//! Domain values and rules, independent of execution and external I/O.

pub mod capability;
pub mod contracts;
pub mod environments;
mod failure;
pub mod flow;
pub mod literals;
mod numeric;
pub mod package_schema;
mod provenance;
mod shape;
mod temporal;
mod value;

pub use failure::{ErrorId, ErrorValue, InvalidError, ValidationIssue};
pub use provenance::Provenance;
pub use shape::{MetaType, Primitive, RecordShape, Shape};
pub use temporal::{DurationValue, Interval, Timestamp};
pub use value::{Data, Decimal, DecimalOp, ModelError, NumericError, TimeParts, Value};

mod iteration;
pub use iteration::{
    ContractCapture, IterMode, IterPlanError, IterRecipe, IterRegexCache, IterStage, IterValue,
};

mod failure_location;
pub use failure_location::SourceLocation;
