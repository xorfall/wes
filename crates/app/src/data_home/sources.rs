//! Immutable input copies, independent of command acceptance and workspace replay.
use super::*;
use std::{
    io::{Read, Write},
    sync::Mutex,
};
use wes_adapters::api_library::digest;
use wes_adapters::{
    api_library::Library,
    source_archive::{SourceArchive, SourceKind},
};

pub struct Sources {
    home: PathBuf,
    gate: Mutex<()>,
}
impl Sources {
    pub fn new(home: PathBuf) -> Self {
        Self {
            home,
            gate: Mutex::new(()),
        }
    }
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct Receipt<'a> {
    version: u32,
    kind: &'static str,
    origin: &'a str,
    format: &'a str,
    revision: &'a str,
    object: String,
}

impl SourceArchive for Sources {
    fn retain(&self, kind: SourceKind, origin: &str, format: &str, source: &str) -> io::Result<()> {
        if source.len() > 1024 * 1024 || origin.len() > 8192 || format.len() > 128 {
            return Err(io::Error::other(
                "captured input exceeds its archive budget",
            ));
        }
        let _gate = self
            .gate
            .lock()
            .map_err(|_| io::Error::other("input archive unavailable"))?;
        let imports = self.home.join("imports");
        directory(&imports)?;
        let _lock = exclusive_lock(&imports, ".capture.lock")?;
        let revision = digest(source.as_bytes());
        let (kind, object) = match kind {
            SourceKind::Spec => {
                // The same lock order as library management; capture of a documentation URL has
                // already released its conversion lock before returning the descriptor here.
                let _api = exclusive_lock(&self.home, ".api-library.lock")?;
                let library = Library::open(&self.home.join("api-library"))?;
                library.retain_input(source)?;
                ("spec", format!("api-library/objects/{revision}.json"))
            }
            SourceKind::Types | SourceKind::Environments => {
                let packages = imports.join("packages");
                directory(&packages)?;
                immutable(
                    &packages.join(format!("{revision}.yaml")),
                    source.as_bytes(),
                )?;
                let kind = match kind {
                    SourceKind::Environments => "environments",
                    _ => "types",
                };
                (kind, format!("imports/packages/{revision}.yaml"))
            }
        };
        let receipt = Receipt {
            version: 1,
            kind,
            origin,
            format,
            revision: &revision,
            object,
        };
        let bytes = serde_json::to_vec_pretty(&receipt)?;
        let records = imports.join("records");
        directory(&records)?;
        immutable(&records.join(format!("{}.json", digest(&bytes))), &bytes)
    }
}

fn directory(path: &Path) -> io::Result<()> {
    private_directory(path)?;
    #[cfg(unix)]
    fs::File::open(path.parent().expect("archive directory parent"))?.sync_all()?;
    Ok(())
}

fn immutable(path: &Path, bytes: &[u8]) -> io::Result<()> {
    match fs::symlink_metadata(path) {
        Ok(meta) => {
            if !meta.is_file() || meta.file_type().is_symlink() || meta.len() != bytes.len() as u64
            {
                return Err(io::Error::other(
                    "immutable captured input conflicts with existing bytes",
                ));
            }
            let mut existing = Vec::new();
            fs::File::open(path)?
                .take(bytes.len() as u64 + 1)
                .read_to_end(&mut existing)?;
            if existing != bytes {
                return Err(io::Error::other(
                    "immutable captured input conflicts with existing bytes",
                ));
            }
            wes_adapters::sync_existing_file(path)?;
            #[cfg(unix)]
            fs::File::open(path.parent().expect("archive object parent"))?.sync_all()?;
            return Ok(());
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => (),
        Err(error) => return Err(error),
    }
    let parent = path.parent().expect("archive object parent");
    let mut temporary = tempfile::NamedTempFile::new_in(parent)?;
    temporary.write_all(bytes)?;
    temporary.as_file().sync_all()?;
    temporary.persist_noclobber(path).map_err(|e| e.error)?;
    #[cfg(unix)]
    fs::File::open(parent)?.sync_all()?;
    Ok(())
}
