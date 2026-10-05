use crate::{Data, Primitive, RecordShape, Shape, ValidationIssue};
use bigdecimal::BigDecimal;
use indexmap::IndexMap;
use regex::Regex;
use std::{cmp::Ordering, sync::Arc};
use thiserror::Error;

#[derive(Clone, Copy, Debug, Error, PartialEq, Eq)]
#[error("contract validation cancelled")]
pub struct ValidationCancelled;

#[derive(Clone, Debug)]
pub enum Kind {
    Scalar(Primitive),
    Unknown,
    Record(IndexMap<String, Field>),
    List(Arc<Contract>),
    Map(Arc<Contract>, Arc<Contract>),
    Option(Arc<Contract>),
    Iter(Arc<Contract>),
    Union(Arc<Contract>, Arc<Contract>),
}

#[derive(Clone, Debug)]
pub struct Field {
    pub contract: Arc<Contract>,
    pub optional: bool,
}

#[derive(Clone, Debug, Default)]
pub struct Limits {
    pub enumeration: Vec<Data>,
    pub min: Option<BigDecimal>,
    pub max: Option<BigDecimal>,
    pub min_length: Option<usize>,
    pub max_length: Option<usize>,
    pub min_items: Option<usize>,
    pub max_items: Option<usize>,
    pub patterns: Vec<Regex>,
}

#[derive(Clone, Debug)]
pub struct Contract {
    pub(super) name: String,
    pub(super) kind: Kind,
    pub(super) limits: Limits,
}

impl Contract {
    /// Bounded presentation hints from the same resolved rules used by validation.
    /// These strings are documentation, never a second source of validation rules.
    pub fn constraint_hints(&self) -> Vec<String> {
        let mut hints = Vec::new();
        let mut contract = self;
        for _ in 0..64 {
            let limits = &contract.limits;
            if limits.min.is_some() || limits.max.is_some() {
                hints.push(range_hint(
                    "number",
                    limits.min.as_ref(),
                    limits.max.as_ref(),
                ));
            }
            if limits.min_length.is_some() || limits.max_length.is_some() {
                hints.push(range_hint(
                    "text length (characters)",
                    limits.min_length.as_ref(),
                    limits.max_length.as_ref(),
                ));
            }
            if limits.min_items.is_some() || limits.max_items.is_some() {
                hints.push(range_hint(
                    "item count",
                    limits.min_items.as_ref(),
                    limits.max_items.as_ref(),
                ));
            }
            if !limits.enumeration.is_empty() {
                hints.push(enum_hint(&limits.enumeration));
            }
            for pattern in limits.patterns.iter().take(8) {
                hints.push(format!(
                    "text must match {}",
                    metadata_text(pattern.as_str())
                ));
            }
            if let Kind::Option(inner) = &contract.kind {
                contract = inner;
            } else {
                break;
            }
        }
        hints.truncate(32);
        hints
    }
    /// Resolved immutable metadata, including optional fields and inherited constraints.
    /// Describing a contract never validates or changes a value.
    pub fn kind(&self) -> &Kind {
        &self.kind
    }
    pub fn constraints(&self) -> &Limits {
        &self.limits
    }

