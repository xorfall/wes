//! Generation/client-bound user management, never an execution or agent source command.
use super::*;
#[derive(Deserialize)]
#[serde(tag = "action", rename_all = "camelCase", deny_unknown_fields)]
enum Action {
    Preview {
        client: String,
    },
    Confirm {
        client: String,
        token: String,
        stop: bool,
        protected: bool,
    },
}
pub(super) async fn change(Scoped(shared): Scoped, request: Request) -> Response {
    let current = match shared.application.current() {
        Ok(c) => c,
        Err(_) => return StatusCode::GONE.into_response(),
    };
    if request
        .headers()
        .get("X-Wes-Session")
        .and_then(|v| v.to_str().ok())
        != Some(current.generation.as_str())
    {
        return (
            StatusCode::CONFLICT,
            "Workspace changed; request a new deletion preview.",
        )
            .into_response();
    }
    if request
        .headers()
        .get(header::CONTENT_TYPE)
        .is_none_or(|v| v != "application/json")
    {
        return StatusCode::UNSUPPORTED_MEDIA_TYPE.into_response();
    }
    let Ok(Ok(bytes)) =
        tokio::time::timeout(Duration::from_secs(10), to_bytes(request.into_body(), 4096)).await
    else {
        return StatusCode::BAD_REQUEST.into_response();
    };
    let Ok(action) = serde_json::from_slice::<Action>(&bytes) else {
        return StatusCode::BAD_REQUEST.into_response();
    };
    let result = match action {
        Action::Preview { client } => shared
            .application
            .preview_workspace_deletion(current.generation, client)
            .await
            .map(|p| serde_json::to_value(p).expect("preview")),
        Action::Confirm {
            client,
            token,
            stop,
            protected,
        } => shared
            .application
            .delete_workspace(current.generation, client, token, stop, protected)
            .await
            .map(|()| serde_json::json!({"deleted":true,"workspace":current.name.as_str()})),
    };
    match result {
        Ok(value) => services::management_json(StatusCode::OK, &value),
        Err(error) => services::management_json(
            StatusCode::CONFLICT,
            &serde_json::json!({"code":error.code(),"message":error.to_string()}),
        ),
    }
}
