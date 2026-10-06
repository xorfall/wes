//! Editor documents project current captured definitions, never a stale scratch-file copy.
use super::*;
use serde_json::{Value as Json, json};
use std::collections::BTreeSet;
use wes_core::environments::{MAX_SOURCE_BYTES, SourceKey};
use wes_engine::{
    environments::{EnvironmentDocument, EnvironmentRecord},
    imports::CapturedImport,
};

pub(super) fn default_document(
    loader: &LocalEnvironments,
    name: &str,
    imports: &[CapturedImport],
) -> Result<LoadedDefinitions, EnvironmentError> {
    let mut document = json!({"version":1,"package":"default","targets":{"local":{"kind":"local"}},"environments":{name:{
        "id":format!("builtin.{name}"), "owner":"default", "protected":true, "imports":{
            (crate::process::SHELL_NAME):{"source":{"kind":"builtin","name":"sh"},"bind":{"target":"local"}},
            "http":{"source":{"kind":"builtin","name":"http"},"bind":{"target":"local","transport":"internal"}},
            "docker":{"source":{"kind":"builtin","name":"docker"},"bind":"auto"}
        }
    }}});
    let mut sources = CapturedSources::default();
    for builtin in ["sh", "http", "docker"] {
        sources.insert(
            SourceKey::new("builtin", builtin)?,
            CapturedSource::new("builtin/v1", builtin)?,
        )?;
    }
    for captured in imports {
        insert_import(loader, &mut document, &mut sources, name, captured)?;
    }
    finish(document, &sources)
}

fn insert_import(
    loader: &LocalEnvironments,
    document: &mut Json,
    sources: &mut CapturedSources,
    environment: &str,
    captured: &CapturedImport,
) -> Result<(), EnvironmentError> {
    let request = captured.request();
    let recipe = captured.recipe();
    let text = |key: &str| {
        request.arguments().get(key).and_then(|v| {
            if let Data::Text(s) = v.data() {
                Some(s.as_ref())
            } else {
                None
            }
        })
    };
    let alias = captured.product().description().name();
    let (key, source, mut declaration) = match request.kind() {
        "docker" => {
            let socket =
                text("socket").ok_or_else(|| invalid("Captured Docker import lacks socket"))?;
            (
                SourceKey::new("builtin", "docker")?,
                CapturedSource::new("builtin/v1", "docker")?,
                json!({"source":{"kind":"builtin","name":"docker"},"bind":{"target":"local","endpoint":crate::docker::LocalEndpoint::from_socket(socket).map_err(invalid)?.declaration()}}),
            )
        }
        "process" => (
            SourceKey::new("process", recipe.source())?,
            CapturedSource::new(recipe.format(), recipe.source())?,
            json!({"source":{"kind":"process","bin":recipe.source()},"bind":{"target":"local"}}),
        ),
        "spec" | "openapi" => {
            let location = if let Some(url) = text("url") {
                url.to_owned()
            } else {
                loader
                    .files
                    .resolve(
                        text("file").ok_or_else(|| invalid("Captured spec lacks file or URL"))?,
                    )
                    .map_err(|_| invalid("Captured spec path is unavailable"))?
                    .to_str()
                    .ok_or_else(|| invalid("Spec path is not UTF-8"))?
                    .to_owned()
            };
            let key = SourceKey::new(request.kind(), &location)?;
            let mut declaration = json!({"source":{"kind":request.kind(),key.source_field():location},"bind":{"target":"local"}});
            if let Some(endpoint) = text("endpoint") {
                declaration["bind"]["endpoint"] = json!(endpoint);
            }
            (
                key,
                CapturedSource::new(recipe.format(), recipe.source())?,
                declaration,
            )
        }
        _ => {
            return Err(invalid(
                "This importer cannot be represented in an environment document",
            ));
        }
    };
    for secret in captured.product().description().secrets() {
        document["environments"][environment]["secretSlots"][secret] = json!({"required": true});
        document["environments"][environment]["secretRefs"][secret] = json!(secret);
        declaration["bind"]["credentials"][secret] = json!({"secret": secret});
    }
    let local = reserve_target(document, "local", &json!({"kind":"local", "env":{}}));
    declaration["bind"]["target"] = json!(local);
    // A captured replacement supplies new evidence for its source identity.
    let mut replacement = CapturedSources::default();
    for (existing, value) in sources.iter().filter(|(existing, _)| **existing != key) {
        replacement.insert(existing.clone(), value.as_ref().clone())?;
    }
    replacement.insert(key, source)?;
    *sources = replacement;
    document["environments"][environment]["imports"][alias] = declaration;
    Ok(())
}

