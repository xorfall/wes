use super::*;
use crate::{input_files::InputFiles, process::ProcessConfig};
use serde::{Deserialize, Serialize};
use std::path::Path;
use wes_core::{Data, Primitive, Shape, Value, capability::Parameter};
use wes_engine::imports::{
    ImportError, ImportMode, ImportProduct, ImportRecipe, ImportRequest, ImportSnapshot, Importer,
    ImporterMetadata,
};

pub(crate) const FORMAT: &str = "provider/process/v1";
#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Capture {
    version: u32,
    source: String,
    executable: String,
    target_namespace: bool,
}

/// Reads a definition relative to a captured base; reconstruction uses only the recipe.
pub struct ProviderImporter {
    files: InputFiles,
    config: ProcessConfig,
    archive: Option<Arc<dyn crate::source_archive::SourceArchive>>,
}
impl ProviderImporter {
    pub fn new(base: impl AsRef<Path>, config: ProcessConfig) -> Result<Self, ImportError> {
        Ok(Self {
            files: InputFiles::new(base).map_err(|_| ImportError::Unavailable)?,
            config,
            archive: None,
        })
    }
    pub fn with_archive(mut self, archive: Arc<dyn crate::source_archive::SourceArchive>) -> Self {
        self.archive = Some(archive);
        self
    }
}
impl Importer for ProviderImporter {
    fn metadata(&self) -> ImporterMetadata {
        ImporterMetadata {
            parameters: vec![Parameter::new(
                "file",
                Shape::Primitive(Primitive::Text),
                true,
            )],
            summary: Some(
                "Import a declarative YAML executable provider. Process operations use typed literal argv mappings; session operations use versioned bounded duplex frames, scoped credential handoff, actual readiness and typed events/controls. Optional read_access declares required Text scope parameters and one finite Bool revalidate operation. Only explicit native revalidation returning true opens that captured scope; credentials, retained bytes and re-enabling do not grant reads or revive a source. Read domains require managed environment binding. No executable is launched by import. Session targets must be local. Replacing a definition requires replace:true; previous work keeps its captured revision.",
            ),
            ..Default::default()
        }
    }
    fn capture(
        &self,
        request: &ImportRequest,
        max_bytes: usize,
    ) -> Result<ImportRecipe, ImportError> {
        let path = argument(request)?;
        let recipe = capture_file(&self.files, path, false, max_bytes)?;
        if let Some(archive) = &self.archive {
            let origin = self
                .files
                .resolve(path)
                .map_err(|_| ImportError::Unavailable)?;
            archive
                .retain(
                    crate::source_archive::SourceKind::Provider,
                    &origin.to_string_lossy(),
                    FORMAT,
                    recipe.source(),
                )
                .map_err(|_| {
                    ImportError::Input("captured provider definition could not be archived")
                })?;
        }
        Ok(recipe)
    }
    fn build(
        &self,
        snapshot: &ImportSnapshot,
        _: ImportMode,
    ) -> Result<ImportProduct, ImportError> {
        argument(snapshot.request())?;
        if snapshot.recipe().format() != FORMAT {
            return Err(ImportError::InvalidRecipe);
        }
        let (definition, executable) = reconstruct(
            snapshot.recipe().source(),
            false,
            snapshot.request().alias(),
        )?;
        if definition.session.is_some() {
            return super::session::unbound(definition, self.config);
        }
        let (base, invoker) =
            crate::process::wrapping(definition.description.name(), executable, self.config)
                .map_err(|_| ImportError::InvalidRecipe)?;
        super::invocation::wrap(definition, &base, Arc::new(invoker), self.config, vec![])
    }
}
fn argument(request: &ImportRequest) -> Result<&str, ImportError> {
    if request.kind() != "provider" || request.arguments().len() != 1 {
        return Err(ImportError::InvalidRecipe);
    }
    match request.arguments().get("file").map(Value::data) {
        Some(Data::Text(path))
            if !path.is_empty() && path.len() <= 4096 && !path.chars().any(char::is_control) =>
        {
            Ok(path)
        }
        _ => Err(ImportError::InvalidRecipe),
    }
}
pub(crate) fn capture_file(
    files: &InputFiles,
    path: &str,
    target_namespace: bool,
    max_bytes: usize,
) -> Result<ImportRecipe, ImportError> {
    let source = files
        .read(
            path,
            max_bytes.min(wes_core::environments::MAX_SOURCE_BYTES),
        )
        .map_err(|error| match error {
            crate::input_files::InputFileError::TooLarge { .. } => ImportError::Capacity,
            _ => ImportError::Unavailable,
        })?;
    let definition = super::schema::compile(&source, None).map_err(|e| ImportError::Input(e.0))?;
    let executable = if target_namespace {
        if !definition.executable.starts_with('/') {
            return Err(ImportError::Input(
                "target provider executable requires an absolute POSIX path",
            ));
        }
        definition.executable
    } else {
        // Relative binaries belong to the YAML file's directory, never the invoking CWD.
        let location = files.resolve(path).map_err(|_| ImportError::Unavailable)?;
        let importer = crate::imports::ProcessImporter::new(
            location.parent().ok_or(ImportError::InvalidRecipe)?,
            ProcessConfig::default(),
        )?;
        let request = ImportRequest::new(
            "process".into(),
            None,
            [(
                "bin".into(),
                Value::new(
                    Shape::Unknown,
                    Data::Text(definition.executable.into()),
                    Default::default(),
                )
                .expect("text"),
            )]
            .into_iter()
            .collect(),
        )?;
        importer.capture(&request, 4096)?.source().to_owned()
    };
    let capture = Capture {
        version: 1,
        source,
        executable,
        target_namespace,
    };
    let encoded = serde_json::to_string(&capture).map_err(|_| ImportError::InvalidRecipe)?;
    if encoded.len() > max_bytes {
        return Err(ImportError::Capacity);
    }
    ImportRecipe::new(FORMAT.into(), encoded)
}
pub(crate) fn reconstruct(
    source: &str,
    target_namespace: bool,
    alias: Option<&str>,
) -> Result<(Definition, String), ImportError> {
    let capture = read_capture(source, target_namespace)?;
    let mut definition =
        super::schema::compile(&capture.source, alias).map_err(|e| ImportError::Input(e.0))?;
    if target_namespace && definition.executable != capture.executable {
        return Err(ImportError::InvalidRecipe);
    }
    definition.executable = capture.executable.clone();
    Ok((definition, capture.executable))
}
fn read_capture(source: &str, target_namespace: bool) -> Result<Capture, ImportError> {
    if source.len() > wes_engine::imports::max_recipe_bytes() {
        return Err(ImportError::Capacity);
    }
    let capture: Capture = serde_json::from_str(source).map_err(|_| ImportError::InvalidRecipe)?;
    if capture.version != 1
        || capture.target_namespace != target_namespace
        || capture.executable.len() > 4096
        || capture.executable.chars().any(char::is_control)
        || !(if target_namespace {
            capture.executable.starts_with('/')
        } else {
            Path::new(&capture.executable).is_absolute()
        })
    {
        return Err(ImportError::InvalidRecipe);
    }
    Ok(capture)
}
pub(crate) fn captured_executable(
    source: &str,
    target_namespace: bool,
) -> Result<String, &'static str> {
    // The definition was compiled when its immutable binding was constructed.
    // Drivers need only the captured executable, not a fresh contract compiler per call.
    read_capture(source, target_namespace)
        .map(|capture| capture.executable)
        .map_err(|_| "invalid captured provider executable")
}
