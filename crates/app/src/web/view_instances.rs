//! Finite, demand-driven view frames. References are resolved by the workspace, not from JSON.
use super::*;
use sha2::{Digest, Sha256};

pub(super) async fn read(
    Scoped(shared): Scoped,
    Path((node, instance)): Path<(String, String)>,
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
    let Ok(permit) = shared.reads.clone().try_acquire_owned() else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    let previous = request
        .headers()
        .get("X-Wes-View-Revisions")
        .and_then(|v| v.to_str().ok())
        .filter(|s| s.len() <= 8192)
        .and_then(|s| serde_json::from_str::<Vec<[String; 4]>>(s).ok())
        .filter(|v| v.len() <= 32)
        .unwrap_or_default();
    let observation = request
        .headers()
        .get("X-Wes-View-Mount")
        .and_then(|v| v.to_str().ok())
        .map(str::to_owned);
    let read = match observation {
        Some(token) => {
            current
                .session
                .observed_view_frame(node.clone(), instance.clone(), token)
                .await
        }
        None => current.session.view_frame(node.clone()).await,
    };
    let frame = match read {
        Ok(frame) => frame,
        Err(error) => return (StatusCode::FORBIDDEN, error.to_string()).into_response(),
    };
    if frame
        .instances
        .first()
        .is_none_or(|root| root.identity.as_ref() != instance)
    {
        return (
            StatusCode::CONFLICT,
            "View identity changed; open the current result",
        )
            .into_response();
    }
    let revisions = frame.revisions();
    let bindings = frame.binding_revisions();
    let etag = format!("\"{:x}\"", Sha256::digest(format!("{revisions:?}")));
    if request
        .headers()
        .get(header::IF_NONE_MATCH)
        .and_then(|v| v.to_str().ok())
        == Some(etag.as_str())
    {
        return (
            StatusCode::NOT_MODIFIED,
            [(header::CACHE_CONTROL, "no-store")],
        )
            .into_response();
    }
    let following = frame.instances.iter().any(|i| {
        i.query.as_ref().is_some_and(|q| q.adapter.is_some())
            || i.input
                .as_ref()
                .is_some_and(|_| i.input_delivery == wes_engine::views::InputDelivery::Window)
    });
    let byte_limit = if following {
        wes_budgets::get("view.frame.live.bytes") as usize
    } else {
        wes_budgets::get("view.frame.finite.bytes") as usize
    };
    let encoded = shared.encoders.spawn_blocking(move || -> Result<Vec<u8>, String> {
        let _permit = permit;
        let mut entries = Vec::new();
        let mut bytes = 0;
        for instance in frame.instances {
            let unchanged=previous.iter().any(|p|p[0]==instance.id.as_str() && p[1]==instance.identity.as_ref() && p[2]==instance.revision.to_string() && p[3]==instance.input_revision.to_string());
            let input = if unchanged {None}else{instance.input.as_ref().and_then(|input| input.value()).map(|value| {
                let encoded = encode_display_value(value, Limits { bytes: byte_limit, nodes: wes_budgets::get("view.frame.nodes") as usize })
                    .map_err(|e| e.to_string())?;
                bytes += encoded.len();
                if bytes > byte_limit { return Err("View frame exceeds its display budget".into()); }
                serde_json::from_slice::<serde_json::Value>(&encoded).map_err(|e| e.to_string())
            }).transpose()?};
            entries.push(serde_json::json!({"id":instance.id.as_str(),"instance":instance.identity.as_ref(),"revision":instance.revision.to_string(),
                "definition":instance.definition.manifest.id,"digest":instance.definition.digest,"artifact":instance.definition.artifact,
                "query":instance.query.as_ref().map(|q|serde_json::json!({"environment":q.environment.as_ref().and_then(|c|c.selected.as_deref()),"template":q.template,"mode":if q.adapter.is_some(){"live"}else{"finite"},"adapter":q.adapter.as_ref().map(|a|a.template.as_str()),"source":q.source.id().as_str(),"output":q.port,"trigger":q.trigger.as_str(),"running":instance.query_running})),"unchanged":unchanged,"inputReference":reference(instance.input.as_ref()),"inputDelivery":instance.input_delivery.as_str(),"observing":instance.observing,"inputProblem":instance.input_problem,"inputCautions":instance.input.as_ref().and_then(|i|i.value()).map(|v|v.provenance().cautions().iter().cloned().collect::<Vec<_>>()).unwrap_or_default(),"inputRevision":instance.input_revision.to_string(),"input":input,"linkedInputs":instance.linked_inputs,"members":instance.members.into_iter().map(|(s,ids)|
                    (s,ids.into_iter().map(|id|id.to_string()).collect::<Vec<_>>())).collect::<std::collections::BTreeMap<_,_>>() }));
        }
        serde_json::to_vec(&serde_json::json!({"root":frame.root.as_str(),"instances":entries})).map_err(|e| e.to_string())
    }).await;
    if !shared
        .application
        .current()
        .is_ok_and(|now| now.generation == current.generation)
    {
        return StatusCode::CONFLICT.into_response();
    }
    match current.session.view_frame(node.clone()).await {
        Ok(frame) if frame.binding_revisions() == bindings => {}
        Ok(frame)
            if frame
                .instances
                .first()
                .is_some_and(|root| root.identity.as_ref() == instance) =>
        {
            return StatusCode::SERVICE_UNAVAILABLE.into_response();
        }
        Ok(_) => return StatusCode::CONFLICT.into_response(),
        Err(error) => return (StatusCode::FORBIDDEN, error.to_string()).into_response(),
    }
    match encoded {
        Ok(Ok(bytes)) => (
            [
                (header::CONTENT_TYPE, "application/json"),
                (header::CACHE_CONTROL, "no-store"),
                (header::ETAG, &etag),
            ],
            bytes,
        )
            .into_response(),
        _ => {
            if following {
                let _ = current
                    .session
                    .view_mount(node, instance, wes_engine::views::MountAction::Stop)
                    .await;
            }
            (StatusCode::PAYLOAD_TOO_LARGE,if following {"Observation paused: changed view inputs exceed the 1 MiB frame budget. Reduce the source window."}else{"View frame exceeds its 8 MiB display budget"}).into_response()
        }
    }
}

fn reference(input: Option<&wes_engine::views::Input>) -> serde_json::Value {
    use wes_engine::views::InputBinding;
    match input.map(|i| &i.binding) {
        Some(InputBinding::Current(source)) => {
            serde_json::json!({"kind":"current","node":source.output.node.as_str(),"port":port_name(source.output.port),"fields":source.fields,"shownRun":source.run().map(|r|r.as_str())})
        }
        Some(InputBinding::Retained(saved)) => {
            serde_json::json!({"kind":"retained","node":saved.node().as_str(),"run":saved.run().as_str(),"handle":saved.handle().as_str(),"origin":saved.origin().map(|s|serde_json::json!({"node":s.node.as_str(),"port":port_name(s.port),"fields":s.fields,"run":s.run.as_ref().map(|r|r.as_str())}))})
        }
        _ => serde_json::json!({"kind":"unlinked"}),
    }
}

fn port_name(port: wes_engine::graph::OutputPort) -> &'static str {
    match port {
        wes_engine::graph::OutputPort::Data => "data",
        wes_engine::graph::OutputPort::Error => "error",
        wes_engine::graph::OutputPort::Cancel => "cancel",
    }
}
