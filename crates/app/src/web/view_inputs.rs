//! Small input-field overlays. Large immutable input frames are deliberately absent.
use super::*;
use sha2::{Digest, Sha256};
pub(super) async fn read(
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
    let Ok(node) = NodeId::new(node) else {
        return StatusCode::BAD_REQUEST.into_response();
    };
    let Ok(_permit) = read_admission::acquire(&shared.reads, &shared.stopped).await else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    let Ok(patches) = current
        .session
        .view_input_patches(node.clone(), identity.clone())
        .await
    else {
        return (StatusCode::FORBIDDEN, "View bindings are unavailable").into_response();
    };
    let etag = format!(
        "\"{:x}\"",
        Sha256::digest(format!("{:?}:{:?}", patches.revisions, patches.problems))
    );
    if request
        .headers()
        .get(header::IF_NONE_MATCH)
        .and_then(|v| v.to_str().ok())
        == Some(&etag)
    {
        return StatusCode::NOT_MODIFIED.into_response();
    }
    let mut values = serde_json::Map::new();
    let mut bytes = 0;
    for (node, fields) in patches.values {
        let mut object = serde_json::Map::new();
        for (name, value) in fields {
            let Ok(encoded) = encode_display_value(
                &value,
                Limits {
                    bytes: 16 * 1024,
                    nodes: 1024,
                },
            ) else {
                return StatusCode::PAYLOAD_TOO_LARGE.into_response();
            };
            bytes += encoded.len();
            if bytes > 256 * 1024 {
                return StatusCode::PAYLOAD_TOO_LARGE.into_response();
            }
            object.insert(
                name,
                serde_json::from_slice(&encoded).expect("encoded value"),
            );
        }
        values.insert(node.to_string(), object.into());
    }
    let problems = patches
        .problems
        .iter()
        .map(|(id, p)| (id.to_string(), p.clone()))
        .collect::<std::collections::BTreeMap<_, _>>();
    let cautions = patches
        .cautions
        .iter()
        .map(|(id, items)| (id.to_string(), items))
        .collect::<std::collections::BTreeMap<_, _>>();
    let body = serde_json::to_vec(
        &serde_json::json!({"values":values,"problems":problems,"cautions":cautions}),
    )
    .expect("bounded input overlays");
    if !shared
        .application
        .current()
        .is_ok_and(|c| c.generation == current.generation)
    {
        return StatusCode::CONFLICT.into_response();
    }
    match current.session.view_input_patches(node, identity).await {
        Ok(now) if now.revisions == patches.revisions && now.problems == patches.problems => {}
        Ok(_) => return StatusCode::SERVICE_UNAVAILABLE.into_response(),
        Err(_) => return StatusCode::FORBIDDEN.into_response(),
    }
    (
        [
            (header::CONTENT_TYPE, "application/json"),
            (header::CACHE_CONTROL, "no-store"),
            (header::ETAG, etag.as_str()),
        ],
        body,
    )
        .into_response()
}
