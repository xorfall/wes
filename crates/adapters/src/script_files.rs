//! Capture existing wes source, not a new interpreter or a path confinement boundary.
use crate::input_files::InputFiles;
use std::path::{Path, PathBuf};
use thiserror::Error;

pub struct ScriptFile {
    pub path: PathBuf,
    pub directory: PathBuf,
    pub text: String,
}
#[derive(Clone, Copy, Debug, Error)]
#[error("{0}")]
pub struct ScriptFileError(&'static str);

impl ScriptFile {
    /// Blocking: run on a joined input worker before opening the workspace. Preserve exact bytes
    /// after UTF-8 validation; no newline/BOM/shebang rewriting or later source reread.
    pub fn read(base: &Path, path: &Path) -> Result<Self, ScriptFileError> {
        let path = path
            .to_str()
            .ok_or(ScriptFileError("script path must be UTF-8"))?;
        let files = InputFiles::new(base).map_err(convert)?;
        let path = files.resolve(path).map_err(convert)?;
        let directory = path
            .parent()
            .ok_or(ScriptFileError("invalid script directory"))?
            .canonicalize()
            .map_err(|_| ScriptFileError("script directory is unavailable"))?;
        let path = directory.join(
            path.file_name()
                .ok_or(ScriptFileError("invalid script filename"))?,
        );
        let text = files
            .read(
                path.to_str()
                    .ok_or(ScriptFileError("script path must be UTF-8"))?,
                wes_engine::source::max_source_bytes(),
            )
            .map_err(convert)?;
        Ok(Self {
            path,
            directory,
            text,
        })
    }
}
fn convert(error: crate::input_files::InputFileError) -> ScriptFileError {
    use crate::input_files::InputFileError::*;
    ScriptFileError(match error {
        InvalidPath => "invalid script path",
        Io(_) | NotRegularFile => "script is unavailable or not a regular file",
        TooLarge { .. } => "script exceeds the 1 MiB source limit",
        InvalidText => "script is not UTF-8 text",
    })
}
