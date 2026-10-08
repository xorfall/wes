//! Installed view assets are read-only, workspace-scoped and pinned to an immutable digest.
use super::*;

pub(super) async fn read(
    Scoped(shared): Scoped,
    Path(digest): Path<String>,
    request: Request,
) -> Response {
    if digest.len() != 64 || !digest.bytes().all(|c| c.is_ascii_hexdigit()) {
        return StatusCode::BAD_REQUEST.into_response();
    }
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
    let Ok(permit) = read_admission::acquire(&shared.reads, &shared.stopped).await else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    let catalogue = match current.session.view_catalogue().await {
        Ok(c) => c,
        Err(e) => return (StatusCode::FORBIDDEN, e.to_string()).into_response(),
    };
    let Some(artifact) = catalogue.artifact(&digest).cloned() else {
        return StatusCode::NOT_FOUND.into_response();
    };
    let encoded=shared.encoders.spawn_blocking(move || {
        let _permit=permit;
        serde_json::to_vec(&serde_json::json!({"digest":artifact.digest,"definition":artifact.package.description(),
            "javascript":artifact.source.javascript,"css":artifact.source.css}))
    }).await;
    if !shared
        .application
        .current()
        .is_ok_and(|now| now.generation == current.generation)
    {
        return StatusCode::CONFLICT.into_response();
    }
    match encoded {
        Ok(Ok(bytes)) => ([(header::CONTENT_TYPE, "application/json")], bytes).into_response(),
        _ => StatusCode::SERVICE_UNAVAILABLE.into_response(),
    }
}
