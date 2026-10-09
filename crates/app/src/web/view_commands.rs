//! Read-only command planning. Admission and execution remain in the ordinary source endpoint.
use super::*;
#[derive(Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct CommandRequest {
    root: String,
    instance: String,
    member: String,
    revision: String,
    input_revision: String,
    template: String,
    arguments: std::collections::BTreeMap<String, serde_json::Value>,
    environments: Option<EnvironmentContext>,
}
fn count(s: &str) -> Option<u64> {
    let n: u64 = s.parse().ok()?;
    (n.to_string() == s).then_some(n)
}

pub(super) async fn prepare(Scoped(shared): Scoped, request: Request) -> Response {
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
    let Ok(Ok(bytes)) = tokio::time::timeout(
        Duration::from_secs(2),
        to_bytes(request.into_body(), 16 * 1024),
    )
    .await
    else {
        return StatusCode::PAYLOAD_TOO_LARGE.into_response();
    };
    let Ok(request) = serde_json::from_slice::<CommandRequest>(&bytes) else {
        return StatusCode::BAD_REQUEST.into_response();
    };
    let (Some(revision), Some(input_revision)) =
        (count(&request.revision), count(&request.input_revision))
    else {
        return StatusCode::BAD_REQUEST.into_response();
    };
    let environments = match request
        .environments
        .map(|c| {
            let context = wes_core::environments::EnvironmentContext {
                selected: c.selected,
                revisions: c
                    .revisions
                    .into_iter()
                    .map(|(n, r)| r.parse().map(|r| (n, r)))
                    .collect::<Result<_, _>>()?,
            };
            context.validate()?;
            Ok::<_, wes_core::environments::EnvironmentError>(context)
        })
        .transpose()
    {
        Ok(c) => c,
        Err(_) => return StatusCode::BAD_REQUEST.into_response(),
    };
    let request = wes_engine::views::commands::CommandRequest {
        root: request.root,
        instance: request.instance,
        member: request.member,
        revision,
        input_revision,
        template: request.template,
        arguments: request.arguments,
        environments,
    };
    match current.session.prepare_view_command(request).await {
        Ok(draft) => (
            [
                (header::CONTENT_TYPE, "application/json"),
                (header::CACHE_CONTROL, "no-store"),
            ],
            serde_json::to_vec(&draft).expect("command draft"),
        )
            .into_response(),
        Err(_) => StatusCode::CONFLICT.into_response(),
    }
}
