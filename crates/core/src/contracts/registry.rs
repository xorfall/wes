use std::{collections::BTreeSet, sync::Arc};

use bigdecimal::BigDecimal;
use indexmap::IndexMap;
use regex::Regex;

use super::{
    Contract, ContractError, TypeExpression,
    model::{Field, Kind, Limits, number, same},
    package::{self, Node, ScalarKind},
};
use crate::{Data, Decimal, Primitive};

type Definitions = IndexMap<String, Arc<Contract>>;
type Mapping = IndexMap<String, Node>;

/// An owned registry. A load either installs every definition or changes nothing.
/// Callers decide how to synchronize workspace access; contracts are immutable and shareable.
#[derive(Clone, Debug)]
pub struct ContractRegistry {
    contracts: Definitions,
    sources: Vec<String>,
    iterators: IndexMap<String, crate::IterRecipe>,
}

impl Default for ContractRegistry {
    fn default() -> Self {
        let mut contracts = IndexMap::new();
        for (name, primitive) in [
            ("Text", Primitive::Text),
            ("Int", Primitive::Int),
            ("Decimal", Primitive::Decimal),
            ("Bool", Primitive::Bool),
            ("Instant", Primitive::Instant),
            ("Duration", Primitive::Duration),
            ("Interval", Primitive::Interval),
            ("Bytes", Primitive::Bytes),
        ] {
            contracts.insert(name.into(), contract(name, Kind::Scalar(primitive)));
        }
        contracts.insert("Unknown".into(), contract("Unknown", Kind::Unknown));
        contracts.insert(
            "Record".into(),
            contract("Record", Kind::Record(IndexMap::new())),
        );
        for shape in [
            crate::ErrorValue::issue_shape(),
            crate::ErrorValue::shape(),
            crate::ErrorValue::cancellation_shape(),
        ] {
            let builtin = builtin(&shape, &contracts);
            contracts.insert(builtin.name.clone(), builtin);
        }
        Self {
            contracts,
            sources: vec![],
            iterators: IndexMap::new(),
        }
    }
}