    pub fn iter_element(&self) -> Option<&Arc<Contract>> {
        if let Kind::Iter(c) = &self.kind {
            Some(c)
        } else {
            None
        }
    }
    pub fn name(&self) -> &str {
        &self.name
    }
    /// Read-only contract hints for foreign JSON readers. These do not validate values or
    /// relax constraints; callers must still validate the complete decoded contract.
    pub fn json_field(&self, name: &str) -> Option<&Contract> {
        match &self.kind {
            Kind::Record(fields) => fields.get(name).map(|field| field.contract.as_ref()),
            Kind::Map(_, value) => Some(value),
            Kind::Option(inner) => inner.json_field(name),
            _ => None,
        }
    }
    pub fn json_element(&self) -> Option<&Contract> {
        match &self.kind {
            Kind::List(element) => Some(element),
            Kind::Option(inner) => inner.json_element(),
            _ => None,
        }
    }
    pub fn json_scalar(&self) -> Option<Primitive> {
        match &self.kind {
            Kind::Scalar(kind) => Some(*kind),
            Kind::Option(inner) => inner.json_scalar(),
            _ => None,
        }
    }
    pub fn shape(&self) -> Shape {
        self.project_shape(&mut 100_000, 0)
    }
    fn project_shape(&self, remaining: &mut usize, depth: usize) -> Shape {
        // Shared named alternatives can form a DAG with exponentially many paths.
        // Shape metadata may conservatively lose precision, never contract checks.
        if *remaining == 0 || depth > 64 {
            return Shape::Unknown;
        }
        *remaining -= 1;
        match &self.kind {
            Kind::Scalar(p) => Shape::Primitive(*p),
            Kind::List(element) => {
                Shape::List(Box::new(element.project_shape(remaining, depth + 1)))
            }
            Kind::Record(fields) => Shape::Record(
                RecordShape::new(
                    &self.name,
                    fields
                        .iter()
                        .filter(|(_, field)| !field.optional)
                        .map(|(key, field)| {
                            (
                                key.clone(),
                                field.contract.project_shape(remaining, depth + 1),
                            )
                        }),
                )
                .expect("resolved contract fields have unique keys"),
            ),
            Kind::Map(_, _) => {
                Shape::Record(RecordShape::new(&self.name, []).expect("empty record"))
            }
            Kind::Unknown => Shape::Unknown,
            Kind::Union(a, b) => {
                let a = a.project_shape(remaining, depth + 1);
                if a == Shape::Unknown {
                    return a;
                }
                if a == b.project_shape(remaining, depth + 1) {
                    a
                } else {
                    Shape::Unknown
                }
            }
            Kind::Iter(element) => {
                Shape::Iter(Box::new(element.project_shape(remaining, depth + 1)))
            }
            Kind::Option(element) => {
                Shape::Option(Box::new(element.project_shape(remaining, depth + 1)))
            }
        }
    }
    pub fn issues(&self, data: &Data) -> Vec<ValidationIssue> {
        self.issues_with_cancel(data, &|| false)
            .expect("non-cancellable inspection")
    }
    pub fn issues_with_cancel(
        &self,
        data: &Data,
        cancelled: &dyn Fn() -> bool,
    ) -> Result<Vec<ValidationIssue>, ValidationCancelled> {
        self.issues_with_budget(data, cancelled, &mut 100_000)
    }
    /// Share validation work across foreign alternative interpretations. Failed
    /// alternatives consume budget too; callers cannot reset work by backtracking.
    pub fn issues_with_budget(
        &self,
        data: &Data,
        cancelled: &dyn Fn() -> bool,
        remaining: &mut usize,
    ) -> Result<Vec<ValidationIssue>, ValidationCancelled> {
        let mut context = Validation {
            issues: Vec::new(),
            remaining: *remaining,
            exhausted: false,
            cancelled,
        };
        let result = context.inspect(self, data, "", 0);
        *remaining = context.remaining;
        result?;
        Ok(context.issues)
    }
    pub fn is_subtype_of(&self, expected: &Self) -> bool {
        self.subtype(expected, &mut 100_000, 0)
    }
    fn subtype(&self, expected: &Self, remaining: &mut usize, depth: usize) -> bool {
        if std::ptr::eq(self, expected) {
            return true;
        }
        if *remaining == 0 || depth > 128 {
            return false;
        }
        *remaining -= 1;
        // Union aliases share immutable alternatives and cannot declare outer
        // constraints. Prove unchanged structure without expanding its DAG.
        if let (Kind::Union(a, b), Kind::Union(c, d)) = (&self.kind, &expected.kind)
            && ((Arc::ptr_eq(a, c) && Arc::ptr_eq(b, d))
                || (Arc::ptr_eq(a, d) && Arc::ptr_eq(b, c)))
        {
            return true;
        }
        if let Kind::Union(a, b) = &self.kind {
            return a.subtype(expected, remaining, depth + 1)
                && b.subtype(expected, remaining, depth + 1);
        }
        if let Kind::Union(a, b) = &expected.kind {
            return self.subtype(a, remaining, depth + 1) || self.subtype(b, remaining, depth + 1);
        }
        if matches!(expected.kind, Kind::Unknown) {
            return true;
        }
        let same_kind = matches!(
            (&self.kind, &expected.kind),
            (Kind::Unknown, Kind::Unknown)
                | (Kind::Record(_), Kind::Record(_))
                | (Kind::List(_), Kind::List(_))
                | (Kind::Map(_, _), Kind::Map(_, _))
                | (Kind::Option(_), Kind::Option(_))
                | (Kind::Iter(_), Kind::Iter(_))
        ) || matches!((&self.kind,&expected.kind),(Kind::Scalar(a),Kind::Scalar(b)) if a == b);
        if !same_kind {
            return false;
        }
        let actual = &self.limits;
        let required = &expected.limits;
        if !required.enumeration.is_empty()
            && (actual.enumeration.is_empty()
                || !actual
                    .enumeration
                    .iter()
                    .all(|a| required.enumeration.iter().any(|b| same(a, b))))
        {
            return false;
        }
        let finite_proof = !actual.enumeration.is_empty()
            && actual
                .enumeration
                .iter()
                .all(|d| expected.issues(d).is_empty());
        if !finite_proof
            && (!lower(actual.min.as_ref(), required.min.as_ref())
                || !upper(actual.max.as_ref(), required.max.as_ref())
                || !lower(actual.min_length.as_ref(), required.min_length.as_ref())
                || !upper(actual.max_length.as_ref(), required.max_length.as_ref())
                || !lower(actual.min_items.as_ref(), required.min_items.as_ref())
                || !upper(actual.max_items.as_ref(), required.max_items.as_ref())
                || !required
                    .patterns
                    .iter()
                    .all(|p| actual.patterns.iter().any(|a| a.as_str() == p.as_str())))
        {
            return false;
        }
        match (&self.kind, &expected.kind) {
            (Kind::Record(a), Kind::Record(b)) => b.iter().all(|(key, required)| {
                a.get(key).is_some_and(|actual| {
                    (required.optional || !actual.optional)
                        && actual
                            .contract
                            .subtype(&required.contract, remaining, depth + 1)
                })
            }),
            (Kind::List(a), Kind::List(b))
            | (Kind::Option(a), Kind::Option(b))
            | (Kind::Iter(a), Kind::Iter(b)) => a.subtype(b, remaining, depth + 1),
            (Kind::Map(ak, av), Kind::Map(bk, bv)) => {
                ak.subtype(bk, remaining, depth + 1)
                    && bk.subtype(ak, remaining, depth + 1)
                    && av.subtype(bv, remaining, depth + 1)
            }
            _ => true,
        }
    }
}

