//! One portable settings contract shared by direct service use and data-home startup.
use super::{Saved, Settings};
use std::{
    fs,
    io::{self, Read},
    path::{Path, PathBuf},
};
use wes_adapters::api_library::{atomic_json, validate_directory};

const MAX_BYTES: u64 = 16 * 1024;
const FILE: &str = "api-library-settings.json";

pub(super) fn validate(home: &Path, settings: &Settings) -> io::Result<()> {
    if settings.local_directory != home.join("api-library") {
        return Err(io::Error::other(
            "The API library belongs to this data folder. Open another data folder to change its location.",
        ));
    }
    validate_directory(&settings.local_directory)?;
    Ok(())
}

pub(crate) fn load(home: &Path) -> io::Result<Option<Settings>> {
    let path = home.join(FILE);
    let meta = match fs::symlink_metadata(&path) {
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
        result => result?,
    };
    if !meta.is_file() || meta.file_type().is_symlink() || meta.len() > MAX_BYTES {
        return Err(io::Error::other("Invalid API library settings file."));
    }
    let mut bytes = Vec::new();
    fs::File::open(&path)?
        .take(MAX_BYTES + 1)
        .read_to_end(&mut bytes)?;
    wes_adapters::codec::decode_json_preserving(
        &bytes,
        wes_adapters::codec::Limits {
            bytes: MAX_BYTES as usize,
            nodes: 1000,
        },
    )
    .map_err(|_| io::Error::other("Invalid or excessive API library settings."))?;
    let mut saved: Saved = serde_json::from_slice(&bytes)
        .map_err(|_| io::Error::other("Invalid API library settings fields."))?;
    if saved.version != 1 || saved.settings.local_directory != Path::new("api-library") {
        return Err(io::Error::other(
            "Unsupported API library settings; choose a new data folder.",
        ));
    }
    saved.settings.local_directory = home.join("api-library");
    validate(home, &saved.settings)?;
    Ok(Some(saved.settings))
}

pub(crate) fn store(home: &Path, settings: &Settings) -> io::Result<()> {
    validate(home, settings)?;
    let mut relative = settings.clone();
    relative.local_directory = PathBuf::from("api-library");
    let saved = Saved {
        version: 1,
        settings: relative,
    };
    if serde_json::to_vec(&saved).map_err(io::Error::other)?.len() as u64 > MAX_BYTES {
        return Err(io::Error::other("API library settings exceed 16 KiB."));
    }
    atomic_json(&home.join(FILE), &saved)
}