impl ContractRegistry {
    pub fn new() -> Self {
        Self::default()
    }
    pub fn snapshot(&self) -> &IndexMap<String, Arc<Contract>> {
        &self.contracts
    }
    pub fn resolve(&self, expression: &str) -> Result<Arc<Contract>, ContractError> {
        resolve(&TypeExpression::parse(expression)?, &mut |name| {
            self.contracts
                .get(name)
                .cloned()
                .ok_or_else(|| unknown(name))
        })
    }
    pub fn sources(&self) -> &[String] {
        &self.sources
    }
    pub fn iterators(&self) -> &IndexMap<String, crate::IterRecipe> {
        &self.iterators
    }
    pub fn capture(&self, name: &str) -> Result<crate::ContractCapture, ContractError> {
        self.resolve(name)?;
        if self.sources.iter().map(String::len).sum::<usize>() > 1024 * 1024 {
            return Err(problem("captured type package limit exceeded"));
        }
        Ok(crate::ContractCapture {
            name: name.into(),
            packages: self.sources.clone(),
        })
    }
    pub fn load(&mut self, yaml: &str) -> Result<IndexMap<String, Arc<Contract>>, ContractError> {
        let mut staged = self.clone();
        let result = staged.load_inner(yaml)?;
        staged.sources.push(yaml.into());
        *self = staged;
        Ok(result)
    }
    /// Import a self-contained package, reusing equal definitions while rejecting conflicts.
    /// Captures retain only new declarations, so ordinary strict load can restore them.
    pub fn import_package(
        &mut self,
        yaml: &str,
    ) -> Result<IndexMap<String, Arc<Contract>>, ContractError> {
        let mut incoming = Self::new();
        let imported = incoming.load(yaml)?;
        for (name, contract) in &imported {
            if let Some(existing) = self.contracts.get(name) {
                if !existing.is_subtype_of(contract) || !contract.is_subtype_of(existing) {
                    return Err(ContractError {
                        code: "TYP004",
                        message: format!("conflicting imported type: {name}"),
                    });
                }
            }
        }
        let mut root = package::read(yaml)?;
        let Node::Mapping(ref mut fields) = root else {
            return Err(problem("expected package mapping"));
        };
        let Some(Node::Mapping(definitions)) = fields.get_mut("types") else {
            return Err(problem("expected type mapping"));
        };
        definitions.retain(|name, _| !self.contracts.contains_key(name));
        if definitions.is_empty() && !fields.contains_key("iterators") {
            return Ok(IndexMap::new());
        }
        self.load(&root.source())
    }
    fn load_inner(&mut self, yaml: &str) -> Result<IndexMap<String, Arc<Contract>>, ContractError> {
        let root = package::read(yaml)?;
        crate::package_schema::declarations()
            .validate("contract.package", &root)
            .map_err(|issue| ContractError {
                code: issue.code,
                message: issue.message,
            })?;
        let root = mapping(&root)?;
        if let Some(version) = root.get("version")
            && !matches!(version, Node::Scalar(ScalarKind::Int, value) if value == "1")
        {
            return Err(problem("unsupported type-package version"));
        }
        let definitions = mapping(required(root, "types")?)?;
        if definitions.len() > 1000 {
            return Err(problem("a package may define at most 1000 types"));
        }
        for name in definitions.keys() {
            let parsed = TypeExpression::parse(name)?;
            if parsed.name != *name || !parsed.arguments.is_empty() {
                return Err(problem(format!("invalid type name: {name}")));
            }
            if self.contracts.contains_key(name) || super::Constructor::named(name).is_some() {
                return Err(ContractError {
                    code: "TYP004",
                    message: format!("type is already defined or reserved: {name}"),
                });
            }
        }
        let mut resolver = Resolver {
            existing: &self.contracts,
            definitions,
            staged: IndexMap::new(),
            resolving: BTreeSet::new(),
        };
        for name in definitions.keys() {
            resolver.find(name)?;
        }
        let staged = resolver.staged;
        self.contracts.extend(staged.clone());
        if let Some(recipes) = root.get("iterators") {
            let recipes = mapping(recipes)?;
            if recipes.len() + self.iterators.len() > 1000 {
                return Err(problem("too many iterator recipes"));
            }
            for (name, recipe) in recipes {
                let parsed = TypeExpression::parse(name)?;
                if parsed.name != *name
                    || !parsed.arguments.is_empty()
                    || self.iterators.contains_key(name)
                {
                    return Err(problem("invalid or conflicting iterator recipe name"));
                }
                let fields = mapping(recipe)?;
                let input = self.resolve(required(fields, "input")?.text()?)?;
                let output = self.resolve(required(fields, "output")?.text()?)?;
                if output.iter_element().is_none() {
                    return Err(problem("iterator output must be Iter<T>"));
                }
                let mode = crate::IterMode::parse(required(fields, "mode")?.text()?)
                    .ok_or_else(|| problem("unknown iterator mode"))?;
                let argument = fields
                    .get(if mode == crate::IterMode::Split {
                        "delimiter"
                    } else {
                        "pattern"
                    })
                    .map(Node::text)
                    .transpose()?
                    .map(str::to_owned);
                let pattern = mode.argument_kind().is_some();
                if pattern != argument.is_some()
                    || (mode == crate::IterMode::Split && fields.contains_key("pattern"))
                    || (mode != crate::IterMode::Split && fields.contains_key("delimiter"))
                {
                    return Err(problem("invalid iterator extraction argument"));
                }
                if let Some(arg) = &argument {
                    if arg.len() > 16 * 1024 || (mode == crate::IterMode::Split && arg.is_empty()) {
                        return Err(problem("invalid iterator pattern size"));
                    }
                    if mode != crate::IterMode::Split {
                        regex::RegexBuilder::new(arg)
                            .size_limit(1024 * 1024)
                            .build()
                            .map_err(|_| problem("invalid or oversized iterator regex"))?;
                    }
                }
                let shape = input.shape();
                let valid = match mode {
                    crate::IterMode::Items => {
                        matches!(shape, crate::Shape::List(_) | crate::Shape::Unknown)
                    }
                    crate::IterMode::Keys | crate::IterMode::Values | crate::IterMode::Entries => {
                        matches!(shape, crate::Shape::Record(_) | crate::Shape::Unknown)
                    }
                    _ => matches!(
                        shape,
                        crate::Shape::Primitive(Primitive::Text) | crate::Shape::Unknown
                    ),
                };
                if !valid {
                    return Err(problem("iterator mode does not match input contract"));
                }
                self.iterators.insert(
                    name.clone(),
                    crate::IterRecipe {
                        input,
                        output,
                        mode,
                        argument,
                    },
                );
            }
        }
        Ok(staged)
    }
}