pub(super) fn same(a: &Data, b: &Data) -> bool {
    match (a, b) {
        (Data::Decimal(a), Data::Decimal(b)) => a.numeric_cmp(b) == Ordering::Equal,
        _ => a == b,
    }
}
pub(super) fn number(data: &Data) -> Option<BigDecimal> {
    match data {
        Data::Int(value) => Some((*value).into()),
        Data::Decimal(value) => Some(value.number().clone()),
        _ => None,
    }
}
fn lower<T: PartialOrd>(actual: Option<&T>, expected: Option<&T>) -> bool {
    expected.is_none_or(|e| actual.is_some_and(|a| a >= e))
}
fn upper<T: PartialOrd>(actual: Option<&T>, expected: Option<&T>) -> bool {
    expected.is_none_or(|e| actual.is_some_and(|a| a <= e))
}
fn outside(value: usize, min: Option<usize>, max: Option<usize>) -> bool {
    min.is_some_and(|m| value < m) || max.is_some_and(|m| value > m)
}
fn pointer(key: &str) -> String {
    key.replace('~', "~0").replace('/', "~1")
}

struct Validation<'a> {
    issues: Vec<ValidationIssue>,
    remaining: usize,
    exhausted: bool,
    cancelled: &'a dyn Fn() -> bool,
}

