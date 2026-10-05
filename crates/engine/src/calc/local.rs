//! Failure of fixed adapter-owned local functions; no external I/O or registry.
use wes_core::{ErrorId, ErrorValue};
#[derive(Debug)]
pub enum LocalError {
    Failed(ErrorValue),
    Cancelled,
}
impl LocalError {
    pub fn failure(code: &str, message: impl Into<String>) -> Self {
        Self::Failed(
            ErrorValue::new(
                ErrorId::new(uuid::Uuid::new_v4().to_string()).unwrap(),
                code,
                message,
                vec![],
                None,
            )
            .unwrap(),
        )
    }
}