struct Resolver<'a> {
    existing: &'a Definitions,
    definitions: &'a Mapping,
    staged: Definitions,
    resolving: BTreeSet<String>,
}
impl Resolver<'_> {
    fn find(&mut self, name: &str) -> Result<Arc<Contract>, ContractError> {
        if let Some(found) = self.existing.get(name).or_else(|| self.staged.get(name)) {
            return Ok(found.clone());
        }
        let definition = self.definitions.get(name).ok_or_else(|| unknown(name))?;
        if self.resolving.len() >= 64 || !self.resolving.insert(name.into()) {
            return Err(problem(format!(
                "cyclic or overly deep type definition: {name}"
            )));
        }
        let definition = mapping(definition)?;
        let parent = resolve(
            &TypeExpression::parse(required(definition, "base")?.text()?)?,
            &mut |name| self.find(name),
        )?;
        let mut kind = parent.kind.clone();
        if let Some(fields) = definition.get("fields") {
            let Kind::Record(inherited) = &mut kind else {
                return Err(problem("fields require a Record base"));
            };
            for (key, node) in mapping(fields)? {
                let (expression, optional) = if let Node::Mapping(field) = node {
                    (
                        required(field, "type")?.text()?,
                        field
                            .get("optional")
                            .map(boolean)
                            .transpose()?
                            .unwrap_or(false),
                    )
                } else {
                    (node.text()?, false)
                };
                let next = Field {
                    contract: resolve(&TypeExpression::parse(expression)?, &mut |name| {
                        self.find(name)
                    })?,
                    optional,
                };
                if inherited.get(key).is_some_and(|previous| {
                    (!previous.optional && optional)
                        || !next.contract.is_subtype_of(&previous.contract)
                }) {
                    return Err(problem(format!(
                        "field {key} weakens its inherited contract"
                    )));
                }
                inherited.insert(key.clone(), next);
            }
        }
        let limits = limits(&parent, definition)?;
        let next = Arc::new(Contract {
            name: name.into(),
            kind,
            limits,
        });
        if !next.is_subtype_of(&parent) {
            return Err(problem(format!("{name} weakens its base contract")));
        }
        for item in &next.limits.enumeration {
            if !next.issues(item).is_empty() {
                return Err(problem(format!(
                    "{name} has an enum member outside its constraints"
                )));
            }
        }
        self.staged.insert(name.into(), next.clone());
        self.resolving.remove(name);
        Ok(next)
    }
}

fn resolve(
    expression: &TypeExpression,
    names: &mut impl FnMut(&str) -> Result<Arc<Contract>, ContractError>,
) -> Result<Arc<Contract>, ContractError> {
    if expression.arguments.is_empty() {
        return names(&expression.name);
    }
    let arguments = &expression.arguments;
    let constructor = super::Constructor::named(&expression.name)
        .ok_or_else(|| problem(format!("unknown type constructor: {}", expression.name)))?;
    if arguments.len() != constructor.parameters.len() {
        return Err(problem(format!("wrong generic arity: {}", expression.name)));
    }
    let kind = match (constructor.constructor, arguments.as_slice()) {
        (super::Constructor::List, [element]) => Kind::List(resolve(element, names)?),
        (super::Constructor::Option, [element]) => Kind::Option(resolve(element, names)?),
        (super::Constructor::Iter, [element]) => Kind::Iter(resolve(element, names)?),
        (super::Constructor::Union, [a, b]) => Kind::Union(resolve(a, names)?, resolve(b, names)?),
        (super::Constructor::Map, [key, value]) => {
            let key = resolve(key, names)?;
            if !matches!(key.kind, Kind::Scalar(Primitive::Text)) {
                return Err(problem("Map keys must be Text-based"));
            }
            Kind::Map(key, resolve(value, names)?)
        }
        _ => unreachable!("constructor arity checked against catalogue"),
    };
    Ok(contract(&expression.to_string(), kind))
}

