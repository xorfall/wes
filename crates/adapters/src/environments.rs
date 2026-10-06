//! Package-relative local input capture and exact managed binding construction.
mod builtin;
mod documents;
mod edit;
pub use edit::configure_authentication;
mod lock;
mod ownership;
use crate::{
    execution_targets::{ProcessNamespace, driver},
    http::HttpConfig,
    imports::ProcessImporter,
    input_files::InputFiles,
    process::ProcessConfig,
};
use std::{path::Path, sync::Arc, time::Duration};
use wes_core::{
    Data, Shape, Value,
    environments::{Binding, CapturedSource, CapturedSources, EnvironmentError, Package},
};
use wes_engine::{
    environments::{Authority, EnvironmentLoader, LoadedDefinitions},
    imports::{ImportMode, ImportProduct, ImportRequest, Importer},
};

pub struct LocalEnvironments {
    files: InputFiles,
    authority: Authority,
    documents: Option<Arc<dyn crate::imports::SpecDocuments>>,
    openapi: Option<Arc<dyn crate::imports::OpenApiCompiler>>,
    archive: Option<Arc<dyn crate::source_archive::SourceArchive>>,
    docker_candidates: Vec<std::path::PathBuf>,
}
impl LocalEnvironments {
    pub fn with_authority(mut self, authority: Authority) -> Self {
        self.authority = authority;
        self
    }
    /// Embedding-owned local discovery roots, also used by isolated synthetic fixtures.
    pub fn with_docker_candidates(mut self, candidates: Vec<std::path::PathBuf>) -> Self {
        self.docker_candidates = candidates;
        self
    }
    pub fn with_archive(mut self, archive: Arc<dyn crate::source_archive::SourceArchive>) -> Self {
        self.archive = Some(archive);
        self
    }
    pub fn with_documents(mut self, documents: Arc<dyn crate::imports::SpecDocuments>) -> Self {
        self.documents = Some(documents);
        self
    }
    pub fn with_openapi(mut self, compiler: Arc<dyn crate::imports::OpenApiCompiler>) -> Self {
        self.openapi = Some(compiler);
        self
    }
    fn spec_source(
        &self,
        inputs: &InputFiles,
        key: &wes_core::environments::SourceKey,
    ) -> Result<CapturedSource, EnvironmentError> {
        let captured = if key.kind() == "openapi" {
            let compiler = self
                .openapi
                .as_ref()
                .ok_or_else(|| invalid("OpenAPI compiler is unavailable"))?;
            let recipe = crate::imports::openapi::capture_source(
                inputs,
                key.source_field(),
                key.location(),
                compiler.as_ref(),
                wes_core::environments::MAX_SOURCE_BYTES,
            )
            .map_err(|error| invalid(&error.to_string()))?;
            CapturedSource::new(recipe.format(), recipe.source())?
        } else if key.source_field() == "url"
            && let Some(documents) = &self.documents
        {
            let recipe = documents
                .capture(key.location(), wes_core::environments::MAX_SOURCE_BYTES)
                .map_err(|e| invalid(&e.to_string()))?;
            CapturedSource::new(recipe.format(), recipe.source())?
        } else {
            let source = if key.source_field() == "url" {
                crate::imports::read_url(key.location(), wes_core::environments::MAX_SOURCE_BYTES)
                    .map_err(|e| invalid(&e.to_string()))?
            } else {
                read_input(
                    inputs,
                    key.location(),
                    wes_core::environments::MAX_SOURCE_BYTES,
                    "environment descriptor",
                )?
            };
            CapturedSource::new("spec/json/v1", &source)?
        };
        if let Some(archive) = &self.archive {
            let origin = if key.source_field() == "url" {
                key.location().to_owned()
            } else {
                inputs
                    .resolve(key.location())
                    .map_err(|_| invalid("invalid descriptor input path"))?
                    .to_string_lossy()
                    .into_owned()
            };
            archive.retain(crate::source_archive::SourceKind::Spec, &origin, captured.format(), captured.bytes())
                .map_err(|_| invalid("captured spec could not be saved in the data folder; check its storage and permissions"))?;
        }
        Ok(captured)
    }
    fn capture_package(
        &self,
        yaml: &str,
        origin: &str,
        base: Option<&Path>,
    ) -> Result<LoadedDefinitions, EnvironmentError> {
        if yaml.len() > 1024 * 1024 {
            return Err(invalid("environment package exceeds its byte limit"));
        }
        if let Some(archive) = &self.archive {
            archive.retain(crate::source_archive::SourceKind::Environments, origin, "environments/yaml/v1", yaml)
                .map_err(|_| invalid("captured environment package could not be saved in the data folder; check its storage and permissions"))?;
        }
        let package = resolve_ssh_inputs(Package::parse(yaml)?, base)?;
        for target in package.targets().values() {
            driver(target).validate(target).map_err(invalid)?;
            driver(target).validate_inputs(target).map_err(invalid)?;
        }
        if base.is_none()
            && package.required_sources().iter().any(|key| {
                key.kind() != "builtin"
                    && key.source_field() != "url"
                    && !Path::new(key.location()).is_absolute()
                    && !(!matches!(key.kind(), "spec" | "openapi")
                        && package
                            .definitions()
                            .values()
                            .flat_map(|d| d.imports.values().chain(d.overrides.values()))
                            .filter(|i| i.source == *key)
                            .all(|i| {
                                package.targets().get(&i.target).is_some_and(|t| {
                                    driver(t).namespace() == ProcessNamespace::Target
                                })
                            }))
            })
        {
            return Err(invalid(
                "text environment packages with relative local inputs require an explicit absolute base directory",
            ));
        }
        // Without relative local inputs root is an inert base, never guessed app CWD.
        let base = base.unwrap_or_else(|| Path::new(std::path::MAIN_SEPARATOR_STR));
        let inputs = InputFiles::new(base)
            .map_err(|_| invalid("environment package directory unavailable"))?;
        let mut sources = CapturedSources::default();
        for key in package.required_sources() {
            let source = if key.kind() == "builtin" {
                CapturedSource::new("builtin/v1", key.location())?
            } else if key.kind() == "docker" {
                CapturedSource::new("docker/observe/v1", key.location())?
            } else if matches!(key.kind(), "spec" | "openapi") {
                self.spec_source(&inputs, &key)?
            } else if package
                .definitions()
                .values()
                .flat_map(|d| d.imports.values().chain(d.overrides.values()))
                .filter(|i| i.source == key)
                .any(|i| {
                    package
                        .targets()
                        .get(&i.target)
                        .is_some_and(|t| driver(t).namespace() == ProcessNamespace::Target)
                })
            {
                if package
                    .definitions()
                    .values()
                    .flat_map(|d| d.imports.values().chain(d.overrides.values()))
                    .filter(|i| i.source == key)
                    .any(|i| {
                        package
                            .targets()
                            .get(&i.target)
                            .is_some_and(|t| driver(t).namespace() == ProcessNamespace::Host)
                    })
                {
                    return Err(invalid(
                        "one executable source cannot mix host and target namespaces; use distinct logical source paths",
                    ));
                }
                CapturedSource::new("process/target/v1", key.location())?
            } else {
                let importer = ProcessImporter::new(base, ProcessConfig::default())
                    .map_err(|_| invalid("local process input directory unavailable"))?;
                let request = ImportRequest::new(
                    "process".into(),
                    None,
                    indexmap::IndexMap::from_iter([(
                        "bin".into(),
                        Value::new(
                            Shape::Unknown,
                            Data::Text(key.location().into()),
                            Default::default(),
                        )
                        .expect("text"),
                    )]),
                )
                .map_err(|_| invalid("invalid process capture request"))?;
                let recipe = importer
                    .capture(&request, wes_core::environments::MAX_SOURCE_BYTES)
                    .map_err(|_| invalid("local executable unavailable"))?;
                CapturedSource::new(recipe.format(), recipe.source())?
            };
            sources.insert(key, source)?;
        }
        ownership::normalize(&package, sources, &inputs)
    }
    pub fn read_lock(
        &self,
        path: &str,
    ) -> Result<Vec<wes_engine::environments::EnvironmentRecord>, EnvironmentError> {
        lock::read(self, path)
    }
    pub fn new(base: impl AsRef<Path>) -> Result<Self, EnvironmentError> {
        Ok(Self {
            files: InputFiles::new(base)
                .map_err(|_| invalid("environment input directory unavailable"))?,
            authority: Authority::default(),
            documents: None,
            openapi: None,
            archive: None,
            docker_candidates: crate::docker::automatic::host_candidates(),
        })
    }
}
impl EnvironmentLoader for LocalEnvironments {
    fn default_document(
        &self,
        name: &str,
        imports: &[wes_engine::imports::CapturedImport],
    ) -> Result<LoadedDefinitions, EnvironmentError> {
        documents::default_document(self, name, imports)
    }
    fn editor_documents(
        &self,
        current: Option<&wes_engine::environments::EnvironmentRecord>,
        default: Option<LoadedDefinitions>,
        token: &str,
    ) -> Result<Vec<wes_engine::environments::EnvironmentDocument>, EnvironmentError> {
        documents::list(current, default, token)
    }
    fn capture_editor(
        &self,
        current: Option<&wes_engine::environments::EnvironmentRecord>,
        default: Option<LoadedDefinitions>,
        source: &str,
        package: &str,
        base: Option<&str>,
    ) -> Result<LoadedDefinitions, EnvironmentError> {
        documents::capture(self, current, default, source, package, base)
    }
    fn capture_import(
        &self,
        current: &wes_engine::environments::EnvironmentRecord,
        environment: &str,
        captured: &wes_engine::imports::CapturedImport,
    ) -> Result<LoadedDefinitions, EnvironmentError> {
        documents::import(self, current, environment, captured)
    }
    fn validate_target(
        &self,
        target: &wes_core::environments::Target,
    ) -> Result<(), EnvironmentError> {
        driver(target).validate(target).map_err(invalid)
    }
    fn merge(
        &self,
        current: Option<&wes_engine::environments::EnvironmentRecord>,
        loaded: LoadedDefinitions,
        reconcile_file: bool,
    ) -> Result<LoadedDefinitions, EnvironmentError> {
        ownership::merge(current, loaded, reconcile_file)
    }
    fn export(
        &self,
        path: &str,
        records: &[wes_engine::environments::EnvironmentRecord],
    ) -> Result<(), EnvironmentError> {
        lock::export(self, path, records)
    }
    fn edit(
        &self,
        current: &wes_engine::environments::EnvironmentRecord,
        change: &wes_engine::environments::DefinitionEdit,
    ) -> Result<LoadedDefinitions, EnvironmentError> {
        edit::edit(self, current, change)
    }
    fn authority(&self) -> Option<Authority> {
        Some(self.authority.clone())
    }
    fn capture(&self, path: &str) -> Result<LoadedDefinitions, EnvironmentError> {
        let yaml = read_input(&self.files, path, 1024 * 1024, "environment package")?;
        let resolved = self
            .files
            .resolve(path)
            .map_err(|_| invalid("invalid environment package path"))?;
        let base = resolved
            .parent()
            .ok_or_else(|| invalid("invalid environment package directory"))?;
        self.capture_package(&yaml, &resolved.to_string_lossy(), Some(base))
    }
    fn capture_text(
        &self,
        source: &str,
        origin: Option<&str>,
        base: Option<&str>,
    ) -> Result<LoadedDefinitions, EnvironmentError> {
        let origin = wes_engine::type_sources::text_origin(origin, source)
            .map_err(|e| invalid(&e.to_string()))?;
        let base = match base {
            Some(base)
                if !base.is_empty()
                    && base.len() <= 4096
                    && !base.chars().any(char::is_control)
                    && Path::new(base).is_absolute() =>
            {
                Some(Path::new(base))
            }
            Some(_) => {
                return Err(invalid(
                    "environment text base must be an absolute directory",
                ));
            }
            None => None,
        };
        self.capture_package(source, &origin, base)
    }
    fn build(&self, alias: &str, binding: &Binding) -> Result<ImportProduct, EnvironmentError> {
        let import = binding.import();
        if import.source().format() == "builtin/v1" {
            return builtin::build(
                alias,
                binding,
                self.authority.clone(),
                &self.docker_candidates,
            );
        }
        if import.source().format() == "docker/observe/v1" {
            return crate::docker::observation::build(alias, binding, self.authority.clone())
                .map_err(invalid);
        }
        let target_driver = driver(import.target());
        target_driver.validate(import.target()).map_err(invalid)?;
        if matches!(
            import.source().format(),
            "process/path/v1" | "process/target/v1"
        ) {
            return target_driver
                .build_process(alias, binding, self.authority.clone())
                .map_err(invalid);
        }
        let credentials = self
            .authority
            .credentials(binding, alias)
            .map_err(|_| invalid("invalid scoped credential binding"))?;
        match import.source().format() {
            "spec/json/v1" | crate::imports::openapi::FORMAT => {
                let normalized;
                let source = if import.source().format() == crate::imports::openapi::FORMAT {
                    normalized = crate::imports::openapi::descriptor(import.source().bytes())
                        .map_err(|error| invalid(&error.to_string()))?;
                    normalized.as_str()
                } else {
                    import.source().bytes()
                };
                let mut config = HttpConfig::default();
                if let Some(ms) = import.timeout_ms() {
                    config.request_timeout = Duration::from_millis(ms as u64);
                }
                let reading = crate::descriptor::read_selected(
                    source.as_bytes(),
                    Some(alias),
                    credentials,
                    config,
                    ImportMode::Replay,
                    import.endpoint(),
                    &import.declaration().auth,
                )
                .map_err(|error| invalid(error.0))?;
                if !import.credential_refs().is_empty()
                    && let Some(Data::Record(info)) = reading.description.information()
                    && let Some(Data::List(operations)) = info.get("authentication")
                {
                    for operation in operations {
                        if let Data::Record(fields) = operation
                            && fields.get("state") == Some(&Data::Text("selection-required".into()))
                        {
                            let operation = match fields.get("operation") {
                                Some(Data::List(parts)) => parts
                                    .iter()
                                    .filter_map(|part| match part {
                                        Data::Text(name) => Some(name.as_ref()),
                                        _ => None,
                                    })
                                    .collect::<Vec<_>>()
                                    .join(" "),
                                _ => "operation".into(),
                            };
                            return Err(invalid(&format!(
                                "Authentication choice is missing for '{}'; select its schemes in environment bind.auth before binding credentials. Inspect the API's authentication options; supplying credentials does not select a method.",
                                operation.chars().take(256).collect::<String>()
                            )));
                        }
                    }
                }
                if reading
                    .description
                    .secrets()
                    .iter()
                    .collect::<std::collections::BTreeSet<_>>()
                    != import.credential_refs().keys().collect()
                {
                    return Err(invalid(
                        "HTTP credential slots must exactly match declared adapter requirements",
                    ));
                }
                let transport =
                    crate::http::transport::Transport::bound(binding, self.authority.clone())
                        .map_err(invalid)?;
                let invoker = Arc::new(reading.invoker.with_transport(transport));
                ImportProduct::new_with_warnings(
                    reading.description,
                    invoker.clone(),
                    reading.warnings,
                )
                .map(|p| p.with_streams(invoker))
                .map_err(|_| invalid("HTTP environment metadata exceeds budget"))
            }
            _ => Err(invalid("unsupported captured environment importer format")),
        }
    }
}
fn invalid(message: &str) -> EnvironmentError {
    EnvironmentError {
        code: "ENV010",
        message: message.into(),
    }
}

