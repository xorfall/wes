//! Canonical JSON is a YAML 1.2 flow document; the strict package parser remains the authority.
use super::*;
use serde_json::{Value as Json, json};
use wes_core::environments::*;
use wes_engine::environments::{DefinitionEdit, EnvironmentRecord};

pub(super) fn edit(
    loader: &LocalEnvironments,
    current: &EnvironmentRecord,
    edit: &DefinitionEdit,
) -> Result<LoadedDefinitions, EnvironmentError> {
    let package = Package::parse(current.yaml())?;
    let mut document = canonical(&package);
    let definitions = document["environments"]
        .as_object_mut()
        .expect("canonical object");
    let mut updated = None;
    match edit {
        DefinitionEdit::Import {
            environment,
            alias,
            kind,
            location,
            target,
            replace,
            endpoint,
        } => {
            let definition = package
                .definitions()
                .get(environment)
                .ok_or_else(|| invalid("import environment unavailable"))?;
            if definition.retired {
                return Err(invalid("retired environments cannot receive new imports"));
            }
            if definition.imports.contains_key(alias) != *replace {
                return Err(invalid(
                    "existing local aliases require replace:true; replace cannot override inherited imports",
                ));
            }
            if !package.targets().contains_key(target) {
                return Err(invalid("import requires a declared target"));
            }
            let key = SourceKey::new(kind, location)?;
            let source = if kind == "docker" {
                CapturedSource::new("docker/observe/v1", location)?
            } else if matches!(kind.as_str(), "spec" | "openapi") {
                loader.spec_source(&loader.files, &key)?
            } else if driver(&package.targets()[target]).namespace() == ProcessNamespace::Target {
                CapturedSource::new("process/target/v1", location)?
            } else {
                let base = loader
                    .files
                    .resolve(".")
                    .map_err(|_| invalid("import directory unavailable"))?;
                let importer = ProcessImporter::new(&base, ProcessConfig::default())
                    .map_err(|_| invalid("import directory unavailable"))?;
                let request = ImportRequest::new(
                    "process".into(),
                    None,
                    indexmap::IndexMap::from_iter([(
                        "bin".into(),
                        Value::new(
                            Shape::Unknown,
                            Data::Text(location.as_str().into()),
                            Default::default(),
                        )
                        .expect("text"),
                    )]),
                )
                .map_err(|_| invalid("invalid process import"))?;
                let captured = importer
                    .capture(&request, MAX_SOURCE_BYTES)
                    .map_err(|_| invalid("local executable unavailable"))?;
                CapturedSource::new(captured.format(), captured.source())?
            };
            let field = key.source_field();
            definitions.get_mut(environment).expect("checked")["imports"][alias] =
                json!({"source":{"kind":kind, (field):location}, "bind":{"target":target}});
            if let Some(endpoint) = endpoint {
                definitions.get_mut(environment).expect("checked")["imports"][alias]["bind"]["endpoint"] =
                    json!(endpoint);
            }
            definitions.get_mut(environment).expect("checked")["drift"] = json!(true);
            updated = Some((key, source));
        }
        DefinitionEdit::Rename { name, to } => {
            if definitions.contains_key(to) {
                return Err(invalid("rename destination exists"));
            }
            let mut d = definitions
                .remove(name)
                .ok_or_else(|| invalid("rename source unavailable"))?;
            if package.definitions()[name].retired {
                return Err(invalid("retired environment cannot be renamed"));
            }
            if package.definitions()[name].id.is_none() {
                d["id"] = json!(name);
            }
            d["drift"] = json!(true);
            definitions.insert(to.clone(), d);
            for d in definitions.values_mut() {
                if d.pointer("/extends/env").and_then(Json::as_str) == Some(name)
                    && d.pointer("/extends/track").is_some()
                {
                    d["extends"]["env"] = json!(to);
                    d["drift"] = json!(true);
                }
            }
        }
        DefinitionEdit::Retire { name } => {
            definitions
                .get_mut(name)
                .ok_or_else(|| invalid("retirement source unavailable"))?["retired"] = json!(true);
            definitions.get_mut(name).expect("checked")["drift"] = json!(true);
        }
        DefinitionEdit::Delete { name } => {
            if !package.definitions().get(name).is_some_and(|d| d.retired) {
                return Err(invalid(
                    "retire an environment before deleting its active definition",
                ));
            }
            if package
                .definitions()
                .values()
                .any(|d| d.parent.as_ref().is_some_and(|p| p.name() == name))
            {
                return Err(invalid(
                    "environment is still referenced by a child; delete never cascades",
                ));
            }
            definitions.remove(name);
        }
    }
    let yaml = serde_json::to_string_pretty(&document)
        .map_err(|_| invalid("definition encoding failed"))?;
    let changed = Package::parse(&yaml)?;
    let mut sources = CapturedSources::default();
    for key in changed.required_sources() {
        let source = updated
            .as_ref()
            .filter(|(k, _)| k == &key)
            .map(|(_, s)| s.clone())
            .or_else(|| current.sources().get(&key).map(|s| s.as_ref().clone()))
            .ok_or_else(|| invalid("missing captured source"))?;
        sources.insert(key, source)?;
    }
    Ok(LoadedDefinitions { yaml, sources })
}

