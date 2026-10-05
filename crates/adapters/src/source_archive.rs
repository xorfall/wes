//! Application-owned retention of already captured inputs, before engine admission.
use std::io;

#[derive(Clone, Copy)]
pub enum SourceKind {
    Spec,
    Types,
    Environments,
}

/// Blocking, called on the joined input worker. Persist these exact bytes or refuse capture.
/// This is an input archive, not registration or permission to execute imported definitions.
pub trait SourceArchive: Send + Sync {
    fn retain(&self, kind: SourceKind, origin: &str, format: &str, source: &str) -> io::Result<()>;
}
