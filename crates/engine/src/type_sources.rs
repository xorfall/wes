//! Per-submission type-package input snapshots. Replay cannot fall back to a live reader.
use crate::driver::CancellationToken;
use indexmap::IndexMap;
use std::sync::Arc;
use thiserror::Error;

pub fn max_package_bytes() -> usize {
    wes_budgets::get("type_sources.package_bytes") as usize
}
pub fn max_capture_bytes() -> usize {
    wes_budgets::get("type_sources.capture_bytes") as usize
}
fn max_source_bytes() -> usize {
    wes_budgets::get("type_sources.source_bytes") as usize
}
fn max_packages() -> usize {
    wes_budgets::get("type_sources.packages") as usize
}
const MAX_PATH_BYTES: usize = 4096;

#[derive(Clone, Copy, Debug, Error, PartialEq, Eq)]
pub enum TypeSourceError {
    #[error(
        "the package input path or origin is empty, contains a control character, or exceeds its byte limit"
    )]
    InvalidPath,
    #[error("type load requires exactly one literal path or source; origin belongs only to source")]
    InvalidInput,
    #[error("the type package could not be read")]
    Unavailable,
    #[error(
        "the captured type package could not be saved in the data folder; check its storage and permissions"
    )]
    Persistence,
    #[error("the type package must contain valid UTF-8 text")]
    InvalidText,
    #[error("the type package exceeds its byte limit")]
    TooLarge,
    #[error("the submission exceeds its captured type-package budget")]
    Capacity,
    #[error("the replay record does not contain the requested type package")]
    MissingSnapshot,
    #[error("type-package reading was cancelled")]
    Cancelled,
    #[error("the type-package reader terminated unexpectedly")]
    Worker,
    #[error("the package belongs to another source capture")]
    ForeignCapture,
}

/// Blocking local input port. Implementations must enforce `max_bytes` while reading, not only
/// after allocating the complete file, and must return valid UTF-8. This port has no workspace or
/// execution authority. The capture joins physical reader exit even after cancellation.
pub trait TypeSourceReader: Send + Sync + 'static {
    fn read(&self, path: &str, max_bytes: usize) -> Result<String, TypeSourceError>;
    /// Persist externally supplied bytes in the same archive as file input before installation.
    fn retain_text(&self, _origin: &str, _source: &str) -> Result<(), TypeSourceError> {
        Ok(())
    }
}

pub enum TypeInput {
    File(String),
    Text { source: String, origin: String },
}
/// A text origin is an attribution label plus content identity, never a pretend file path.
pub fn text_origin(label: Option<&str>, source: &str) -> Result<String, TypeSourceError> {
    use sha2::{Digest, Sha256};
    let label = label.unwrap_or("inline");
    if label.trim().is_empty() || label.len() > 1024 || label.chars().any(char::is_control) {
        return Err(TypeSourceError::InvalidPath);
    }
    if source.len() > max_package_bytes() {
        return Err(TypeSourceError::TooLarge);
    }
    Ok(format!(
        "wes-text:{label}:sha256:{:x}",
        Sha256::digest(source.as_bytes())
    ))
}
enum SourceMode {
    Live(Arc<dyn TypeSourceReader>),
    Replay,
}

/// Immutable successful input, not evidence that its declarations were accepted. The coordinator
/// promotes it only after the associated statement has been checked and staged successfully.
#[derive(Clone)]
pub struct CapturedTypePackage {
    owner: Arc<()>,
    path: Arc<str>,
    source: Arc<str>,
}
impl CapturedTypePackage {
    pub fn path(&self) -> &str {
        &self.path
    }
    pub fn source(&self) -> &str {
        &self.source
    }
}
impl std::fmt::Debug for CapturedTypePackage {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CapturedTypePackage")
            .field("bytes", &self.source.len())
            .finish_non_exhaustive()
    }
}

