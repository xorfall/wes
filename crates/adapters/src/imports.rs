//! Concrete spec/process input capture. Construction never registers or executes a provider.
pub(crate) mod openapi;
mod remote;
use crate::{
    descriptor,
    http::HttpConfig,
    input_files::{InputFileError, InputFiles},
    process::{self, ProcessConfig},
};
pub use openapi::{OpenApiCompiler, OpenApiImporter};
pub use remote::{read_url, validate_url as validate_spec_url};
use std::{path::Path, sync::Arc};
use wes_core::{Data, Primitive, Shape, capability::Parameter};
use wes_engine::{
    credentials::Credentials,
    imports::{
        ImportError, ImportMode, ImportProduct, ImportRecipe, ImportRequest, ImportSnapshot,
        Importer,
    },
};

pub struct SpecImporter {
    files: InputFiles,
    credentials: Arc<dyn Credentials>,
    config: HttpConfig,
    documents: Option<Arc<dyn SpecDocuments>>,
    archive: Option<Arc<dyn crate::source_archive::SourceArchive>>,
}
/// Application-owned documentation conversion. Called only during live input capture.
pub trait SpecDocuments: Send + Sync {
    fn capture(&self, url: &str, max_bytes: usize) -> Result<ImportRecipe, ImportError>;
}
impl SpecImporter {
    pub fn new(
        base: impl AsRef<Path>,
        credentials: Arc<dyn Credentials>,
        config: HttpConfig,
    ) -> Result<Self, ImportError> {
        Ok(Self {
            files: InputFiles::new(base).map_err(convert)?,
            credentials,
            config,
            documents: None,
            archive: None,
        })
    }
    pub fn with_documents(mut self, documents: Arc<dyn SpecDocuments>) -> Self {
        self.documents = Some(documents);
        self
    }
    pub fn with_archive(mut self, archive: Arc<dyn crate::source_archive::SourceArchive>) -> Self {
        self.archive = Some(archive);
        self
    }
}
impl Importer for SpecImporter {
    fn parameters(&self) -> Vec<Parameter> {
        http_metadata("Import a ready Wes JSON descriptor. Convert OpenAPI with :describe, or use :import openapi. Supply exactly one of file/url and an explicit endpoint; documented servers never select the execution destination.").parameters
    }
    fn metadata(&self) -> wes_engine::imports::ImporterMetadata {
        http_metadata(
            "Import a ready Wes JSON descriptor. Convert OpenAPI with :describe, or use :import openapi. Supply exactly one of file/url and an explicit endpoint; documented servers never select the execution destination.",
        )
    }
    fn capture(
        &self,
        request: &ImportRequest,
        max_bytes: usize,
    ) -> Result<ImportRecipe, ImportError> {
        let (key, location) = spec_argument(request)?;
        let recipe = if key == "url"
            && let Some(documents) = &self.documents
        {
            documents.capture(location, max_bytes)?
        } else {
            let source = if key == "url" {
                read_url(location, max_bytes)?
            } else {
                self.files.read(location, max_bytes).map_err(convert)?
            };
            ImportRecipe::new("spec/json/v1".into(), source)?
        };
        if let Some(archive) = &self.archive {
            let origin = if key == "url" {
                location.to_owned()
            } else {
                self.files
                    .resolve(location)
                    .map_err(convert)?
                    .to_string_lossy()
                    .into_owned()
            };
            archive.retain(crate::source_archive::SourceKind::Spec, &origin, recipe.format(), recipe.source())
                .map_err(|_| ImportError::Input("captured spec could not be saved in the data folder; check its storage and permissions"))?;
        }
        Ok(recipe)
    }
    fn build(
        &self,
        snapshot: &ImportSnapshot,
        mode: ImportMode,
    ) -> Result<ImportProduct, ImportError> {
        spec_argument(snapshot.request())?;
        if snapshot.recipe().format() != "spec/json/v1" {
            return Err(ImportError::InvalidRecipe);
        }
        let explicit = snapshot
            .request()
            .arguments()
            .get("endpoint")
            .map(|v| match v.data() {
                Data::Text(s) => s.as_ref(),
                _ => unreachable!("validated argument"),
            });
        let reading = descriptor::read_bound(
            snapshot.recipe().source().as_bytes(),
            snapshot.request().alias(),
            self.credentials.clone(),
            self.config,
            mode,
            explicit,
        )
        .map_err(|error| ImportError::Input(error.0))?;
        let invoker = Arc::new(reading.invoker);
        ImportProduct::new_with_warnings(reading.description, invoker.clone(), reading.warnings)
            .map(|product| product.with_streams(invoker))
    }
}
const SPEC_ARGUMENTS: &[&str] = &["file", "url", "endpoint"];
const PROCESS_ARGUMENT: &str = "bin";

