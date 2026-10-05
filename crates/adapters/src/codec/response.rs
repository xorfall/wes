//! JSON interpretation preserves presence and uses native contracts for numeric/nullable context.
use super::{
    CodecError, Limits,
    raw::{self, Context},
};
use wes_core::Data;

pub fn decode_json_preserving(bytes: &[u8], limits: Limits) -> Result<Data, CodecError> {
    let mut context = Context::new(limits);
    let root = context.root(bytes)?;
    raw::present(root, &mut context, 0)
}

/// Read response tokens with numeric contract context. Full contract validation and nullable
/// lifting remain the caller's responsibility. Native values and calculation parsing use the
/// separate preserving reader above.
pub fn decode_json_for_contract(
    bytes: &[u8],
    limits: Limits,
    contract: &wes_core::contracts::Contract,
) -> Result<Data, CodecError> {
    let mut context = Context::new(limits);
    let root = context.root(bytes)?;
    raw::present_for_contract(root, &mut context, 0, Some(contract))
}
