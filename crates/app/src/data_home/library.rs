//! Initialize the current library inside its owning data home.
use super::*;
use crate::api_library::{Settings, settings};
use wes_adapters::api_library::Library;

pub(super) fn prepare(home: &Path) -> Result<(), Error> {
    let _lock = exclusive_lock(home, ".api-library.lock")?;
    let settings = settings::load(home)?.unwrap_or_else(|| Settings {
        local_directory: home.join("api-library"),
        repository: None,
        extractor: None,
    });
    let _library = Library::open(&settings.local_directory)?;
    settings::store(home, &settings)?;
    Ok(())
}
