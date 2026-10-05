//! Application-wide operating budgets, saved for the next executable launch.
//! Changing a data folder or saving a profile never replaces the active policy or runs a command.
use cap_fs_ext::{FollowSymlinks, OpenOptionsFollowExt, OpenOptionsSyncExt};
use cap_std::{
    ambient_authority,
    fs::{Dir, OpenOptions},
};
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeMap,
    io::{self, Read},
    path::{Path, PathBuf},
    sync::{Arc, Mutex, OnceLock},
};

// Independent control-plane envelope: even a tiny data budget must leave settings recoverable.
pub const PROFILE_BYTES: usize = 128 * 1024;
static STORE: OnceLock<Arc<Store>> = OnceLock::new();
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Profile {
    pub version: u8,
    pub revision: u64,
    #[serde(deserialize_with = "wes_budgets::unique_values")]
    pub values: BTreeMap<String, u64>,
}
impl Profile {
    fn empty() -> Self {
        Self {
            version: 1,
            revision: 0,
            values: BTreeMap::new(),
        }
    }
    fn validate(&self) -> io::Result<()> {
        if self.version != 1 || self.revision > 9_007_199_254_740_990 {
            return Err(io::Error::other("Unsupported operating budget profile."));
        }
        wes_budgets::validate(&self.values).map_err(io::Error::other)
    }
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Change {
    pub revision: u64,
    #[serde(deserialize_with = "wes_budgets::unique_values")]
    pub values: BTreeMap<String, u64>,
}
#[derive(Debug, thiserror::Error)]
pub enum SaveError {
    #[error("The saved budgets changed in another window. Reload before saving.")]
    Conflict,
    #[error("{0}")]
    Invalid(String),
    #[error("Could not read or save operating budgets: {0}")]
    Io(#[from] io::Error),
}
pub struct Store {
    directory: PathBuf,
    lock: Mutex<()>,
}
impl Store {
    pub fn for_user_home(home: &Path) -> Self {
        Self::new(home.join(".wes-settings"))
    }
    pub fn new(directory: PathBuf) -> Self {
        Self {
            directory,
            lock: Mutex::new(()),
        }
    }
    fn directory(&self, create: bool) -> io::Result<Option<Dir>> {
        match std::fs::symlink_metadata(&self.directory) {
            Err(error) if error.kind() == io::ErrorKind::NotFound && !create => return Ok(None),
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                crate::data_home::private_directory(&self.directory)?;
            }
            Ok(meta) if meta.is_dir() && !meta.file_type().is_symlink() => {}
            Ok(_) => {
                return Err(io::Error::other(
                    "Operating budget directory must be an ordinary private directory.",
                ));
            }
            Err(error) => return Err(error),
        }
        crate::data_home::private_directory(&self.directory)?;
        Ok(Some(Dir::open_ambient_dir(
            &self.directory,
            ambient_authority(),
        )?))
    }
    fn read_in(dir: Option<&Dir>) -> io::Result<Profile> {
        let Some(dir) = dir else {
            return Ok(Profile::empty());
        };
        let mut options = OpenOptions::new();
        options.read(true).follow(FollowSymlinks::No).nonblock(true);
        let mut file = match dir.open_with("limits.json", &options) {
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(Profile::empty()),
            other => other?,
        };
        let meta = file.metadata()?;
        if !meta.is_file() || meta.len() > PROFILE_BYTES as u64 {
            return Err(io::Error::other(
                "Invalid or oversized operating budget profile.",
            ));
        }
        let mut bytes = Vec::new();
        file.by_ref()
            .take(PROFILE_BYTES as u64 + 1)
            .read_to_end(&mut bytes)?;
        if bytes.len() > PROFILE_BYTES {
            return Err(io::Error::other("Operating budget profile is too large."));
        }
        let profile: Profile = serde_json::from_slice(&bytes).map_err(io::Error::other)?;
        profile.validate()?;
        Ok(profile)
    }
    pub fn read(&self) -> io::Result<Profile> {
        let _guard = self
            .lock
            .lock()
            .map_err(|_| io::Error::other("Operating budget lock unavailable."))?;
        Self::read_in(self.directory(false)?.as_ref())
    }
    pub fn save(&self, change: Change) -> Result<Profile, SaveError> {
        wes_budgets::validate(&change.values).map_err(SaveError::Invalid)?;
        let _guard = self
            .lock
            .lock()
            .map_err(|_| io::Error::other("Operating budget lock unavailable."))?;
        let dir = self.directory(true)?.expect("created settings directory");
        let mut opts = OpenOptions::new();
        opts.read(true)
            .write(true)
            .create(true)
            .follow(FollowSymlinks::No)
            .nonblock(true);
        #[cfg(unix)]
        {
            use cap_std::fs::OpenOptionsExt;
            opts.mode(0o600);
        }
        let lock = dir.open_with(".limits.lock", &opts)?.into_std();
        if !lock.metadata()?.is_file() {
            return Err(io::Error::other("Invalid operating budget lock file.").into());
        }
        let _file_lock =
            wes_adapters::file_lock::ExclusiveLock::try_lock(lock).map_err(io::Error::other)?;
        let mut profile = Self::read_in(Some(&dir))?;
        if profile.revision != change.revision {
            return Err(SaveError::Conflict);
        }
        for (id, value) in change.values {
            if value == wes_budgets::catalogue()[&id].default {
                profile.values.remove(&id);
            } else {
                profile.values.insert(id, value);
            }
        }
        profile.revision = profile
            .revision
            .checked_add(1)
            .ok_or_else(|| io::Error::other("Operating budget revision exhausted."))?;
        profile.validate()?;
        let bytes = serde_json::to_vec_pretty(&profile).map_err(io::Error::other)?;
        wes_adapters::api_library::atomic_bytes_in(
            &dir,
            std::ffi::OsStr::new("limits.json"),
            &bytes,
            PROFILE_BYTES,
            "Operating budget profile is too large.",
        )?;
        Ok(profile)
    }
}
/// Executable startup only. Library runtimes remain independent of a contributor's home.
pub fn initialize(home: &Path) -> io::Result<()> {
    let store = Arc::new(Store::for_user_home(home));
    let profile = store.read()?;
    wes_budgets::activate(profile.values).map_err(io::Error::other)?;
    STORE
        .set(store)
        .map_err(|_| io::Error::other("Operating budget store already initialized."))
}
pub fn configured_store() -> Option<Arc<Store>> {
    STORE.get().cloned()
}
pub fn snapshot(profile: &Profile) -> serde_json::Value {
    let entries: Vec<_> = wes_budgets::catalogue().values().map(|entry| serde_json::json!({
        "id":entry.id,"label":entry.label,"description":entry.description,"group":entry.group,"unit":entry.unit,
        "default":entry.default,"min":entry.min,"max":entry.max,"source":entry.source,
        "active":wes_budgets::get(&entry.id),"saved":profile.values.get(&entry.id).copied().unwrap_or(entry.default),
        "editable":true,"restart":true
    })).collect();
    serde_json::json!({"revision":profile.revision,"entries":entries,"scope":"application"})
}
#[cfg(test)]
mod tests;
