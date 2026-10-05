//! The type-package reader port backed by shared bounded local input I/O.
use crate::input_files::{InputFileError, InputFiles};
use crate::source_archive::{SourceArchive, SourceKind};
use std::{path::Path, sync::Arc};
use wes_engine::type_sources::{TypeSourceError, TypeSourceReader, max_capture_bytes};

pub struct FileTypeSources {
    files: InputFiles,
    archive: Option<Arc<dyn SourceArchive>>,
}
impl std::fmt::Debug for FileTypeSources {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FileTypeSources").finish_non_exhaustive()
    }
}
impl FileTypeSources {
    /// Capture the relative-path base. Absolute paths and ordinary symlinks remain available;
    /// this is not a path sandbox. Construct/read on a joined input worker, not the state actor.
    pub fn new(base: impl AsRef<Path>) -> Result<Self, TypeSourceError> {
        Ok(Self {
            files: InputFiles::new(base).map_err(convert)?,
            archive: None,
        })
    }
    pub fn with_archive(mut self, archive: Arc<dyn SourceArchive>) -> Self {
        self.archive = Some(archive);
        self
    }
}
impl TypeSourceReader for FileTypeSources {
    fn retain_text(&self, origin: &str, source: &str) -> Result<(), TypeSourceError> {
        if source.len() > max_capture_bytes() {
            return Err(TypeSourceError::TooLarge);
        }
        if let Some(archive) = &self.archive {
            archive
                .retain(SourceKind::Types, origin, source_format(source), source)
                .map_err(|_| TypeSourceError::Persistence)?;
        }
        Ok(())
    }
    fn read(&self, path: &str, max_bytes: usize) -> Result<String, TypeSourceError> {
        let source = self
            .files
            .read(path, max_bytes.min(max_capture_bytes()))
            .map_err(convert)?;
        if let Some(archive) = &self.archive {
            let origin = self.files.resolve(path).map_err(convert)?;
            archive
                .retain(
                    SourceKind::Types,
                    &origin.to_string_lossy(),
                    source_format(&source),
                    &source,
                )
                .map_err(|_| TypeSourceError::Persistence)?;
        }
        Ok(source)
    }
}
fn convert(error: InputFileError) -> TypeSourceError {
    match error {
        InputFileError::InvalidPath => TypeSourceError::InvalidPath,
        InputFileError::Io(_) | InputFileError::NotRegularFile => TypeSourceError::Unavailable,
        InputFileError::TooLarge { .. } => TypeSourceError::TooLarge,
        InputFileError::InvalidText => TypeSourceError::InvalidText,
    }
}

fn source_format(source: &str) -> &'static str {
    if wes_views::Artifact::recognizes(source) {
        "view/compiled/1"
    } else {
        "types/yaml/v1"
    }
}
