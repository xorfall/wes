//! Capture/normalization is live work; rebuilding consumes only the frozen native descriptor.
use super::*;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

pub const FORMAT: &str = "openapi/normalized/v1";
const CONVERTER: &str = "wes-openapi-normalizer/v1";

/// Host-owned deterministic conversion on an owned blocking capture worker. No model,
/// source retrieval, provider calls or credential access may be performed by this port.
pub trait OpenApiCompiler: Send + Sync {
    fn compile(&self, source: &[u8]) -> Result<Vec<u8>, ImportError>;
}

pub struct OpenApiImporter {
    spec: Arc<SpecImporter>,
    compiler: Arc<dyn OpenApiCompiler>,
}
impl OpenApiImporter {
    pub fn new(spec: Arc<SpecImporter>, compiler: Arc<dyn OpenApiCompiler>) -> Self {
        Self { spec, compiler }
    }
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Captured {
    converter: String,
    source: String,
    source_digest: String,
    descriptor: String,
    descriptor_digest: String,
}
fn digest(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}
/// Shared by environment reload/edit and importer replay. No compiler or source I/O.
pub(crate) fn descriptor(source: &str) -> Result<String, ImportError> {
    crate::codec::decode_json_preserving(
        source.as_bytes(),
        crate::codec::Limits {
            bytes: wes_engine::imports::max_recipe_bytes(),
            nodes: 32,
        },
    )
    .map_err(|_| ImportError::InvalidRecipe)?;
    let captured: Captured =
        serde_json::from_str(source).map_err(|_| ImportError::InvalidRecipe)?;
    if captured.converter != CONVERTER
        || digest(captured.source.as_bytes()) != captured.source_digest
        || digest(captured.descriptor.as_bytes()) != captured.descriptor_digest
    {
        return Err(ImportError::InvalidRecipe);
    }
    Ok(captured.descriptor)
}
pub(crate) fn capture_source(
    files: &InputFiles,
    key: &str,
    location: &str,
    compiler: &dyn OpenApiCompiler,
    max_bytes: usize,
) -> Result<ImportRecipe, ImportError> {
    let source = if key == "url" {
        read_url(location, max_bytes)?
    } else {
        files.read(location, max_bytes).map_err(convert)?
    };
    let converted = compiler.compile(source.as_bytes())?;
    crate::api_library::validate_descriptor(&converted).map_err(|_| {
        ImportError::Input(
            "OpenAPI conversion failed Wes contract validation; no provider was installed.",
        )
    })?;
    let descriptor = String::from_utf8(converted).map_err(|_| ImportError::InvalidRecipe)?;
    let captured = Captured {
        converter: CONVERTER.into(),
        source_digest: digest(source.as_bytes()),
        descriptor_digest: digest(descriptor.as_bytes()),
        source,
        descriptor,
    };
    let bytes = serde_json::to_string(&captured).map_err(|_| ImportError::InvalidRecipe)?;
    if bytes.len() > max_bytes {
        return Err(ImportError::Capacity);
    }
    let recipe = ImportRecipe::new(FORMAT.into(), bytes)?;
    Ok(recipe)
}
impl Importer for OpenApiImporter {
    fn metadata(&self) -> wes_engine::imports::ImporterMetadata {
        http_metadata(
            "Import supported OpenAPI 3.0/3.1 JSON/YAML through deterministic Wes conversion. Supply exactly one of file/url and an explicit endpoint. No service calls, external reference fetching, partial conversion or credential grants. Unsupported constructs require :describe and review; replay uses the frozen converted contract.",
        )
    }
    fn capture(
        &self,
        request: &ImportRequest,
        max_bytes: usize,
    ) -> Result<ImportRecipe, ImportError> {
        let (key, location) = http_argument(request, "openapi")?;
        let recipe = capture_source(
            &self.spec.files,
            key,
            location,
            self.compiler.as_ref(),
            max_bytes,
        )?;
        if let Some(archive) = &self.spec.archive {
            let origin = if key == "url" {
                location.to_owned()
            } else {
                self.spec
                    .files
                    .resolve(location)
                    .map_err(convert)?
                    .to_string_lossy()
                    .into_owned()
            };
            archive
                .retain(
                    crate::source_archive::SourceKind::Spec,
                    &origin,
                    FORMAT,
                    recipe.source(),
                )
                .map_err(|_| {
                    ImportError::Input(
                        "Captured OpenAPI input could not be saved; check storage and permissions.",
                    )
                })?;
        }
        Ok(recipe)
    }
    fn build(
        &self,
        snapshot: &ImportSnapshot,
        mode: ImportMode,
    ) -> Result<ImportProduct, ImportError> {
        http_argument(snapshot.request(), "openapi")?;
        if snapshot.recipe().format() != FORMAT {
            return Err(ImportError::InvalidRecipe);
        }
        let source = descriptor(snapshot.recipe().source())?;
        let request = ImportRequest::new(
            "spec".into(),
            snapshot.request().alias().map(str::to_owned),
            snapshot.request().arguments().clone(),
        )?;
        self.spec.build(
            &ImportSnapshot::new(request, ImportRecipe::new("spec/json/v1".into(), source)?),
            mode,
        )
    }
}
