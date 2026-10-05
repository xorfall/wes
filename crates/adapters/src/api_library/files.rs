use super::*;
use crate::file_lock::ExclusiveLock;
use cap_std::{
    ambient_authority,
    fs::{Dir, OpenOptions},
};
use std::{
    fs,
    io::{Read, Write},
    path::{Path, PathBuf},
};

pub fn validate_directory(path: &Path) -> Result<PathBuf> {
    if !path.is_absolute()
        || path
            .components()
            .any(|p| matches!(p, std::path::Component::ParentDir))
    {
        return Err(error(
            "library and repository directories must be explicit absolute paths without '..'",
        ));
    }
    let mut current = PathBuf::new();
    for part in path.components() {
        current.push(part);
        match fs::symlink_metadata(&current) {
            Ok(meta) if meta.file_type().is_symlink() => {
                return Err(error("library/repository paths must not contain symlinks"));
            }
            Ok(meta) if !meta.is_dir() => {
                return Err(error("directory path contains a non-directory"));
            }
            Ok(_) => (),
            Err(e) if e.kind() == io::ErrorKind::NotFound => (),
            Err(e) => return Err(e),
        }
    }
    Ok(path.to_path_buf())
}
pub fn read_json_file<T: serde::de::DeserializeOwned>(path: &Path) -> Result<T> {
    let file = fs::File::open(path)?;
    let mut bytes = vec![];
    file.take((max_source() + 1) as u64)
        .read_to_end(&mut bytes)?;
    decode(&bytes)
}
fn decode<T: serde::de::DeserializeOwned>(bytes: &[u8]) -> Result<T> {
    if bytes.len() > max_source() {
        return Err(error("library metadata exceeds its configured byte budget"));
    }
    // Reuse duplicate-key/depth/node validation before typed deserialization.
    crate::codec::decode_json_preserving(
        bytes,
        crate::codec::Limits {
            bytes: max_source(),
            nodes: 100_000,
        },
    )
    .map_err(|_| error("invalid or excessive library metadata"))?;
    serde_json::from_slice(bytes).map_err(|_| error("invalid library metadata fields"))
}
fn options() -> OpenOptions {
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use cap_std::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    options
}
fn sync_directory(dir: &Dir) -> Result<()> {
    Ok(crate::sync_directory(dir)?)
}
pub fn exclusive_lock(directory: &Path, name: &str) -> Result<ExclusiveLock> {
    fs::create_dir_all(directory)?;
    let dir = Dir::open_ambient_dir(directory, ambient_authority())?;
    if dir
        .symlink_metadata(name)
        .is_ok_and(|m| m.file_type().is_symlink())
    {
        return Err(error("lock must not be a symlink"));
    }
    let mut opts = OpenOptions::new();
    opts.read(true).write(true).create(true);
    #[cfg(unix)]
    {
        use cap_std::fs::OpenOptionsExt;
        opts.mode(0o600);
    }
    let file = dir.open_with(name, &opts)?.into_std();
    if !file.metadata()?.is_file() {
        return Err(error("lock is not a regular file"));
    }
    let lock = ExclusiveLock::try_lock(file).map_err(|e| match e {
        fs::TryLockError::WouldBlock => {
            error("API library is busy in another operation; retry when it finishes")
        }
        fs::TryLockError::Error(e) => e,
    })?;
    Ok(lock)
}
pub fn atomic_json(path: &Path, value: &impl Serialize) -> Result<()> {
    let bytes = serde_json::to_vec_pretty(value).map_err(io::Error::other)?;
    atomic_bytes(
        path,
        &bytes,
        max_source(),
        "library metadata exceeds its configured byte budget",
    )
}

/// Writes `bytes` to `path` as a whole file or not at all: a temporary name in the same directory,
/// synced, then renamed over the target — so a reader never sees a partial write, and a crash
/// mid-write leaves the previous file (or none) rather than a truncated one.
pub fn atomic_bytes(path: &Path, bytes: &[u8], max: usize, over_limit: &str) -> Result<()> {
    let parent = path
        .parent()
        .ok_or_else(|| error("missing destination directory"))?;
    fs::create_dir_all(parent)?;
    let dir = Dir::open_ambient_dir(parent, ambient_authority())?;
    let target = path
        .file_name()
        .ok_or_else(|| error("invalid destination path"))?;
    atomic_bytes_in(&dir, target, bytes, max, over_limit)
}

