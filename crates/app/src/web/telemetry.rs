//! Same-origin local diagnostics; generation protects stale UI intent, not local-process identity.
use super::*;
use crate::telemetry::{
    Mode,
    schema::{Notice, Operation, Outcome, Record},
};
use serde_json::json;
fn response(value: serde_json::Value) -> Response {
    (
        [
            (header::CONTENT_TYPE, "application/json"),
            (header::CACHE_CONTROL, "no-store"),
        ],
        value.to_string(),
    )
        .into_response()
}
pub(super) async fn status(Scoped(shared): Scoped) -> Response {
    let Some(c) = &shared.services.telemetry else {
        return StatusCode::NOT_IMPLEMENTED.into_response();
    };
    let Ok(current) = shared.application.current() else {
        return StatusCode::GONE.into_response();
    };
    let mut value = c.status();
    value["generation"] = json!(current.generation);
    response(value)
}
#[derive(Deserialize)]
#[serde(tag = "action", rename_all = "snake_case", deny_unknown_fields)]
enum Action {
    Set { mode: Mode },
    Start {},
    Stop {},
    Export {},
    Clear {},
}
async fn read<T: serde::de::DeserializeOwned>(
    shared: &Shared,
    request: Request,
) -> Result<(T, String), StatusCode> {
    let current = shared.application.current().map_err(|_| StatusCode::GONE)?;
    if request
        .headers()
        .get("X-Wes-Session")
        .and_then(|v| v.to_str().ok())
        != Some(current.generation.as_str())
    {
        return Err(StatusCode::CONFLICT);
    }
    if request
        .headers()
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        != Some("application/json")
    {
        return Err(StatusCode::UNSUPPORTED_MEDIA_TYPE);
    }
    let bytes = tokio::time::timeout(Duration::from_secs(3), to_bytes(request.into_body(), 4096))
        .await
        .map_err(|_| StatusCode::REQUEST_TIMEOUT)?
        .map_err(|_| StatusCode::PAYLOAD_TOO_LARGE)?;
    let body = serde_json::from_slice(&bytes).map_err(|_| StatusCode::BAD_REQUEST)?;
    Ok((body, current.generation))
}
pub(super) async fn control(Scoped(shared): Scoped, request: Request) -> Response {
    let Some(c) = shared.services.telemetry.clone() else {
        return StatusCode::NOT_IMPLEMENTED.into_response();
    };
    let (action, generation) = match read::<Action>(&shared, request).await {
        Ok(v) => v,
        Err(r) => return r.into_response(),
    };
    let Ok(permit) = shared.queries.clone().try_acquire_owned() else {
        return StatusCode::TOO_MANY_REQUESTS.into_response();
    };
    let app = shared.application.clone();
    let result = shared.encoders.spawn_blocking(move || {
        let _permit = permit;
        let _control = c.control.try_lock().map_err(|_| "Another diagnostics operation is finishing.")?;
        if !app.current().is_ok_and(|s| s.generation == generation) { return Err("The workspace changed. Refresh diagnostics.") }
        let exported = matches!(action, Action::Export {});
        let value = match action {
            Action::Set { mode } => c.set_mode(mode).map(|_| None),
            Action::Start {} => c.start_capture().map(|_| None),
            Action::Stop {} => { c.stop_capture(); Ok(None) },
            Action::Clear {} => c.clear().map(|_| None),
            Action::Export {} => c.export().map(Some),
        }.map_err(|_| "Diagnostics operation could not complete. Check the mode and writer status, then try again.")?;
        // A workspace change during control cannot leave an old capture running.
        if !app.current().is_ok_and(|s| s.generation == generation) { c.stop_capture(); return Err("The workspace changed. Refresh diagnostics.") }
        let mut value = value.unwrap_or_else(|| c.status());
        if !exported { value["generation"] = json!(generation); }
        Ok((value, exported))
    }).await;
    match result {
        Ok(Ok((value, exported))) => {
            let mut r = response(value);
            if exported {
                r.headers_mut().insert(
                    header::CONTENT_DISPOSITION,
                    "attachment; filename=\"wes-diagnostics.json\""
                        .parse()
                        .unwrap(),
                );
            }
            r
        }
        Ok(Err(message)) => (StatusCode::CONFLICT, message).into_response(),
        Err(_) => StatusCode::SERVICE_UNAVAILABLE.into_response(),
    }
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ClientBatch {
    events: Vec<ClientEvent>,
}
#[derive(Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
enum ClientEvent {
    Error {},
    Rejection {},
    Render {},
    Reconnect {},
    Submit {
        elapsed_us: u64,
        outcome: ClientOutcome,
    },
}
#[derive(Deserialize)]
#[serde(rename_all = "snake_case")]
enum ClientOutcome {
    Ok,
    Error,
}
pub(super) async fn client(Scoped(shared): Scoped, request: Request) -> Response {
    let Some(c) = &shared.services.telemetry else {
        return StatusCode::NOT_IMPLEMENTED.into_response();
    };
    let (batch, generation) = match read::<ClientBatch>(&shared, request).await {
        Ok(v) => v,
        Err(r) => return r.into_response(),
    };
    if batch.events.len() > 16 {
        return StatusCode::PAYLOAD_TOO_LARGE.into_response();
    }
    if !shared
        .application
        .current()
        .is_ok_and(|s| s.generation == generation)
    {
        return StatusCode::CONFLICT.into_response();
    }
    if !c.enabled() {
        return StatusCode::NO_CONTENT.into_response();
    }
    if !c.admit_client() {
        return StatusCode::TOO_MANY_REQUESTS.into_response();
    }
    if batch
        .events
        .iter()
        .any(|e| matches!(e, ClientEvent::Submit { elapsed_us, .. } if *elapsed_us > 3_600_000_000))
    {
        return StatusCode::BAD_REQUEST.into_response();
    }
    for event in batch.events {
        match event {
            ClientEvent::Error {} => c.notice(Notice::UiError),
            ClientEvent::Rejection {} => c.notice(Notice::UiRejection),
            ClientEvent::Render {} => c.notice(Notice::UiRender),
            ClientEvent::Reconnect {} => c.notice(Notice::Reconnect),
            ClientEvent::Submit {
                elapsed_us,
                outcome,
            } if elapsed_us <= 3_600_000_000 => c.record(
                c.epoch.load(std::sync::atomic::Ordering::Acquire),
                Record::Operation {
                    at_ms: c.elapsed_ms(),
                    id: c
                        .sequence
                        .fetch_add(1, std::sync::atomic::Ordering::Relaxed),
                    parent: None,
                    kind: Operation::UiSubmit,
                    outcome: match outcome {
                        ClientOutcome::Ok => Outcome::Ok,
                        ClientOutcome::Error => Outcome::Error,
                    },
                    elapsed_us,
                },
            ),
            _ => return StatusCode::BAD_REQUEST.into_response(),
        }
    }
    StatusCode::NO_CONTENT.into_response()
}
