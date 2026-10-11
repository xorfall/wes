//! Backend-owned result-read diagnostics. HTTP status alone is not result lifecycle state.
use axum::{
    http::{StatusCode, header},
    response::{IntoResponse, Response},
};
use serde_json::json;
use wes_engine::storage::{ReadFailureKind, StoreError, ValueHandle};

pub(super) fn read_status(kind: ReadFailureKind) -> StatusCode {
    use ReadFailureKind::*;
    match kind {
        Capacity => StatusCode::PAYLOAD_TOO_LARGE,
        Forbidden | Withdrawn => StatusCode::FORBIDDEN,
        Stale | Uncertain => StatusCode::CONFLICT,
        Unavailable => StatusCode::NOT_FOUND,
        Closed => StatusCode::SERVICE_UNAVAILABLE,
        Corrupt | Failed => StatusCode::INTERNAL_SERVER_ERROR,
    }
}
fn read_code(kind: ReadFailureKind) -> &'static str {
    use ReadFailureKind::*;
    match kind {
        Capacity => "VALUE_CAPACITY",
        Forbidden => "VALUE_ACCESS_REFUSED",
        Withdrawn => "VALUE_ACCESS_WITHDRAWN",
        Stale => "VALUE_REVISION_CONFLICT",
        Unavailable => "VALUE_UNAVAILABLE",
        Closed => "VALUE_OWNER_UNAVAILABLE",
        Corrupt => "VALUE_CORRUPT",
        Uncertain => "VALUE_STORAGE_UNCONFIRMED",
        Failed => "VALUE_READ_FAILED",
    }
}

pub(super) enum Failure {
    Retiring,
    InvalidHandle,
    Busy,
    Missing,
    Storage(StoreError),
    ReadScope {
        kind: ReadFailureKind,
        message: String,
    },
    InvalidScope,
    SessionUnavailable,
    SessionChanged,
    PrivateUnavailable,
    Encoding,
}

