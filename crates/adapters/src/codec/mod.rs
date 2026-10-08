//! Bounded JSON boundary formats. Retained values and history are distinct from display JSON.
mod decode;
mod encode;
pub mod history;
pub(crate) mod raw;
mod response;
pub use decode::{DecodedValue, decode_value};
pub use encode::{
    ValueSelection, encode_display_value, encode_json, encode_json_human, encode_json_pretty,
    encode_request_data, encode_selection, encode_value, select_value,
};
pub(crate) use encode::{check_arguments, check_arguments_detailed, encode_protected_value};
pub use response::{decode_json_for_contract, decode_json_preserving};

use thiserror::Error;

const VALUE_FORMAT: &str = "wes.value";
const VALUE_VERSION: u32 = 2;

#[derive(Clone, Copy, Debug)]
pub struct Limits {
    pub bytes: usize,
    pub nodes: usize,
}
impl Default for Limits {
    fn default() -> Self {
        Self {
            bytes: wes_budgets::get("codec.bytes") as usize,
            nodes: wes_budgets::get("codec.nodes") as usize,
        }
    }
}

#[derive(Debug, Error)]
pub enum CodecError {
    #[error("encoded data exceeds its byte budget")]
    Bytes,
    #[error("JSON does not satisfy any contract alternative")]
    Contract,
    #[error("JSON has multiple distinct native interpretations under this contract")]
    AmbiguousContract,
    #[error("data exceeds its node or parsing-work budget")]
    Work,
    #[error("data exceeds the supported nesting depth")]
    Depth,
    #[error("invalid JSON: {0}")]
    Json(#[from] serde_json::Error),
    #[error("invalid value selection: {0}")]
    Selection(String),
    #[error("value export refused: {0}")]
    Export(String),
    #[error("invalid stored data: {0}")]
    Invalid(String),
    #[error(transparent)]
    Model(#[from] wes_core::ModelError),
}

mod calculation;
pub use calculation::CalculationServices;
