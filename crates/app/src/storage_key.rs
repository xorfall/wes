//! A startup-only key provider. Key bytes never enter workspace declarations or transports.
use cap_fs_ext::{FollowSymlinks, OpenOptionsFollowExt};
use cap_std::{
    ambient_authority,
    fs::{Dir, OpenOptions},
};
use std::{
    io::{self, Read},
    path::Path,
    sync::Arc,
};
use wes_adapters::protected_storage::ObjectProtection;
use zeroize::Zeroizing;

pub(crate) fn load(path: &Path, home: &Path, identity: &str) -> io::Result<Arc<ObjectProtection>> {
    let invalid = || {
        io::Error::other(
            "storage key must be a separate, owner-only regular file containing exactly 32 bytes; no key was created or replaced",
        )
    };
    let absolute = if path.is_absolute() {
        path.to_owned()
    } else {
        std::env::current_dir()?.join(path)
    };
    let parent = absolute.parent().ok_or_else(invalid)?.canonicalize()?;
    let home = home.canonicalize()?;
    // A key copied with the data home does not protect that copy. Require independent provisioning.
    if parent.starts_with(&home) {
        return Err(invalid());
    }
    let name = absolute.file_name().ok_or_else(invalid)?;
    let dir = Dir::open_ambient_dir(parent, ambient_authority())?;
    let mut options = OpenOptions::new();
    options.read(true).follow(FollowSymlinks::No);
    let file = dir.open_with(name, &options)?;
    let metadata = file.metadata()?;
    if !metadata.is_file() || metadata.len() != 32 {
        return Err(invalid());
    }
    #[cfg(unix)]
    {
        use cap_std::fs::PermissionsExt;
        if metadata.permissions().mode() & 0o077 != 0 {
            return Err(invalid());
        }
    }
    let mut bytes = Zeroizing::new(Vec::new());
    file.take(33).read_to_end(&mut bytes)?;
    if bytes.len() != 32 {
        return Err(invalid());
    }
    ObjectProtection::new(
        identity,
        bytes.as_slice().try_into().map_err(|_| invalid())?,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn key_is_explicit_bounded_and_separate_from_data() {
        let root = tempfile::tempdir().unwrap();
        let home = root.path().join("home");
        std::fs::create_dir(&home).unwrap();
        let key = root.path().join("key");
        std::fs::write(&key, [4; 32]).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&key, std::fs::Permissions::from_mode(0o600)).unwrap();
        }
        let id = uuid::Uuid::new_v4().to_string();
        assert!(load(&key, &home, &id).is_ok());
        std::fs::copy(&key, home.join("key")).unwrap();
        assert!(load(&home.join("key"), &home, &id).is_err());
        std::fs::write(&key, [4; 33]).unwrap();
        assert!(load(&key, &home, &id).is_err());
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink(&key, root.path().join("link")).unwrap();
            assert!(load(&root.path().join("link"), &home, &id).is_err());
        }
    }
}
