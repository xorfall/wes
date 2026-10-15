use super::*;
use serde::Deserialize;
use serde_json::Value as Json;
use wes_core::{
    Primitive, Shape,
    capability::{Capability, Parameter, Safety},
    contracts::{ContractRegistry, PackageNode as Node, PackageScalarKind as Scalar},
};

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Document {
    version: u32,
    provider: String,
    #[serde(default = "empty_object")]
    types: Json,
    backend: Backend,
    operations: Vec<OperationDoc>,
    read_access: Option<wes_engine::provider_reads::ReadDomain>,
}
#[derive(Deserialize)]
#[serde(tag = "kind", rename_all = "lowercase", deny_unknown_fields)]
enum Backend {
    Process {
        bin: String,
    },
    Session {
        bin: String,
        #[serde(default)]
        credentials: Vec<String>,
    },
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct OperationDoc {
    path: Vec<String>,
    #[serde(default)]
    summary: String,
    #[serde(default)]
    parameters: Vec<ParameterDoc>,
    argv: Vec<Argument>,
    result: ResultDoc,
    #[serde(default = "success_codes")]
    success_codes: Vec<i64>,
    #[serde(default)]
    streaming: bool,
    control: Option<ControlDoc>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ControlDoc {
    input: String,
    output: String,
    #[serde(default)]
    output_policy: ControlPolicy,
}
#[derive(Default, Deserialize)]
#[serde(rename_all = "kebab-case")]
enum ControlPolicy {
    #[default]
    Public,
    Private,
    ConfidentialTemporary,
    Confidential,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ParameterDoc {
    name: String,
    #[serde(rename = "type")]
    contract: String,
    #[serde(default = "required")]
    required: bool,
}
#[derive(Clone, Deserialize)]
#[serde(untagged)]
pub(super) enum Argument {
    Literal(String),
    Parameter(ArgumentFields),
}
#[derive(Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct ArgumentFields {
    pub parameter: String,
    #[serde(default)]
    pub prefix: Vec<String>,
    #[serde(default)]
    pub encoding: Encoding,
}
#[derive(Clone, Copy, Default, Deserialize)]
#[serde(rename_all = "lowercase")]
pub(super) enum Encoding {
    #[default]
    Scalar,
    Spread,
    Flag,
    Json,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ResultDoc {
    format: OutputFormat,
    #[serde(rename = "type")]
    contract: Option<String>,
}
#[derive(Clone, Copy, Deserialize)]
#[serde(rename_all = "lowercase")]
pub(super) enum OutputFormat {
    Json,
    Text,
    Bytes,
    Process,
}
fn empty_object() -> Json {
    serde_json::json!({})
}
fn success_codes() -> Vec<i64> {
    vec![0]
}
fn required() -> bool {
    true
}

/// Normalize strict YAML to JSON while preserving exact numeric tokens. No YAML aliases,
/// tags, duplicate keys or extra documents are accepted by the shared bounded reader.
fn json(node: &Node) -> Result<Json, DefinitionError> {
    Ok(match node {
        Node::Scalar(Scalar::Text, s) => Json::String(s.clone()),
        Node::Scalar(Scalar::Bool, s) => Json::Bool(s.eq_ignore_ascii_case("true")),
        Node::Scalar(Scalar::Int | Scalar::Decimal, s) => serde_json::from_str(s)
            .map_err(|_| DefinitionError::new("numeric fields require JSON decimal spelling"))?,
        Node::Sequence(values) => Json::Array(values.iter().map(json).collect::<Result<_, _>>()?),
        Node::Mapping(values) => Json::Object(
            values
                .iter()
                .map(|(k, v)| Ok((k.clone(), json(v)?)))
                .collect::<Result<_, DefinitionError>>()?,
        ),
    })
}
fn identifier(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 128
        && name.bytes().enumerate().all(|(i, c)| {
            c.is_ascii_alphabetic() || c == b'_' || (i > 0 && (c.is_ascii_digit() || c == b'-'))
        })
}
fn scalar(shape: &Shape) -> bool {
    matches!(shape, Shape::Primitive(p) if *p != Primitive::Bytes)
}
fn materialized(contract: &Contract) -> bool {
    use wes_core::contracts::ContractKind as Kind;
    let mut pending = vec![contract];
    let mut remaining = 100_000;
    while let Some(contract) = pending.pop() {
        if remaining == 0 {
            return false;
        }
        remaining -= 1;
        match contract.kind() {
            Kind::Scalar(_) | Kind::Unknown => {}
            Kind::List(c) | Kind::Option(c) => pending.push(c),
            Kind::Record(fields) => pending.extend(fields.values().map(|f| f.contract.as_ref())),
            Kind::Union(a, b) | Kind::Map(a, b) => {
                pending.push(a);
                pending.push(b);
            }
            Kind::Iter(_) | Kind::Dataset(_) => return false,
        }
    }
    true
}
pub(super) fn compile(source: &str, alias: Option<&str>) -> Result<Definition, DefinitionError> {
    if source.len() > wes_core::environments::MAX_SOURCE_BYTES {
        return Err(DefinitionError::new(
            "definition exceeds its source byte limit",
        ));
    }
    let node = wes_core::contracts::read_strict_package(source)
        .map_err(|_| DefinitionError::new("expected one bounded strict YAML document"))?;
    wes_core::package_schema::declarations()
        .validate("provider.document", &node)
        .map_err(|issue| DefinitionError::new(issue.message))?;
    let document = json(&node)?;
    let doc: Document = serde_json::from_value(document)
        .map_err(|_| DefinitionError::new("unknown or malformed provider fields"))?;
    if doc.version != 1
        || !identifier(&doc.provider)
        || !identifier(alias.unwrap_or(&doc.provider))
        || doc.operations.is_empty()
        || doc.operations.len() > 256
        || !doc.types.is_object()
    {
        return Err(DefinitionError::new(
            "expected version 1, a provider name, types and 1–256 operations",
        ));
    }
    let (bin, session) = match doc.backend {
        Backend::Process { bin } => (bin, None),
        Backend::Session { bin, credentials } => {
            if credentials.len() > 16
                || credentials
                    .iter()
                    .any(|s| !identifier(s) || s.contains('-'))
                || credentials
                    .iter()
                    .collect::<std::collections::BTreeSet<_>>()
                    .len()
                    != credentials.len()
            {
                return Err(DefinitionError::new(
                    "session credential slots must be 0–16 distinct identifiers",
                ));
            }
            (bin, Some(credentials))
        }
    };
    if bin.is_empty() || bin.len() > 4096 || bin.chars().any(char::is_control) {
        return Err(DefinitionError::new(
            "process backend requires an explicit executable path",
        ));
    }
    let mut registry = ContractRegistry::new();
    registry
        .load(&serde_json::json!({"version":1,"types":doc.types}).to_string())
        .map_err(|_| DefinitionError::new("invalid type contracts"))?;
    let mut capabilities = Vec::new();
    let mut operations = Vec::new();
    for op in doc.operations {
        if session.is_none() && (op.streaming || op.control.is_some()) {
            return Err(DefinitionError::new(
                "streaming and live control require a session backend",
            ));
        }
        if session.is_some()
            && (!matches!(op.result.format, OutputFormat::Json)
                || op.argv.iter().any(|a| !matches!(a, Argument::Literal(_)))
                || op.success_codes != [0])
        {
            return Err(DefinitionError::new(
                "session operations require JSON results, literal argv and success code 0",
            ));
        }
        let control = op
            .control
            .map(|control| {
                if !op.streaming {
                    return Err(DefinitionError::new(
                        "live control requires a streaming operation",
                    ));
                }
                let input = registry
                    .resolve(&control.input)
                    .map_err(|_| DefinitionError::new("unknown control input contract"))?;
                let output = registry
                    .resolve(&control.output)
                    .map_err(|_| DefinitionError::new("unknown control output contract"))?;
                if !materialized(&input) || !materialized(&output) {
                    return Err(DefinitionError::new(
                        "control contracts must be materialized",
                    ));
                }
                let policy = match control.output_policy {
                    ControlPolicy::Public => wes_core::flow::OutputPolicy::Public,
                    ControlPolicy::Private => wes_core::flow::OutputPolicy::Private,
                    ControlPolicy::ConfidentialTemporary => {
                        wes_core::flow::OutputPolicy::ConfidentialTemporary
                    }
                    ControlPolicy::Confidential => wes_core::flow::OutputPolicy::Confidential,
                };
                Ok(Control {
                    input,
                    output,
                    policy,
                })
            })
            .transpose()?;
        if op.path.is_empty()
            || op.path.len() > 16
            || op.path.iter().any(|p| !identifier(p))
            || op.summary.len() > 4096
            || op.parameters.len() > 256
            || op.argv.len() >= crate::process::MAX_ARGUMENTS
            || op.success_codes.is_empty()
            || op.success_codes.len() > 256
            || op
                .success_codes
                .iter()
                .any(|code| !(0..=u32::MAX as i64).contains(code))
        {
            return Err(DefinitionError::new(
                "operation metadata exceeds its limits or contains invalid names",
            ));
        }
        let contract = match op.result.format {
            OutputFormat::Process if op.result.contract.is_none() => None,
            OutputFormat::Process => {
                return Err(DefinitionError::new(
                    "process format has the fixed ProcessOutput contract",
                ));
            }
            OutputFormat::Json => Some(
                registry
                    .resolve(
                        op.result
                            .contract
                            .as_deref()
                            .ok_or(DefinitionError::new("JSON output requires a type"))?,
                    )
                    .map_err(|_| DefinitionError::new("unknown result contract"))?,
            ),
            OutputFormat::Text | OutputFormat::Bytes => Some(
                registry
                    .resolve(op.result.contract.as_deref().unwrap_or(
                        if matches!(op.result.format, OutputFormat::Text) {
                            "Text"
                        } else {
                            "Bytes"
                        },
                    ))
                    .map_err(|_| DefinitionError::new("unknown result contract"))?,
            ),
        };
        if let Some(contract) = &contract {
            let shape = contract.shape();
            if !materialized(contract)
                || (matches!(op.result.format, OutputFormat::Text)
                    && shape != Shape::Primitive(Primitive::Text))
                || (matches!(op.result.format, OutputFormat::Bytes)
                    && shape != Shape::Primitive(Primitive::Bytes))
            {
                return Err(DefinitionError::new(
                    "output format and materialized contract must agree",
                ));
            }
        }
        let result_shape = contract
            .as_ref()
            .map_or_else(crate::process::output_shape, |c| c.shape());
        let mut cap = Capability::new(op.path.clone(), result_shape, Safety::Unsafe);
        cap.summary = op.summary;
        cap.streaming = op.streaming;
        let mut params = Vec::new();
        for parameter in op.parameters {
            if !identifier(&parameter.name) || params.iter().any(|(n, _, _)| n == &parameter.name) {
                return Err(DefinitionError::new("invalid or duplicate parameter name"));
            }
            let contract = registry
                .resolve(&parameter.contract)
                .map_err(|_| DefinitionError::new("unknown parameter contract"))?;
            if !materialized(&contract) {
                return Err(DefinitionError::new(
                    "parameters require materialized contracts",
                ));
            }
            cap.parameters.push(
                Parameter::new(&parameter.name, contract.shape(), parameter.required)
                    .constrained_by(&contract),
            );
            params.push((parameter.name, contract, parameter.required));
        }
        for arg in &op.argv {
            match arg {
                Argument::Literal(s) if s.contains('\0') => {
                    return Err(DefinitionError::new("argv literals cannot contain NUL"));
                }
                Argument::Literal(_) => {}
                Argument::Parameter(ArgumentFields {
                    parameter,
                    prefix,
                    encoding,
                }) => {
                    let (_, contract, _) = params.iter().find(|(n, _, _)| n == parameter).ok_or(
                        DefinitionError::new("argv mapping names an undeclared parameter"),
                    )?;
                    let shape = contract.shape();
                    let shape = if let Shape::Option(inner) = &shape {
                        inner.as_ref()
                    } else {
                        &shape
                    };
                    let compatible = match encoding {
                        Encoding::Scalar => scalar(shape),
                        Encoding::Spread => matches!(shape,Shape::List(inner) if scalar(inner)),
                        Encoding::Flag => {
                            *shape == Shape::Primitive(Primitive::Bool) && !prefix.is_empty()
                        }
                        Encoding::Json => materialized(contract),
                    };
                    if !compatible
                        || prefix.len() >= crate::process::MAX_ARGUMENTS
                        || prefix.iter().any(|p| p.contains('\0'))
                    {
                        return Err(DefinitionError::new(
                            "argv encoding, parameter type or prefix is invalid",
                        ));
                    }
                }
            }
        }
        if session.is_none()
            && params.iter().any(|(n, _, _)| {
                !op.argv.iter().any(
                |a| matches!(a,Argument::Parameter(ArgumentFields{parameter,..}) if parameter == n),
            )
            })
        {
            return Err(DefinitionError::new(
                "every parameter must have an explicit argv mapping",
            ));
        }
        capabilities.push(cap);
        operations.push(Operation {
            path: op.path,
            parameters: params,
            argv: op.argv,
            result: contract,
            format: op.result.format,
            success_codes: op.success_codes,
            streaming: op.streaming,
            control,
        });
    }
    let description = ProviderDescription::new(
        alias.unwrap_or(&doc.provider),
        capabilities,
        session.clone().unwrap_or_default(),
    )
    .map_err(|_| DefinitionError::new("invalid or duplicate operation paths"))?;
    if let Some(domain) = &doc.read_access {
        domain
            .validate(&description)
            .map_err(DefinitionError::new)?;
    }
    Ok(Definition {
        description,
        executable: bin,
        operations,
        session,
        read_domain: doc.read_access,
    })
}
