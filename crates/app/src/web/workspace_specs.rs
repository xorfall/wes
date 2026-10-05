//! User-only read of retained API contracts. Never publishes library records or reopens sources.
use super::*;
use serde::Deserialize;
use serde_json::json;
use wes_adapters::api_library::digest;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Selection {
    environment: Option<String>,
    alias: Option<String>,
    revision: Option<String>,
}
pub(super) async fn read(Scoped(shared): Scoped, request: Request) -> Response {
    let query = request.uri().query().unwrap_or("");
    if query.len() > 16 * 1024 {
        return StatusCode::URI_TOO_LONG.into_response();
    }
    let mut fields = serde_json::Map::new();
    for (key, value) in url::form_urlencoded::parse(query.as_bytes()) {
        if fields
            .insert(key.into_owned(), value.into_owned().into())
            .is_some()
        {
            return (StatusCode::BAD_REQUEST, "Duplicate inspection parameter.").into_response();
        }
    }
    let Ok(selection) = serde_json::from_value::<Selection>(fields.into()) else {
        return (StatusCode::BAD_REQUEST, "Invalid inspection selection.").into_response();
    };
    let Ok(current) = shared.application.current() else {
        return StatusCode::GONE.into_response();
    };
    if request
        .headers()
        .get("X-Wes-Session")
        .and_then(|v| v.to_str().ok())
        != Some(current.generation.as_str())
    {
        return (StatusCode::CONFLICT, "Workspace changed; refresh /spec.").into_response();
    }
    let selected = match (selection.environment, selection.alias, selection.revision) {
        (None, None, None) => None,
        (Some(env), Some(alias), Some(revision)) => Some((env, alias, revision)),
        _ => {
            return (
                StatusCode::BAD_REQUEST,
                "Select environment, alias and revision together.",
            )
                .into_response();
        }
    };
    let Ok(permit) = shared.reads.clone().try_acquire_owned() else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    if current.session.check_retirement_access().await.is_err() {
        return services::management_error(crate::retention::ManagementError::Retiring);
    }
    let Ok(specs) = current.session.imported_specs().await else {
        return StatusCode::GONE.into_response();
    };
    let workspace = current.name.as_str().to_owned();
    let generation = current.generation.clone();
    let result = shared.encoders.spawn_blocking(move || {
        let _permit = permit;
        let summary = |s: &wes_engine::workspace::ImportedSpec| json!({
            "environment": s.environment, "alias": s.alias, "origin": s.origin,
            "revision": digest(s.source.as_bytes()), "bytes": s.source.len(),
        });
        let (value, limit) = if let Some((env, alias, revision)) = selected {
            let Some(spec) = specs.iter().find(|s| s.environment == env && s.alias == alias) else {
                return Err((StatusCode::NOT_FOUND, "Imported API is no longer available; refresh /spec."));
            };
            let entry = summary(spec);
            if entry["revision"] != revision {
                return Err((StatusCode::CONFLICT, "Imported API changed; refresh /spec."));
            }
            (json!({"workspace":workspace,"generation":generation,"spec":entry,"source":spec.source.as_ref()}), 16 * 1024 * 1024)
        } else {
            (json!({"workspace":workspace,"generation":generation,"specs":specs.iter().map(summary).collect::<Vec<_>>()}), 1024 * 1024)
        };
        let bytes = serde_json::to_vec(&value).map_err(|_| (StatusCode::INTERNAL_SERVER_ERROR, "Could not encode captured APIs."))?;
        if bytes.len() > limit { return Err((StatusCode::PAYLOAD_TOO_LARGE, "Captured API inspection exceeds the response limit.")); }
        Ok(bytes)
    }).await;
    if !shared
        .application
        .current()
        .is_ok_and(|now| now.generation == current.generation)
    {
        return (StatusCode::CONFLICT, "Workspace changed; refresh /spec.").into_response();
    }
    match result {
        Ok(Ok(bytes)) => (
            [
                (header::CONTENT_TYPE, "application/json"),
                (header::CACHE_CONTROL, "no-store"),
            ],
            bytes,
        )
            .into_response(),
        Ok(Err((status, message))) => (status, message).into_response(),
        Err(_) => StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    }
}
