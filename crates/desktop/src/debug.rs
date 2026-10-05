//! Opt-in, debug-build-only isolation for a debugger-launched desktop.
use std::{ffi::OsString, io, path::PathBuf};

pub(crate) struct Paths {
    pub user_home: PathBuf,
    pub diagnostics: Option<PathBuf>,
    pub isolated: bool,
}

impl Paths {
    pub fn resolve(
        debug_build: bool,
        override_home: Option<OsString>,
        user_home: Option<PathBuf>,
        config: Option<PathBuf>,
    ) -> io::Result<Self> {
        if let Some(home) = override_home.filter(|_| debug_build) {
            let home = PathBuf::from(home);
            if !home.is_absolute() {
                return Err(io::Error::other("WES_DEBUG_HOME must be an absolute path"));
            }
            std::fs::create_dir_all(&home)?;
            return Ok(Self {
                diagnostics: Some(home.join("diagnostics")),
                user_home: home,
                isolated: true,
            });
        }
        Ok(Self {
            user_home: user_home
                .ok_or_else(|| io::Error::other("Cannot determine the user home directory."))?,
            diagnostics: config.map(|path| path.join("diagnostics")),
            isolated: false,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn debug_override_isolated_and_invalid_values_never_fall_back() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("debug user");
        let paths = Paths::resolve(true, Some(root.clone().into()), None, None).unwrap();
        assert_eq!(paths.user_home, root);
        assert_eq!(paths.diagnostics, Some(root.join("diagnostics")));
        assert!(paths.isolated);
        assert!(root.is_dir());
        for invalid in ["", "relative"] {
            assert!(
                Paths::resolve(true, Some(invalid.into()), Some(temp.path().into()), None).is_err()
            );
        }
        let file = temp.path().join("file");
        std::fs::write(&file, "fixture").unwrap();
        assert!(Paths::resolve(true, Some(file.into()), None, None).is_err());
    }

    #[test]
    fn normal_and_release_launches_preserve_existing_paths() {
        let temp = tempfile::tempdir().unwrap();
        let user = temp.path().join("regular");
        let config = temp.path().join("config");
        let debug = temp.path().join("unused");
        for (enabled, value) in [(true, None), (false, Some(debug.clone().into()))] {
            let paths =
                Paths::resolve(enabled, value, Some(user.clone()), Some(config.clone())).unwrap();
            assert_eq!(paths.user_home, user);
            assert_eq!(paths.diagnostics, Some(config.join("diagnostics")));
            assert!(!paths.isolated);
        }
        assert!(!debug.exists());
    }
}
