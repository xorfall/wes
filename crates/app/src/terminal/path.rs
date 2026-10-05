//! Desktop launchers need not know about per-user command installations.
use std::{
    ffi::{OsStr, OsString},
    io,
    path::{Path, PathBuf},
};

pub(super) fn terminal(
    inherited: Option<&OsStr>,
    home: Option<&OsStr>,
    directory: &Path,
) -> io::Result<OsString> {
    let mut paths = vec![directory.join("assistants")];
    if let Some(inherited) = inherited.filter(|value| !value.is_empty()) {
        paths.extend(std::env::split_paths(inherited));
    }
    if let Some(home) = home.map(Path::new).filter(|home| home.is_absolute()) {
        let local = home.join(".local/bin");
        // A colon in HOME cannot be represented as one Unix PATH entry. Never split it
        // into unintended search locations. No user startup file needs to be evaluated.
        if std::env::join_paths([&local]).is_ok() {
            paths.push(local);
        }
    }
    paths.extend(["/opt/homebrew/bin", "/usr/local/bin", "/usr/bin", "/bin"].map(PathBuf::from));
    paths.push(directory.to_owned());
    std::env::join_paths(paths).map_err(io::Error::other)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::ffi::OsStrExt;

    #[test]
    fn inherited_precedence_and_non_utf8_paths_survive_local_fallback() {
        let inherited = OsStr::from_bytes(b"/preferred/bin:/non-utf8/\xff/bin");
        let value = terminal(
            Some(inherited),
            Some(OsStr::new("/synthetic/Üser Home")),
            Path::new("/bridge"),
        )
        .unwrap();
        let paths: Vec<_> = std::env::split_paths(&value).collect();
        assert_eq!(paths[0], Path::new("/bridge/assistants"));
        assert_eq!(paths[1], Path::new("/preferred/bin"));
        assert_eq!(paths[2].as_os_str().as_bytes(), b"/non-utf8/\xff/bin");
        assert_eq!(paths[3], Path::new("/synthetic/Üser Home/.local/bin"));
        assert_eq!(paths.last().unwrap(), Path::new("/bridge"));
    }

    #[test]
    fn missing_path_and_invalid_home_never_add_implicit_working_directory() {
        for inherited in [None, Some(OsStr::new(""))] {
            for home in [None, Some(""), Some("relative"), Some("/home/colon:name")] {
                let value =
                    terminal(inherited, home.map(OsStr::new), Path::new("/bridge")).unwrap();
                let paths: Vec<_> = std::env::split_paths(&value).collect();
                assert!(paths.iter().all(|path| path.is_absolute()));
                assert_eq!(paths.len(), 6);
            }
        }
    }
}
