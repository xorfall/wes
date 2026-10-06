//! Private shell startup files; user configuration is never sourced or changed.
use std::{
    io::{self, Write},
    path::Path,
};

#[cfg(unix)]
pub(super) fn prepare(directory: &Path, zsh: bool) -> io::Result<std::path::PathBuf> {
    let directory = directory.join("prompt");
    std::fs::create_dir(&directory)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&directory, std::fs::Permissions::from_mode(0o700))?;
    }
    if zsh {
        // zsh must enable RCS to read only this private .zshrc. -d and this .zshenv prevent
        // global rc files from subsequently enabling the user's initialization paths.
        std::fs::write(directory.join(".zshenv"), "unsetopt GLOBAL_RCS\n")?;
        std::fs::write(directory.join(".zshrc"), include_str!("prompt.zsh"))?;
        Ok(directory)
    } else {
        let rc = directory.join("bashrc");
        std::fs::write(&rc, include_str!("prompt.bash"))?;
        Ok(rc)
    }
}

/// One complete UTF-8 line, never shell source. Rename prevents readers seeing a partial update.
pub(super) fn environment_file(directory: &Path) -> std::path::PathBuf {
    directory.join("prompt-environment")
}

pub(super) fn publish_environment(directory: &Path, name: &str) -> io::Result<()> {
    let label: String = name
        .chars()
        .map(|c| if c.is_control() { '?' } else { c })
        .collect();
    let mut temporary = tempfile::NamedTempFile::new_in(directory)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        temporary
            .as_file()
            .set_permissions(std::fs::Permissions::from_mode(0o600))?;
    }
    writeln!(temporary, "{label}")?;
    temporary
        .persist(environment_file(directory))
        .map_err(|error| error.error)?;
    Ok(())
}

/// Only absolute native PATH entries are eligible; never execute a repository-local git shim.
pub(super) fn git() -> Option<std::path::PathBuf> {
    let path = std::env::var_os("PATH").unwrap_or_default();
    let directories = std::env::split_paths(&path).chain(
        FALLBACK_DIRECTORIES
            .iter()
            .copied()
            .map(std::path::PathBuf::from),
    );
    directories
        .filter(|directory| directory.is_absolute())
        .map(|directory| directory.join(GIT))
        .find(|candidate| executable(candidate) && system_git_ready(candidate))
}
#[cfg(unix)]
const GIT: &str = "git";
#[cfg(unix)]
const FALLBACK_DIRECTORIES: &[&str] = &["/opt/homebrew/bin", "/usr/local/bin", "/usr/bin", "/bin"];
#[cfg(windows)]
const GIT: &str = "git.exe";
#[cfg(windows)]
const FALLBACK_DIRECTORIES: &[&str] = &[];
#[cfg(windows)]
fn executable(path: &Path) -> bool {
    path.is_file()
}
#[cfg(unix)]
fn executable(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    std::fs::metadata(path)
        .is_ok_and(|metadata| metadata.is_file() && metadata.permissions().mode() & 0o111 != 0)
}
#[cfg(target_os = "macos")]
fn system_git_ready(path: &Path) -> bool {
    let Ok(path) = std::fs::canonicalize(path) else {
        return false;
    };
    if path != Path::new("/usr/bin/git") && path != Path::new("/bin/git") {
        return true;
    }
    // Apple's launcher may offer to install developer tools. Probe xcode-select instead, once
    // at terminal creation, and leave the optional Git suffix absent when no tools are selected.
    wes_adapters::process::serialized_spawn(|| {
        std::process::Command::new("/usr/bin/xcode-select")
            .arg("-p")
            .stdin(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .stdout(std::process::Stdio::piped())
            .spawn()
    })
    .and_then(|child| child.wait_with_output())
    .is_ok_and(|result| {
        result.status.success()
            && String::from_utf8(result.stdout)
                .is_ok_and(|directory| executable(&Path::new(directory.trim()).join("usr/bin/git")))
    })
}
#[cfg(not(target_os = "macos"))]
fn system_git_ready(_: &Path) -> bool {
    true
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    #[test]
    fn environment_label_is_private_atomic_plaintext_and_clears_prior_selection() {
        let directory = tempfile::tempdir().unwrap();
        let label = "Üretim%F{red}$(touch BAD)`touch BAD`\\e[31m\n\u{1b}\u{85}";
        publish_environment(directory.path(), label).unwrap();
        let path = environment_file(directory.path());
        assert_eq!(
            std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o600
        );
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            "Üretim%F{red}$(touch BAD)`touch BAD`\\e[31m???\n"
        );
        let old = std::fs::File::open(&path).unwrap();
        publish_environment(directory.path(), "no-env").unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "no-env\n");
        use std::io::Read;
        let mut previous = String::new();
        (&old).read_to_string(&mut previous).unwrap();
        assert!(
            previous.starts_with("Üretim"),
            "an already-open reader keeps a complete old record"
        );
        assert_eq!(std::fs::read_dir(directory.path()).unwrap().count(), 1);
    }
}
