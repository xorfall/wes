//! Native-only per-pane shell history. No terminal output or shell source is interpreted here.
use super::TerminalSession;
use cap_fs_ext::{DirExt, FollowSymlinks, OpenOptionsFollowExt, OpenOptionsSyncExt};
use cap_std::{
    ambient_authority,
    fs::{Dir, OpenOptions},
};
use std::{
    collections::{BTreeMap, BTreeSet},
    io::{self, Read},
    path::{Path, PathBuf},
    sync::Weak,
};

#[derive(Default)]
pub(super) struct Registry {
    pub closing: bool,
    pub active: BTreeMap<String, Weak<TerminalSession>>,
    // Session-local tombstones block delayed UI requests after explicit pane deletion.
    pub forgotten: BTreeSet<String>,
}

pub(super) fn key(value: &str) -> io::Result<String> {
    let id = uuid::Uuid::parse_str(value)
        .map_err(|_| io::Error::other("Invalid terminal history identity."))?;
    if id.is_nil() || id.to_string() != value {
        return Err(io::Error::other("Invalid terminal history identity."));
    }
    Ok(value.into())
}

fn child(parent: &Dir, name: &str) -> io::Result<Dir> {
    match parent.create_dir(name) {
        Ok(()) => (),
        Err(error) if error.kind() == io::ErrorKind::AlreadyExists => (),
        Err(error) => return Err(error),
    }
    let directory = parent.open_dir_nofollow(name)?;
    #[cfg(unix)]
    {
        use cap_std::fs::PermissionsExt;
        directory.set_permissions(".", cap_std::fs::Permissions::from_mode(0o700))?;
    }
    Ok(directory)
}

pub(super) fn prepare(home: &Path, identity: &str, name: &str) -> io::Result<PathBuf> {
    let identity = key(identity)?;
    let home_dir = Dir::open_ambient_dir(home, ambient_authority())?;
    let root = child(&home_dir, "terminal-history")?;
    let directory = child(&root, &identity)?;
    // Discard only this shell's fixed private scratch name without following links.
    // A crash may leave a partial write; a pre-existing symlink must never be opened
    // by the shell's native history writer. The active lease excludes another writer.
    match directory.remove_file(format!("{name}.next")) {
        Ok(()) => (),
        Err(error) if error.kind() == io::ErrorKind::NotFound => (),
        Err(error) => return Err(error),
    }
    let mut options = OpenOptions::new();
    options
        .read(true)
        .write(true)
        .create(true)
        .follow(FollowSymlinks::No)
        .nonblock(true);
    #[cfg(unix)]
    {
        use cap_std::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = directory.open_with(name, &options)?;
    let metadata = file.metadata()?;
    // Bounded read, including maliciously replaced FIFOs/non-files. Shell formats are text;
    // rejected files remain intact, so closing the pane is an explicit way to discard them.
    fn max_bytes() -> u64 {
        wes_budgets::get("terminal.history.bytes") as u64
    }
    if !metadata.is_file() || metadata.len() > max_bytes() {
        return Err(io::Error::other(
            "Invalid or oversized terminal history; close this pane and open a new one.",
        ));
    }
    let mut bytes = Vec::new();
    (&mut file).take(max_bytes() + 1).read_to_end(&mut bytes)?;
    if bytes.len() as u64 > max_bytes() || !valid_records(&bytes) {
        return Err(io::Error::other(
            "Invalid terminal history; close this pane and open a new one.",
        ));
    }
    #[cfg(unix)]
    {
        use cap_std::fs::PermissionsExt;
        file.set_permissions(cap_std::fs::Permissions::from_mode(0o600))?;
    }
    Ok(home.join("terminal-history").join(identity).join(name))
}

fn valid_records(bytes: &[u8]) -> bool {
    // Every shell uses private UTF-8 commands separated by NUL, never shell source.
    std::str::from_utf8(bytes).is_ok()
        && (bytes.is_empty()
            || (bytes.last() == Some(&0)
                && bytes.iter().filter(|b| **b == 0).count() <= 1000
                && bytes[..bytes.len() - 1]
                    .split(|b| *b == 0)
                    .all(|record| !record.is_empty())))
}

pub(super) fn forget(home: &Path, identity: &str) -> io::Result<()> {
    let identity = key(identity)?;
    let home = Dir::open_ambient_dir(home, ambient_authority())?;
    let root = match home.open_dir_nofollow("terminal-history") {
        Ok(root) => root,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error),
    };
    // Never traverse symlinked pane directories, even when forgetting corrupt history.
    match root.open_dir_nofollow(&identity) {
        Ok(directory) => {
            // Windows does not remove a directory that is still open.
            drop(directory);
            root.remove_dir_all(&identity)
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error),
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::os::unix::fs::{PermissionsExt, symlink};
    #[test]
    fn shell_records_are_bounded_and_validated_in_their_actual_encoding() {
        assert!(valid_records(b"printf first\nsecond\0: last\0"));
        assert!(!valid_records(b"truncated"));
        assert!(!valid_records(b"\0"));
        assert!(!valid_records(&b": x\0".repeat(1001)));
        assert!(!valid_records(&[255, 0]));
        assert!(valid_records("ĞğİıŞşÇçÖöÜü\0".as_bytes()));
    }

    #[test]
    fn identities_private_paths_and_corrupt_history_are_checked() {
        let home = tempfile::tempdir().unwrap();
        let id = uuid::Uuid::new_v4().to_string();
        let path = prepare(home.path(), &id, "zsh").unwrap();
        assert_eq!(
            std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o600
        );
        assert_eq!(
            std::fs::metadata(path.parent().unwrap())
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o700
        );
        assert_eq!(
            std::fs::metadata(home.path().join("terminal-history"))
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o700
        );
        for bad in [
            "../other",
            "p2",
            "00000000-0000-0000-0000-000000000000",
            &id.to_uppercase(),
        ] {
            assert!(prepare(home.path(), bad, "zsh").is_err());
        }
        std::fs::write(&path, b"bad\0history").unwrap();
        assert!(prepare(home.path(), &id, "zsh").is_err());
        std::fs::write(&path, [255]).unwrap();
        assert!(prepare(home.path(), &id, "zsh").is_err());
        std::fs::remove_file(&path).unwrap();
        let sentinel = home.path().join("untouched");
        std::fs::write(&sentinel, "safe").unwrap();
        symlink(&sentinel, &path).unwrap();
        assert!(prepare(home.path(), &id, "zsh").is_err());
        forget(home.path(), &id).unwrap();
        assert_eq!(std::fs::read_to_string(sentinel).unwrap(), "safe");
        assert!(!path.parent().unwrap().exists());
        forget(home.path(), &id).unwrap();
        let path = prepare(home.path(), &id, "bash").unwrap();
        let sentinel = home.path().join("scratch-sentinel");
        std::fs::write(&sentinel, "safe").unwrap();
        symlink(&sentinel, path.with_extension("next")).unwrap();
        prepare(home.path(), &id, "bash").unwrap();
        assert!(!path.with_extension("next").exists());
        assert_eq!(std::fs::read_to_string(&sentinel).unwrap(), "safe");
        std::fs::remove_file(&path).unwrap();
        std::fs::create_dir(&path).unwrap();
        assert!(prepare(home.path(), &id, "bash").is_err());
        forget(home.path(), &id).unwrap();
        symlink(home.path(), path.parent().unwrap()).unwrap();
        assert!(prepare(home.path(), &id, "bash").is_err());
        assert!(forget(home.path(), &id).is_err());
    }
}
