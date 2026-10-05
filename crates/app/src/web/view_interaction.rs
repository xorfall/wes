//! Small, typed interaction state. No source result or execution capability crosses this endpoint.
use super::*;
use std::collections::BTreeMap;
use wes_adapters::codec::{decode_json_for_contract, encode_request_data};
use wes_engine::views::{InteractionEdit, InteractionState, Snapshot};

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Edit {
    owner: String,
    identity: String,
    definition_revision: String,
    revision: String,
    fields: BTreeMap<String, serde_json::Value>,
    outputs: BTreeMap<String, serde_json::Value>,
    #[serde(default)]
    events: Vec<Event>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Event {
    port: String,
    value: serde_json::Value,
}
const LIMITS: Limits = Limits {
    bytes: 16 * 1024,
    nodes: 1024,
};
fn decode(edit: Edit, owner: &Snapshot) -> Result<InteractionEdit, ()> {
    let package = &owner.definition;
    let interaction = package.manifest.interaction.as_ref().ok_or(())?;
    let state = package
        .contracts
        .resolve(&interaction.state)
        .map_err(|_| ())?;
    let wes_core::contracts::ContractKind::Record(fields) = state.kind() else {
        return Err(());
    };
    let fields = edit
        .fields
        .into_iter()
        .map(|(name, json)| {
            if !interaction.shared_fields.contains(&name) {
                return Err(());
            }
            let contract = &fields.get(&name).ok_or(())?.contract;
            let bytes = serde_json::to_vec(&json).map_err(|_| ())?;
            Ok((
                name,
                decode_json_for_contract(&bytes, LIMITS, contract).map_err(|_| ())?,
            ))
        })
        .collect::<Result<_, ()>>()?;
    let outputs = edit
        .outputs
        .into_iter()
        .map(|(name, json)| {
            let port = package
                .manifest
                .outputs
                .get(&name)
                .filter(|p| p.shared && p.mode == wes_views::Mode::State)
                .ok_or(())?;
            let contract = package.contracts.resolve(&port.r#type).map_err(|_| ())?;
            let bytes = serde_json::to_vec(&json).map_err(|_| ())?;
            Ok((
                name,
                decode_json_for_contract(&bytes, LIMITS, &contract).map_err(|_| ())?,
            ))
        })
        .collect::<Result<_, ()>>()?;
    if edit.events.len() > 16 {
        return Err(());
    }
    let events = edit
        .events
        .into_iter()
        .map(|event| {
            let port = package
                .manifest
                .outputs
                .get(&event.port)
                .filter(|p| p.shared && p.mode == wes_views::Mode::Event)
                .ok_or(())?;
            let contract = package.contracts.resolve(&port.r#type).map_err(|_| ())?;
            let data = decode_json_for_contract(
                &serde_json::to_vec(&event.value).map_err(|_| ())?,
                LIMITS,
                &contract,
            )
            .map_err(|_| ())?;
            Ok(wes_engine::views::EventEmission {
                port: event.port,
                data,
            })
        })
        .collect::<Result<Vec<_>, ()>>()?;
    Ok(InteractionEdit {
        events,
        owner: NodeId::new(edit.owner).map_err(|_| ())?,
        identity: edit.identity,
        definition_revision: edit.definition_revision.parse().map_err(|_| ())?,
        revision: edit.revision.parse().map_err(|_| ())?,
        fields,
        outputs,
    })
}
fn envelope(owner: &Snapshot, state: &InteractionState) -> Result<serde_json::Value, ()> {
    let data = wes_core::Data::Record(
        [
            (
                "fields".into(),
                wes_core::Data::Record(state.fields.clone().into_iter().collect()),
            ),
            (
                "outputs".into(),
                wes_core::Data::Record(state.outputs.clone().into_iter().collect()),
            ),
        ]
        .into(),
    );
    let mut json: serde_json::Value =
        serde_json::from_slice(&encode_request_data(&data, LIMITS).map_err(|_| ())?)
            .map_err(|_| ())?;
    json["owner"] = owner.id.as_str().into();
    json["identity"] = owner.identity.as_ref().into();
    json["definitionRevision"] = owner.revision.to_string().into();
    json["revision"] = state.revision.to_string().into();
    json["definition"] = owner.definition.manifest.id.as_str().into();
    json["digest"] = owner.definition.digest.as_str().into();
    json["artifact"] = serde_json::json!(owner.definition.artifact);
    Ok(json)
}
pub(super) async fn state(
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
    let Ok(_permit) = shared.reads.clone().try_acquire_owned() else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    let Ok((mut owner, mut state)) = current
        .session
        .view_interaction(node.clone(), identity.clone(), None)
        .await
    else {
        return (StatusCode::FORBIDDEN, "View interaction is unavailable").into_response();
    };
    let mut status = StatusCode::OK;
    let mut problem = None;
    if request.method() == axum::http::Method::PUT {
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
        let Ok(edit) = serde_json::from_slice::<Edit>(&bytes)
            .map_err(|_| ())
            .and_then(|e| decode(e, &owner))
        else {
            return StatusCode::BAD_REQUEST.into_response();
        };
        match current
            .session
            .view_interaction(node.clone(), identity.clone(), Some(edit))
            .await
        {
            Ok(pair) => (owner, state) = pair,
            Err(error) => {
                problem = Some(error.to_string());
                status = StatusCode::CONFLICT;
                let Ok(pair) = current
                    .session
                    .view_interaction(node.clone(), identity.clone(), None)
                    .await
                else {
                    return StatusCode::GONE.into_response();
                };
                (owner, state) = pair;
            }
        }
    } else {
        let tag = format!(
            "\"{}-{}-{}\"",
            owner.identity, owner.revision, state.revision
        );
        if request
            .headers()
            .get(header::IF_NONE_MATCH)
            .and_then(|v| v.to_str().ok())
            == Some(&tag)
        {
            return StatusCode::NOT_MODIFIED.into_response();
        }
    }
    if !shared
        .application
        .current()
        .is_ok_and(|now| now.generation == current.generation)
    {
        return StatusCode::CONFLICT.into_response();
    }
    let Ok(mut json) = envelope(&owner, &state) else {
        return StatusCode::PAYLOAD_TOO_LARGE.into_response();
    };
    if let Some(problem) = problem {
        json["problem"] = problem.into();
    }
    let tag = format!(
        "\"{}-{}-{}\"",
        owner.identity, owner.revision, state.revision
    );
    (
        status,
        [
            (header::CONTENT_TYPE, "application/json"),
            (header::CACHE_CONTROL, "no-store"),
            (header::ETAG, tag.as_str()),
        ],
        serde_json::to_vec(&json).expect("bounded state"),
    )
        .into_response()
}