fn snapshot(
    current: Option<&EnvironmentRecord>,
    default: Option<LoadedDefinitions>,
) -> Result<LoadedDefinitions, EnvironmentError> {
    let mut document = match current {
        Some(record) => edit::canonical(&Package::parse(record.yaml())?),
        None => json!({"version":1,"targets":{},"environments":{}}),
    };
    let mut sources = current.map(|r| r.sources().clone()).unwrap_or_default();
    if let Some(default) = default {
        let mut incoming = edit::canonical(&Package::parse(&default.yaml)?);
        for (name, target) in incoming["targets"].as_object().expect("targets").clone() {
            let actual = reserve_target(&mut document, &name, &target);
            if actual != name {
                for definition in incoming["environments"]
                    .as_object_mut()
                    .expect("definitions")
                    .values_mut()
                {
                    for field in ["imports", "overrides"] {
                        let imports = if field == "imports" {
                            &mut definition[field]
                        } else {
                            &mut definition[field]["imports"]
                        };
                        for import in imports.as_object_mut().expect("imports").values_mut() {
                            if import["bind"]["target"] == name {
                                import["bind"]["target"] = json!(actual);
                            }
                        }
                    }
                    if let Some(targets) = definition["targets"].as_array_mut() {
                        for target in targets {
                            if *target == name {
                                *target = json!(actual);
                            }
                        }
                    }
                }
            }
        }
        for (name, definition) in incoming["environments"].as_object().expect("definitions") {
            if document["environments"].get(name).is_some() {
                return Err(invalid("Default definition conflicts with a package"));
            }
            document["environments"][name] = definition.clone();
        }
        for (key, value) in default.sources.iter() {
            if sources
                .get(key)
                .is_some_and(|old| old.as_ref() != value.as_ref())
            {
                return Err(invalid(
                    "Two captured providers use different versions of the same source; give them distinct source locations before editing",
                ));
            }
            if sources.get(key).is_none() {
                sources.insert(key.clone(), value.as_ref().clone())?;
            }
        }
    }
    finish(document, &sources)
}

pub(super) fn list(
    current: Option<&EnvironmentRecord>,
    default: Option<LoadedDefinitions>,
    token: &str,
) -> Result<Vec<EnvironmentDocument>, EnvironmentError> {
    let snapshot = snapshot(current, default)?;
    let package = Package::parse(&snapshot.yaml)?;
    let all = edit::canonical(&package);
    let owners: BTreeSet<_> = package
        .definitions()
        .values()
        .map(|d| d.owner.as_deref().unwrap_or("workspace"))
        .collect();
    owners
        .into_iter()
        .map(|owner| {
            let mut document = all.clone();
            document["package"] = json!(owner);
            document["environments"]
                .as_object_mut()
                .expect("definitions")
                .retain(|_, d| d["owner"].as_str().unwrap_or("workspace") == owner);
            let environments = document["environments"]
                .as_object()
                .expect("definitions")
                .keys()
                .cloned()
                .collect();
            let names: BTreeSet<_> = package
                .definitions()
                .values()
                .filter(|d| d.owner.as_deref().unwrap_or("workspace") == owner)
                .flat_map(|d| {
                    d.targets.iter().cloned().chain(
                        d.imports
                            .values()
                            .chain(d.overrides.values())
                            .filter(|i| {
                                i.binding_mode == wes_core::environments::BindingMode::Explicit
                            })
                            .map(|i| i.target.clone()),
                    )
                })
                .collect();
            document["targets"]
                .as_object_mut()
                .expect("targets")
                .retain(|name, _| names.contains(name));
            for d in document["environments"]
                .as_object_mut()
                .expect("definitions")
                .values_mut()
            {
                d.as_object_mut().expect("definition").remove("owner");
                d.as_object_mut().expect("definition").remove("drift");
            }
            compact(&mut document);
            let source = yaml(&document)?;
            Ok(EnvironmentDocument {
                name: owner.into(),
                environments,
                source,
                origin: format!("environment-editor/{token}/{owner}"),
            })
        })
        .collect()
}

