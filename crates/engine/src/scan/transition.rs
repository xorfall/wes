//! Capture existing pure definitions once; invocations never consult a registry.
use crate::calc::Failure;
use std::{collections::HashSet, sync::Arc};
use wes_core::{
    Shape, Value,
    contracts::{Contract, ContractKind, boundary},
};
use wes_language::{
    Span,
    templates::{CalculationDefinition, Definition},
};

#[derive(Clone, Debug)]
pub struct Transition {
    pub(super) definition: Arc<CalculationDefinition>,
    pub(super) state: Arc<Contract>,
    pub(super) context: Arc<Contract>,
    pub(super) input: Arc<Contract>,
    pub(super) finish: bool,
    pub(super) output: Shape,
    pub(super) output_contract: Arc<Contract>,
    pub(super) code_charge: u64,
}
impl Transition {
    pub fn capture(
        definition: &Definition,
        finish: bool,
        code_limit: u64,
        span: Span,
    ) -> Result<Self, Failure> {
        let invalid = |message| Failure::new("CAL009", span, message);
        let Some(calculation) = definition.calculation.clone() else {
            return Err(invalid(
                "scan transition must name a declared pure calculation definition",
            ));
        };
        let compiled = &calculation.compiled;
        if compiled.effectful() || !compiled.calls.is_empty() || !compiled.workspace.is_empty() {
            return Err(invalid(
                "scan transition must have only captured pure operations and explicit parameters",
            ));
        }
        let input_name = if finish { "end" } else { "item" };
        if compiled.parameters.len() != 3
            || definition.contracts.len() != 3
            || ["state", "context", input_name].iter().any(|name| {
                !definition.contracts.contains_key(*name)
                    || !compiled.parameters.contains_key(*name)
            })
        {
            return Err(invalid(
                "scan definitions require state, context and item parameters (end replaces item for finish)",
            ));
        }
        let ContractKind::Record(fields) = calculation.output.kind() else {
            return Err(invalid("scan definition output must be a declared record"));
        };
        if fields.len() != 2
            || fields.values().any(|field| field.optional)
            || !fields.contains_key("state")
            || !fields.contains_key("outputs")
        {
            return Err(invalid(
                "scan definition output requires exactly state and outputs, both mandatory",
            ));
        }
        let ContractKind::List(output_contract) = fields["outputs"].contract.kind() else {
            return Err(invalid(
                "scan definition outputs require a declared List contract",
            ));
        };
        let output_contract = output_contract.clone();
        if fields["state"].contract.digest() != definition.contracts["state"].digest() {
            return Err(invalid(
                "scan result state must use the same captured contract as its state parameter",
            ));
        }
        let result = calculation.output.shape();
        let Shape::Record(result) = &result else {
            return Err(invalid(
                "scan definition output must be a record with state and outputs fields",
            ));
        };
        if result.fields().len() != 2 || result.field("state").is_none() {
            return Err(invalid(
                "scan definition output must contain exactly state and outputs",
            ));
        }
        let Some(Shape::List(output)) = result.field("outputs") else {
            return Err(invalid(
                "scan definition outputs must have a declared List element contract",
            ));
        };
        let state = definition.contracts["state"].clone();
        if !result
            .field("state")
            .unwrap()
            .is_assignable_to(&state.shape())
        {
            return Err(invalid(
                "scan result state must satisfy its declared state parameter shape",
            ));
        }
        let code_charge = definition_charge(
            compiled,
            definition.contracts.values().map(Arc::as_ref),
            &calculation.output,
            code_limit,
            span,
        )?;
        // The transition owns the body and its diagnostics, not the surrounding
        // submission (which may contain unrelated large inputs). Preserve absolute
        // spans through origin_offset without retaining that entire source cell.
        let mut program = compiled.program.as_ref().clone();
        program.origin = Arc::new(wes_language::SourceText::new(
            compiled.program.origin.name(),
            program.source.as_ref(),
        ));
        program.origin_offset = program.span.start();
        let mut captured = compiled.as_ref().clone();
        captured.program = Arc::new(program);
        let calculation = Arc::new(CalculationDefinition {
            compiled: Arc::new(captured),
            output: calculation.output.clone(),
            revision: calculation.revision.clone(),
        });
        Ok(Self {
            definition: calculation,
            state,
            context: definition.contracts["context"].clone(),
            input: definition.contracts[input_name].clone(),
            finish,
            output: output.as_ref().clone(),
            output_contract,
            code_charge,
        })
    }
    pub fn revision(&self) -> &str {
        &self.definition.revision
    }
    pub fn output_shape(&self) -> &Shape {
        &self.output
    }
    pub fn code_charge(&self) -> u64 {
        self.code_charge
    }
    pub(super) fn argument(
        &self,
        name: &str,
        value: &Value,
        cancelled: &dyn Fn() -> bool,
        span: Span,
    ) -> Result<Value, Failure> {
        let contract = match name {
            "state" => &self.state,
            "context" => &self.context,
            _ => &self.input,
        };
        checked_argument(name, contract, value, cancelled, span)
    }
    pub(super) fn result(
        &self,
        value: &Value,
        cancelled: &dyn Fn() -> bool,
        span: Span,
    ) -> Result<Value, Failure> {
        if !value.data().is_inline()
            || !value.shape().is_inline()
            || value.shape().contains_meta()
            || value.management_authority().is_some()
        {
            return Err(Failure::new(
                "CAL004",
                span,
                "scan result cannot contain executable or management handles",
            ));
        }
        if !matches!(value.data(),wes_core::Data::Record(fields) if fields.len()==2 && fields.contains_key("state") && fields.contains_key("outputs"))
        {
            return Err(Failure::new(
                "CAL017",
                span,
                "scan result requires exactly state and outputs",
            ));
        }
        boundary::checked_result(&self.definition.output, value, cancelled)
            .map_err(|error| boundary_failure(error, span))
    }
}
pub(super) fn checked_argument(
    name: &str,
    contract: &Arc<Contract>,
    value: &Value,
    cancelled: &dyn Fn() -> bool,
    span: Span,
) -> Result<Value, Failure> {
    if !value.data().is_inline()
        || !value.shape().is_inline()
        || value.shape().contains_meta()
        || value.management_authority().is_some()
    {
        return Err(Failure::new(
            "CAL004",
            span,
            "scan arguments must be bounded materialized data, without executable or management handles",
        ));
    }
    boundary::require(
        name,
        std::slice::from_ref(contract),
        &Shape::Unknown,
        value,
        cancelled,
    )
    .map_err(|error| boundary_failure(error, span))
}
fn boundary_failure(error: boundary::BoundaryError, span: Span) -> Failure {
    if matches!(error, boundary::BoundaryError::Cancelled(_)) {
        return Failure::cancelled(span);
    }
    let message = error.to_string();
    let mut failure = Failure::new("CAL017", span, message);
    if let boundary::BoundaryError::Invalid { issues, .. } = error {
        failure.issues = issues;
    }
    failure
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn capture_keeps_only_its_body_and_checks_actual_owned_code_budget() {
        let body = ":def step(state:Int,context:Int,item:Int) -> Step as :calc pure { return {state:state+item,outputs:[item]}; }";
        let source = format!("{body}\n// {}", "unrelated input ".repeat(100_000));
        let parsed = wes_language::parse(&wes_language::SourceText::new("synthetic", source));
        let wes_language::Expression::Definition(syntax) =
            parsed.script.statements[0].expression.clone()
        else {
            panic!()
        };
        let mut registry = wes_core::contracts::ContractRegistry::new();
        registry
            .load("types: {Step: {base: Record, fields: {state: Int, outputs: 'List<Int>'}}}")
            .unwrap();
        let mut templates = wes_language::templates::Templates::new();
        templates
            .define_calculation(
                syntax,
                &registry,
                &Default::default(),
                wes_language::calc::Package::standard(),
            )
            .unwrap();
        let definitions = templates.snapshot();
        let definition = definitions.values().next().unwrap();
        let captured =
            Transition::capture(definition, false, 4 * 1024 * 1024, Span::at(0)).unwrap();
        let program = &captured.definition.compiled.program;
        assert_eq!(program.origin.text(), program.source.as_ref());
        assert_eq!(program.origin_offset, program.span.start());
        assert!(program.origin.text().len() < body.len());
        let limit = captured.code_charge() - 1;
        assert_eq!(
            Transition::capture(definition, false, limit, Span::at(0))
                .unwrap_err()
                .code,
            "CAL006"
        );
    }
}
pub(super) fn definition_charge<'a>(
    compiled: &'a wes_language::calc::Compiled,
    parameters: impl IntoIterator<Item = &'a Contract>,
    output: &'a Contract,
    limit: u64,
    span: Span,
) -> Result<u64, Failure> {
    let contracts = materialized_contracts_charge(
        parameters
            .into_iter()
            .chain(std::iter::once(output))
            .chain(compiled.contracts.values().map(Arc::as_ref)),
        limit,
        span,
    )?;
    // Bound the copied syntax/source arenas, package semantics and retained
    // resolved contracts before creating a captured execution owner.
    (compiled.program.source.len() as u64)
        .checked_mul(6)
        .and_then(|n| {
            n.checked_add((compiled.program.package.source().len() as u64).checked_mul(6)?)
        })
        .and_then(|n| n.checked_add((compiled.program.expressions.len() as u64).checked_mul(1024)?))
        .and_then(|n| n.checked_add((compiled.program.statements.len() as u64).checked_mul(512)?))
        .and_then(|n| n.checked_add((compiled.program.functions.len() as u64).checked_mul(512)?))
        .and_then(|n| n.checked_add((compiled.resolutions.len() as u64).checked_mul(512)?))
        .and_then(|n| n.checked_add(contracts))
        .filter(|n| *n <= limit)
        .ok_or_else(|| {
            Failure::new(
                "CAL006",
                span,
                "scan captured definition exceeds its code charge limit",
            )
        })
}
pub(super) fn materialized_contracts_charge<'a>(
    roots: impl IntoIterator<Item = &'a Contract>,
    limit: u64,
    span: Span,
) -> Result<u64, Failure> {
    let mut pending = Vec::new();
    for root in roots {
        if pending.len() >= 65_536 {
            return Err(Failure::new("CAL006", span, "scan declaration is too wide"));
        }
        pending.push((root, 0usize));
    }
    // Arc-owned contract graphs share resolved nodes. Charge each retained node
    // once across every parameter, output and local checked contract. Distinct
    // owners with the same spelling still incur their own charge.
    let mut retained = HashSet::new();
    let mut visited = 0usize;
    let mut charge = 0u64;
    while let Some((contract, depth)) = pending.pop() {
        visited += 1;
        if visited > 65_536 || depth > 64 {
            return Err(Failure::new(
                "CAL006",
                span,
                "scan declaration exceeds its structural limit",
            ));
        }
        if !retained.insert(std::ptr::from_ref(contract)) {
            continue;
        }
        let mut amount = 512u64.saturating_add(contract.name().len() as u64 * 6);
        for data in &contract.constraints().enumeration {
            amount = amount.saturating_add(
                crate::value_size::data_charge(data, limit).ok_or_else(|| {
                    Failure::new(
                        "CAL006",
                        span,
                        "scan declaration domain exceeds its charge limit",
                    )
                })?,
            );
        }
        amount = amount.saturating_add(
            (contract.constraints().patterns.len() as u64)
                .saturating_mul(wes_core::IterRegexCache::COMPILED_CHARGE),
        );
        match contract.kind() {
            ContractKind::Iter(_) | ContractKind::Dataset(_) => {
                return Err(Failure::new(
                    "CAL009",
                    span,
                    "scan declarations cannot contain Iter, including optional or union members",
                ));
            }
            ContractKind::List(inner) | ContractKind::Option(inner) => {
                pending.push((inner, depth + 1))
            }
            ContractKind::Map(a, b) | ContractKind::Union(a, b) => {
                pending.push((a, depth + 1));
                pending.push((b, depth + 1));
            }
            ContractKind::Record(fields) => {
                if fields.len() > 65_536usize.saturating_sub(visited + pending.len()) {
                    return Err(Failure::new("CAL006", span, "scan declaration is too wide"));
                }
                for (name, field) in fields {
                    amount = amount.saturating_add(name.len() as u64 * 6 + 128);
                    pending.push((&field.contract, depth + 1));
                }
            }
            _ => {}
        }
        if pending.len() > 65_536usize.saturating_sub(visited) {
            return Err(Failure::new(
                "CAL006",
                span,
                "scan declaration pending work exceeds its limit",
            ));
        }
        charge = charge
            .checked_add(amount)
            .filter(|n| *n <= limit)
            .ok_or_else(|| {
                Failure::new(
                    "CAL006",
                    span,
                    "scan declaration exceeds its captured code charge",
                )
            })?;
    }
    Ok(charge)
}
