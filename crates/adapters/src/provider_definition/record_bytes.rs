//! A finite typed record header followed by one opaque binary leaf. No domain format is decoded.
use super::{DefinitionError, invocation::failed};
use crate::codec::{self, Limits};
use wes_core::{
    Data, Primitive, Provenance, Value,
    contracts::{Contract, ContractKind, boundary},
};
use wes_engine::{driver::CancellationToken, providers::InvocationError};

pub(super) const HEADER_BYTES: usize = 256 * 1024;
pub(super) fn validate_leaf(contract: &Contract, fields: &[String]) -> Result<(), DefinitionError> {
    if fields.is_empty()
        || fields.len() > 64
        || fields.iter().any(|f| f.is_empty() || f.len() > 1024)
        || fields.iter().map(String::len).sum::<usize>() > 4096
    {
        return Err(DefinitionError::new(
            "byte_field requires a bounded static record path",
        ));
    }
    let mut leaf = contract;
    for field in fields {
        if !leaf.constraints().enumeration.is_empty() {
            return Err(DefinitionError::new(
                "record-bytes ancestors cannot enumerate the binary payload",
            ));
        }
        let ContractKind::Record(record) = leaf.kind() else {
            return Err(DefinitionError::new(
                "byte_field traverses only required record fields",
            ));
        };
        let Some(next) = record.get(field).filter(|f| !f.optional) else {
            return Err(DefinitionError::new(
                "byte_field traverses only required record fields",
            ));
        };
        leaf = &next.contract;
    }
    let limits = leaf.constraints();
    if !matches!(leaf.kind(), ContractKind::Scalar(Primitive::Bytes))
        || !limits.enumeration.is_empty()
        || !limits.patterns.is_empty()
        || limits.min.is_some()
        || limits.max.is_some()
        || limits.min_length.is_some()
        || limits.max_length.is_some()
        || limits.min_items.is_some()
        || limits.max_items.is_some()
    {
        return Err(DefinitionError::new(
            "byte_field must be an unconstrained Bytes leaf",
        ));
    }
    Ok(())
}
fn leaf<'a>(data: &'a mut Data, fields: &[String]) -> Result<&'a mut Data, InvocationError> {
    if fields.is_empty() {
        return Ok(data);
    }
    let Data::Record(record) = data else {
        return Err(invalid());
    };
    leaf(
        record.get_mut(&fields[0]).ok_or_else(invalid)?,
        &fields[1..],
    )
}
fn invalid() -> InvocationError {
    failed(
        "PRV004",
        "record-bytes stdout requires a complete bounded typed JSON header with an empty Bytes placeholder",
    )
}

#[derive(Default)]
pub(super) struct Decoder {
    prefix: Vec<u8>,
    expected: Option<usize>,
    header: Vec<u8>,
    value: Option<Value>,
}
impl Decoder {
    pub(super) fn push(
        &mut self,
        mut input: &[u8],
        contract: &std::sync::Arc<Contract>,
        fields: &[String],
        token: &CancellationToken,
    ) -> Result<Vec<u8>, InvocationError> {
        if token.is_cancelled() {
            return Err(InvocationError::Cancelled);
        }
        if self.value.is_some() {
            return Ok(input.to_vec());
        }
        if self.expected.is_none() {
            let take = (4 - self.prefix.len()).min(input.len());
            self.prefix.extend_from_slice(&input[..take]);
            input = &input[take..];
            if self.prefix.len() < 4 {
                return Ok(vec![]);
            }
            let expected =
                u32::from_be_bytes(self.prefix.as_slice().try_into().expect("four bytes")) as usize;
            if expected == 0 || expected > HEADER_BYTES {
                return Err(invalid());
            }
            self.expected = Some(expected);
        }
        let take = (self.expected.expect("prefix") - self.header.len()).min(input.len());
        self.header.extend_from_slice(&input[..take]);
        input = &input[take..];
        if self.header.len() != self.expected.expect("prefix") {
            return Ok(vec![]);
        }
        let mut data = codec::decode_json_for_contract(
            &self.header,
            Limits {
                bytes: HEADER_BYTES,
                nodes: 32_000,
            },
            contract,
        )
        .map_err(|_| invalid())?;
        if !matches!(leaf(&mut data, fields)?, Data::Bytes(bytes) if bytes.is_empty()) {
            return Err(invalid());
        }
        let value =
            Value::new(contract.shape(), data, Provenance::default()).map_err(|_| invalid())?;
        self.value = Some(
            boundary::checked_result(contract, &value, &|| token.is_cancelled()).map_err(|e| {
                match e {
                    boundary::BoundaryError::Cancelled(_) => InvocationError::Cancelled,
                    _ => invalid(),
                }
            })?,
        );
        self.header.clear();
        Ok(input.to_vec())
    }
    pub(super) fn finish(
        &self,
        provenance: Provenance,
        token: &CancellationToken,
    ) -> Result<Value, InvocationError> {
        if token.is_cancelled() {
            return Err(InvocationError::Cancelled);
        }
        self.value
            .clone()
            .map(|v| v.with_provenance(provenance))
            .ok_or_else(invalid)
    }
}
pub(super) fn materialized(
    stdout: &[u8],
    contract: &std::sync::Arc<Contract>,
    fields: &[String],
    provenance: Provenance,
    token: &CancellationToken,
) -> Result<Value, InvocationError> {
    let mut decoder = Decoder::default();
    let bytes = decoder.push(stdout, contract, fields, token)?;
    let shell = decoder.finish(provenance, token)?;
    let mut data = shell.data().clone();
    *leaf(&mut data, fields)? = Data::Bytes(bytes.into());
    let value = Value::new(shell.shape().clone(), data, shell.provenance().clone())
        .map_err(|_| invalid())?;
    boundary::checked_result(contract, &value, &|| token.is_cancelled()).map_err(|e| match e {
        boundary::BoundaryError::Cancelled(_) => InvocationError::Cancelled,
        _ => invalid(),
    })
}