pub(super) fn capture(
    loader: &LocalEnvironments,
    current: Option<&EnvironmentRecord>,
    default: Option<LoadedDefinitions>,
    source: &str,
    owner: &str,
    base: Option<&str>,
) -> Result<LoadedDefinitions, EnvironmentError> {
    let base = base.map(Path::new);
    if base.is_some_and(|path| !path.is_absolute()) {
        return Err(invalid(
            "Environment editor base must be an absolute directory",
        ));
    }
    let default = default.filter(|default| {
        Package::parse(&default.yaml)
            .ok()
            .and_then(|p| p.package_name().map(str::to_owned))
            .as_deref()
            == Some(owner)
    });
    let baseline = snapshot(current, default)?;
    let old = Package::parse(&baseline.yaml)?;
    if !old
        .definitions()
        .values()
        .any(|d| d.owner.as_deref().unwrap_or("workspace") == owner)
    {
        return Err(invalid(
            "Environment document no longer exists; reopen /edit env",
        ));
    }
    let candidate = Package::parse(source)?;
    if candidate.package_name().unwrap_or("workspace") != owner {
        return Err(invalid(
            "Keep this document's package name; use New to create another document",
        ));
    }
    if candidate
        .definitions()
        .values()
        .any(|d| d.owner.is_some() || d.drift)
    {
        return Err(invalid(
            "Environment owner and drift are maintained by the engine",
        ));
    }
    let mut sources = CapturedSources::default();
    for key in candidate.required_sources() {
        if let Some(captured) = baseline.sources.get(&key) {
            sources.insert(key, captured.as_ref().clone())?;
        } else {
            // Capture only new declarations. Unchanged descriptors remain the reviewed snapshot.
            let mut minimal = edit::canonical(&candidate);
            for definition in minimal["environments"]
                .as_object_mut()
                .expect("definitions")
                .values_mut()
            {
                for field in ["imports", "overrides"] {
                    let entries = if field == "imports" {
                        &mut definition[field]
                    } else {
                        &mut definition[field]["imports"]
                    };
                    entries.as_object_mut().expect("imports").retain(|_, d| {
                        d["source"]["kind"] == key.kind()
                            && d["source"][key.source_field()] == key.location()
                    });
                }
                definition
                    .as_object_mut()
                    .expect("definition")
                    .remove("owner");
                definition
                    .as_object_mut()
                    .expect("definition")
                    .remove("drift");
            }
            let captured = loader.capture_package(
                &serde_json::to_string(&minimal)
                    .map_err(|_| invalid("Document encoding failed"))?,
                "environment-editor",
                base,
            )?;
            let value = captured
                .sources
                .iter()
                .next()
                .ok_or_else(|| invalid("New source capture is missing"))?
                .1;
            sources.insert(key, value.as_ref().clone())?;
        }
    }
    let inputs = InputFiles::new(base.unwrap_or_else(|| Path::new("/")))
        .map_err(|_| invalid("Input root unavailable"))?;
    let normalized = ownership::normalize(&candidate, sources, &inputs)?;
    ownership::merge_definitions(&baseline, normalized, true)
}

pub(super) fn import(
    loader: &LocalEnvironments,
    current: &EnvironmentRecord,
    environment: &str,
    captured: &CapturedImport,
) -> Result<LoadedDefinitions, EnvironmentError> {
    let package = Package::parse(current.yaml())?;
    let mut document = edit::canonical(&package);
    if !package.definitions().contains_key(environment) {
        return Err(invalid("Import environment is unavailable"));
    }
    let mut sources = current.sources().clone();
    insert_import(loader, &mut document, &mut sources, environment, captured)?;
    document["environments"][environment]["drift"] = json!(true);
    finish(document, &sources)
}

fn finish(
    document: Json,
    available: &CapturedSources,
) -> Result<LoadedDefinitions, EnvironmentError> {
    let yaml = serde_json::to_string(&document).map_err(|_| invalid("Document encoding failed"))?;
    let package = Package::parse(&yaml)?;
    let mut sources = CapturedSources::default();
    for key in package.required_sources() {
        sources.insert(
            key.clone(),
            available
                .get(&key)
                .ok_or_else(|| invalid("Document lacks captured source"))?
                .as_ref()
                .clone(),
        )?;
    }
    Ok(LoadedDefinitions { yaml, sources })
}
fn compact(value: &mut Json) {
    if let Json::Object(object) = value {
        for value in object.values_mut() {
            compact(value);
        }
        object.retain(|key, value| {
            !matches!(key.as_str(), "abstract" | "protected" | "retired")
                || *value != Json::Bool(false)
        });
        object.retain(|key, value| {
            matches!(key.as_str(), "environments" | "imports" | "bind")
                || !matches!(value, Json::Object(o) if o.is_empty())
                    && !matches!(value, Json::Array(a) if a.is_empty())
        });
    }
}
fn yaml(document: &Json) -> Result<String, EnvironmentError> {
    let json = serde_json::to_string(document).map_err(|_| invalid("Document encoding failed"))?;
    if json.len() > MAX_SOURCE_BYTES {
        return Err(invalid("Environment document exceeds its byte budget"));
    }
    let parsed = yaml_rust2::YamlLoader::load_from_str(&json)
        .map_err(|_| invalid("Document encoding failed"))?;
    let mut output = String::new();
    yaml_rust2::YamlEmitter::new(&mut output)
        .dump(&parsed[0])
        .map_err(|_| invalid("Document encoding failed"))?;
    Ok(output)
}

// A package may already own `local` with different settings. Never change its meaning.
fn reserve_target(document: &mut Json, preferred: &str, target: &Json) -> String {
    let mut name = preferred.to_owned();
    let mut suffix = 1;
    while document["targets"].get(&name).is_some_and(|old| {
        old != target
            && !(old == &json!({"kind":"local"}) && target == &json!({"kind":"local","env":{}}))
    }) {
        name = format!("{preferred}_{suffix}");
        suffix += 1;
    }
    document["targets"][&name] = target.clone();
    name
}