impl Validation<'_> {
    fn issue(&mut self, path: &str, code: &'static str, message: impl Into<String>) {
        if self.issues.len() < 100 {
            self.issues.push(ValidationIssue {
                path: path.into(),
                code: code.into(),
                message: message.into(),
            });
        }
    }
    fn full(&self) -> bool {
        self.exhausted || self.issues.len() >= 100
    }
    fn inspect(
        &mut self,
        contract: &Contract,
        data: &Data,
        path: &str,
        depth: usize,
    ) -> Result<(), ValidationCancelled> {
        if self.full() {
            return Ok(());
        }
        if (self.cancelled)() {
            return Err(ValidationCancelled);
        }
        if depth > 64 || self.remaining == 0 {
            self.issue(path, "TYP006", "validation resource limit reached");
            self.exhausted = true;
            return Ok(());
        }
        self.remaining -= 1;
        if let Kind::Union(a, b) = &contract.kind {
            let checkpoint = self.issues.len();
            self.inspect(a, data, path, depth + 1)?;
            if self.exhausted || self.issues.len() == checkpoint {
                return Ok(());
            }
            self.issues.truncate(checkpoint);
            self.inspect(b, data, path, depth + 1)?;
            if !self.exhausted && self.issues.len() > checkpoint {
                self.issues.truncate(checkpoint);
                self.issue(
                    path,
                    "TYP005",
                    format!("expected an alternative of {}", contract.name),
                );
            }
            return Ok(());
        }
        if let Kind::Option(element) = &contract.kind {
            return match data {
                Data::Option(None) => Ok(()),
                Data::Option(Some(value)) => self.inspect(element, value, path, depth + 1),
                _ => {
                    self.issue(path, "TYP005", format!("expected {}", contract.name));
                    Ok(())
                }
            };
        }
        if let Kind::Iter(expected) = &contract.kind {
            if let Data::Iter(iter) = data
                && iter
                    .item_contract()
                    .is_some_and(|actual| actual.is_subtype_of(expected))
            {
                return Ok(());
            }
            self.issue(
                path,
                "TYP008",
                format!("{} requires a compatible typed Iter recipe", contract.name),
            );
            return Ok(());
        }
        let fits = match (&contract.kind, data) {
            (Kind::Record(_) | Kind::Map(_, _), Data::Record(_))
            | (Kind::List(_), Data::List(_)) => true,
            (Kind::Record(_) | Kind::Map(_, _) | Kind::List(_), _) => false,
            _ => data.fits_shallow(&contract.shape()),
        };
        if !fits {
            self.issue(path, "TYP005", format!("expected {}", contract.name));
            return Ok(());
        }
        match (&contract.kind, data) {
            (Kind::Record(fields), Data::Record(values)) => {
                for (key, field) in fields {
                    if self.full() {
                        break;
                    }
                    let child = format!("{path}/{}", pointer(key));
                    if let Some(value) = values.get(key) {
                        self.inspect(&field.contract, value, &child, depth + 1)?;
                    } else if !field.optional {
                        self.issue(&child, "TYP005", "required field is missing");
                    }
                }
            }
            (Kind::List(element), Data::List(values)) => {
                for (i, value) in values.iter().enumerate() {
                    if self.full() {
                        break;
                    }
                    self.inspect(element, value, &format!("{path}/{i}"), depth + 1)?;
                }
            }
            (Kind::Map(key_type, value_type), Data::Record(values)) => {
                for (key, value) in values {
                    if self.full() {
                        break;
                    }
                    let child = format!("{path}/{}", pointer(key));
                    self.inspect(
                        key_type,
                        &Data::Text(key.as_str().into()),
                        &child,
                        depth + 1,
                    )?;
                    self.inspect(value_type, value, &child, depth + 1)?;
                }
            }
            _ => {}
        }
        let limits = &contract.limits;
        if !limits.enumeration.is_empty() && !limits.enumeration.iter().any(|item| same(item, data))
        {
            self.issue(
                path,
                "TYP005",
                format!("value is not allowed; {}", enum_hint(&limits.enumeration)),
            );
        }
        if let Some(number) = number(data)
            && (limits.min.as_ref().is_some_and(|min| &number < min)
                || limits.max.as_ref().is_some_and(|max| &number > max))
        {
            self.issue(
                path,
                "TYP005",
                format!(
                    "number is outside the permitted bounds; {}",
                    range_hint("number", limits.min.as_ref(), limits.max.as_ref())
                ),
            );
        }
        if let Data::Text(text) = data {
            if outside(text.chars().count(), limits.min_length, limits.max_length) {
                self.issue(
                    path,
                    "TYP005",
                    format!(
                        "text length is outside the permitted bounds; {}",
                        range_hint(
                            "text length (characters)",
                            limits.min_length.as_ref(),
                            limits.max_length.as_ref()
                        )
                    ),
                );
            }
            for pattern in &limits.patterns {
                if !pattern.is_match(text) {
                    self.issue(
                        path,
                        "TYP005",
                        format!("text does not match {}", pattern.as_str()),
                    );
                }
            }
        }
        if let Data::List(items) = data
            && outside(items.len(), limits.min_items, limits.max_items)
        {
            self.issue(
                path,
                "TYP005",
                format!(
                    "item count is outside the permitted bounds; {}",
                    range_hint(
                        "item count",
                        limits.min_items.as_ref(),
                        limits.max_items.as_ref()
                    )
                ),
            );
        }
        Ok(())
    }
}

