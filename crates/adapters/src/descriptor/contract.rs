//! Versioned contracts use the existing type-package language; server metadata has no authority.
use super::*;
use wes_engine::imports::{ImportWarning, ImportWarningKind};
mod auth;
mod information;
use crate::http::explicit::{Argument, Operation, Response};
use serde::Deserialize;
use std::collections::BTreeMap;
use wes_core::contracts::ContractRegistry;
#[cfg(test)]
mod tests;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Document {
    version: u32,
    provider: String,
    types: serde_json::Value,
    operations: Vec<OperationDoc>,
    // Opaque review metadata is bounded by the outer JSON reader and is never executed.
    #[serde(default)]
    source: Option<serde_json::Value>,
    #[serde(default)]
    servers: Vec<serde_json::Value>,
    #[serde(default)]
    diagnostics: Vec<String>,
    #[serde(default)]
    notes: Vec<super::notes::Note>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct OperationDoc {
    path: Vec<String>,
    #[serde(default)]
    summary: String,
    #[serde(default)]
    description: String,
    #[serde(default)]
    response_descriptions: BTreeMap<String, String>,
    method: String,
    #[serde(default)]
    safety: Option<DeclaredSafety>,
    route: String,
    #[serde(default)]
    stream: bool,
    // Explicit wire configuration: [] attaches no credentials. Documentation may
    // describe auth as unknown in inert provenance; [] alone does not prove public access.
    auth: Vec<serde_json::Value>,
    #[serde(default, rename = "authOptions")]
    auth_options: Option<Vec<serde_json::Value>>,
    parameters: Vec<ParameterDoc>,
    responses: BTreeMap<String, Option<String>>,
    #[serde(default)]
    evidence: Option<String>,
}
#[derive(Clone, Copy, Deserialize)]
#[serde(rename_all = "lowercase")]
enum DeclaredSafety {
    Safe,
    Unsafe,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct ParameterDoc {
    #[serde(default)]
    description: String,
    name: String,
    wire: String,
    location: String,
    #[serde(rename = "type")]
    kind: String,
    required: bool,
    // scalar, repeat (form+explode array), deepObject (scalar values).
    encoding: String,
}

pub(super) fn read(
    bytes: &[u8],
    fields: &Fields,
    name: Option<&str>,
    credentials: Arc<dyn Credentials>,
    config: HttpConfig,
    mode: wes_engine::imports::ImportMode,
    endpoint: Option<&str>,
    auth_selection: &BTreeMap<String, Vec<String>>,
) -> Result<Reading> {
    const OPERATION_FIELDS: &[&str] = &[
        "path",
        "summary",
        "description",
        "responseDescriptions",
        "method",
        "safety",
        "route",
        "stream",
        "auth",
        "authOptions",
        "parameters",
        "responses",
        "evidence",
    ];
    if let Some(Data::List(operations)) = fields.get("operations") {
        for operation in operations {
            if let Data::Record(operation) = operation {
                if operation
                    .keys()
                    .any(|field| !OPERATION_FIELDS.contains(&field.as_str()))
                {
                    return Err(DescriptorError(
                        "unknown operation field; allowed fields: path, summary, description, responseDescriptions, method, safety, route, stream, auth, authOptions, parameters, responses, evidence",
                    ));
                }
                if operation.get("safety").is_some_and(|value| !matches!(value, Data::Text(text) if text.as_ref() == "safe" || text.as_ref() == "unsafe")) {
                    return Err(DescriptorError("operation safety must be safe or unsafe; SAFE permits automatic repetition, not merely idempotent requests"));
                }
            }
        }
    }
    let doc: Document = serde_json::from_slice(bytes).map_err(|_| {
        DescriptorError("expected current descriptor fields (version, provider, types, operations)")
    })?;
    if doc.version != super::VERSION {
        return Err(DescriptorError("unsupported descriptor version"));
    }
    super::notes::validate(
        &serde_json::to_value(&doc.notes).map_err(|_| DescriptorError("invalid notes"))?,
    )
    .map_err(DescriptorError)?;
    let endpoint = endpoint.ok_or(DescriptorError("descriptor requires an explicit endpoint or environment bind.endpoint; documented servers are not execution destinations"))?;
    if doc.provider.trim().is_empty() || doc.operations.is_empty() || doc.operations.len() > 1000 {
        return Err(DescriptorError(
            "descriptor requires a provider and 1 to 1000 operations",
        ));
    }
    if !doc.types.is_object() {
        return Err(DescriptorError("types must be contract definitions"));
    }
    let mut registry = ContractRegistry::new();
    registry
        .load(&serde_json::json!({"version":1,"types":doc.types}).to_string())
        .map_err(|_| DescriptorError("invalid type contracts"))?;
    let mut builder = HttpProvider::new(endpoint, credentials.clone(), config)
        .map_err(|_| DescriptorError("invalid environment endpoint"))?;
    let raw_ops = match required(fields, "operations")? {
        Data::List(v) => v,
        _ => return Err(DescriptorError("operations must be an array")),
    };
    let mut authentication = Vec::new();
    let mut safety_information = Vec::new();
    let mut selected_operations = BTreeSet::new();
    let mut undocumented_authentication = 0;
    // Evidence is advisory only: it cannot choose credentials or authorize execution.
    let undocumented_auth: BTreeSet<&str> = doc
        .source
        .as_ref()
        .and_then(|source| source.pointer("/provenance/entries"))
        .and_then(serde_json::Value::as_array)
        .into_iter()
        .flatten()
        .filter(|entry| entry["basis"] == "unknown")
        .filter_map(|entry| entry["target"].as_str())
        .collect();
    for (index, (op, raw)) in doc.operations.into_iter().zip(raw_ops).enumerate() {
        let _ = (
            &op.evidence,
            &op.auth,
            &op.auth_options,
            &op.description,
            &op.response_descriptions,
        );
        let operation_key = op.path.join(" ");
        selected_operations.insert(operation_key.clone());
        let selection = auth::select(raw, &op.path, auth_selection.get(&operation_key))?;
        if op.auth.is_empty()
            && op.auth_options.is_none()
            && undocumented_auth.contains(format!("#/operations/{index}/auth").as_str())
        {
            undocumented_authentication += 1;
        }
        authentication.push(selection.information);
        let mut responses = BTreeMap::new();
        for (status, kind) in op.responses {
            let code: u16 = status.parse().map_err(|_| {
                DescriptorError("response status must be an exact supported HTTP code")
            })?;
            if !(if !op.stream { 100..600 } else { 200..300 }).contains(&code)
                || status != code.to_string()
            {
                return Err(DescriptorError(
                    "response status must be an exact supported HTTP code",
                ));
            }
            let contract = kind
                .map(|kind| {
                    registry
                        .resolve(&kind)
                        .map_err(|_| DescriptorError("unknown response contract"))
                })
                .transpose()?;
            if (matches!(code, 204 | 205 | 304) || op.method == "HEAD") && contract.is_some() {
                return Err(DescriptorError(
                    "bodyless status/method cannot declare a JSON response",
                ));
            }
            responses.insert(code, Response { contract });
        }
        if responses.is_empty() {
            return Err(DescriptorError("operation needs documented responses"));
        }
        if op.stream
            && (op.method == "HEAD"
                || !responses.values().any(|r| r.contract.is_some())
                || responses.iter().any(|(status, r)| {
                    if *status == 204 {
                        r.contract.is_some()
                    } else {
                        r.contract.is_none()
                    }
                }))
        {
            return Err(DescriptorError(
                "stream needs event contracts and may only use 204 as a bodyless response",
            ));
        }
        let shapes = responses
            .values()
            .filter(|r| !op.stream || r.contract.is_some())
            .map(|r| r.shape())
            .collect::<Vec<_>>();
        let result = if !op.stream {
            crate::http::response::shape()
        } else if shapes.iter().all(|s| s == &shapes[0]) {
            shapes[0].clone()
        } else {
            Shape::Unknown
        };
        let safety = match op.safety {
            Some(DeclaredSafety::Safe) => Safety::Safe,
            Some(DeclaredSafety::Unsafe) => Safety::Unsafe,
            None if matches!(op.method.as_str(), "GET" | "HEAD" | "OPTIONS") => Safety::Safe,
            None => Safety::Unsafe,
        };
        safety_information.push(Data::Record(IndexMap::from([
            ("operation".into(), Data::Text(operation_key.into())),
            (
                "classification".into(),
                Data::Text(
                    if safety == Safety::Safe {
                        "SAFE"
                    } else {
                        "UNSAFE"
                    }
                    .into(),
                ),
            ),
            (
                "basis".into(),
                Data::Text(
                    if op.safety.is_some() {
                        "explicit local contract"
                    } else {
                        "HTTP method default"
                    }
                    .into(),
                ),
            ),
        ])));
        let mut cap = Capability::new(op.path, result, safety);
        cap.streaming = op.stream;
        cap.summary = op.summary;
        let mut arguments = Vec::new();
        for param in op.parameters {
            let _ = &param.description;
            let contract = registry
                .resolve(&param.kind)
                .map_err(|_| DescriptorError("unknown parameter contract"))?;
            cap.parameters.push(
                // Retain the resolved full enum domain beside bounded constraint hints.
                Parameter::new(&param.name, contract.shape(), param.required)
                    .constrained_by(&contract),
            );
            arguments.push(Argument {
                name: param.name,
                wire: param.wire,
                location: param.location,
                encoding: param.encoding,
                required: param.required,
                contract,
            });
        }
        // Validate every alternative, including unselected mappings and parameter collisions.
        for candidate in selection.alternatives {
            Operation::new(
                op.method.clone(),
                op.route.clone(),
                arguments.clone(),
                responses.clone(),
                candidate,
            )
            .map_err(|_| {
                DescriptorError("invalid authentication alternative or HTTP argument collision")
            })?;
        }
        let operation = Operation::new(op.method, op.route, arguments, responses, selection.chosen)
            .map_err(|_| DescriptorError("invalid explicit HTTP mapping or authentication"))?;
        builder
            .offer_explicit(cap, operation)
            .map_err(|_| DescriptorError("invalid or duplicate explicit HTTP operation"))?;
    }
    if auth_selection
        .keys()
        .any(|path| !selected_operations.contains(path))
    {
        return Err(DescriptorError("auth selection names an unknown operation"));
    }
    let mut warnings: Vec<ImportWarning> = builder
        .hazards()
        .into_iter()
        .map(|message| ImportWarning::new(ImportWarningKind::QueryCredential, message))
        .collect();
    for info in &authentication {
        if info["state"] == "selection-required" {
            warnings.push(ImportWarning::new(ImportWarningKind::AuthenticationChoice, format!("Authentication choice required for {}; select its schemes in environment bind.auth",info["operation"].as_array().unwrap().iter().filter_map(|v|v.as_str()).collect::<Vec<_>>().join(" "))));
        }
    }
    for _ in 0..undocumented_authentication {
        warnings.push(ImportWarning::new(
            ImportWarningKind::AuthenticationUndocumented,
            "Authentication is not documented in the source; no credentials are attached.",
        ));
    }
    let authentication = crate::codec::decode_json_preserving(
        &serde_json::to_vec(&authentication)
            .map_err(|_| DescriptorError("invalid auth metadata"))?,
        Limits {
            bytes: 1024 * 1024,
            nodes: 20_000,
        },
    )
    .map_err(|_| DescriptorError("auth metadata exceeds budget"))?;
    warnings.extend(doc.diagnostics.into_iter().map(ImportWarning::from));
    let information = Data::Record(IndexMap::from([
        ("authentication".into(), authentication),
        ("safety".into(), Data::List(safety_information)),
        (
            "notes".into(),
            fields.get("notes").cloned().unwrap_or(Data::List(vec![])),
        ),
        (
            "advisories".into(),
            fields
                .get("diagnostics")
                .cloned()
                .unwrap_or(Data::List(vec![])),
        ),
        (
            "evidence".into(),
            fields
                .get("source")
                .cloned()
                .unwrap_or(Data::Record(IndexMap::new())),
        ),
    ]));
    let _ = (doc.source, doc.servers);
    let (description, invoker) = builder
        .build(name.unwrap_or(&doc.provider))
        .map_err(|_| DescriptorError("invalid provider metadata"))?;
    if mode == wes_engine::imports::ImportMode::Live {
        for secret in description.secrets() {
            if !matches!(credentials.lookup(secret), Ok(Some(_))) {
                warnings.push(ImportWarning::new(
                    ImportWarningKind::CredentialUnavailable,
                    format!("credential '{secret}' is not available"),
                ));
            }
        }
    }
    Ok(Reading {
        description: description.with_typed_information(information::typed(information)?),
        invoker,
        warnings,
    })
}
