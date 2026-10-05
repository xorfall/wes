//! Same-origin operating policy settings; never submits engine commands or restarts the host.
use super::*;
use crate::budgets::{Change, PROFILE_BYTES, SaveError};
pub(super) async fn read(State(shared): State<Shared>) -> Response {
    perform(shared, None).await
}
pub(super) async fn write(State(shared): State<Shared>, request: Request) -> Response {
    if !request
        .headers()
        .get(header::CONTENT_TYPE)
        .is_some_and(|h| h == "application/json")
    {
        return StatusCode::UNSUPPORTED_MEDIA_TYPE.into_response();
    }
    let bytes = match tokio::time::timeout(
        Duration::from_secs(5),
        to_bytes(request.into_body(), PROFILE_BYTES),
    )
    .await
    {
        Ok(Ok(bytes)) => bytes,
        _ => return StatusCode::PAYLOAD_TOO_LARGE.into_response(),
    };
    let change = match serde_json::from_slice::<Change>(&bytes) {
        Ok(change) if wes_budgets::validate(&change.values).is_ok() => change,
        _ => return (StatusCode::BAD_REQUEST, "Invalid operating budget values.").into_response(),
    };
    perform(shared, Some(change)).await
}
async fn perform(shared: Shared, change: Option<Change>) -> Response {
    let Some(store) = shared.services.budgets.clone() else {
        return (
            StatusCode::NOT_IMPLEMENTED,
            "Operating budgets are not configured by this host.",
        )
            .into_response();
    };
    // Settings remain accessible independently of configured data/query capacity.
    let capacity = *shared.application.subscribe_capacity().borrow();
    match shared.encoders.spawn_blocking(move || {
        let profile = if let Some(change) = change { store.save(change)? } else { store.read()? };
        let mut snapshot = crate::budgets::snapshot(&profile);
        for row in snapshot["entries"].as_array_mut().expect("budget rows") {
            match row["id"].as_str() {
                Some("execution.operations") => row["active"] = capacity.operations.limit.into(),
                Some("execution.streams") => row["active"] = capacity.streams.limit.into(),
                _ => {},
            }
        }
        Ok::<_, SaveError>(snapshot)
    }).await {
        Ok(Ok(snapshot)) => ([(header::CONTENT_TYPE, "application/json"), (header::CACHE_CONTROL, "no-store")], snapshot.to_string()).into_response(),
        Ok(Err(SaveError::Conflict)) => (StatusCode::CONFLICT, SaveError::Conflict.to_string()).into_response(),
        Ok(Err(SaveError::Invalid(message))) => (StatusCode::BAD_REQUEST, message).into_response(),
        _ => (StatusCode::INTERNAL_SERVER_ERROR, "Could not read or save operating budgets. Reload to verify the saved values; do not retry automatically.").into_response(),
    }
}
