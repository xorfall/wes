//! Describes resolved contracts. No schema parsing, validation, provider or filesystem I/O.
use super::{Budget, Data, Failure, fields};
use std::sync::Arc;
use wes_core::contracts::{
    Contract, ContractError, ContractKind as Kind, ContractRegistry, TypeExpression,
};

pub(super) const CONSTRUCTORS: &[(&str, &[&str])] = &[
    ("List", &["T"]),
    ("Map", &["K", "V"]),
    ("Option", &["T"]),
    ("Iter", &["T"]),
    ("Union", &["A", "B"]),
];
#[derive(Clone, Debug)]
pub(crate) enum Captured {
    Contract(Arc<Contract>),
    Constructor(&'static str, &'static [&'static str]),
    Invalid(ContractError),
}
pub(crate) fn capture(registry: &ContractRegistry, name: &str) -> Captured {
    if let Ok(expression) = TypeExpression::parse(name)
        && expression.arguments.is_empty()
        && let Some((name, parameters)) = CONSTRUCTORS.iter().find(|(n, _)| *n == expression.name)
    {
        return Captured::Constructor(name, parameters);
    }
    match registry.resolve(name) {
        Ok(contract) => Captured::Contract(contract),
        Err(error) => Captured::Invalid(error),
    }
}
pub(crate) fn completion_names(registry: &ContractRegistry) -> Vec<String> {
    registry
        .snapshot()
        .keys()
        .take(super::max_rows().saturating_sub(CONSTRUCTORS.len()))
        .cloned()
        .chain(CONSTRUCTORS.iter().map(|(name, _)| (*name).to_owned()))
        .collect()
}
fn kind(contract: &Contract) -> &'static str {
    match contract.kind() {
        Kind::Scalar(_) => "scalar",
        Kind::Unknown => "unknown",
        Kind::Record(_) => "record",
        Kind::List(_) => "list",
        Kind::Map(_, _) => "map",
        Kind::Option(_) => "option",
        Kind::Iter(_) => "iter",
        Kind::Union(_, _) => "union",
    }
}
fn constructor(name: &str, parameters: &[&str], budget: &mut Budget<'_>) -> Result<Data, Failure> {
    Ok(fields([
        ("name", budget.text(name)?),
        ("kind", budget.text("constructor")?),
        ("scope", budget.text("language")?),
        (
            "parameters",
            Data::List(
                parameters
                    .iter()
                    .map(|name| budget.text(name))
                    .collect::<Result<_, _>>()?,
            ),
        ),
    ]))
}
pub(super) fn list(contracts: &[Arc<Contract>], budget: &mut Budget<'_>) -> Result<Data, Failure> {
    let builtins = ContractRegistry::new();
    let mut rows = Vec::new();
    for contract in contracts {
        budget.step()?;
        rows.push(fields([
            ("name", budget.text(contract.name())?),
            ("kind", budget.text(kind(contract))?),
            ("scope", budget.text("workspace")?),
            (
                "origin",
                budget.text(if builtins.snapshot().contains_key(contract.name()) {
                    "builtin"
                } else {
                    "loaded"
                })?,
            ),
            ("parameters", Data::List(vec![])),
        ]));
    }
    for (name, parameters) in CONSTRUCTORS {
        rows.push(constructor(name, parameters, budget)?);
    }
    Ok(Data::List(rows))
}
pub(super) fn describe(captured: &Captured, budget: &mut Budget<'_>) -> Result<Data, Failure> {
    match captured {
        Captured::Contract(contract) => description(contract, budget, 0),
        Captured::Constructor(name, parameters) => constructor(name, parameters, budget),
        Captured::Invalid(_) => Err(Failure::missing("type", "invalid type expression")), // routed as the original TYP error by the query
    }
}
fn description(
    contract: &Contract,
    budget: &mut Budget<'_>,
    depth: usize,
) -> Result<Data, Failure> {
    budget.step()?;
    if depth > 64 {
        return Err(Failure::Limit);
    }
    let mut result = indexmap::IndexMap::new();
    result.insert("name".into(), budget.text(contract.name())?);
    result.insert("kind".into(), budget.text(kind(contract))?);
    result.insert("scope".into(), budget.text("workspace")?);
    match contract.kind() {
        Kind::Scalar(primitive) => {
            result.insert("primitive".into(), formatted(primitive, budget)?);
        }
        Kind::Record(fields) => {
            let mut rows = Vec::new();
            for (name, field) in fields {
                budget.step()?;
                rows.push(super::fields([
                    ("name", budget.text(name)?),
                    ("optional", Data::Bool(field.optional)),
                    ("contract", description(&field.contract, budget, depth + 1)?),
                ]));
            }
            result.insert("fields".into(), Data::List(rows));
        }
        Kind::List(element) | Kind::Option(element) | Kind::Iter(element) => {
            result.insert("element".into(), description(element, budget, depth + 1)?);
        }
        Kind::Map(key, value) => {
            result.insert("key".into(), description(key, budget, depth + 1)?);
            result.insert("value".into(), description(value, budget, depth + 1)?);
        }
        Kind::Union(a, b) => {
            result.insert(
                "alternatives".into(),
                Data::List(vec![
                    description(a, budget, depth + 1)?,
                    description(b, budget, depth + 1)?,
                ]),
            );
        }
        Kind::Unknown => {}
    }
    let limits = contract.constraints();
    let mut constraints = indexmap::IndexMap::new();
    for (name, number) in [("min", &limits.min), ("max", &limits.max)] {
        if let Some(number) = number {
            constraints.insert(name.into(), formatted(number, budget)?);
        }
    }
    for (name, number) in [
        ("minLength", limits.min_length),
        ("maxLength", limits.max_length),
        ("minItems", limits.min_items),
        ("maxItems", limits.max_items),
    ] {
        if let Some(number) = number {
            budget.step()?;
            constraints.insert(
                name.into(),
                Data::Int(number.try_into().map_err(|_| Failure::Limit)?),
            );
        }
    }
    if !limits.enumeration.is_empty() {
        let mut values = Vec::new();
        for value in &limits.enumeration {
            budget.step()?;
            values.push(match value {
                Data::Text(text) => budget.text(text)?,
                Data::Int(_) | Data::Bool(_) => value.clone(),
                Data::Decimal(number) => {
                    formatted(number, budget)?;
                    value.clone()
                }
                _ => return Err(Failure::Unsupported), // enum contracts admit scalar literals only
            });
        }
        constraints.insert("enum".into(), Data::List(values));
    }
    if !limits.patterns.is_empty() {
        constraints.insert(
            "patterns".into(),
            Data::List(
                limits
                    .patterns
                    .iter()
                    .map(|p| budget.text(p.as_str()))
                    .collect::<Result<_, _>>()?,
            ),
        );
    }
    result.insert("constraints".into(), Data::Record(constraints));
    result.insert("digest".into(), budget.text(contract.digest())?);
    if !contract.display().enum_tones().is_empty() {
        let tones = contract
            .display()
            .enum_tones()
            .iter()
            .map(|(member, tone)| {
                budget.step()?;
                Ok((member.clone(), budget.text(tone.name())?))
            })
            .collect::<Result<indexmap::IndexMap<_, _>, Failure>>()?;
        result.insert(
            "display".into(),
            fields([("enumTones", Data::Record(tones))]),
        );
    }
    Ok(Data::Record(result))
}
/// Charge numeric formatting while writing, before allocating an unbounded temporary string.
fn formatted(value: &impl std::fmt::Display, budget: &mut Budget<'_>) -> Result<Data, Failure> {
    use std::fmt::Write;
    struct Writer<'a, 'b> {
        budget: &'a mut Budget<'b>,
        text: String,
        error: Option<Failure>,
    }
    impl std::fmt::Write for Writer<'_, '_> {
        fn write_str(&mut self, s: &str) -> std::fmt::Result {
            self.budget.append(&mut self.text, s).map_err(|error| {
                self.error = Some(error);
                std::fmt::Error
            })
        }
    }
    let mut writer = Writer {
        budget,
        text: String::new(),
        error: None,
    };
    write!(&mut writer, "{value}").map_err(|_| writer.error.unwrap_or(Failure::Limit))?;
    Ok(Data::Text(writer.text.into()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::driver::CancellationToken;
    #[test]
    fn constructor_metadata_keeps_the_existing_expression_grammar_and_bounds() {
        let registry = ContractRegistry::new();
        assert!(matches!(
            capture(&registry, "  List\t"),
            Captured::Constructor("List", _)
        ));
        for source in [
            "\u{00a0}List\u{00a0}".to_owned(),
            format!("{}List", " ".repeat(4096)),
        ] {
            let expected = registry.resolve(&source).unwrap_err();
            let Captured::Invalid(actual) = capture(&registry, &source) else {
                panic!("constructor metadata bypassed expression parsing")
            };
            assert_eq!(actual, expected);
        }
    }
    #[test]
    fn descriptions_obey_query_budgets_and_cancellation_and_constructor_inventory_resolves() {
        let registry = ContractRegistry::new();
        for (name, parameters) in CONSTRUCTORS {
            let args = parameters
                .iter()
                .map(|_| "Text")
                .collect::<Vec<_>>()
                .join(",");
            assert!(registry.resolve(&format!("{name}<{args}>")).is_ok());
        }
        let captured = capture(&registry, "List<Int>");
        let token = CancellationToken::new();
        let mut budget = Budget {
            bytes: super::super::max_bytes(),
            work: 0,
            token: &token,
        };
        assert!(matches!(
            describe(&captured, &mut budget),
            Err(Failure::Limit)
        ));
        let mut budget = Budget {
            bytes: 0,
            work: super::super::max_work(),
            token: &token,
        };
        assert!(matches!(
            describe(&captured, &mut budget),
            Err(Failure::Limit)
        ));
        let nested = registry.resolve("List<Int>").unwrap();
        let mut budget = Budget {
            bytes: 0,
            work: 0,
            token: &token,
        };
        assert!(matches!(
            description(&nested, &mut budget, 65),
            Err(Failure::Limit)
        ));
        token.cancel();
        assert!(matches!(
            describe(&captured, &mut budget),
            Err(Failure::Cancelled)
        ));
    }
}
