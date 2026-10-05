use super::*;

pub(super) async fn status(State(shared): State<Shared>) -> Response {
    perform(shared, crate::api_library::Request::Status).await
}
pub(super) async fn action(State(shared): State<Shared>, request: Request) -> Response {
    if !request
        .headers()
        .get(header::CONTENT_TYPE)
        .is_some_and(|v| v == "application/json")
    {
        return (
            StatusCode::UNSUPPORTED_MEDIA_TYPE,
            "application/json is required",
        )
            .into_response();
    }
    let bytes = match tokio::time::timeout(
        Duration::from_secs(10),
        to_bytes(
            request.into_body(),
            2 * wes_adapters::api_library::max_descriptor(),
        ),
    )
    .await
    {
        Ok(Ok(bytes)) => bytes,
        _ => return StatusCode::PAYLOAD_TOO_LARGE.into_response(),
    };
    match crate::api_library::parse_request(&bytes) {
        Ok(request) => perform(shared, request).await,
        Err(error) => (StatusCode::BAD_REQUEST, error.to_string()).into_response(),
    }
}
async fn perform(shared: Shared, request: crate::api_library::Request) -> Response {
    let Some(service) = shared.services.api_library.clone() else {
        return (
            StatusCode::NOT_IMPLEMENTED,
            "API library service unavailable",
        )
            .into_response();
    };
    let Ok(permit) = shared.queries.clone().try_acquire_owned() else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    // Once admitted, the tracked owner finishes or fails the transaction even if the client closes.
    // Server shutdown joins this worker; HTTP disconnect never causes an untracked repository write.
    match shared
        .encoders
        .spawn_blocking(move || {
            let _permit = permit;
            service.perform(request)
        })
        .await
    {
        Ok(Ok(value)) => (
            [(header::CONTENT_TYPE, "application/json")],
            value.to_string(),
        )
            .into_response(),
        Ok(Err(error)) => (StatusCode::BAD_REQUEST, error.to_string()).into_response(),
        Err(_) => (StatusCode::INTERNAL_SERVER_ERROR, "library worker failed").into_response(),
    }
}
