//! Saves a named editor buffer to disk: `POST /edit-files`. Loading it back into the workspace
//! (`:package load path:…`) is a separate, explicit step — this endpoint only writes the file.
use super::*;
use cap_fs_ext::DirExt;
use cap_std::{ambient_authority, fs::Dir};
use serde::Deserialize;
use serde_json::json;

fn max_bytes() -> usize {
    wes_budgets::get("ui.edit.bytes") as usize
}
const CONTEXTS: &[&str] = &["env", "types"];

pub(super) async fn environments(Scoped(shared): Scoped) -> Response {
    let Ok(current) = shared.application.current() else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    let Ok(_permit) = shared.queries.clone().try_acquire_owned() else {
        return StatusCode::TOO_MANY_REQUESTS.into_response();
    };
    match current.session.environment_documents().await {
        Ok(documents) => {
            let body = json!({"generation":current.generation,"documents":documents.into_iter().map(|document| json!({
                "name":document.name,"environments":document.environments,"source":document.source,"origin":document.origin
            })).collect::<Vec<_>>()}).to_string();
            if body.len() > 2 * max_bytes() {
                return (
                    StatusCode::PAYLOAD_TOO_LARGE,
                    "Environment documents exceed 2 MiB",
                )
                    .into_response();
            }
            (
                [
                    (header::CONTENT_TYPE, "application/json"),
                    (header::CACHE_CONTROL, "no-store"),
                ],
                body,
            )
                .into_response()
        }
        Err(error) => (StatusCode::CONFLICT, error.to_string()).into_response(),
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Save {
    context: String,
    name: String,
    content: String,
}

/// A single path segment: no separator, no `.`/`..`, non-empty, and short enough to be a filename.
fn safe_segment(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 128
        && value != "."
        && value != ".."
        && !value.contains(std::path::MAIN_SEPARATOR)
        && !value.contains('/')
        && !value.contains('\0')
}

pub(super) async fn save(State(shared): State<Shared>, request: Request) -> Response {
    if request
        .headers()
        .get(header::CONTENT_TYPE)
        .is_none_or(|v| v != "application/json")
    {
        return StatusCode::UNSUPPORTED_MEDIA_TYPE.into_response();
    }
    let Some(edit_home) = shared.services.edit_home.clone() else {
        return StatusCode::NOT_IMPLEMENTED.into_response();
    };
    let bytes = match tokio::time::timeout(
        Duration::from_secs(5),
        to_bytes(request.into_body(), max_bytes() * 6 + 4096),
    )
    .await
    {
        Ok(Ok(bytes)) => bytes,
        _ => return StatusCode::PAYLOAD_TOO_LARGE.into_response(),
    };
    let request: Save = match serde_json::from_slice(&bytes) {
        Ok(request) => request,
        Err(_) => return (StatusCode::BAD_REQUEST, "invalid save request").into_response(),
    };
    if !CONTEXTS.contains(&request.context.as_str()) {
        return (StatusCode::BAD_REQUEST, "unknown edit context").into_response();
    }
    if !safe_segment(&request.name) {
        return (StatusCode::BAD_REQUEST, "invalid file name").into_response();
    }
    if request.content.len() > max_bytes() {
        return (StatusCode::PAYLOAD_TOO_LARGE, "content exceeds 1 MiB").into_response();
    }
    let path = edit_home
        .join(&request.context)
        .join(format!("{}.yaml", request.name));
    let Ok(permit) = shared.queries.clone().try_acquire_owned() else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    let saved = shared
        .encoders
        .spawn_blocking({
            let path = path.clone();
            move || {
                let _permit = permit;
                // Open each managed component without following symlinks, then keep the
                // directory capability through the write to avoid a check/open race.
                let home = Dir::open_ambient_dir(
                    edit_home
                        .parent()
                        .ok_or_else(|| io::Error::other("missing edit parent"))?,
                    ambient_authority(),
                )?;
                let edit_name = edit_home
                    .file_name()
                    .ok_or_else(|| io::Error::other("missing edit directory"))?;
                let edit = child_directory(&home, edit_name)?;
                let context = child_directory(&edit, std::ffi::OsStr::new(&request.context))?;
                wes_adapters::api_library::atomic_bytes_in(
                    &context,
                    path.file_name()
                        .ok_or_else(|| io::Error::other("missing file name"))?,
                    request.content.as_bytes(),
                    max_bytes(),
                    "content exceeds 1 MiB",
                )
            }
        })
        .await;
    match saved {
        Ok(Ok(())) => (
            [
                (header::CONTENT_TYPE, "application/json"),
                (header::CACHE_CONTROL, "no-store"),
            ],
            json!({"path": path.to_string_lossy()}).to_string(),
        )
            .into_response(),
        _ => (StatusCode::INTERNAL_SERVER_ERROR, "could not save the file").into_response(),
    }
}

fn child_directory(parent: &Dir, name: &std::ffi::OsStr) -> io::Result<Dir> {
    match parent.create_dir(name) {
        Ok(()) => (),
        Err(error) if error.kind() == io::ErrorKind::AlreadyExists => (),
        Err(error) => return Err(error),
    }
    parent.open_dir_nofollow(name)
}