/// Atomically replace one filename relative to an already-open directory capability.
pub fn atomic_bytes_in(
    dir: &Dir,
    target: &std::ffi::OsStr,
    bytes: &[u8],
    max: usize,
    over_limit: &str,
) -> Result<()> {
    if Path::new(target).components().count() != 1
        || !matches!(
            Path::new(target).components().next(),
            Some(std::path::Component::Normal(_))
        )
    {
        return Err(error("invalid destination filename"));
    }
    let temporary = format!(".api-write-{}", uuid::Uuid::new_v4());
    if bytes.len() > max {
        return Err(error(over_limit));
    }
    let result = (|| {
        let mut file = dir.open_with(&temporary, &options())?;
        file.write_all(bytes)?;
        file.sync_all()?;
        drop(file);
        dir.rename(&temporary, dir, target)?;
        sync_directory(dir)
    })();
    if result.is_err() {
        let _ = dir.remove_file(&temporary);
    }
    result
}

pub struct Library {
    pub(super) root: PathBuf,
    pub(super) dir: Dir,
    _lock: ExclusiveLock,
    pub index: Index,
}
impl Library {
    pub fn open(root: &Path) -> Result<Self> {
        validate_directory(root)?;
        let mut builder = fs::DirBuilder::new();
        builder.recursive(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::DirBuilderExt;
            builder.mode(0o700);
        }
        builder.create(root)?;
        let dir = Dir::open_ambient_dir(root, ambient_authority())?;
        const MARKER: &str = ".wes-api-library-v1";
        // Do not leave a lock file in an unrelated directory on a rejected configuration.
        if !dir.try_exists(MARKER)? {
            for entry in dir.entries()? {
                if entry?.file_name() != ".library.lock" {
                    return Err(error(
                        "choose an empty directory or an existing wes API library",
                    ));
                }
            }
        }
        let lock = exclusive_lock(root, ".library.lock")?;
        if !dir.try_exists(MARKER)? {
            for entry in dir.entries()? {
                if entry?.file_name() != ".library.lock" {
                    return Err(error(
                        "choose an empty directory or an existing wes API library",
                    ));
                }
            }
            let mut marker = dir.open_with(MARKER, &options())?;
            marker.write_all(b"wes-api-library-v1\n")?;
            marker.sync_all()?;
            sync_directory(&dir)?;
        } else if read_confined(&dir, MARKER, 64)? != b"wes-api-library-v1\n" {
            return Err(error("invalid API library marker"));
        }
        for folder in ["objects", "sources"] {
            if dir
                .symlink_metadata(folder)
                .is_ok_and(|m| !m.is_dir() || m.file_type().is_symlink())
            {
                return Err(error("invalid library object directory"));
            }
            dir.create_dir_all(folder)?;
        }
        let index = if dir.try_exists("index.json")? {
            decode(&read_confined(&dir, "index.json", max_source())?)?
        } else {
            Index::default()
        };
        let result = Self {
            root: root.into(),
            dir,
            _lock: lock,
            index,
        };
        if result.index.version != 1 || result.index.packages.len() > 4000 {
            return Err(error("invalid library index version or size"));
        }
        let mut seen = std::collections::HashSet::new();
        for p in &result.index.packages {
            p.key.validate()?;
            if !valid_hash(&p.revision)
                || p.source_digest.as_ref().is_some_and(|v| !valid_hash(v))
                || p.origin.len() > 4096
                || !seen.insert((
                    p.key.service.clone(),
                    p.key.api_version.clone(),
                    p.key.scope.clone(),
                    p.revision.clone(),
                ))
            {
                return Err(error("invalid or duplicate library index entry"));
            }
        }
        Ok(result)
    }
    pub fn find(&self, key: &PackageKey, revision: Option<&str>) -> Option<Package> {
        self.index
            .packages
            .iter()
            .rev()
            .find(|p| &p.key == key && revision.is_none_or(|r| r == p.revision))
            .cloned()
    }
    pub fn descriptor(&self, revision: &str) -> Result<Vec<u8>> {
        if !valid_hash(revision) {
            return Err(error(
                "revision requires 64 lowercase SHA-256 hex characters",
            ));
        }
        let bytes = read_confined(
            &self.dir,
            &format!("objects/{revision}.json"),
            max_descriptor(),
        )?;
        if digest(&bytes) != revision {
            return Err(error(
                "local descriptor hash mismatch; refusing changed content",
            ));
        }
        validate_descriptor(&bytes)?;
        Ok(bytes)
    }
    pub fn descriptor_path(&self, revision: &str) -> Result<PathBuf> {
        self.descriptor(revision)?;
        Ok(self.root.join(format!("objects/{revision}.json")))
    }
    /// Archive exact input bytes independently of acceptance, without publishing a catalog
    /// package. Engine import validation still decides whether the input can be used.
    pub fn retain_input(&self, source: &str) -> Result<String> {
        if source.len() > max_descriptor() {
            return Err(error("descriptor input exceeds its configured byte budget"));
        }
        let revision = digest(source.as_bytes());
        self.object(&format!("objects/{revision}.json"), source.as_bytes())?;
        Ok(revision)
    }
    pub(super) fn object(&self, path: &str, bytes: &[u8]) -> Result<()> {
        let parent = Path::new(path)
            .parent()
            .ok_or_else(|| error("invalid library artifact path"))?;
        if self.dir.try_exists(path)? {
            if read_confined(&self.dir, path, max_source())? != bytes {
                return Err(error(
                    "immutable library artifact conflicts with existing bytes",
                ));
            }
            self.dir.open(path)?.sync_all()?;
            sync_directory(&self.dir.open_dir(parent)?)?;
            return Ok(());
        }
        // Publish an immutable object only after its complete bytes have been synced.
        let temporary = format!(".api-write-{}", uuid::Uuid::new_v4());
        let result = (|| {
            let mut file = self.dir.open_with(&temporary, &options())?;
            file.write_all(bytes)?;
            file.sync_all()?;
            drop(file);
            self.dir.hard_link(&temporary, &self.dir, path)?;
            self.dir.remove_file(&temporary)?;
            sync_directory(&self.dir.open_dir(parent)?)
        })();
        if result.is_err() {
            let _ = self.dir.remove_file(&temporary);
        }
        result
    }
    pub fn save(
        &mut self,
        key: PackageKey,
        bytes: &[u8],
        origin: String,
        source: Option<&[u8]>,
        accepted: bool,
    ) -> Result<Package> {
        key.validate()?;
        validate_descriptor(bytes)?;
        if origin.len() > 4096 {
            return Err(error("origin exceeds metadata budget"));
        }
        if source.is_some_and(|s| s.len() > max_source() || std::str::from_utf8(s).is_err()) {
            return Err(error("source must be UTF-8 within 4 MiB"));
        }
        let revision = digest(bytes);
        if let Some(old) = self.find(&key, Some(&revision)) {
            self.descriptor(&revision)?;
            return Ok(old);
        }
        if self.index.packages.len() == 4000 {
            return Err(error("library revision capacity reached"));
        }
        self.object(&format!("objects/{revision}.json"), bytes)?;
        let source_digest = source.map(digest);
        if let (Some(s), Some(hash)) = (source, &source_digest) {
            self.object(&format!("sources/{hash}.txt"), s)?;
        }
        let package = Package {
            key,
            revision,
            origin,
            source_digest,
            accepted,
        };
        self.index.packages.push(package.clone());
        atomic_json(&self.root.join("index.json"), &self.index)?;
        Ok(package)
    }
    pub fn accept(&mut self, key: &PackageKey, revision: &str) -> Result<Package> {
        self.descriptor(revision)?;
        let package = self
            .index
            .packages
            .iter_mut()
            .find(|p| &p.key == key && p.revision == revision)
            .ok_or_else(|| error("local revision not found"))?;
        package.accepted = true;
        let result = package.clone();
        atomic_json(&self.root.join("index.json"), &self.index)?;
        Ok(result)
    }
}

pub(super) fn read_confined(dir: &Dir, path: &str, limit: usize) -> Result<Vec<u8>> {
    if path.is_empty()
        || Path::new(path).is_absolute()
        || path.contains('\\')
        || path
            .split('/')
            .any(|p| p.is_empty() || p == "." || p == "..")
    {
        return Err(error("invalid repository-relative path"));
    }
    let mut prefix = PathBuf::new();
    for part in path.split('/') {
        prefix.push(part);
        if dir.symlink_metadata(&prefix)?.file_type().is_symlink() {
            return Err(error("repository/library files must not be symlinks"));
        }
    }
    let file = dir.open(path)?;
    if !file.metadata()?.is_file() {
        return Err(error("artifact is not a regular file"));
    }
    let mut bytes = vec![];
    file.take((limit + 1) as u64).read_to_end(&mut bytes)?;
    if bytes.len() > limit {
        return Err(error("artifact exceeds its byte budget"));
    }
    Ok(bytes)
}
pub(super) fn decode_metadata<T: serde::de::DeserializeOwned>(bytes: &[u8]) -> Result<T> {
    decode(bytes)
}