/// One source snapshot per submission. Repeated successful reads of the same written path return
/// identical content, including when an earlier statement was rejected. Captured input sources
/// replay every accepted statement deterministically.
/// Failed reads are not cached. Paths are opaque spellings, not canonical filesystem identities.
pub struct TypeSourceCapture {
    owner: Arc<()>,
    mode: SourceMode,
    sources: IndexMap<Arc<str>, Arc<str>>,
    accepted: IndexMap<String, String>,
    bytes: usize,
}
impl TypeSourceCapture {
    pub fn live(reader: Arc<dyn TypeSourceReader>) -> Self {
        Self {
            owner: Arc::new(()),
            mode: SourceMode::Live(reader),
            sources: IndexMap::new(),
            accepted: IndexMap::new(),
            bytes: 0,
        }
    }
    /// Validate the complete imported snapshot before making any part available. No reader exists
    /// in replay mode, so even a missing entry cannot consult a changed file or environment.
    pub fn replay(sources: IndexMap<String, String>) -> Result<Self, TypeSourceError> {
        let mut capture = Self {
            owner: Arc::new(()),
            mode: SourceMode::Replay,
            sources: IndexMap::new(),
            accepted: IndexMap::new(),
            bytes: 0,
        };
        for (path, source) in sources {
            valid_path(&path)?;
            capture.insert(Arc::from(path), Arc::from(source))?;
        }
        Ok(capture)
    }
    /// The submission owner retains this future until completion, including client disconnect.
    /// Cooperative cancellation uses the token; dropping the future cannot stop an OS reader.
    pub async fn read(
        &mut self,
        path: &str,
        cancellation: CancellationToken,
    ) -> Result<CapturedTypePackage, TypeSourceError> {
        self.read_bounded(path, max_package_bytes(), cancellation)
            .await
    }
    pub async fn read_package(
        &mut self,
        path: &str,
        cancellation: CancellationToken,
    ) -> Result<CapturedTypePackage, TypeSourceError> {
        self.read_bounded(path, max_capture_bytes(), cancellation)
            .await
    }
    async fn read_bounded(
        &mut self,
        path: &str,
        limit: usize,
        cancellation: CancellationToken,
    ) -> Result<CapturedTypePackage, TypeSourceError> {
        if cancellation.is_cancelled() {
            return Err(TypeSourceError::Cancelled);
        }
        valid_path(path)?;
        if let Some((path, source)) = self.sources.get_key_value(path) {
            if source.len() > limit {
                return Err(TypeSourceError::TooLarge);
            }
            return Ok(self.package(path.clone(), source.clone()));
        }
        let SourceMode::Live(reader) = &self.mode else {
            return Err(TypeSourceError::MissingSnapshot);
        };
        // Include the path charge before calling any reader. At most the remaining aggregate
        // budget may be allocated by a conforming reader; cache hits do not consume credit again.
        if self.sources.len() >= max_packages() {
            return Err(TypeSourceError::Capacity);
        }
        let remaining = max_source_bytes()
            .checked_sub(self.bytes)
            .and_then(|bytes| bytes.checked_sub(path.len()))
            .ok_or(TypeSourceError::Capacity)?;
        let max_bytes = remaining.min(limit);
        let reader = reader.clone();
        let path: Arc<str> = Arc::from(path);
        let worker_path = path.clone();
        let token = cancellation.clone();
        let result = tokio::task::spawn_blocking(move || {
            if token.is_cancelled() {
                return Err(TypeSourceError::Cancelled);
            }
            reader.read(&worker_path, max_bytes)
        })
        .await;
        // Always join an entered reader. Cancellation cannot claim it has stopped before exit.
        if cancellation.is_cancelled() {
            return Err(TypeSourceError::Cancelled);
        }
        let source = result.map_err(|_| TypeSourceError::Worker)??;
        if source.len() > max_bytes {
            return Err(if max_bytes == limit {
                TypeSourceError::TooLarge
            } else {
                TypeSourceError::Capacity
            });
        }
        let source: Arc<str> = Arc::from(source);
        self.insert(path.clone(), source.clone())?;
        Ok(self.package(path, source))
    }
    pub async fn text(
        &mut self,
        origin: &str,
        source: &str,
        cancellation: CancellationToken,
    ) -> Result<CapturedTypePackage, TypeSourceError> {
        if cancellation.is_cancelled() {
            return Err(TypeSourceError::Cancelled);
        }
        valid_path(origin)?;
        if source.len() > max_package_bytes() {
            return Err(TypeSourceError::TooLarge);
        }
        if let Some((key, stored)) = self.sources.get_key_value(origin) {
            return if stored.as_ref() == source {
                Ok(self.package(key.clone(), stored.clone()))
            } else {
                Err(TypeSourceError::ForeignCapture)
            };
        }
        let SourceMode::Live(reader) = &self.mode else {
            return Err(TypeSourceError::MissingSnapshot);
        };
        if self.sources.len() >= max_packages()
            || self
                .bytes
                .saturating_add(origin.len())
                .saturating_add(source.len())
                > max_source_bytes()
        {
            return Err(TypeSourceError::Capacity);
        }
        let reader = reader.clone();
        let origin: Arc<str> = Arc::from(origin);
        let source: Arc<str> = Arc::from(source);
        let (o, s) = (origin.clone(), source.clone());
        tokio::task::spawn_blocking(move || reader.retain_text(&o, &s))
            .await
            .map_err(|_| TypeSourceError::Worker)??;
        if cancellation.is_cancelled() {
            return Err(TypeSourceError::Cancelled);
        }
        self.insert(origin.clone(), source.clone())?;
        Ok(self.package(origin, source))
    }
    pub fn accept(&mut self, package: &CapturedTypePackage) -> Result<(), TypeSourceError> {
        if !Arc::ptr_eq(&self.owner, &package.owner) {
            return Err(TypeSourceError::ForeignCapture);
        }
        self.accepted
            .entry(package.path.to_string())
            .or_insert_with(|| package.source.to_string());
        Ok(())
    }
    /// Only accepted statement inputs are serialized. Cached rejected inputs are deliberately
    /// excluded. This is bounded payload accounting, not an allocator/RSS guarantee.
    pub fn finish(self) -> IndexMap<String, String> {
        self.accepted
    }
    fn package(&self, path: Arc<str>, source: Arc<str>) -> CapturedTypePackage {
        CapturedTypePackage {
            owner: self.owner.clone(),
            path,
            source,
        }
    }
    fn insert(&mut self, path: Arc<str>, source: Arc<str>) -> Result<(), TypeSourceError> {
        let limit = if wes_views::Artifact::recognizes(&source) {
            max_capture_bytes()
        } else {
            max_package_bytes()
        };
        if source.len() > limit {
            return Err(TypeSourceError::TooLarge);
        }
        let bytes = self
            .bytes
            .checked_add(path.len())
            .and_then(|bytes| bytes.checked_add(source.len()))
            .ok_or(TypeSourceError::Capacity)?;
        if self.sources.len() >= max_packages() || bytes > max_source_bytes() {
            return Err(TypeSourceError::Capacity);
        }
        self.sources.insert(path, source);
        self.bytes = bytes;
        Ok(())
    }
}
fn valid_path(path: &str) -> Result<(), TypeSourceError> {
    if path.trim().is_empty() || path.len() > MAX_PATH_BYTES || path.chars().any(char::is_control) {
        Err(TypeSourceError::InvalidPath)
    } else {
        Ok(())
    }
}
