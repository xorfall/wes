//! Imports capture ready descriptor bytes; document interpretation belongs to :describe.
use super::*;
use wes_adapters::imports::{SpecDocuments, read_url, validate_spec_url};
use wes_engine::imports::{ImportError, ImportRecipe};
impl wes_adapters::imports::OpenApiCompiler for ApiLibrary {
    fn compile(&self, source: &[u8]) -> std::result::Result<Vec<u8>, ImportError> {
        let executable = self.settings().map_err(|_| failure("OpenAPI compiler settings are unavailable."))?
            .and_then(|settings| settings.extractor).or_else(bundled_extractor)
            .ok_or_else(|| failure("OpenAPI import requires the bundled wes-extract executable or an explicitly configured extractor."))?;
        // This port is invoked by the import coordinator's owned blocking worker.
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .map_err(|_| failure("OpenAPI conversion worker could not start."))?;
        let key = PackageKey {
            service: "api".into(),
            api_version: "openapi".into(),
            scope: "direct".into(),
        };
        let options = Extraction {
            location: "captured source".into(),
            allow_partial: false,
        };
        runtime.block_on(io_layer::extract(&executable, &key, source, &options))
            .map_err(|error| match io_layer::extraction_code(&error) {
                "DSC002" => failure("OpenAPI contains unsupported or invalid declarations. Use :describe and /spec to inspect them; no provider was installed."),
                "DSC007" => failure("OpenAPI conversion exceeded its time budget; no provider was installed."),
                _ => failure("OpenAPI conversion failed. Check the extractor setup or use :describe to inspect the document; no provider was installed."),
            })
    }
}

fn failure(message: &'static str) -> ImportError {
    ImportError::Input(message)
}

fn recipe(bytes: &[u8], limit: usize) -> std::result::Result<ImportRecipe, ImportError> {
    if bytes.len() > limit {
        return Err(ImportError::Capacity);
    }
    let source = String::from_utf8(bytes.to_vec()).map_err(|_| ImportError::InvalidRecipe)?;
    ImportRecipe::new("spec/json/v1".into(), source)
}

impl SpecDocuments for ApiLibrary {
    fn capture(
        &self,
        location: &str,
        limit: usize,
    ) -> std::result::Result<ImportRecipe, ImportError> {
        validate_spec_url(location)?;
        let body = read_url(location, limit.min(max_source()))?;
        let parsed: Option<Value> = serde_json::from_str(body.trim_start_matches('\u{feff}')).ok();
        if !parsed.as_ref().is_some_and(|v| v.get("version").is_some()) {
            return Err(failure(
                "Import requires a ready wes spec. Convert OpenAPI JSON/YAML with :describe url:\"...\" provider:name, review it in /spec, then import the saved descriptor.",
            ));
        }
        recipe(body.as_bytes(), limit)
    }
}

/// Installed resource directory resolved by the native host for its platform bundle layout.
static BUNDLED_RESOURCES: std::sync::OnceLock<PathBuf> = std::sync::OnceLock::new();
/// The extractor's file name; Windows runs only executables with their `.exe` suffix.
fn extractor_name() -> String {
    format!("wes-extract{}", std::env::consts::EXE_SUFFIX)
}

/// Record the host's installed resource directory once at startup; later calls are ignored.
///
/// # Arguments
/// * `directory` - The resource directory the native bundle installs `wes-extract` into.
pub fn use_bundled_resources(directory: PathBuf) {
    let _ = BUNDLED_RESOURCES.set(directory);
}

pub(super) fn bundled_extractor() -> Option<PathBuf> {
    // A command-line binary inside a macOS application bundle has no native host to register.
    let macos_bundle = std::env::current_exe()
        .ok()
        .and_then(|exe| exe.parent().map(|directory| directory.join("../Resources")));
    let development = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../tools/describe");
    locate_extractor(
        BUNDLED_RESOURCES
            .get()
            .cloned()
            .into_iter()
            .chain(macos_bundle)
            .chain([development]),
    )
}

fn locate_extractor(directories: impl IntoIterator<Item = PathBuf>) -> Option<PathBuf> {
    directories
        .into_iter()
        .map(|directory| directory.join(extractor_name()))
        .find(|candidate| candidate.is_file())
}

#[cfg(test)]
mod extractor_location_tests {
    use super::*;

    #[test]
    fn should_prefer_the_first_directory_holding_the_extractor() {
        // Arrange
        let missing = tempfile::tempdir().unwrap();
        let installed = tempfile::tempdir().unwrap();
        let development = tempfile::tempdir().unwrap();
        for directory in [installed.path(), development.path()] {
            std::fs::write(directory.join(extractor_name()), "").unwrap();
        }

        // Act
        let located = locate_extractor([
            missing.path().into(),
            installed.path().into(),
            development.path().into(),
        ]);

        // Assert
        assert_eq!(located, Some(installed.path().join(extractor_name())));
    }

    #[test]
    fn should_report_no_extractor_when_no_directory_holds_one() {
        // Arrange
        let empty = tempfile::tempdir().unwrap();

        // Act
        let located = locate_extractor([empty.path().into()]);

        // Assert
        assert_eq!(located, None);
    }
}
