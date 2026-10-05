//! Owned portable engine state; the launcher remembers only a path outside the selected home.
pub mod host;
mod library;
pub mod sources;

use serde::{Deserialize, Serialize};
use std::{
    fs,
    io::{self, Read},
    path::{Path, PathBuf},
};
use wes_adapters::{
    api_library::{atomic_json, exclusive_lock},
    file_lock::ExclusiveLock,
};

pub type Error = Box<dyn std::error::Error + Send + Sync>;
const IDENTITY: &str = "identity.json";
const SELECTION: &str = "desktop-location.json";

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct Identity {
    version: u32,
    pub id: String,
}
impl Identity {
    pub fn keychain_account(&self) -> String {
        format!("data-home:{}", self.id)
    }
}

/// The root writer lock outlives the runtime and all its joined store workers.
pub struct DataHome {
    pub path: PathBuf,
    pub identity: Identity,
    _lock: ExclusiveLock,
}

pub fn default_home(user_home: &Path) -> PathBuf {
    user_home.join(".wes")
}

/// Expand only ~/; no shell interpolation. Resolve parent aliases, reject a linked root itself.
pub fn resolve(path: &Path) -> io::Result<PathBuf> {
    if !path.is_absolute()
        || path
            .components()
            .any(|c| matches!(c, std::path::Component::ParentDir))
    {
        return Err(io::Error::other(
            "Choose an absolute data folder without '..'.",
        ));
    }
    if fs::symlink_metadata(path).is_ok_and(|m| m.file_type().is_symlink()) {
        return Err(io::Error::other(
            "The data folder must not be a symbolic link.",
        ));
    }
    let mut ancestor = path;
    let mut missing = Vec::new();
    while !ancestor.exists() {
        missing.push(
            ancestor
                .file_name()
                .ok_or_else(|| io::Error::other("Invalid data folder."))?,
        );
        ancestor = ancestor
            .parent()
            .ok_or_else(|| io::Error::other("Invalid data folder."))?;
    }
    let mut result = ancestor.canonicalize()?;
    for name in missing.into_iter().rev() {
        result.push(name);
    }
    Ok(result)
}

pub fn expand(path: &str, user_home: &Path) -> io::Result<PathBuf> {
    if path.is_empty() || path.len() > 4096 || path.chars().any(char::is_control) {
        return Err(io::Error::other("Enter a valid data folder path."));
    }
    resolve(&if path == "~" {
        user_home.to_path_buf()
    } else if let Some(rest) = path.strip_prefix("~/") {
        user_home.join(rest)
    } else {
        PathBuf::from(path)
    })
}

pub(crate) fn private_directory(path: &Path) -> io::Result<()> {
    let mut builder = fs::DirBuilder::new();
    builder.recursive(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        builder.mode(0o700);
    }
    builder.create(path)?;
    let meta = fs::symlink_metadata(path)?;
    if !meta.is_dir() || meta.file_type().is_symlink() {
        return Err(io::Error::other(
            "Data directories must be ordinary directories.",
        ));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if meta.permissions().mode() & 0o077 != 0 {
            return Err(io::Error::other(
                "The data folder must be private (permissions 0700).",
            ));
        }
    }
    Ok(())
}

pub(crate) fn metadata<T: serde::de::DeserializeOwned>(path: &Path) -> io::Result<T> {
    let meta = fs::symlink_metadata(path)?;
    if !meta.is_file() || meta.file_type().is_symlink() || meta.len() > 16 * 1024 {
        return Err(io::Error::other("Invalid data-home metadata file."));
    }
    let mut bytes = Vec::new();
    fs::File::open(path)?
        .take(16 * 1024 + 1)
        .read_to_end(&mut bytes)?;
    serde_json::from_slice(&bytes).map_err(io::Error::other)
}

pub fn identity(path: &Path) -> io::Result<Option<Identity>> {
    let file = path.join(IDENTITY);
    match fs::symlink_metadata(&file) {
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e),
        Ok(_) => (),
    }
    let identity: Identity = metadata(&file)?;
    if identity.version != 1 || uuid::Uuid::parse_str(&identity.id).is_err() {
        return Err(io::Error::other(
            "Unsupported or invalid data-home identity.",
        ));
    }
    Ok(Some(identity))
}

/// Credential ownership is tied to the current data-home identity, never its path.
pub fn keychain_account(home: &Path) -> io::Result<String> {
    identity(home)?
        .map(|i| i.keychain_account())
        .ok_or_else(|| io::Error::other("The data folder has no wes identity."))
}