impl Failure {
    pub(super) fn response(self, handle: Option<&ValueHandle>) -> Response {
        // StoreError's Display deliberately excludes its private backend source chain.
        // Preserve that safe port-level explanation rather than erasing it with Err(_).
        let storage_message = match &self {
            Self::Storage(error) => Some(error.read_message()),
            Self::ReadScope { message, .. } => Some(message.clone()),
            _ => None,
        };
        let (status, code, message, retryable) = match self {
            Self::Retiring => (
                StatusCode::SERVICE_UNAVAILABLE,
                "VALUE_RETIREMENT_BUSY",
                "Work deletion is in progress. This result read was refused; wait for the updated workspace before inspecting work.",
                false,
            ),
            Self::InvalidHandle => (
                StatusCode::NOT_FOUND,
                "VALUE_INVALID_HANDLE",
                "The result reference is invalid. Reconnect to obtain its current reference.",
                false,
            ),
            Self::Busy => (
                StatusCode::SERVICE_UNAVAILABLE,
                "VALUE_READ_BUSY",
                "Result readers are busy. Read the same result again shortly; do not rerun the command. This does not mean the result was deleted.",
                true,
            ),
            Self::Missing => (
                StatusCode::NOT_FOUND,
                "VALUE_UNAVAILABLE",
                "This stored result is unavailable. It may have been released or evicted; no command was rerun.",
                false,
            ),
            Self::Storage(error) => {
                let kind = error.read_failure_kind();
                (
                    read_status(kind),
                    read_code(kind),
                    storage_message.as_deref().expect("storage error message"),
                    false,
                )
            }
            Self::ReadScope { kind, .. } => (
                read_status(kind),
                read_code(kind),
                storage_message.as_deref().expect("scope error message"),
                false,
            ),
            Self::InvalidScope => (
                StatusCode::BAD_REQUEST,
                "VALUE_SCOPE_INVALID",
                "The result read scope is invalid. Select the result again to obtain its current scope; no command was rerun.",
                false,
            ),
            Self::SessionUnavailable => (
                StatusCode::SERVICE_UNAVAILABLE,
                "VALUE_SESSION_UNAVAILABLE",
                "The current session could not be observed to authorize this private result read. This does not establish that the result is missing.",
                false,
            ),
            Self::PrivateUnavailable => (
                StatusCode::GONE,
                "VALUE_PRIVATE_UNAVAILABLE",
                "This private result is unavailable in the current session.",
                false,
            ),
            Self::SessionChanged => (
                StatusCode::CONFLICT,
                "VALUE_SESSION_CHANGED",
                "The workspace session changed during this read. Select the result in the current workspace; no command was rerun.",
                false,
            ),
            Self::Encoding => (
                StatusCode::INTERNAL_SERVER_ERROR,
                "VALUE_ENCODING_FAILED",
                "The stored result could not be encoded for display. Check application diagnostics; no command was rerun.",
                false,
            ),
        };
        (
            status,
            [(header::CONTENT_TYPE, "application/json")],
            json!({"error": {
                "code": code, "message": message, "retryable": retryable,
                "retryAfterMs": if retryable { Some(150) } else { None },
                "context": {"operation": "read-value", "handle": handle.map(ValueHandle::as_str)}
            }})
            .to_string(),
        )
            .into_response()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::to_bytes;
    #[tokio::test]
    async fn storage_port_context_survives_without_private_source_details() {
        let handle = ValueHandle::fresh();
        let failure = StoreError::backend(
            "reading stored bytes",
            std::io::Error::other("PRIVATE path or payload"),
        );
        let response = Failure::Storage(failure).response(Some(&handle));
        let bytes = to_bytes(response.into_body(), 4096).await.unwrap();
        let body: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert!(
            body["error"]["message"]
                .as_str()
                .unwrap()
                .starts_with("value storage failed during reading stored bytes")
        );
        assert!(
            body["error"]["message"]
                .as_str()
                .unwrap()
                .contains("does not establish")
        );
        assert_eq!(body["error"]["context"]["handle"], handle.as_str());
        assert!(!String::from_utf8_lossy(&bytes).contains("PRIVATE"));
        assert_eq!(body["error"]["retryable"], false);
    }
    #[tokio::test]
    async fn only_admission_refusal_authorizes_retrying_a_read() {
        let handle = ValueHandle::fresh();
        for (failure, status, code, retryable) in [
            (Failure::Busy, 503, "VALUE_READ_BUSY", true),
            (Failure::Missing, 404, "VALUE_UNAVAILABLE", false),
            (
                Failure::Storage(StoreError::Limit("materialized input")),
                413,
                "VALUE_CAPACITY",
                false,
            ),
            (
                Failure::Storage(StoreError::Restricted),
                403,
                "VALUE_ACCESS_REFUSED",
                false,
            ),
            (
                Failure::Storage(StoreError::Conflict),
                409,
                "VALUE_REVISION_CONFLICT",
                false,
            ),
            (
                Failure::Storage(StoreError::DatasetWithdrawn),
                403,
                "VALUE_ACCESS_WITHDRAWN",
                false,
            ),
            (
                Failure::Storage(StoreError::Closed),
                503,
                "VALUE_OWNER_UNAVAILABLE",
                false,
            ),
            (
                Failure::Storage(StoreError::Corrupt),
                500,
                "VALUE_CORRUPT",
                false,
            ),
            (
                Failure::Storage(StoreError::DatasetUnconfirmed {
                    transaction: "fixture".into(),
                }),
                409,
                "VALUE_STORAGE_UNCONFIRMED",
                false,
            ),
            (Failure::SessionChanged, 409, "VALUE_SESSION_CHANGED", false),
            (
                Failure::SessionUnavailable,
                503,
                "VALUE_SESSION_UNAVAILABLE",
                false,
            ),
            (
                Failure::PrivateUnavailable,
                410,
                "VALUE_PRIVATE_UNAVAILABLE",
                false,
            ),
            (Failure::Encoding, 500, "VALUE_ENCODING_FAILED", false),
        ] {
            let response = failure.response(Some(&handle));
            assert_eq!(response.status().as_u16(), status);
            let body: serde_json::Value =
                serde_json::from_slice(&to_bytes(response.into_body(), 4096).await.unwrap())
                    .unwrap();
            assert_eq!(body["error"]["code"], code);
            assert_eq!(body["error"]["retryable"], retryable);
            assert_eq!(
                body["error"]["context"],
                json!({"operation":"read-value", "handle":handle.as_str()})
            );
            assert!(body["error"]["message"].as_str().unwrap().len() > 20);
        }
    }
    #[tokio::test]
    async fn analysis_reads_preserve_corruption_and_uncertainty_without_guessing_scope_end() {
        use wes_engine::session::SessionError;
        for (error, status, code) in [
            (
                SessionError::ResultRead {
                    kind: ReadFailureKind::Stale,
                    message: "fixture identity mismatch".into(),
                },
                409,
                "ANALYSIS_CHANGED",
            ),
            (
                SessionError::ResultRead {
                    kind: ReadFailureKind::Corrupt,
                    message: "fixture corrupt payload".into(),
                },
                500,
                "ANALYSIS_CORRUPT",
            ),
            (
                SessionError::ResultRead {
                    kind: ReadFailureKind::Uncertain,
                    message: "fixture unconfirmed completion".into(),
                },
                409,
                "ANALYSIS_UNCONFIRMED",
            ),
            (
                SessionError::ResultRead {
                    kind: ReadFailureKind::Failed,
                    message: "fixture read failure".into(),
                },
                500,
                "ANALYSIS_READ_FAILED",
            ),
            (SessionError::Stopped, 503, "ANALYSIS_OWNER_UNAVAILABLE"),
            (SessionError::ScopeEnded, 410, "ANALYSIS_SCOPE_ENDED"),
        ] {
            let response = super::super::sandbox_failure(error);
            assert_eq!(response.status().as_u16(), status);
            let body: serde_json::Value =
                serde_json::from_slice(&to_bytes(response.into_body(), 4096).await.unwrap())
                    .unwrap();
            assert_eq!(body["error"]["code"], code);
            assert_eq!(body["error"]["retryable"], false);
        }
    }
    #[tokio::test]
    async fn invalid_references_are_not_echoed_as_context() {
        let response = Failure::InvalidHandle.response(None);
        let body: serde_json::Value =
            serde_json::from_slice(&to_bytes(response.into_body(), 4096).await.unwrap()).unwrap();
        assert_eq!(body["error"]["context"]["handle"], serde_json::Value::Null);
        assert_eq!(body["error"]["retryable"], false);
    }
    #[tokio::test]
    async fn dataset_reads_share_precise_storage_refusals_and_never_retry_unknown_completion() {
        for (error, status, code) in [
            (StoreError::Closed, 503, "DATASET_OWNER_UNAVAILABLE"),
            (StoreError::Corrupt, 500, "DATASET_CORRUPT"),
            (StoreError::DatasetCorrupt, 500, "DATASET_CORRUPT"),
            (StoreError::MissingValue, 404, "DATASET_MISSING"),
            (
                StoreError::DatasetUnconfirmed {
                    transaction: "fixture".into(),
                },
                409,
                "DATASET_READ_UNCONFIRMED",
            ),
        ] {
            let response =
                super::super::datasets::error_response(crate::dataset_reads::Error::Storage(error));
            assert_eq!(response.status().as_u16(), status);
            let body: serde_json::Value =
                serde_json::from_slice(&to_bytes(response.into_body(), 4096).await.unwrap())
                    .unwrap();
            assert_eq!(body["error"]["code"], code);
            assert_eq!(body["error"]["retryable"], false);
        }
    }
}
