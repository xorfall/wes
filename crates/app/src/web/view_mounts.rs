//! User display leases. Opening and observing never execute a source command.
use super::*;
use wes_engine::views::MountAction;
#[derive(Deserialize)]
#[serde(tag = "action", rename_all = "lowercase", deny_unknown_fields)]
enum Action {
    Open,
    Touch { token: String },
    Close { token: String },
    Start,
    Stop,
}
pub(super) async fn change(
    Scoped(shared): Scoped,
    Path((node, identity)): Path<(String, String)>,
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
    if request
        .headers()
        .get(header::CONTENT_TYPE)
        .is_none_or(|v| v != "application/json")
    {
        return StatusCode::UNSUPPORTED_MEDIA_TYPE.into_response();
    }
    let Ok(node) = NodeId::new(node) else {
        return StatusCode::BAD_REQUEST.into_response();
    };
    let Ok(Ok(bytes)) =
        tokio::time::timeout(Duration::from_secs(2), to_bytes(request.into_body(), 1024)).await
    else {
        return StatusCode::PAYLOAD_TOO_LARGE.into_response();
    };
    let Ok(action) = serde_json::from_slice::<Action>(&bytes) else {
        return StatusCode::BAD_REQUEST.into_response();
    };
    let action = match action {
        Action::Open => MountAction::Open,
        Action::Touch { token } => MountAction::Touch(token),
        Action::Close { token } => MountAction::Close(token),
        Action::Start => MountAction::Start,
        Action::Stop => MountAction::Stop,
    };
    match current.session.view_mount(node, identity, action).await {
        Ok(token) => (
            [
                (header::CONTENT_TYPE, "application/json"),
                (header::CACHE_CONTROL, "no-store"),
            ],
            serde_json::to_vec(&serde_json::json!({"token":token})).expect("mount reply"),
        )
            .into_response(),
        Err(error) => (StatusCode::CONFLICT, error.to_string()).into_response(),
    }
}
