//! Bounded descriptions shared by analysis and execution; never format input payloads.
use super::{OperationSpec, Package};
use wes_core::Shape;

/// Quote a declaration name, escaping controls and bounding even unusually long names.
/// This is for source/metadata labels, not evaluated values or dynamic lookup keys.
pub fn label(name: &str) -> String {
    let mut chars = name.chars();
    let mut result = String::from("'");
    for c in chars.by_ref().take(80) {
        result.extend(c.escape_debug());
    }
    if chars.next().is_some() {
        result.push('…');
    }
    result.push('\'');
    result
}

pub fn arity(subject: &str, min: usize, max: usize, actual: usize) -> String {
    let expected = if min == max {
        min.to_string()
    } else {
        format!("{min}..{max}")
    };
    format!("{subject} expects {expected} argument(s); received {actual}")
}

pub fn operation_arity(
    package: &Package,
    spec: OperationSpec,
    actual: usize,
    method: bool,
) -> String {
    let name = package
        .operations()
        .find(|(_, candidate)| *candidate == spec)
        .map_or(spec.operation.id(), |(name, _)| name);
    let receiver = usize::from(method);
    arity(
        &format!(
            "{} {}",
            if method { "method" } else { "operation" },
            label(name)
        ),
        usize::from(spec.min).saturating_sub(receiver),
        usize::from(spec.max).saturating_sub(receiver),
        actual,
    )
}

/// Describe only the outer kind, without traversing schemas or exposing record fields.
pub fn kind(shape: &Shape) -> String {
    match shape {
        Shape::Dataset(_) => "Dataset".into(),
        Shape::Primitive(p) => p.to_string(),
        Shape::List(_) => "List".into(),
        Shape::Option(_) => "Option".into(),
        Shape::Iter(_) => "Iter".into(),
        Shape::Record(_) => "Record".into(),
        Shape::Meta(t) => t.to_string(),
        Shape::Unknown => "Unknown".into(),
    }
}

/// Stable cause categories shared by analysis, execution and help. No message-text inference.
#[derive(Clone, Copy, Debug)]
pub enum Category {
    Bounds,
    Parse,
    Contract,
}
impl Category {
    pub const fn code(self) -> &'static str {
        match self {
            Self::Bounds => "CAL015",
            Self::Parse => "CAL016",
            Self::Contract => "CAL017",
        }
    }
}
pub const CODES: &[(&str, &str, &str)] = &[
    (
        "CAL001",
        "Calculation syntax",
        "Check delimiters, initialization and allowed assignment targets",
    ),
    (
        "CAL002",
        "Calculation metadata/invariant",
        "Use literal metadata selectors; report an internal invariant failure",
    ),
    (
        "CAL003",
        "Internal calculation state invariant",
        "Report the internal failure with a minimal reproducible calculation",
    ),
    (
        "CAL004",
        "Type or unsupported value operation",
        "Use matching types and explicit conversions",
    ),
    (
        "CAL005",
        "Numeric/temporal value",
        "Check overflow, division, precision and supported ranges",
    ),
    (
        "CAL006",
        "Calculation resource limit",
        "Bound the input, reduce recursion or split the calculation",
    ),
    (
        "CAL007",
        "Cancelled",
        "Start a new run explicitly if needed",
    ),
    (
        "CAL008",
        "Provider call failed",
        "Inspect the nested provider cause and external outcome",
    ),
    (
        "CAL009",
        "Callback purity",
        "Use pure callbacks or explicit for-of for effects",
    ),
    (
        "CAL010",
        "Name resolution",
        "Declare the intended local/reference; avoid duplicate names and reserved keywords, literals or namespaces",
    ),
    (
        "CAL011",
        "Immutable binding assignment",
        "Use let for rebinding; values remain immutable",
    ),
    ("CAL012", "Arity", "Supply the declared arguments"),
    (
        "CAL013",
        "Initialization/control flow",
        "Initialize before use and return on every required path",
    ),
    (
        "CAL014",
        "Duplicate record field",
        "Use distinct fields or withFields to replace fields",
    ),
    (
        "CAL015",
        "Index/bounds",
        "Use a nonnegative index/count within the reported bounds",
    ),
    (
        "CAL016",
        "Data parsing",
        "Check the reported JSON, regex, encoding or ISO format",
    ),
    (
        "CAL017",
        "Contract validation",
        "Inspect bounded issues and satisfy the declared contract",
    ),
];
