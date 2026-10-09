//! One independently authorized input from a declared owned slot; never acquisition.
use super::*;

pub(super) async fn read(
    Scoped(shared): Scoped,
    Path((root, identity, owner, slot, ordinal)): Path<(String, String, String, String, usize)>,
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
    let (Ok(root), Ok(owner)) = (NodeId::new(root), NodeId::new(owner)) else {
        return StatusCode::BAD_REQUEST.into_response();
    };
    if slot.len() > 128 || ordinal >= 32 {
        return StatusCode::BAD_REQUEST.into_response();
    }
    let (revision, input_revision, epoch) = {
        let header = |key| {
            request
                .headers()
                .get(key)
                .and_then(|v| v.to_str().ok())
                .filter(|v| v.len() <= 128)
                .map(str::to_owned)
        };
        (
            header("X-Wes-View-Revision"),
            header("X-Wes-Input-Revision"),
            header("X-Wes-View-Epoch"),
        )
    };
    if revision.is_none() || input_revision.is_none() || epoch.is_none() {
        return StatusCode::BAD_REQUEST.into_response();
    }
    let Ok(permit) = read_admission::acquire(&shared.reads, &shared.stopped).await else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    if current.session.check_retirement_access().await.is_err() {
        return StatusCode::GONE.into_response();
    }
    let Ok(frame) = current.session.view_frame(root.clone()).await else {
        return StatusCode::FORBIDDEN.into_response();
    };
    if frame
        .instances
        .first()
        .is_none_or(|v| v.identity.as_ref() != identity)
        || epoch.as_deref() != Some(frame.authority_epoch.as_str())
    {
        return StatusCode::CONFLICT.into_response();
    }
    let Some(owner) = frame.instances.iter().find(|v| {
        v.id == owner
            && revision.as_deref() == Some(v.revision.to_string().as_str())
            && input_revision.as_deref() == Some(v.input_revision.to_string().as_str())
    }) else {
        return StatusCode::CONFLICT.into_response();
    };
    let Some(member) = owner
        .members
        .get(&slot)
        .and_then(|v| v.get(ordinal))
        .and_then(|id| frame.instances.iter().find(|v| &v.id == id))
    else {
        return StatusCode::FORBIDDEN.into_response();
    };
    let member = member.clone();
    let revisions = frame.revisions();
    let authority = frame.authority_epoch;
    let encoded = shared.encoders.spawn_blocking(move || -> Result<Vec<u8>, String> {
        let _permit = permit;
        let value = member.input.as_ref().and_then(|i| i.value());
        let input = value.map(|v| encode_display_value(v, Limits { bytes: 1024 * 1024, nodes: wes_budgets::get("view.frame.nodes") as usize }).map_err(|_| "limit".to_string()).and_then(|v| serde_json::from_slice::<serde_json::Value>(&v).map_err(|_| "encoding".into()))).transpose()?;
        let cautions = value.map(|v| v.provenance().cautions().iter().cloned().collect::<Vec<_>>()).unwrap_or_default();
        let complete = input.is_some() && member.input_problem.is_none() && !member.query_running && cautions.is_empty() && member.linked_inputs.is_empty();
        let bytes = serde_json::to_vec(&serde_json::json!({"available":input.is_some(),"complete":complete,"input":input,"reference":super::view_instances::reference(member.input.as_ref()),"delivery":member.input_delivery.as_str(),"problem":member.input_problem,"cautions":cautions,"queryRunning":member.query_running,"linkedFields":member.linked_inputs,"consistency":"Independent input snapshot; not a transaction across slots"})).map_err(|_| "encoding".to_string())?;
        if bytes.len() > 1024 * 1024 { return Err("limit".into()) }
        Ok(bytes)
    }).await;
    if !shared
        .application
        .current()
        .is_ok_and(|now| now.generation == current.generation)
        || current.session.check_retirement_access().await.is_err()
    {
        return StatusCode::CONFLICT.into_response();
    }
    match current.session.view_frame(root).await {
        Ok(now) if now.authority_epoch == authority && now.revisions() == revisions => {}
        _ => return StatusCode::CONFLICT.into_response(),
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
        _ => StatusCode::PAYLOAD_TOO_LARGE.into_response(),
    }
}
