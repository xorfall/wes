//! Resolve only current process-owned scope authority, never persisted scope identifiers.
use super::*;
use wes_engine::session::{SessionError, SessionHandle};
pub(super) enum Failure {
    InvalidScope,
    Session(SessionError),
}
impl Failure {
    pub(super) fn value_response(self, handle: &ValueHandle) -> Response {
        use super::value_errors;
        let failure = match self {
            Self::InvalidScope => value_errors::Failure::InvalidScope,
            Self::Session(SessionError::AdmissionBusy) => value_errors::Failure::Busy,
            Self::Session(SessionError::CheckpointBusy) => value_errors::Failure::Retiring,
            Self::Session(error) => value_errors::Failure::ReadScope {
                kind: error
                    .read_failure_kind()
                    .unwrap_or(wes_engine::storage::ReadFailureKind::Failed),
                message: error.to_string(),
            },
        };
        failure.response(Some(handle))
    }
    pub(super) fn response(self) -> Response {
        use wes_engine::storage::ReadFailureKind::*;
        let (status, code, message, retryable) = match self {
            Self::InvalidScope => (
                StatusCode::BAD_REQUEST,
                "DATASET_SCOPE_INVALID",
                "The read scope is invalid.".to_owned(),
                false,
            ),
            Self::Session(error) => {
                let (status, code, retryable) = match &error {
                    SessionError::AdmissionBusy | SessionError::CheckpointBusy => {
                        (StatusCode::SERVICE_UNAVAILABLE, "DATASET_READ_BUSY", true)
                    }
                    SessionError::ScopeEnded => (StatusCode::GONE, "DATASET_SCOPE_ENDED", false),
                    _ => {
                        let kind = error.read_failure_kind().unwrap_or(Failed);
                        let code = match kind {
                            Capacity => "DATASET_READ_LIMIT",
                            Forbidden | Withdrawn => "DATASET_ACCESS_REFUSED",
                            Stale => "DATASET_SESSION_CHANGED",
                            Unavailable => "DATASET_MISSING",
                            Closed => "DATASET_OWNER_UNAVAILABLE",
                            Corrupt => "DATASET_CORRUPT",
                            Uncertain => "DATASET_READ_UNCONFIRMED",
                            Failed => "DATASET_READ_FAILED",
                        };
                        (super::value_errors::read_status(kind), code, false)
                    }
                };
                (status, code, error.to_string(), retryable)
            }
        };
        (
            status,
            [(header::CONTENT_TYPE, "application/json")],
            serde_json::json!({"error":{"code":code,"message":message,"retryable":retryable}})
                .to_string(),
        )
            .into_response()
    }
}
pub(super) async fn resolve(
    session: &SessionHandle,
    worker: &wes_engine::storage::StoreWorker,
    headers: &header::HeaderMap,
) -> Result<(SessionHandle, wes_engine::storage::StoreWorker), Failure> {
    let Some(scope) = headers.get("X-Wes-Scope") else {
        return Ok((session.clone(), worker.clone()));
    };
    if headers.get_all("X-Wes-Scope").iter().count() != 1 {
        return Err(Failure::InvalidScope);
    }
    let scope = scope
        .to_str()
        .ok()
        .filter(|s| wes_engine::storage::ValueHandle::new(s).is_ok())
        .ok_or(Failure::InvalidScope)?;
    let resolved = match session.check_retirement_access().await {
        Ok(()) => session.resolve_read_scope(scope).await,
        Err(error) => Err(error),
    };
    resolved.map_err(Failure::Session)
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::to_bytes;
    #[tokio::test]
    async fn result_scope_errors_keep_the_result_envelope_and_do_not_invent_loss() {
        for (failure, status, code) in [
            (Failure::InvalidScope, 400, "VALUE_SCOPE_INVALID"),
            (
                Failure::Session(SessionError::ScopeEnded),
                403,
                "VALUE_ACCESS_REFUSED",
            ),
            (
                Failure::Session(SessionError::Stopped),
                503,
                "VALUE_OWNER_UNAVAILABLE",
            ),
            (
                Failure::Session(SessionError::AdmissionBusy),
                503,
                "VALUE_READ_BUSY",
            ),
            (
                Failure::Session(SessionError::CheckpointBusy),
                503,
                "VALUE_RETIREMENT_BUSY",
            ),
        ] {
            let handle = ValueHandle::fresh();
            let response = failure.value_response(&handle);
            assert_eq!(response.status().as_u16(), status);
            let body: serde_json::Value =
                serde_json::from_slice(&to_bytes(response.into_body(), 4096).await.unwrap())
                    .unwrap();
            assert_eq!(body["error"]["code"], code);
            assert_eq!(body["error"]["context"]["operation"], "read-value");
            assert_eq!(body["error"]["context"]["handle"], handle.as_str());
            assert_eq!(body["error"]["retryable"], code == "VALUE_READ_BUSY");
        }
        let response = Failure::Session(SessionError::Stopped).response();
        assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
        let body: serde_json::Value =
            serde_json::from_slice(&to_bytes(response.into_body(), 4096).await.unwrap()).unwrap();
        assert_eq!(body["error"]["code"], "DATASET_OWNER_UNAVAILABLE");
        assert_eq!(body["error"]["retryable"], false);
    }
}
