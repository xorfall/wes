//! Portable pure programs contain syntax and pinned full schemas, never callable provider handles.
use super::Transition;
use crate::calc::Failure;
use serde::{Deserialize, Serialize};
use std::{collections::BTreeMap, io::Write, sync::Arc};
use wes_core::contracts::{ContractKind, ContractRegistry, ResolvedContractBundle};
use wes_language::{
    Span,
    calc::{self, Environment},
    templates::CalculationDefinition,
};
const NATIVE: &str = "wes.calc.native.v1";
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Program {
    version: u32,
    native: String,
    revision: String,
    finishing: bool,
    body: String,
    package: String,
    parameters: BTreeMap<String, String>,
    output: String,
    schemas: BTreeMap<String, String>,
}
impl Transition {
    pub fn captured_program(&self, limit: usize, span: Span) -> Result<String, Failure> {
        let failure = || {
            Failure::new(
                "CAL006",
                span,
                "captured scan program exceeds its portable budget",
            )
        };
        let fixed_bytes = self
            .definition
            .compiled
            .program
            .source
            .len()
            .checked_add(self.definition.compiled.program.package.source().len())
            .filter(|n| *n <= limit)
            .ok_or_else(failure)?;
        let mut remaining = limit - fixed_bytes;
        let mut schemas = BTreeMap::new();
        let mut capture =
            |contract: Arc<wes_core::contracts::Contract>| -> Result<String, Failure> {
                let digest = contract.digest().to_owned();
                if !schemas.contains_key(&digest) {
                    if schemas.len() >= 256 || remaining == 0 {
                        return Err(failure());
                    }
                    let bundle = ResolvedContractBundle::capture(
                        contract,
                        wes_core::contracts::SnapshotLimits {
                            bytes: remaining,
                            ..Default::default()
                        },
                    )
                    .map_err(|_| failure())?;
                    remaining = remaining
                        .checked_sub(bundle.encoded().len())
                        .ok_or_else(failure)?;
                    let text = String::from_utf8(bundle.encoded().to_vec())
                        .expect("canonical schema UTF-8");
                    schemas.insert(digest.clone(), text);
                }
                Ok(digest)
            };
        let parameters = [
            ("state", self.state.clone()),
            ("context", self.context.clone()),
            (if self.finish { "end" } else { "item" }, self.input.clone()),
        ]
        .into_iter()
        .map(|(name, contract)| Ok((name.to_owned(), capture(contract)?)))
        .collect::<Result<_, Failure>>()?;
        let output = capture(self.definition.output.clone())?;
        for contract in self.definition.compiled.contracts.values() {
            capture(contract.clone())?;
        }
        let program = Program {
            version: 1,
            native: NATIVE.into(),
            revision: self.revision().into(),
            finishing: self.finish,
            body: self.definition.compiled.program.source.to_string(),
            package: self.definition.compiled.program.package.source().into(),
            parameters,
            output,
            schemas,
        };
        encode_program(&program, limit).map_err(|_| failure())
    }
    /// Reanalysis uses only the captured package and schemas. Loading does not start an attempt.
    pub fn restore_program(text: &str, limit: usize, span: Span) -> Result<Self, Failure> {
        let invalid = || {
            Failure::new(
                "CAL004",
                span,
                "captured scan program is invalid or uses unsupported semantics",
            )
        };
        if text.len() > limit {
            return Err(Failure::new(
                "CAL006",
                span,
                "captured program byte limit reached",
            ));
        }
        let saved: Program = serde_json::from_str(text).map_err(|_| invalid())?;
        if saved.version != 1
            || saved.native != NATIVE
            || saved.schemas.len() > 256
            || saved.parameters.len() != 3
            || !saved.revision.starts_with("sha256:")
            || saved.revision.len() != 71
            || !saved.revision[7..]
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
            || !saved.parameters.contains_key("state")
            || !saved.parameters.contains_key("context")
            || !saved
                .parameters
                .contains_key(if saved.finishing { "end" } else { "item" })
        {
            return Err(invalid());
        }
        let mut schemas = BTreeMap::new();
        for (digest, bytes) in &saved.schemas {
            let bundle = ResolvedContractBundle::decode(bytes.as_bytes(), Default::default())
                .map_err(|_| invalid())?;
            if digest != bundle.digest() {
                return Err(invalid());
            }
            schemas.insert(digest.clone(), bundle);
        }
        let registry =
            ContractRegistry::from_resolved(&schemas.values().cloned().collect::<Vec<_>>())
                .map_err(|_| invalid())?;
        let resolve = |digest: &str| {
            let schema = schemas.get(digest).ok_or_else(invalid)?;
            let contract = registry
                .resolve(schema.root().name())
                .map_err(|_| invalid())?;
            if contract.digest() != digest {
                return Err(invalid());
            }
            Ok(contract)
        };
        let contracts = saved
            .parameters
            .iter()
            .map(|(name, digest)| Ok((name.clone(), resolve(digest)?)))
            .collect::<Result<indexmap::IndexMap<_, _>, Failure>>()?;
        let output = resolve(&saved.output)?;
        let package = Arc::new(calc::Package::load(&saved.package).map_err(|_| invalid())?);
        // Program.source is the parsed body, without the outer :calc block.
        let program = Arc::new(calc::parse_body(&saved.body, 0, package).map_err(|_| invalid())?);
        let shapes = contracts
            .iter()
            .map(|(name, c)| (name.clone(), c.shape()))
            .collect();
        let compiled = calc::analyze_with_parameters(
            program,
            Environment {
                catalogue: &Default::default(),
                contracts: &registry,
                workspace: &|_| None,
            },
            &shapes,
        )
        .map_err(|_| invalid())?;
        if compiled.effectful() || !compiled.calls.is_empty() || !compiled.workspace.is_empty() {
            return Err(invalid());
        }
        let ContractKind::Record(fields) = output.kind() else {
            return Err(invalid());
        };
        if fields.len() != 2
            || fields.values().any(|f| f.optional)
            || !fields.contains_key("state")
            || !fields.contains_key("outputs")
            || fields["state"].contract.digest() != contracts["state"].digest()
        {
            return Err(invalid());
        }
        let ContractKind::List(element) = fields["outputs"].contract.kind() else {
            return Err(invalid());
        };
        let state = contracts["state"].clone();
        let context = contracts["context"].clone();
        let input = contracts[if saved.finishing { "end" } else { "item" }].clone();
        let output_contract = element.clone();
        let output_shape = element.shape();
        if [&state, &context, &input, &output_contract]
            .into_iter()
            .any(|c| !c.shape().is_inline() || c.shape().contains_meta())
        {
            return Err(invalid());
        }
        let code_charge = super::transition::definition_charge(
            &compiled,
            contracts.values().map(Arc::as_ref),
            &output,
            limit as u64,
            span,
        )?;
        let definition = Arc::new(CalculationDefinition {
            compiled: Arc::new(compiled),
            output,
            revision: saved.revision,
        });
        let restored = Self {
            definition,
            state,
            context,
            input,
            finish: saved.finishing,
            output: output_shape,
            output_contract,
            code_charge,
        };
        Ok(restored)
    }
}
struct CodeBuffer {
    bytes: Vec<u8>,
    limit: usize,
}
pub(super) fn encode_program(
    value: &impl Serialize,
    limit: usize,
) -> Result<String, std::io::Error> {
    let mut bytes = CodeBuffer {
        bytes: Vec::new(),
        limit,
    };
    serde_json::to_writer(&mut bytes, value).map_err(std::io::Error::other)?;
    String::from_utf8(bytes.bytes).map_err(std::io::Error::other)
}
impl Write for CodeBuffer {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        if bytes.len() > self.limit.saturating_sub(self.bytes.len()) {
            return Err(std::io::Error::other("portable code byte limit"));
        }
        self.bytes.extend_from_slice(bytes);
        Ok(bytes.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}