pub(super) fn canonical(package: &Package) -> Json {
    let targets: serde_json::Map<_, _> = package
        .targets()
        .iter()
        .map(|(name, target)| {
            let mut d = match target.kind() {
                TargetKind::Local => json!({"kind":"local"}),
                TargetKind::Docker {
                    socket,
                    destination,
                    image,
                    shell,
                } => {
                    let mut d = json!({"kind":"docker","socket":socket,"inherit":"container"});
                    match destination {
                        wes_core::environments::DockerDestination::Container(container) => {
                            d["container"] = json!(container)
                        }
                        wes_core::environments::DockerDestination::Compose {
                            project,
                            service,
                            replica,
                        } => {
                            d["compose"] = json!({"project":project,"service":service});
                            if let Some(replica) = replica {
                                d["compose"]["replica"] = json!(replica);
                            }
                        }
                    }
                    if let Some(shell) = shell {
                        d["shell"] = json!(shell);
                    }
                    if let Some(image) = image {
                        d["image"] = json!(image);
                    }
                    d
                }
                TargetKind::Ssh(config) => json!({"kind":"ssh", "client":config.client,
                "host":config.host,"user":config.user,"port":config.port,
                "identity_file":config.identity_file,"known_hosts":config.known_hosts,
                "shell":"posix","inherit":"remote"}),
            };
            if let Some(cwd) = target.cwd() {
                d["cwd"] = json!(cwd);
            }
            d["env"] = json!(target.variables());
            (name.clone(), d)
        })
        .collect();
    let definitions: serde_json::Map<_, _> = package.definitions().iter().map(|(name, d)| {
        let mut value = json!({"targets":d.targets,"abstract":d.abstract_environment,"imports":imports(&d.imports),"overrides":{"imports":imports(&d.overrides)},"hide":{"imports":d.hidden},
            "parameters":d.parameters.iter().map(|(name,p)| {
                let mut v = json!({"type":match p.kind {ConfigType::Text=>"Text",ConfigType::Int=>"Int",ConfigType::Bool=>"Bool"}});
                if let Some(default) = &p.default { v["default"] = config(default); } (name.clone(),v)
            }).collect::<serde_json::Map<_,_>>(),
            "config":d.config.iter().map(|(k,v)|(k.clone(),config(v))).collect::<serde_json::Map<_,_>>(),
            "secretSlots":d.secret_slots.iter().map(|(k,v)|(k.clone(),json!({"required":v}))).collect::<serde_json::Map<_,_>>(),"secretRefs":d.secret_refs});
        if let Some(id) = &d.id { value["id"] = json!(id); }
        if let Some(owner) = &d.owner { value["owner"] = json!(owner); }
        if d.drift { value["drift"] = json!(true); }
        if d.retired { value["retired"] = json!(true); }
        if d.protected { value["protected"] = json!(true); }
        if let Some(parent) = &d.parent { value["extends"] = match parent {
            Parent::Latest(name) => json!({"env":name,"track":"latest"}),
            Parent::Pinned { name, revision } => json!({"env":name,"revision":revision.to_string()}),
        }; }
        (name.clone(), value)
    }).collect();
    let mut document = json!({"version":1,"targets":targets,"environments":definitions});
    if let Some(name) = package.package_name() {
        document["package"] = json!(name);
    }
    document
}
fn config(v: &ConfigValue) -> Json {
    match v {
        ConfigValue::Text(v) => json!(v),
        ConfigValue::Int(v) => json!(v),
        ConfigValue::Bool(v) => json!(v),
    }
}
fn setting(v: &Setting) -> Json {
    match v {
        Setting::Literal(v) => config(v),
        Setting::Config(n) => json!({"config":n}),
    }
}
fn imports(values: &std::collections::BTreeMap<String, ImportDefinition>) -> Json {
    Json::Object(values.iter().map(|(name, d)| {
        let key = d.source.source_field();
        let mut bind = json!({"target":d.target,"credentials":d.credentials.iter().map(|(k,v)|(k.clone(),json!({"secret":v}))).collect::<serde_json::Map<_,_>>()});
        if d.private_output { bind["output"] = json!("private"); }
        if let Some(v) = &d.transport { bind["transport"] = json!(v); }
        if let Some(v) = &d.endpoint { bind["endpoint"] = setting(v); }
        if let Some(v) = &d.timeout_ms { bind["timeout_ms"] = setting(v); }
        let mut source = json!({"kind":d.source.kind(),(key):d.source.location()});
        if let Some(hash) = &d.source_sha256 { source["sha256"] = json!(hash); }
        if !d.auth.is_empty() {bind["auth"]=json!(d.auth);}
        let declaration = match d.binding_mode {
            BindingMode::Explicit => json!({"source":source,"bind":bind}),
            BindingMode::Automatic => json!({"source":source,"bind":"auto"}),
            BindingMode::Unbound => json!({"source":source}),
        };
        (name.clone(), declaration)
    }).collect())
}

