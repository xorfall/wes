use thiserror::Error;

/// A local workspace label, never a caller-supplied path. Adapters choose its file representation.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct WorkspaceName(String);
#[derive(Clone, Copy, Debug, Error)]
#[error(
    "workspace names must contain 1–96 bytes, with no path separators, '..' or control characters"
)]
pub struct InvalidWorkspaceName;
impl WorkspaceName {
    pub fn new(name: String) -> Result<Self, InvalidWorkspaceName> {
        if name.trim().is_empty()
            || name.len() > 96
            || name.contains("..")
            || name
                .chars()
                .any(|c| c.is_control() || matches!(c, '/' | '\\'))
        {
            return Err(InvalidWorkspaceName);
        }
        Ok(Self(name))
    }
    pub fn as_str(&self) -> &str {
        &self.0
    }
}