fn limits(base: &Contract, written: &Mapping) -> Result<Limits, ContractError> {
    let kind = &base.kind;
    allowed(
        written,
        &["min", "max"],
        matches!(kind, Kind::Scalar(Primitive::Int | Primitive::Decimal)),
    )?;
    allowed(
        written,
        &["minLength", "maxLength", "pattern"],
        matches!(kind, Kind::Scalar(Primitive::Text)),
    )?;
    allowed(
        written,
        &["minItems", "maxItems"],
        matches!(kind, Kind::List(_)),
    )?;
    allowed(
        written,
        &["enum"],
        matches!(
            kind,
            Kind::Scalar(Primitive::Text | Primitive::Int | Primitive::Decimal | Primitive::Bool)
        ),
    )?;
    let mut next = base.limits.clone();
    if let Some(node) = written.get("enum") {
        let Node::Sequence(items) = node else {
            return Err(problem("enum must be a non-empty sequence"));
        };
        if items.is_empty() {
            return Err(problem("enum must be a non-empty sequence"));
        }
        let enumeration = items
            .iter()
            .map(|item| datum(item, kind))
            .collect::<Result<Vec<_>, _>>()?;
        if !next.enumeration.is_empty()
            && !enumeration
                .iter()
                .all(|item| next.enumeration.iter().any(|parent| same(item, parent)))
        {
            return Err(problem("enum cannot add values outside its parent"));
        }
        next.enumeration = enumeration;
    }
    next.min = bound(written, "min", next.min, true, kind)?;
    next.max = bound(written, "max", next.max, false, kind)?;
    next.min_length = count(written, "minLength", next.min_length, true)?;
    next.max_length = count(written, "maxLength", next.max_length, false)?;
    next.min_items = count(written, "minItems", next.min_items, true)?;
    next.max_items = count(written, "maxItems", next.max_items, false)?;
    if reversed(&next.min, &next.max)
        || reversed(&next.min_length, &next.max_length)
        || reversed(&next.min_items, &next.max_items)
    {
        return Err(problem("minimum exceeds maximum"));
    }
    if let Some(pattern) = written.get("pattern") {
        let pattern = pattern.text()?;
        if pattern.encode_utf16().count() > 4096 {
            return Err(problem("pattern is too long"));
        }
        let compiled = Regex::new(pattern)
            .map_err(|e| problem(format!("invalid or unsupported regular expression: {e}")))?;
        if !next.patterns.iter().any(|old| old.as_str() == pattern) {
            next.patterns.push(compiled);
        }
    }
    Ok(next)
}

