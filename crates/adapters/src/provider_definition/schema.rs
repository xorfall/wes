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
    recording: Option<RecordingDoc>,
    byte_input: Option<ByteInputDoc>,
    #[serde(default, deserialize_with = "exclusive_declaration")]
    exclusive: Option<Vec<ExclusiveDoc>>,
    #[serde(default = "empty_object")]
    fixed_arguments: Json,
    source_profile: Option<super::source_profile::Declaration>,
    finite_profile: Option<super::finite_profile::Declaration>,
}
fn exclusive_declaration<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> Result<Option<Vec<ExclusiveDoc>>, D::Error> {
    Vec::<ExclusiveDoc>::deserialize(deserializer).map(Some)
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ExclusiveDoc {
    namespace: String,
    fields: Vec<Vec<String>>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ByteInputDoc {
    parameter: String,
    max_bytes: u64,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RecordingDoc {
    #[serde(rename = "type")]
    contract: String,
    byte_field: Vec<String>,
    max_bytes: u64,
    max_duration_ms: u64,
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
    byte_field: Option<Vec<String>>,
}
#[derive(Clone, Copy, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub(super) enum OutputFormat {
    Json,
    Text,
    Bytes,
    Process,
    RecordBytes,
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
        let recording = op.recording.map(|recording| {
            if session.is_none() || !op.streaming {
                return Err(DefinitionError::new("byte recording requires a streaming session operation"));
            }
            if recording.max_bytes == 0 || recording.max_bytes > wes_budgets::get("storage.acquisition.bytes")
                || recording.max_duration_ms == 0 || recording.max_duration_ms > 86_400_000
            { return Err(DefinitionError::new("recording requires a bounded byte ceiling and a lifetime of 1–86400000 milliseconds")); }
            let contract = registry.resolve(&recording.contract)
                .map_err(|_| DefinitionError::new("unknown recording contract"))?;
            if !materialized(&contract) { return Err(DefinitionError::new("recording shell must be materialized")); }
            super::record_bytes::validate_leaf(&contract, &recording.byte_field)?;
            Ok(Recording { contract, fields: recording.byte_field, maximum:recording.max_bytes,
                duration:std::time::Duration::from_millis(recording.max_duration_ms) })
        }).transpose()?;
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
            OutputFormat::Json | OutputFormat::RecordBytes => Some(
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
        if matches!(op.result.format, OutputFormat::RecordBytes) {
            if session.is_some() || op.streaming {
                return Err(DefinitionError::new(
                    "record-bytes requires a finite process operation",
                ));
            }
            super::record_bytes::validate_leaf(
                contract.as_ref().expect("record-bytes contract"),
                op.result
                    .byte_field
                    .as_deref()
                    .ok_or(DefinitionError::new("record-bytes requires byte_field"))?,
            )?;
        } else if op.result.byte_field.is_some() {
            return Err(DefinitionError::new(
                "byte_field is only valid for record-bytes output",
            ));
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
            if !op
                .fixed_arguments
                .as_object()
                .is_some_and(|fields| fields.contains_key(&parameter.name))
            {
                cap.parameters.push(
                    Parameter::new(&parameter.name, contract.shape(), parameter.required)
                        .constrained_by(&contract),
                );
            }
            params.push((parameter.name, contract, parameter.required));
        }
        let fixed_arguments = super::fixed_arguments::compile(&op.fixed_arguments, &params)?;
        let finite_profile = op
            .finite_profile
            .map(|profile| {
                super::finite_profile::compile(profile, &params, &fixed_arguments, op.streaming)
            })
            .transpose()?;
        if let Some(profile) = &finite_profile
            && operations.iter().any(|operation: &Operation| {
                operation.finite_profile.as_ref().is_some_and(|existing| {
                    existing.name() == profile.name() && existing.kind() == profile.kind()
                })
            })
        {
            return Err(DefinitionError::new(
                "duplicate finite profile name and kind in one provider definition",
            ));
        }
        let source_profile = op
            .source_profile
            .map(|profile| {
                super::source_profile::compile(
                    profile,
                    &params,
                    &fixed_arguments,
                    control.as_ref(),
                    op.streaming,
                )
            })
            .transpose()?;
        if let Some(profile) = &source_profile
            && operations.iter().any(|operation: &Operation| {
                operation
                    .source_profile
                    .as_ref()
                    .is_some_and(|existing| existing.name() == profile.name())
            })
        {
            return Err(DefinitionError::new(
                "duplicate source profile name in one provider definition",
            ));
        }
        let byte_input = op.byte_input.map(|input| {
            if session.is_some() || op.streaming || input.max_bytes == 0 || input.max_bytes > wes_budgets::get("storage.acquisition.bytes") {
                return Err(DefinitionError::new("byte_input requires a finite process and a bounded byte ceiling"));
            }
            let (_, contract, required) = params.iter().find(|(name,_,_)|name==&input.parameter)
                .ok_or(DefinitionError::new("byte_input names an undeclared parameter"))?;
            let limits = contract.constraints();
            if !*required || !matches!(contract.kind(), wes_core::contracts::ContractKind::Scalar(Primitive::Bytes))
                || !limits.enumeration.is_empty() || !limits.patterns.is_empty() || limits.min.is_some() || limits.max.is_some()
                || limits.min_length.is_some() || limits.max_length.is_some() || limits.min_items.is_some() || limits.max_items.is_some() {
                return Err(DefinitionError::new("byte_input requires one required unconstrained Bytes parameter"));
            }
            if op.argv.iter().any(|a|matches!(a,Argument::Parameter(fields) if fields.parameter==input.parameter)) {
                return Err(DefinitionError::new("byte_input cannot also be encoded in argv"));
            }
            if fixed_arguments.contains_key(&input.parameter) {
                return Err(DefinitionError::new("byte_input requires a captured native source, not a configured literal"));
            }
            Ok(wes_engine::byte_input::ByteInputSpec {parameter:input.parameter, maximum:input.max_bytes})
        }).transpose()?;
        let exclusive = op
            .exclusive
            .map(|resources| {
                use wes_core::contracts::ContractKind;
                use wes_engine::streams::{
                    ExclusiveResource, MAX_EXCLUSIVE_RESOURCES, MAX_RESOURCE_PARTS,
                };
                if session.is_none()
                    || !op.streaming
                    || resources.is_empty()
                    || resources.len() > MAX_EXCLUSIVE_RESOURCES
                {
                    return Err(DefinitionError::new(
                        "exclusive resources require a streaming local session and 1–4 keys",
                    ));
                }
                let mut namespaces = std::collections::BTreeSet::new();
                let mut compiled = Vec::new();
                for resource in resources {
                    if !ExclusiveResource::valid_namespace(&resource.namespace)
                        || !namespaces.insert(resource.namespace.clone())
                        || resource.fields.is_empty()
                        || resource.fields.len() > MAX_RESOURCE_PARTS
                        || resource
                            .fields
                            .iter()
                            .collect::<std::collections::BTreeSet<_>>()
                            .len()
                            != resource.fields.len()
                    {
                        return Err(DefinitionError::new(
                            "invalid or duplicate exclusive resource declaration",
                        ));
                    }
                    for path in &resource.fields {
                        if path.is_empty()
                            || path.len() > 16
                            || path.iter().any(|s| {
                                s.is_empty() || s.len() > 1024 || s.chars().any(char::is_control)
                            })
                            || path.iter().map(String::len).sum::<usize>() > 4096
                        {
                            return Err(DefinitionError::new(
                                "exclusive resource field path exceeds its limits",
                            ));
                        }
                        let (_, root, required) = params
                            .iter()
                            .find(|(name, _, _)| name == &path[0])
                            .ok_or(DefinitionError::new(
                                "exclusive resource names an undeclared argument",
                            ))?;
                        if !*required {
                            return Err(DefinitionError::new(
                                "exclusive resource arguments must be required",
                            ));
                        }
                        let mut leaf = root.as_ref();
                        for name in &path[1..] {
                            let ContractKind::Record(fields) = leaf.kind() else {
                                return Err(DefinitionError::new(
                                    "exclusive resource paths must traverse required records",
                                ));
                            };
                            leaf = fields
                                .get(name)
                                .filter(|field| !field.optional)
                                .ok_or(DefinitionError::new(
                                    "exclusive resource fields must exist and be required",
                                ))?
                                .contract
                                .as_ref();
                        }
                        if !matches!(leaf.kind(), ContractKind::Scalar(Primitive::Text)) {
                            return Err(DefinitionError::new(
                                "exclusive resource keys require Text leaves",
                            ));
                        }
                    }
                    compiled.push(Exclusive {
                        namespace: resource.namespace,
                        fields: resource.fields,
                    });
                }
                Ok(compiled)
            })
            .transpose()?
            .unwrap_or_default();
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
                if byte_input
                    .as_ref()
                    .is_some_and(|input| &input.parameter == n)
                {
                    return false;
                }
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
            byte_field: op.result.byte_field,
            success_codes: op.success_codes,
            streaming: op.streaming,
            control,
            recording,
            byte_input,
            exclusive,
            fixed_arguments,
            source_profile,
            finite_profile,
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
