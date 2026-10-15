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
            declaration: Some("provider"),
            parameters: vec![Parameter::new(
                "file",
                Shape::Primitive(Primitive::Text),
                true,
            )],
            summary: Some(
                "Import a declarative YAML executable provider from file:. Read declaration for accepted fields, variants, defaults and semantic constraints. Relative executables resolve from the declaration file directory. Process operations use typed literal argv mappings. Any operation may declare fixed_arguments: {parameter: literal}. Constants cross the declared contracts at import using the shared JSON codec, with an aggregate 65536-byte/10000-node map budget. Strict package literals preserve exact numbers and typed temporal/canonical Bytes context; present optional values are lifted, while null tokens remain unsupported by the package grammar. Fixed fields are absent from caller parameters and cannot be supplied or overridden, including by direct adapter callers. Process argv, session Start and exclusive resource keys use the same captured constants. Source replacement or file edits cannot retarget old invocations. Fixed values are ordinary declaration configuration; credential slots remain separate. A streaming session with typed controls may declare source_profile: {name: lab, kind: metrics, default: true, stop: literal, ready_wait_ms: 10000, tail_ms: 5000, control_ms: 5000, grace_ms: 5000}. Required arguments must be fixed. Its optional narrowing map names distinct optional parameters: kind: maximum uses maximum_from: [fixedArgument, field] for a positive Int ceiling, while kind: backend-condition passes an additional Text condition for backend validation before effects. Names/defaults, bounds and typed stop are frozen declaration metadata for native composition; they grant no execution or controls and introduce no command syntax. A byte_input parameter cannot be fixed because a declaration literal carries no native byte-source owner. A finite local process may declare byte_input: {parameter: source, max_bytes: 1048576} for one required unconstrained Bytes parameter that is absent from argv. Explicit native invocation captures an acknowledged literal/static result field and streams original bytes through bounded owned stdin; WES_BYTE_INPUT_BYTES supplies the exact native extent, overriding any configured variable of that name; missing input does not run its producer. Destination transfer and source read authority remain separate; cooperative calls and unsupported targets refuse. A stored input is preserved as a named input dependency on its typed derived result; explicit Keep protects the raw closure and dependency:input native byte reads resolve it. Copies of ordinary data/provenance grant no dependency authority. Existing manifest metadata ceilings apply to the derived shell. record-bytes output declares type and byte_field: a four-byte big-endian JSON-header length (1–262144), a typed record with an empty Bytes placeholder, then the original binary leaf to EOF. Finite transient execution stores this nonexpanding stream through bounded acquisition; ordinary execution retains the process output cap. Malformed headers or unsuccessful exits publish no result and are not replayed. Session operations use versioned bounded duplex frames, scoped credential handoff, actual readiness and typed events/controls. A streaming local session may declare exclusive: [{namespace: resource, fields: [[scope, name]]}]. Each key selects 1–8 required Text leaves from validated arguments; at most four distinct namespaces reserve atomically before provider entry. Namespaces are process-local and must be shared by aliases addressing the same physical resource. Keys allow 4096 bytes per part and 8192 per resource; blank/control-containing keys, invalid paths and conflicts refuse without launching the source. The shared owner retains reservations through cancellation, producer/controller cleanup and final publication; only physical completion releases them. This grants no permission, does not queue or retry, and does not coordinate other processes or remote targets. A streaming session may declare recording: {type, byte_field, max_bytes, max_duration_ms}; the fixed lifetime is 1–86400000 ms and the byte ceiling cannot exceed the native acquisition budget. Protected raw storage is reserved before launch; private/unknown and unsupported placement refuse. A typed recording shell with an empty Bytes leaf precedes Ready. bytes frames contain contiguous sequences and canonical base64 chunks of 1–65536 bytes; bytes-credit follows physical storage acknowledgement. A final recording shell and End confirm protocol completion. Exact process/read/write cleanup precedes publication; errors and watchdog preserve an explicitly incomplete acknowledged prefix, while failed physical writes refuse sealing. A failed observation/byte credit closes live admission and permits only a bounded terminal drain (at most the configured ready budget); a final recording shell and the exact original End must still validate. Cancellation, foreign/missing terminal receipts, additional live data or failed cleanup cannot become completion. No control or source operation is retried. :source result reads only a physically joined original recording into a native SourceRecording with source/run/acceptedBytes/complete/termination/value; complete describes the protocol, never complete network traffic. It never acquires, stops, reconnects or replays a source. Keep and read authority remain independent. Optional read_access declares required Text scope parameters and one finite Bool revalidate operation. Only explicit native revalidation returning true opens that captured scope; credentials, retained bytes and re-enabling do not grant reads or revive a source. Read domains require managed environment binding. No executable is launched by import. Session targets must be local. Replacing a definition requires replace:true; previous work keeps its captured revision.",
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
    let definition = super::schema::compile(&source, None).map_err(ImportError::from)?;
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
        super::schema::compile(&capture.source, alias).map_err(ImportError::from)?;
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
