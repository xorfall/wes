//! Read-only captured inputs from the current environment namespaces, never live files.
use super::*;

#[derive(Clone)]
pub struct ImportedSpec {
    pub environment: String,
    pub alias: String,
    pub origin: String,
    pub source: Arc<str>,
}
impl std::fmt::Debug for ImportedSpec {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ImportedSpec")
            .field("environment", &self.environment)
            .field("alias", &self.alias)
            .field("bytes", &self.source.len())
            .finish_non_exhaustive()
    }
}
impl Workspace {
    pub(crate) fn provider_placements(
        &self,
    ) -> std::collections::BTreeMap<
        String,
        std::collections::BTreeMap<String, (String, String, Option<String>)>,
    > {
        self.environments
            .names()
            .filter_map(|name| {
                let environment = self.environments.inspect(name)?;
                let providers = environment
                    .imports()
                    .iter()
                    .map(|(alias, import)| {
                        let declaration = import.declaration();
                        let captured =
                            self.default_configuration
                                .as_ref()
                                .and_then(|configuration| {
                                    configuration
                                        .captures()
                                        .find(|(name, _)| *name == alias)
                                        .map(|(_, capture)| capture)
                                });
                        let kind = if declaration.source.kind() == "configured" {
                            captured.map_or("builtin", |capture| capture.request().kind())
                        } else {
                            declaration.source.kind()
                        };
                        let endpoint = import.endpoint().map(str::to_owned).or_else(|| {
                            captured
                                .and_then(|capture| capture.request().arguments().get("endpoint"))
                                .and_then(|value| {
                                    if value.provenance().policy().is_private() {
                                        return None;
                                    }
                                    match value.data() {
                                        wes_core::Data::Text(text) => Some(text.to_string()),
                                        _ => None,
                                    }
                                })
                        });
                        (
                            alias.clone(),
                            (kind.to_owned(), declaration.target.clone(), endpoint),
                        )
                    })
                    .collect();
                Some((name.to_owned(), providers))
            })
            .collect()
    }

    pub(crate) fn imported_specs(&self) -> Vec<ImportedSpec> {
        let mut specs = std::collections::BTreeMap::new();
        if let Some(default) = &self.default_configuration {
            for (alias, captured) in default.captures() {
                if captured.request().kind() != "spec" {
                    continue;
                }
                let origin = ["file", "url"]
                    .into_iter()
                    .find_map(|key| {
                        captured
                            .request()
                            .arguments()
                            .get(key)
                            .and_then(|v| match v.data() {
                                wes_core::Data::Text(text) => Some(text.to_string()),
                                _ => None,
                            })
                    })
                    .unwrap_or_default();
                specs.insert(
                    (default.name.clone(), alias.to_owned()),
                    ImportedSpec {
                        environment: default.name.clone(),
                        alias: alias.into(),
                        origin,
                        source: captured.recipe().shared_source(),
                    },
                );
            }
        }
        for name in self.environments.names() {
            let Some(environment) = self.environments.inspect(name) else {
                continue;
            };
            if environment.is_retired() {
                continue;
            }
            for (alias, import) in environment.imports() {
                if import.declaration().source.kind() != "spec" {
                    continue;
                }
                specs.insert(
                    (name.to_owned(), alias.clone()),
                    ImportedSpec {
                        environment: name.into(),
                        alias: alias.clone(),
                        origin: import.declaration().source.location().into(),
                        source: import.source().shared_bytes(),
                    },
                );
            }
        }
        specs.into_values().collect()
    }
}