impl DataHome {
    pub fn open(path: &Path) -> Result<Self, Error> {
        let path = resolve(path)?;
        if !path.exists() {
            private_directory(&path)?;
        }
        // Never adopt unrelated files. Interrupted atomic writes are recognized but not removed.
        let previous = identity(&path)?;
        for entry in fs::read_dir(&path)? {
            let entry = entry?;
            let name = entry.file_name();
            let name = name
                .to_str()
                .ok_or_else(|| io::Error::other("Invalid data-home filename."))?;
            let known = matches!(
                name,
                IDENTITY
                    | SELECTION
                    | ".desktop-location.lock"
                    | ".wes-home.lock"
                    | "workspaces"
                    | "values"
                    | "api-library"
                    | "imports"
                    | "diagnostics"
                    | "terminal-history"
                    | "edit"
                    | "api-library-settings.json"
                    | "describe-settings.json"
                    | ".api-library.lock"
                    | "desktop-ui.json"
                    | "server.log"
                    | "server.pid"
                    | crate::credential_vault::VAULT_FILE
            ) || name.starts_with(".api-write-")
                || name.starts_with(crate::credential_vault::PENDING_PREFIX);
            let file_type = entry.file_type()?;
            if !known || file_type.is_symlink() || (name == "edit" && !file_type.is_dir()) {
                return Err(io::Error::other("Choose an empty folder or an existing wes data folder; unrelated files and links are not adopted.").into());
            }
            if previous.is_none()
                && matches!(
                    name,
                    "workspaces"
                        | "values"
                        | "api-library"
                        | "imports"
                        | "diagnostics"
                        | "terminal-history"
                        | "edit"
                        | "api-library-settings.json"
                        | "describe-settings.json"
                        | "desktop-ui.json"
                        | crate::credential_vault::VAULT_FILE
                )
            {
                return Err(io::Error::other("Existing data is missing its wes identity.").into());
            }
        }
        let lock = exclusive_lock(&path, ".wes-home.lock").map_err(|_| {
            io::Error::other("The data folder is already open or its lock is unavailable.")
        })?;
        // Store ownership is also used by standalone tools. Respect all active writers.
        let mut store_locks = Vec::new();
        for (folder, file) in [
            ("workspaces", ".wes-workspaces.lock"),
            ("values/live", ".wes-values.lock"),
            ("values/archive", ".wes-values.lock"),
        ] {
            let directory = path.join(folder);
            if directory.join(file).try_exists()? {
                store_locks.push(exclusive_lock(&directory, file).map_err(|_| {
                    io::Error::other("The data folder is already open in another engine.")
                })?);
            }
        }
        // A user-selected empty folder (including Finder-created 0755 folders) becomes private.
        // Validate ownership and acquire locks first, so refusing an unrelated root changes nothing.
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&path, fs::Permissions::from_mode(0o700))?;
        }
        let identity = match identity(&path)? {
            Some(i) => i,
            None => {
                let i = Identity {
                    version: 1,
                    id: uuid::Uuid::new_v4().to_string(),
                };
                atomic_json(&path.join(IDENTITY), &i)?;
                i
            }
        };
        library::prepare(&path)?;
        Ok(Self {
            path,
            identity,
            _lock: lock,
        })
    }
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Selection {
    version: u32,
    path: PathBuf,
}

pub fn selected_home(user_home: &Path) -> Result<PathBuf, Error> {
    let default = default_home(user_home);
    let file = default.join(SELECTION);
    match fs::symlink_metadata(&file) {
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(default),
        Err(e) => return Err(e.into()),
        Ok(_) => (),
    }
    // Do not silently open a new empty home when the remembered one is missing.
    let selected: Selection = metadata(&file)?;
    if selected.version != 1 || !selected.path.is_dir() {
        return Err(io::Error::other("The selected data folder is unavailable. Restore it or update ~/.wes/desktop-location.json.").into());
    }
    Ok(resolve(&selected.path)?)
}
pub fn remember_home(user_home: &Path, home: &Path) -> Result<(), Error> {
    let default = resolve(&default_home(user_home))?;
    if !default.exists() {
        private_directory(&default)?;
    }
    if !default.is_dir() {
        return Err(io::Error::other("The launcher folder ~/.wes is not a directory.").into());
    }
    let _lock = exclusive_lock(&default, ".desktop-location.lock")?;
    atomic_json(
        &default.join(SELECTION),
        &Selection {
            version: 1,
            path: home.to_owned(),
        },
    )?;
    Ok(())
}