fn spec_argument(request: &ImportRequest) -> Result<(&str, &str), ImportError> {
    http_argument(request, "spec")
}
fn http_metadata(summary: &'static str) -> wes_engine::imports::ImporterMetadata {
    wes_engine::imports::ImporterMetadata {
        parameters: SPEC_ARGUMENTS
            .iter()
            .map(|name| {
                Parameter::new(
                    *name,
                    Shape::Primitive(Primitive::Text),
                    *name == "endpoint",
                )
            })
            .collect(),
        exactly_one: vec![vec!["file".into(), "url".into()]],
        summary: Some(summary),
    }
}
fn http_argument<'a>(
    request: &'a ImportRequest,
    kind: &str,
) -> Result<(&'static str, &'a str), ImportError> {
    if request.kind() != kind
        || request
            .arguments()
            .keys()
            .any(|k| !SPEC_ARGUMENTS.contains(&k.as_str()))
    {
        return Err(ImportError::InvalidRecipe);
    }
    http_metadata("").validate(request)?;
    text_argument(request, "endpoint")?;
    let key = if request.arguments().contains_key("url") {
        "url"
    } else {
        "file"
    };
    let location = text_argument(request, key)?;
    if key == "url" {
        remote::validate_url(location)?;
    }
    Ok((key, location))
}

pub struct ProcessImporter {
    files: InputFiles,
    config: ProcessConfig,
    terminal: Option<process::TerminalHandover>,
}
impl ProcessImporter {
    pub fn new(base: impl AsRef<Path>, config: ProcessConfig) -> Result<Self, ImportError> {
        Ok(Self {
            files: InputFiles::new(base).map_err(convert)?,
            config,
            terminal: None,
        })
    }
    /// Select inherited descriptors for a native client; web construction keeps piped interaction.
    pub fn with_terminal_handover(mut self, terminal: process::TerminalHandover) -> Self {
        self.terminal = Some(terminal);
        self
    }
}
impl Importer for ProcessImporter {
    fn parameters(&self) -> Vec<Parameter> {
        vec![Parameter::new(
            PROCESS_ARGUMENT,
            Shape::Primitive(Primitive::Text),
            true,
        )]
    }
    fn capture(
        &self,
        request: &ImportRequest,
        max_bytes: usize,
    ) -> Result<ImportRecipe, ImportError> {
        let path = self
            .files
            .resolve(argument(request, "process", PROCESS_ARGUMENT)?)
            .map_err(convert)?;
        let metadata = std::fs::metadata(&path).map_err(|_| ImportError::Unavailable)?;
        if !metadata.is_file() {
            return Err(ImportError::Unavailable);
        }
        #[cfg(unix)]
        {
            use rustix::fs::{Access, AtFlags, CWD, accessat};
            // Ask the OS using effective credentials, including ACLs. This observation is not
            // authorization for future execution: replacement/permission changes can still occur.
            accessat(CWD, &path, Access::EXEC_OK, AtFlags::EACCESS)
                .map_err(|_| ImportError::Unavailable)?;
        }
        let path = path.to_str().ok_or(ImportError::InvalidRecipe)?;
        if path.len() > max_bytes {
            return Err(ImportError::Capacity);
        }
        ImportRecipe::new("process/path/v1".into(), path.into())
    }
    fn build(
        &self,
        snapshot: &ImportSnapshot,
        _: ImportMode,
    ) -> Result<ImportProduct, ImportError> {
        argument(snapshot.request(), "process", PROCESS_ARGUMENT)?;
        let path = snapshot.recipe().source();
        if snapshot.recipe().format() != "process/path/v1"
            || !Path::new(path).is_absolute()
            || path.len() > 4096
            || path.chars().any(char::is_control)
        {
            return Err(ImportError::InvalidRecipe);
        }
        // No stat/PATH lookup/executable launch on reconstruction. The OS opens this captured
        // location only when a later explicit invocation starts; executable bytes are not archived.
        let (description, invoker) = process::wrapping(
            snapshot.request().alias().unwrap_or("process"),
            path,
            self.config,
        )
        .map_err(|_| ImportError::InvalidRecipe)?;
        let conversations = Arc::new(self.terminal.as_ref().map_or_else(
            || invoker.piped(),
            |terminal| terminal.conversation(&invoker),
        ));
        ImportProduct::new(description, Arc::new(invoker), vec![])
            .map(|product| product.with_conversations(conversations))
    }
}
fn argument<'a>(request: &'a ImportRequest, kind: &str, key: &str) -> Result<&'a str, ImportError> {
    if request.kind() != kind || request.arguments().len() != 1 {
        return Err(ImportError::InvalidRecipe);
    }
    text_argument(request, key)
}
fn text_argument<'a>(request: &'a ImportRequest, key: &str) -> Result<&'a str, ImportError> {
    match request.arguments().get(key).map(|v| v.data()) {
        Some(Data::Text(text))
            if !text.trim().is_empty()
                && text.len() <= 4096
                && !text.chars().any(char::is_control) =>
        {
            Ok(text)
        }
        _ => Err(ImportError::InvalidRecipe),
    }
}

fn convert(error: InputFileError) -> ImportError {
    match error {
        InputFileError::TooLarge { .. } => ImportError::Capacity,
        InputFileError::Io(_) | InputFileError::NotRegularFile => ImportError::Unavailable,
        InputFileError::InvalidPath | InputFileError::InvalidText => ImportError::InvalidRecipe,
    }
}