fn metadata_text(value: impl std::fmt::Display) -> String {
    use std::fmt::Write;
    struct Bounded(String);
    impl std::fmt::Write for Bounded {
        fn write_str(&mut self, text: &str) -> std::fmt::Result {
            for c in text.chars() {
                if self.0.len() + c.len_utf8() > 256 {
                    self.0.push('…');
                    return Err(std::fmt::Error);
                }
                self.0.push(c);
            }
            Ok(())
        }
    }
    let mut writer = Bounded(String::new());
    let _ = write!(&mut writer, "{value}");
    writer.0
}
fn range_hint<T: std::fmt::Display>(label: &str, min: Option<&T>, max: Option<&T>) -> String {
    match (min, max) {
        (Some(min), Some(max)) => format!(
            "{label}: {}..{} (inclusive)",
            metadata_text(min),
            metadata_text(max)
        ),
        (Some(min), None) => format!("{label}: at least {} (inclusive)", metadata_text(min)),
        (None, Some(max)) => format!("{label}: at most {} (inclusive)", metadata_text(max)),
        (None, None) => String::new(),
    }
}
fn enum_hint(values: &[Data]) -> String {
    let mut text = String::from("allowed values: ");
    for (i, value) in values.iter().take(32).enumerate() {
        if i != 0 {
            text.push_str(", ");
        }
        text.push_str(&match value {
            Data::Text(value) => metadata_text(format_args!("{value:?}")),
            Data::Int(value) => metadata_text(value),
            Data::Decimal(value) => metadata_text(value),
            Data::Bool(value) => metadata_text(value),
            _ => "[non-scalar]".into(),
        });
    }
    if values.len() > 32 {
        text.push_str(&format!(", … {} more", values.len() - 32));
    }
    text
}