#[cfg(test)]
mod auth_tests {
    use super::*;
    #[test]
    fn editor_canonical_roundtrip_preserves_auth_choices() {
        let p = Package::parse(include_str!(
            "../../../../examples/http-auth-alternatives/environments.yaml"
        ))
        .unwrap();
        let text = canonical(&p).to_string();
        let reopened = Package::parse(&text).unwrap();
        assert_eq!(p.definitions(), reopened.definitions());
        assert!(text.contains("apiSecret") && text.contains("basic.username"));
    }
}

/// Edit one provider's authentication references, retaining the rest of the editor document.
/// Requirements come from the shared descriptor compiler, never from UI slot names.
pub fn configure_authentication(
    source: &str,
    environment: &str,
    alias: &str,
    inherited: &ImportDefinition,
    auth: &std::collections::BTreeMap<String, Vec<String>>,
    requirements: &[String],
) -> Result<String, EnvironmentError> {
    let package = Package::parse(source)?;
    let definition = package
        .definitions()
        .get(environment)
        .ok_or_else(|| invalid("Environment no longer exists"))?;
    if definition.retired || definition.abstract_environment {
        return Err(invalid("Select a runnable environment"));
    }
    let mut document = canonical(&package);
    let env = &mut document["environments"][environment];
    let section = if definition.imports.contains_key(alias) {
        "imports"
    } else {
        "overrides"
    };
    let mut declaration = if section == "imports" {
        env["imports"][alias].clone()
    } else if definition.overrides.contains_key(alias) {
        env["overrides"]["imports"][alias].clone()
    } else {
        imports(&std::collections::BTreeMap::from([(
            alias.to_owned(),
            inherited.clone(),
        )]))[alias]
            .clone()
    };
    if !declaration["bind"].is_object() {
        return Err(invalid("Authentication needs an explicit provider binding"));
    }
    let previous = declaration["bind"]["credentials"].clone();
    declaration["bind"]["auth"] = json!(auth);
    let mut credentials = serde_json::Map::new();
    for slot in requirements {
        let existing = previous[slot]["secret"]
            .as_str()
            .filter(|name| env["secretRefs"][*name].is_string());
        let name = match existing {
            Some(name) => name.to_owned(),
            None => {
                let id = uuid::Uuid::new_v4().simple().to_string();
                let name = format!("auth_{id}");
                env["secretSlots"][&name] = json!({"required":true});
                env["secretRefs"][&name] = json!(format!("wes/auth/{id}"));
                name
            }
        };
        credentials.insert(slot.clone(), json!({"secret":name}));
    }
    declaration["bind"]["credentials"] = credentials.into();
    if section == "imports" {
        env["imports"][alias] = declaration;
    } else {
        env["overrides"]["imports"][alias] = declaration;
    }
    let source = serde_json::to_string_pretty(&document)
        .map_err(|_| invalid("Authentication document could not be encoded"))?;
    Package::parse(&source)?;
    Ok(source)
}
