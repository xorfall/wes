//! A Windows GUI process is not a console command. Pane programs use the bundled CLI
//! so the shell waits for them and their children inherit the pane's pseudoconsole.
use std::{
    io,
    path::{Path, PathBuf},
};

pub(super) fn program(
    resources: Option<&Path>,
    host: &Path,
    development: bool,
) -> io::Result<PathBuf> {
    if development {
        if let Some(program) = host
            .parent()
            .map(|root| root.join("wes.exe"))
            .filter(|path| path.is_file())
        {
            return Ok(program);
        }
    }
    if let Some(program) = resources
        .map(|root| root.join("wes-terminal.exe"))
        .filter(|path| path.is_file())
    {
        return Ok(program);
    }
    Err(io::Error::new(
        io::ErrorKind::NotFound,
        "The Windows terminal executable is missing. Reinstall the complete WesDesk package.",
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn a_release_host_requires_its_bundled_console_program() {
        let root = tempfile::tempdir().unwrap();
        let resources = root.path().join("resources");
        std::fs::create_dir(&resources).unwrap();
        let host = root.path().join("wes-desktop.exe");
        std::fs::write(root.path().join("wes.exe"), b"development").unwrap();
        assert!(program(Some(&resources), &host, false).is_err());
        assert_eq!(
            program(None, &host, true).unwrap(),
            root.path().join("wes.exe")
        );
        std::fs::write(resources.join("wes-terminal.exe"), b"bundled").unwrap();
        assert_eq!(
            program(Some(&resources), &host, false).unwrap(),
            resources.join("wes-terminal.exe")
        );
        assert_eq!(
            program(Some(&resources), &host, true).unwrap(),
            root.path().join("wes.exe")
        );
        std::fs::remove_file(root.path().join("wes.exe")).unwrap();
        assert_eq!(
            program(Some(&resources), &host, true).unwrap(),
            resources.join("wes-terminal.exe")
        );
        assert!(program(None, &host, false).is_err());
    }
}
