//! Read-only, session-scoped inspection. Never schedules a provider or a query node.
use super::*;
pub(super) async fn read(
    super::Scoped(shared): super::Scoped,
    Path(node): Path<String>,
    request: Request,
) -> Response {
    let Ok(current) = shared.application.current() else {
        return StatusCode::GONE.into_response();
    };
    if request
        .headers()
        .get("X-Wes-Session")
        .and_then(|v| v.to_str().ok())
        != Some(current.generation.as_str())
    {
        return StatusCode::CONFLICT.into_response();
    }
    let Ok(node) = NodeId::new(node) else {
        return StatusCode::BAD_REQUEST.into_response();
    };
    let Ok(permit) = shared.reads.clone().try_acquire_owned() else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    if current.session.check_retirement_access().await.is_err() {
        return services::management_error(crate::retention::ManagementError::Retiring);
    }
    let Ok(observation) = current.session.observe().await else {
        return StatusCode::GONE.into_response();
    };
    if observation.state.execution.graph.node(&node).is_none() {
        return StatusCode::NOT_FOUND.into_response();
    }
    let Some(value) = observation.traces.get(&node, None) else {
        return StatusCode::NOT_FOUND.into_response();
    };
    let encoded = shared
        .encoders
        .spawn_blocking(move || {
            let _permit = permit;
            encode_display_value(
                &value,
                Limits {
                    bytes: 512 * 1024,
                    nodes: 100_000,
                },
            )
        })
        .await;
    if !shared
        .application
        .current()
        .is_ok_and(|now| now.generation == current.generation)
    {
        return StatusCode::CONFLICT.into_response();
    }
    match encoded {
        Ok(Ok(bytes)) => (
            [
                (header::CONTENT_TYPE, "application/json"),
                (header::CACHE_CONTROL, "no-store"),
            ],
            bytes,
        )
            .into_response(),
        _ => StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    }
}
