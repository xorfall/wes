//! Concrete input capture is adapter-owned; image construction never executes a provider.
use super::{Plan, error};
use crate::{imports::ImportProduct, providers::Providers};
use std::collections::BTreeMap;
use wes_core::environments::{Binding, CapturedSources, EnvironmentError, Revision};

pub struct LoadedDefinitions {
    pub yaml: String,
    pub sources: CapturedSources,
}
#[derive(Clone, Debug)]
pub struct EnvironmentDocument {
    pub name: String,
    pub environments: Vec<String>,
    pub source: String,
    pub origin: String,
}
#[derive(Clone, Debug)]
pub enum DefinitionEdit {
    Import {
        environment: String,
        alias: String,
        kind: String,
        location: String,
        target: String,
        replace: bool,
        endpoint: Option<String>,
    },
    Rename {
        name: String,
        to: String,
    },
    Retire {
        name: String,
    },
    Delete {
        name: String,
    },
}
pub trait EnvironmentLoader: Send + Sync + 'static {
    fn default_document(
        &self,
        _name: &str,
        _imports: &[crate::imports::CapturedImport],
    ) -> Result<LoadedDefinitions, EnvironmentError> {
        Err(error(
            "ENV010",
            "Default environment document is unavailable",
        ))
    }
    fn editor_documents(
        &self,
        _current: Option<&super::EnvironmentRecord>,
        _default: Option<LoadedDefinitions>,
        _token: &str,
    ) -> Result<Vec<EnvironmentDocument>, EnvironmentError> {
        Err(error("ENV010", "Environment documents are unavailable"))
    }
    fn capture_editor(
        &self,
        _current: Option<&super::EnvironmentRecord>,
        _default: Option<LoadedDefinitions>,
        _source: &str,
        _package: &str,
        _base: Option<&str>,
    ) -> Result<LoadedDefinitions, EnvironmentError> {
        Err(error(
            "ENV010",
            "Environment document editing is unavailable",
        ))
    }
    fn capture_import(
        &self,
        _current: &super::EnvironmentRecord,
        _environment: &str,
        _captured: &crate::imports::CapturedImport,
    ) -> Result<LoadedDefinitions, EnvironmentError> {
        Err(error("ENV010", "Environment import capture is unavailable"))
    }
    /// Pure compiled-driver validation, including targets with no provider import.
    fn validate_target(
        &self,
        _target: &wes_core::environments::Target,
    ) -> Result<(), EnvironmentError> {
        Ok(())
    }

    fn merge(
        &self,
        _current: Option<&super::EnvironmentRecord>,
        loaded: LoadedDefinitions,
        _reconcile_file: bool,
    ) -> Result<LoadedDefinitions, EnvironmentError> {
        Ok(loaded)
    }
    fn export(
        &self,
        _path: &str,
        _records: &[super::EnvironmentRecord],
    ) -> Result<(), EnvironmentError> {
        Err(error("ENV010", "environment export is unavailable"))
    }
    fn edit(
        &self,
        _current: &super::EnvironmentRecord,
        _edit: &DefinitionEdit,
    ) -> Result<LoadedDefinitions, EnvironmentError> {
        Err(error(
            "ENV010",
            "definition editing is unsupported by this loader",
        ))
    }
    fn authority(&self) -> Option<super::Authority> {
        None
    }
    /// Bounded local reads, relative to the defining package. Never enumerate host environment.
    fn capture(&self, path: &str) -> Result<LoadedDefinitions, EnvironmentError>;
    /// Capture an in-memory package; base is an explicit absolute directory for relative inputs.
    fn capture_text(
        &self,
        _source: &str,
        _origin: Option<&str>,
        _base: Option<&str>,
    ) -> Result<LoadedDefinitions, EnvironmentError> {
        Err(error("ENV010", "environment text input is unavailable"))
    }
    /// Exact captured inputs only. No file/network/credential lookup or executable probing.
    fn build(&self, alias: &str, binding: &Binding) -> Result<ImportProduct, EnvironmentError>;
}

#[derive(Clone, Default)]
pub(crate) struct ExecutionImages {
    entries: BTreeMap<(String, Revision), Providers>,
    bytes: u64,
}
impl ExecutionImages {
    pub(crate) fn configure(
        &mut self,
        environment: &wes_core::environments::EffectiveEnvironment,
        providers: Providers,
        charge: u64,
    ) -> Result<(), EnvironmentError> {
        let key = (environment.name().into(), environment.revision());
        if self.entries.contains_key(&key) {
            return Ok(());
        }
        let bytes = self.bytes.saturating_add(charge);
        if bytes > 64 * 1024 * 1024 {
            return Err(error(
                "ENV007",
                "environment execution metadata budget exceeded",
            ));
        }
        self.entries.insert(key, providers);
        self.bytes = bytes;
        Ok(())
    }
    pub fn build(
        &self,
        plan: &Plan,
        loader: &dyn EnvironmentLoader,
    ) -> Result<Self, EnvironmentError> {
        let mut candidate = self.clone();
        for (name, environment) in &plan.active {
            let key = (name.clone(), environment.revision());
            if candidate.entries.contains_key(&key) {
                continue;
            }
            for target in environment
                .execution_targets()
                .values()
                .filter_map(|target| target.as_ref().ok())
            {
                loader.validate_target(target)?;
            }
            let mut providers = Providers::new();
            if !environment.is_abstract() {
                for alias in environment.imports().keys() {
                    let binding = environment.bind(alias)?;
                    let product = loader.build(alias, &binding).map_err(|mut error| {
                        error.message = format!(
                            "Provider '{alias}' on target '{}': {}",
                            binding.import().target().name(),
                            error.message
                        );
                        error
                    })?;
                    if product.description().name() != alias {
                        return Err(error(
                            "ENV010",
                            "adapter changed the environment provider alias",
                        ));
                    }
                    candidate.bytes =
                        candidate
                            .bytes
                            .checked_add(product.charge())
                            .ok_or_else(|| {
                                error("ENV007", "environment execution metadata budget exceeded")
                            })?;
                    if candidate.bytes > 64 * 1024 * 1024 {
                        return Err(error(
                            "ENV007",
                            "environment execution metadata budget exceeded",
                        ));
                    }
                    providers
                        .register_environment(&product, binding, loader.authority())
                        .map_err(|e| error("ENV010", &e.to_string()))?;
                }
            }
            candidate.entries.insert(key, providers);
        }
        Ok(candidate)
    }
    pub fn get(&self, name: &str, revision: Revision) -> Option<&Providers> {
        self.entries.get(&(name.into(), revision))
    }
}
