//! Result-read refusals describe access and storage, never authorize producer replay.
use super::StoreError;
use crate::runtime::RuntimeCode;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ReadFailureKind {
    Capacity,
    Forbidden,
    Withdrawn,
    Stale,
    Unavailable,
    Closed,
    Corrupt,
    Uncertain,
    Failed,
}

impl ReadFailureKind {
    pub fn runtime_code(self) -> RuntimeCode {
        match self {
            Self::Capacity => RuntimeCode::ResultCapacity,
            Self::Forbidden | Self::Withdrawn => RuntimeCode::ResultForbidden,
            Self::Stale => RuntimeCode::ResultStale,
            Self::Unavailable => RuntimeCode::ResultUnavailable,
            Self::Closed => RuntimeCode::ResultClosed,
            Self::Corrupt => RuntimeCode::ResultCorrupt,
            Self::Uncertain => RuntimeCode::ResultUncertain,
            Self::Failed => RuntimeCode::ResultReadFailed,
        }
    }

    pub fn guidance(self) -> &'static str {
        match self {
            Self::Capacity => {
                "Review capacity limits. For an existing stored result, select a smaller field or use a bounded source scan. A capacity refusal is not evidence that the result was deleted."
            }
            Self::Forbidden => "Review the current read authority before reading this result.",
            Self::Withdrawn => {
                "Access to this result was withdrawn; use a currently authorized selection."
            }
            Self::Stale => {
                "The captured identity could not be verified; read a fresh selection of the existing result."
            }
            Self::Unavailable => {
                "The owner could not find the requested result; inspect retained results and source dependencies."
            }
            Self::Closed => {
                "Reconnect to the owning session before reading. An unavailable storage worker does not establish that the result is missing."
            }
            Self::Corrupt => {
                "Stored result validation failed; inspect diagnostics or another retained copy."
            }
            Self::Uncertain => {
                "Storage completion is unconfirmed; inspect its state before any new operation. Do not automatically repeat Keep, removal or the command."
            }
            Self::Failed => {
                "Inspect application diagnostics. This read failure does not establish that the result is missing."
            }
        }
    }
}

impl StoreError {
    /// Classify only port-level evidence. In particular a backend I/O error, including its
    /// private source chain, is not evidence of missing bytes, corruption or withdrawn access.
    pub fn read_failure_kind(&self) -> ReadFailureKind {
        use ReadFailureKind::*;
        match self {
            Self::Limit(_)
            | Self::ReadWork(_)
            | Self::DatasetRowCharge { .. }
            | Self::DatasetRowBytes { .. } => Capacity,
            Self::Restricted
            | Self::ScopeUnavailable
            | Self::InsecureDirectory
            | Self::UnownedDirectory => Forbidden,
            Self::DatasetWithdrawn => Withdrawn,
            Self::Conflict | Self::DatasetNewerAttempt => Stale,
            Self::MissingValue | Self::DatasetMissing => Unavailable,
            Self::Closed => Closed,
            Self::Corrupt | Self::DatasetCorrupt | Self::NotRegular => Corrupt,
            Self::Published { .. }
            | Self::Released { .. }
            | Self::DatasetUnconfirmed { .. }
            | Self::DatasetAdmissionUnconfirmed { .. }
            | Self::DatasetRecoveryUnknown
            | Self::DatasetNeedsReconciliation => Uncertain,
            Self::NonMaterialized
            | Self::RetentionUnavailable
            | Self::InvalidHandle
            | Self::Locked
            | Self::DatasetUnavailable
            | Self::SourceRange(_)
            | Self::Backend { .. } => Failed,
        }
    }

    /// Display is the storage port's public explanation; never format the source chain.
    pub fn read_message(&self) -> String {
        format!(
            "{}. {} No provider was replayed.",
            self,
            self.read_failure_kind().guidance()
        )
    }

    pub fn read_error(&self, operation: &'static str) -> wes_core::ErrorValue {
        self.read_failure_kind()
            .runtime_code()
            .error(format!("{operation}: {}", self.read_message()), None)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ReadFailureKind::*;

    #[test]
    fn read_refusals_preserve_evidence_and_never_export_private_causes() {
        let handle = super::super::ValueHandle::fresh();
        for (error, kind, code) in [
            (StoreError::Limit("decoded memory"), Capacity, "RUN007"),
            (StoreError::MissingValue, Unavailable, "RUN008"),
            (StoreError::Restricted, Forbidden, "RUN009"),
            (StoreError::ScopeUnavailable, Forbidden, "RUN009"),
            (StoreError::DatasetWithdrawn, Withdrawn, "RUN009"),
            (StoreError::Conflict, Stale, "RUN010"),
            (StoreError::Closed, Closed, "RUN011"),
            (StoreError::Corrupt, Corrupt, "RUN012"),
            (StoreError::DatasetCorrupt, Corrupt, "RUN012"),
            (
                StoreError::Published {
                    handle: handle.clone(),
                    source: Box::new(std::io::Error::other("PRIVATE_MARKER")),
                },
                Uncertain,
                "RUN013",
            ),
            (
                StoreError::Released {
                    handle,
                    source: Box::new(std::io::Error::other("PRIVATE_MARKER")),
                },
                Uncertain,
                "RUN013",
            ),
            (
                StoreError::DatasetUnconfirmed {
                    transaction: "fixture".into(),
                },
                Uncertain,
                "RUN013",
            ),
            (
                StoreError::backend(
                    "reading stored bytes",
                    std::io::Error::other("PRIVATE_MARKER"),
                ),
                Failed,
                "RUN014",
            ),
            (
                StoreError::backend(
                    "reading stored bytes",
                    std::io::Error::from(std::io::ErrorKind::NotFound),
                ),
                Failed,
                "RUN014",
            ),
        ] {
            assert_eq!(error.read_failure_kind(), kind);
            let diagnostic = error.read_error("Stored input preparation refused");
            assert_eq!(diagnostic.code(), code);
            assert!(diagnostic.message().contains(kind.guidance()));
            assert!(diagnostic.message().contains("No provider was replayed"));
            assert!(!diagnostic.message().contains("PRIVATE_MARKER"));
        }
    }
}