fn bound(
    written: &Mapping,
    key: &str,
    old: Option<BigDecimal>,
    lower: bool,
    kind: &Kind,
) -> Result<Option<BigDecimal>, ContractError> {
    let Some(node) = written.get(key) else {
        return Ok(old);
    };
    let next = number(&datum(node, kind)?).ok_or_else(|| problem("expected numeric bound"))?;
    if weakens(&next, old.as_ref(), lower) {
        return Err(problem(format!("{key} weakens the inherited bound")));
    }
    Ok(Some(next))
}
fn count(
    written: &Mapping,
    key: &str,
    old: Option<usize>,
    lower: bool,
) -> Result<Option<usize>, ContractError> {
    let Some(node) = written.get(key) else {
        return Ok(old);
    };
    let Data::Int(value) = datum(node, &Kind::Scalar(Primitive::Int))? else {
        unreachable!("Int datum")
    };
    if !(0..=i64::from(i32::MAX)).contains(&value) {
        return Err(problem(format!(
            "{key} must be a non-negative 32-bit integer"
        )));
    }
    let next = value as usize;
    if weakens(&next, old.as_ref(), lower) {
        return Err(problem(format!("{key} weakens the inherited bound")));
    }
    Ok(Some(next))
}
fn datum(node: &Node, kind: &Kind) -> Result<Data, ContractError> {
    let Node::Scalar(tag, raw) = node else {
        return Err(problem("expected scalar constraint"));
    };
    match (kind, tag) {
        (Kind::Scalar(Primitive::Text), ScalarKind::Text) => Ok(Data::Text(raw.as_str().into())),
        (Kind::Scalar(Primitive::Bool), ScalarKind::Bool) => Ok(Data::Bool(boolean(node)?)),
        (Kind::Scalar(Primitive::Int), ScalarKind::Int) => crate::numeric::integer(raw)
            .map(Data::Int)
            .ok_or_else(|| problem("expected a signed 64-bit decimal integer")),
        (Kind::Scalar(Primitive::Decimal), ScalarKind::Int | ScalarKind::Decimal) => raw
            .parse::<Decimal>()
            .map(Data::Decimal)
            .map_err(|_| problem("expected finite decimal notation in range")),
        _ => Err(problem("scalar constraint does not match its base type")),
    }
}
fn boolean(node: &Node) -> Result<bool, ContractError> {
    match node {
        Node::Scalar(ScalarKind::Bool, value) if value.eq_ignore_ascii_case("true") => Ok(true),
        Node::Scalar(ScalarKind::Bool, value) if value.eq_ignore_ascii_case("false") => Ok(false),
        _ => Err(problem("expected true/false boolean")),
    }
}
fn mapping(node: &Node) -> Result<&Mapping, ContractError> {
    match node {
        Node::Mapping(entries) => Ok(entries),
        _ => Err(problem("expected a mapping")),
    }
}
fn required<'a>(mapping: &'a Mapping, key: &str) -> Result<&'a Node, ContractError> {
    mapping
        .get(key)
        .ok_or_else(|| problem(format!("missing required key: {key}")))
}
fn allowed(mapping: &Mapping, keys: &[&str], permitted: bool) -> Result<(), ContractError> {
    if !permitted && let Some(key) = keys.iter().find(|key| mapping.contains_key(**key)) {
        return Err(problem(format!("{key} does not apply to this base")));
    }
    Ok(())
}
fn weakens<T: PartialOrd>(next: &T, old: Option<&T>, lower: bool) -> bool {
    old.is_some_and(|old| if lower { next < old } else { next > old })
}
fn reversed<T: PartialOrd>(min: &Option<T>, max: &Option<T>) -> bool {
    matches!((min,max),(Some(min),Some(max)) if min > max)
}
fn contract(name: &str, kind: Kind) -> Arc<Contract> {
    Arc::new(Contract {
        name: name.into(),
        kind,
        limits: Limits::default(),
    })
}

fn builtin(shape: &crate::Shape, contracts: &Definitions) -> Arc<Contract> {
    match shape {
        crate::Shape::Meta(_) => {
            unreachable!("built-in data contracts contain no management types")
        }
        crate::Shape::Primitive(_) | crate::Shape::Unknown => contracts[&shape.to_string()].clone(),
        crate::Shape::List(element) => {
            contract(&shape.to_string(), Kind::List(builtin(element, contracts)))
        }
        crate::Shape::Iter(element) => {
            contract(&shape.to_string(), Kind::Iter(builtin(element, contracts)))
        }
        crate::Shape::Option(element) => contract(
            &shape.to_string(),
            Kind::Option(builtin(element, contracts)),
        ),
        crate::Shape::Record(record) => contract(
            record.name(),
            Kind::Record(
                record
                    .fields()
                    .map(|(name, shape)| {
                        (
                            name.into(),
                            Field {
                                contract: builtin(shape, contracts),
                                optional: false,
                            },
                        )
                    })
                    .collect(),
            ),
        ),
    }
}
fn unknown(name: &str) -> ContractError {
    ContractError {
        code: "TYP003",
        message: format!("unknown type: {name}"),
    }
}
fn problem(message: impl Into<String>) -> ContractError {
    ContractError::declaration(message)
}
