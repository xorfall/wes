//! Complete local file snapshots; publication makes no directory-durability claim.
use std::{fs::File, io, path::Path};

/// Stage beside the destination and close the writer before one native atomic rename.
/// Open readers retain the previous file. No remove-then-write fallback or retry.
pub(crate) fn write(
    destination: &Path,
    encode: impl FnOnce(&mut File) -> io::Result<()>,
) -> io::Result<()> {
    let parent = destination.parent().ok_or(io::ErrorKind::InvalidInput)?;
    let mut temporary = tempfile::NamedTempFile::new_in(parent)?;
    encode(temporary.as_file_mut())?;
    // keep clears the platform's temporary-file attributes before publication. The
    // TempPath then owns failure cleanup, while std supplies native rename semantics.
    let (file, pending) = temporary.keep().map_err(|error| error.error)?;
    drop(file);
    let mut pending = tempfile::TempPath::try_from_path(pending)?;
    std::fs::rename(&pending, destination)?;
    pending.disable_cleanup(true);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Write};

    #[test]
    fn open_readers_keep_the_complete_old_snapshot_during_replacement() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("snapshot");
        write(&path, |file| file.write_all(b"old synthetic snapshot")).unwrap();
        let mut reader = File::open(&path).unwrap();
        write(&path, |file| file.write_all(b"new synthetic snapshot")).unwrap();
        let mut old = String::new();
        reader.read_to_string(&mut old).unwrap();
        assert_eq!(old, "old synthetic snapshot");
        assert_eq!(std::fs::read(path).unwrap(), b"new synthetic snapshot");
    }

    #[test]
    fn failed_encoding_preserves_the_previous_snapshot_and_cleans_staging() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("snapshot");
        std::fs::write(&path, b"complete original").unwrap();
        let error = write(&path, |file| {
            file.write_all(b"incomplete replacement")?;
            Err(io::ErrorKind::InvalidData.into())
        })
        .unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::InvalidData);
        assert_eq!(std::fs::read(&path).unwrap(), b"complete original");
        assert_eq!(std::fs::read_dir(root.path()).unwrap().count(), 1);
    }
}
