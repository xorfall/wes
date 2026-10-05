//! Shared bounded local input files, not a managed-store directory or path sandbox.
use cap_fs_ext::OpenOptionsSyncExt;
use cap_std::{
    ambient_authority,
    fs::{File, OpenOptions},
};
use std::{
    io::{ErrorKind, Read},
    path::{Path, PathBuf},
};
use thiserror::Error;

#[derive(Clone, Copy, Debug, Error)]
pub(crate) enum InputFileError {
    #[error("invalid local input path")]
    InvalidPath,
    #[error("local input I/O failed: {0}")]
    Io(ErrorKind),
    #[error("local input is not a regular file")]
    NotRegularFile,
    #[error("local input exceeds its byte budget (at least {observed} bytes; limit {limit} bytes)")]
    TooLarge { observed: u64, limit: usize },
    #[error("local input is not UTF-8 text")]
    InvalidText,
}
pub(crate) struct InputFiles {
    base: PathBuf,
}
impl InputFiles {
    /// Capture the relative-path base at construction. Absolute paths and ordinary file symlinks
    /// retain normal local-file semantics; this reader is explicitly not a confinement boundary.
    /// Construction and reads are blocking and belong on the input worker, never the state actor.
    pub fn new(base: impl AsRef<Path>) -> Result<Self, InputFileError> {
        let base = base
            .as_ref()
            .canonicalize()
            .map_err(|error| InputFileError::Io(error.kind()))?;
        if !base.is_dir() {
            return Err(InputFileError::Io(ErrorKind::NotADirectory));
        }
        Ok(Self { base })
    }
}
impl std::fmt::Debug for InputFiles {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("InputFiles").finish_non_exhaustive()
    }
}
impl InputFiles {
    pub fn resolve(&self, path: &str) -> Result<PathBuf, InputFileError> {
        if path.trim().is_empty() || path.len() > 4096 || path.chars().any(char::is_control) {
            return Err(InputFileError::InvalidPath);
        }
        let path = Path::new(path);
        #[cfg(windows)]
        if !path.is_absolute()
            && matches!(
                path.components().next(),
                Some(std::path::Component::Prefix(_))
            )
        {
            // C:relative depends on the process's per-drive working directory, not the captured base.
            return Err(InputFileError::InvalidPath);
        }
        Ok(if path.is_absolute() {
            path.to_path_buf()
        } else {
            self.base.join(path)
        })
    }
    /// Explain a failed read at the adapter boundary, without including input contents.
    pub fn explain(&self, path: &str, error: InputFileError) -> String {
        let (reason, hint) = match error {
            InputFileError::InvalidPath => return "invalid local input path. Use a non-empty path of at most 4096 bytes without control characters.".into(),
            InputFileError::Io(ErrorKind::NotFound) => ("file not found".into(), "Check the path or supply an absolute file path."),
            InputFileError::Io(ErrorKind::PermissionDenied) => ("permission denied".into(), "Check the file and parent directory permissions."),
            InputFileError::Io(kind) => (format!("could not read file ({kind})"), "Check that the file is accessible and readable."),
            InputFileError::NotRegularFile => ("not a regular file".into(), "Supply a file path, not a directory or special file."),
            InputFileError::TooLarge { observed, limit } => (format!("file too large (at least {observed} bytes; limit {limit} bytes)"), "Use a smaller input file."),
            InputFileError::InvalidText => ("file is not valid UTF-8 text".into(), "Save the file as UTF-8 text."),
        };
        // Debug path formatting escapes control characters in filesystem-provided names.
        // Do not include the original path when validation fails.
        let Ok(resolved) = self.resolve(path) else {
            return format!("{reason}. {hint}");
        };
        let base = if Path::new(path).is_absolute() {
            String::new()
        } else {
            format!(" Relative-path base: {:?}.", self.base)
        };
        format!("{reason}. Resolved path: {resolved:?}.{base} {hint}")
    }
    pub fn read(&self, path: &str, max_bytes: usize) -> Result<String, InputFileError> {
        let path = self.resolve(path)?;
        let max_bytes = max_bytes.min(64 * 1024 * 1024);
        let before = std::fs::metadata(&path).map_err(|error| InputFileError::Io(error.kind()))?;
        if !before.is_file() {
            return Err(InputFileError::NotRegularFile);
        }
        if before.len() > max_bytes as u64 {
            return Err(InputFileError::TooLarge {
                observed: before.len(),
                limit: max_bytes,
            });
        }
        let mut options = OpenOptions::new();
        options.read(true).nonblock(true);
        let file = File::open_ambient_with(&path, &options, ambient_authority())
            .map_err(|error| InputFileError::Io(error.kind()))?;
        let opened = file
            .metadata()
            .map_err(|error| InputFileError::Io(error.kind()))?;
        if !opened.is_file() {
            return Err(InputFileError::NotRegularFile);
        }
        if opened.len() > max_bytes as u64 {
            return Err(InputFileError::TooLarge {
                observed: opened.len(),
                limit: max_bytes,
            });
        }
        let mut bytes = Vec::new();
        // Bound bytes actually read, not merely the potentially stale metadata length. Regular
        // files may still block despite nonblock; each owning capture joins this worker's exit.
        file.take(max_bytes as u64 + 1)
            .read_to_end(&mut bytes)
            .map_err(|error| InputFileError::Io(error.kind()))?;
        if bytes.len() > max_bytes {
            return Err(InputFileError::TooLarge {
                observed: bytes.len() as u64,
                limit: max_bytes,
            });
        }
        String::from_utf8(bytes).map_err(|_| InputFileError::InvalidText)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn io_explanations_distinguish_permissions_missing_and_other_failures() {
        let root = tempfile::tempdir().unwrap();
        let files = InputFiles::new(root.path()).unwrap();
        for (kind, reason) in [
            (ErrorKind::PermissionDenied, "permission denied"),
            (ErrorKind::NotFound, "file not found"),
            (ErrorKind::Interrupted, "could not read file"),
        ] {
            let message = files.explain("env.yaml", InputFileError::Io(kind));
            assert!(message.contains(reason), "{message}");
            assert!(message.contains("Relative-path base:"));
            assert!(message.contains("env.yaml"));
        }
    }

    #[cfg(unix)] // Windows does not allow control characters in directory names.
    #[test]
    fn bounded_reads_accept_exact_limit_and_escape_diagnostic_paths() {
        let root = tempfile::tempdir().unwrap();
        let base = root.path().join("folder\nwith-control");
        std::fs::create_dir(&base).unwrap();
        std::fs::write(base.join("exact"), "1234").unwrap();
        let files = InputFiles::new(&base).unwrap();
        assert_eq!(files.read("exact", 4).unwrap(), "1234");
        let error = files.read("exact", 3).unwrap_err();
        assert!(matches!(
            error,
            InputFileError::TooLarge {
                observed: 4,
                limit: 3
            }
        ));
        let explanation = files.explain("missing", files.read("missing", 4).unwrap_err());
        assert!(!explanation.contains('\n'));
        assert!(explanation.contains("folder\\nwith-control"));
        let invalid = files.explain("bad\npath", files.read("bad\npath", 4).unwrap_err());
        assert!(invalid.contains("invalid local input path"));
        assert!(!invalid.contains("bad"));
    }
}