/// Keep local I/O details in adapters; the core's existing message carries the explanation.
fn read_input(
    files: &InputFiles,
    path: &str,
    limit: usize,
    role: &str,
) -> Result<String, EnvironmentError> {
    files
        .read(path, limit)
        .map_err(|error| invalid(&format!("{role}: {}", files.explain(path, error))))
}

/// Bind host-side SSH paths at capture time. Restore/lock replay uses this exact configuration.
fn resolve_ssh_inputs(package: Package, base: Option<&Path>) -> Result<Package, EnvironmentError> {
    let mut document = edit::canonical(&package);
    let mut changed = false;
    for target in document["targets"]
        .as_object_mut()
        .expect("canonical targets")
        .values_mut()
    {
        if target["kind"] != "ssh" {
            continue;
        }
        for key in ["client", "identity_file", "known_hosts"] {
            let path = target[key].as_str().expect("validated SSH path");
            if path.starts_with('~') {
                return Err(invalid(
                    "SSH paths do not expand '~'; use an absolute path or a path relative to the package directory",
                ));
            }
            if key == "client" && !path.contains('/') && !path.contains('\\') {
                return Err(invalid(
                    "SSH client does not search PATH; use ./ssh or an absolute executable path",
                ));
            }
            if Path::new(path).is_absolute() {
                continue;
            }
            let base = base.filter(|base| base.is_absolute()).ok_or_else(|| invalid("relative SSH client/key/known-hosts paths require the package file directory or an explicit absolute base; invocation CWD is not used"))?;
            let inputs =
                InputFiles::new(base).map_err(|_| invalid("SSH package directory unavailable"))?;
            let resolved = inputs
                .resolve(path)
                .map_err(|_| invalid("invalid SSH input path"))?;
            target[key] = serde_json::json!(
                resolved
                    .to_str()
                    .ok_or_else(|| invalid("SSH input path must be UTF-8"))?
            );
            changed = true;
        }
    }
    if changed {
        Package::parse(
            &serde_json::to_string(&document)
                .map_err(|_| invalid("SSH target capture encoding failed"))?,
        )
    } else {
        Ok(package)
    }
}
