//! Resolve only current process-owned scope authority, never persisted scope identifiers.
use super::*;
use wes_engine::session::{SessionError, SessionHandle};
pub(super) async fn resolve(
    session: &SessionHandle,
    worker: &wes_engine::storage::StoreWorker,
    headers: &header::HeaderMap,
) -> Result<(SessionHandle, wes_engine::storage::StoreWorker), Response> {
    let Some(scope) = headers.get("X-Wes-Scope") else {
        return Ok((session.clone(), worker.clone()));
    };
    if headers.get_all("X-Wes-Scope").iter().count() != 1 {
        return Err(StatusCode::BAD_REQUEST.into_response());
    }
    let scope = scope
        .to_str()
        .ok()
        .filter(|s| wes_engine::storage::ValueHandle::new(s).is_ok())
        .ok_or_else(|| StatusCode::BAD_REQUEST.into_response())?;
    let resolved = match session.check_retirement_access().await {
        Ok(()) => session.resolve_read_scope(scope).await,
        Err(error) => Err(error),
    };
    resolved.map_err(|error| {
        let (status,code)=match error {
            SessionError::AdmissionBusy | SessionError::CheckpointBusy=>(StatusCode::SERVICE_UNAVAILABLE,"DATASET_READ_BUSY"),
            SessionError::Authority | SessionError::AccessDenied(_)=>(StatusCode::FORBIDDEN,"DATASET_ACCESS_REFUSED"),
            _=>(StatusCode::GONE,"DATASET_SCOPE_ENDED"),
        };
        (status,[(header::CONTENT_TYPE,"application/json")],serde_json::json!({"error":{"code":code,"message":error.to_string(),"retryable":status == StatusCode::SERVICE_UNAVAILABLE}}).to_string()).into_response()
    })
}
