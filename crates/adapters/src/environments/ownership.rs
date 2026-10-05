use super::*;
use serde_json::json;
use wes_core::environments::SourceKey;
use wes_engine::environments::EnvironmentRecord;

pub(super) fn normalize(
    package: &Package,
    sources: CapturedSources,
    inputs: &InputFiles,
) -> Result<LoadedDefinitions, EnvironmentError> {
    let mut document = edit::canonical(package);
    let owner = package.package_name().unwrap_or("workspace");
    document["package"] = json!(owner);
    let mut normalized = CapturedSources::default();
    let mut keys = std::collections::BTreeMap::new();
    for (key, source) in sources.iter() {
        let location = if key.source_field() == "file" {
            inputs
                .resolve(key.location())
                .map_err(|_| invalid("invalid package source path"))?
                .to_str()
                .ok_or_else(|| invalid("source path must be UTF-8"))?
                .to_owned()
        } else if source.format() == "process/path/v1" {
            source.bytes().into()
        } else {
            key.location().into()
        };
        let next = SourceKey::new(key.kind(), &location)?;
        normalized.insert(next.clone(), source.as_ref().clone())?;
        keys.insert(key.clone(), next);
    }
    for (name, d) in document["environments"]
        .as_object_mut()
        .expect("canonical object")
    {
        if package.definitions()[name].owner.is_some() || package.definitions()[name].drift {
            return Err(invalid(
                "package definitions cannot set engine-owned owner/drift fields; use the exported lock for captured state",
            ));
        }
        d["owner"] = json!(owner);
        for member in ["imports", "overrides"] {
            let imports = if member == "imports" {
                &mut d[member]
            } else {
                &mut d[member]["imports"]
            };
            for declaration in imports
                .as_object_mut()
                .expect("canonical imports")
                .values_mut()
            {
                let source = &mut declaration["source"];
                let kind = source["kind"].as_str().expect("source kind").to_owned();
                let field = if kind == "builtin" {
                    "name"
                } else if kind == "docker" {
                    "socket"
                } else if kind == "process" {
                    "bin"
                } else if source.get("url").is_some() {
                    "url"
                } else {
                    "file"
                };
                let previous = SourceKey::new(&kind, source[field].as_str().expect("source path"))?;
                source[field] = json!(keys[&previous].location());
            }
        }
    }
    Ok(LoadedDefinitions {
        yaml: serde_json::to_string_pretty(&document)
            .map_err(|_| invalid("package encoding failed"))?,
        sources: normalized,
    })
}

pub(super) fn merge(
    current: Option<&EnvironmentRecord>,
    loaded: LoadedDefinitions,
    reconcile: bool,
) -> Result<LoadedDefinitions, EnvironmentError> {
    let Some(current) = current else {
        return Ok(loaded);
    };
    merge_definitions(
        &LoadedDefinitions {
            yaml: current.yaml().into(),
            sources: current.sources().clone(),
        },
        loaded,
        reconcile,
    )
}

pub(super) fn merge_definitions(
    current: &LoadedDefinitions,
    loaded: LoadedDefinitions,
    reconcile: bool,
) -> Result<LoadedDefinitions, EnvironmentError> {
    let incoming = Package::parse(&loaded.yaml)?;
    let owner = incoming.package_name().unwrap_or("workspace");
    let existing = Package::parse(&current.yaml)?;
    if !reconcile
        && existing
            .definitions()
            .values()
            .any(|d| d.owner.as_deref().unwrap_or("workspace") == owner && d.drift)
    {
        return Err(invalid(
            "workspace edits diverged from this package. Omit --env-file to inspect installed definitions without applying a package. To replace this drift, reconcile the package and plan with reconcile:file (file input) or reconcile:source (text input)",
        ));
    }
    let mut document = edit::canonical(&existing);
    let candidate = edit::canonical(&incoming);
    let defs = document["environments"]
        .as_object_mut()
        .expect("definitions");
    defs.retain(|_, d| d["owner"].as_str().unwrap_or("workspace") != owner);
    for (name, definition) in candidate["environments"].as_object().expect("definitions") {
        if defs.contains_key(name) {
            return Err(invalid(
                "environment belongs to a different package; ownership cannot be silently replaced",
            ));
        }
        defs.insert(name.clone(), definition.clone());
    }
    let targets = document["targets"].as_object_mut().expect("targets");
    for (name, target) in candidate["targets"].as_object().expect("targets") {
        if targets.get(name).is_some_and(|old| old != target) {
            // A target name is workspace-wide; changing a shared destination needs one owning package.
            if existing.definitions().iter().any(|(_, d)| {
                d.owner.as_deref().unwrap_or("workspace") != owner
                    && (d.targets.contains(name)
                        || d.imports
                            .values()
                            .chain(d.overrides.values())
                            .any(|i| i.target == *name))
            }) {
                return Err(invalid(
                    "target is referenced by another package; use a distinct target name",
                ));
            }
        }
        targets.insert(name.clone(), target.clone());
    }
    document["package"] = json!(owner);
    let yaml = serde_json::to_string_pretty(&document)
        .map_err(|_| invalid("merged package encoding failed"))?;
    let merged = Package::parse(&yaml)?;
    let mut sources = CapturedSources::default();
    for key in merged.required_sources() {
        let new = loaded.sources.get(&key);
        let old = current.sources.get(&key);
        if new.zip(old).is_some_and(|(n, o)| n != o)
            && existing.definitions().values().any(|d| {
                d.owner.as_deref().unwrap_or("workspace") != owner
                    && d.imports
                        .values()
                        .chain(d.overrides.values())
                        .any(|i| i.source == key)
            })
        {
            return Err(invalid(
                "changed descriptor evidence is shared by another package; reconcile it explicitly under one owner",
            ));
        }
        let source = new
            .or(old)
            .ok_or_else(|| invalid("merged source evidence missing"))?;
        sources.insert(key, source.as_ref().clone())?;
    }
    Ok(LoadedDefinitions { yaml, sources })
}
