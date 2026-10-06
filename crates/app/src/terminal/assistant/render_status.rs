// UI delivery receipts are observations, never authority, input data or proof of pixels.
use crate::terminal::ui::render::{self, RenderState};
fn render_unverified(status: &str) -> Value {
    json!({"status":status,"deliveryVerified":false,"visualVerified":false,
        "coverage":"selected live instance on the originating UI document; no nested-member or cross-window guarantee"})
}
fn render_target(
    observation: &SessionObservation,
    name: &str,
) -> Option<(wes_engine::graph::NodeId, String)> {
    use wes_core::{Data, MetaType, Shape};
    let (value, stopped) = bridge::named_observation(observation, name).ok()?;
    if stopped.is_some()
        || value.shape() != &Shape::Meta(MetaType::ViewInstance)
        || value.management_authority().is_none()
        || value.provenance().policy().is_private()
        || value.provenance().policy().is_unknown()
    {
        return None;
    }
    let Data::Record(fields) = value.data() else {
        return None;
    };
    let (Some(Data::Text(node)), Some(Data::Text(instance))) =
        (fields.get("id"), fields.get("instance"))
    else {
        return None;
    };
    Some((
        wes_engine::graph::NodeId::new(node.to_string()).ok()?,
        instance.to_string(),
    ))
}
async fn render_status(
    terminal: &TerminalSession,
    application: &ApplicationHandle,
    current: &CurrentSession,
    name: &str,
) -> Result<Value, BridgeReply> {
    let observation = observe_current(terminal, application, current).await?;
    let Some((node, identity)) = render_target(&observation, name) else {
        return Ok(render_unverified("reference_unavailable"));
    };
    let Ok(frame) = current.session.view_frame(node.clone()).await else {
        return Ok(render_unverified("reference_unavailable"));
    };
    let Some(root) = frame
        .instances
        .iter()
        .find(|entry| entry.id == node && entry.identity.as_ref() == identity)
    else {
        return Ok(render_unverified("reference_unavailable"));
    };
    if root
        .input
        .as_ref()
        .and_then(|input| input.value())
        .is_some_and(|value| {
            value.provenance().policy().is_private() || value.provenance().policy().is_unknown()
        })
    {
        return Ok(render_unverified("reference_unavailable"));
    }
    let scope = crate::terminal::ui::RenderScope {
        workspace: current.name.as_str().into(),
        generation: current.generation.clone(),
        node: node.as_str().into(),
        instance: identity.clone(),
    };
    let reply = match ui_request(
        terminal,
        crate::terminal::ui::Operation::ViewRenderStatus {
            scope: scope.clone(),
        },
    )
    .await
    {
        Ok(reply) => reply,
        Err(_) => return Ok(render_unverified("ui_unavailable")),
    };
    // Revalidate live value, privacy, instance and revision after the asynchronous UI read.
    let observation = observe_current(terminal, application, current).await?;
    if render_target(&observation, name) != Some((node.clone(), identity.clone())) {
        return Ok(render_unverified("reference_changed"));
    }
    let Ok(now) = current.session.view_frame(node.clone()).await else {
        return Ok(render_unverified("reference_unavailable"));
    };
    let Some(latest) = now
        .instances
        .iter()
        .find(|entry| entry.id == node && entry.identity.as_ref() == identity)
    else {
        return Ok(render_unverified("reference_changed"));
    };
    if latest
        .input
        .as_ref()
        .and_then(|input| input.value())
        .is_some_and(|value| {
            value.provenance().policy().is_private() || value.provenance().policy().is_unknown()
        })
    {
        return Ok(render_unverified("reference_unavailable"));
    }
    let Some(hosts) = render::decode(reply, &scope, &root.definition.digest) else {
        return Ok(render_unverified("invalid_ui_receipt"));
    };
    let current_revision = latest.input_revision.to_string();
    let stable =
        latest.revision == root.revision && latest.definition.artifact == root.definition.artifact;
    let acknowledged = latest.input.is_some()
        && stable
        && latest.linked_inputs.is_empty()
        && hosts.iter().any(|host| {
            host.status == RenderState::Drawn
                && host.error.is_none()
                && host.sent_sequence > 0
                && host.sent_sequence == host.ack_sequence
                && host.requested_input_revision.as_deref() == Some(&current_revision)
                && host.drawn_input_revision.as_deref() == Some(&current_revision)
        });
    let status = if !stable {
        "reference_changed"
    } else if !latest.linked_inputs.is_empty() {
        "linked_inputs_unverified"
    } else if hosts.is_empty() {
        "not_mounted"
    } else if acknowledged {
        "drawn"
    } else if hosts.iter().any(|host| host.status == RenderState::Failed) {
        "failed"
    } else {
        "awaiting_draw"
    };
    let mut result = render_unverified(status);
    result["scope"] = json!(scope);
    result["currentInputRevision"] = json!(current_revision);
    result["deliveryVerified"] = json!(acknowledged);
    result["hosts"] = json!(hosts);
    Ok(result)
}
