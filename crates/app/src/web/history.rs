//! Bounded saved Log pages beside live SSE metadata. Reading never submits or replays source.
use super::projection;
use axum::{
    body::{Body, Bytes},
    extract::Request,
    http::{StatusCode, header},
    response::{IntoResponse, Response},
};
use serde_json::json;
use std::{
    convert::Infallible,
    io::{self, Write},
};
use wes_engine::{
    history::{HistoryCursor, HistoryPageLimits, Persistence, RecordError},
    recording::CapturedPage,
};

fn max_bytes() -> usize {
    wes_budgets::get("transport.history.bytes") as usize
}
pub(super) async fn read(super::Scoped(shared): super::Scoped, request: Request) -> Response {
    let Ok(current) = shared.application.current() else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    if request
        .headers()
        .get("x-wes-session")
        .and_then(|value| value.to_str().ok())
        != Some(current.generation.as_str())
    {
        return (
            StatusCode::CONFLICT,
            "session changed; reconnect before reading saved history",
        )
            .into_response();
    }
    if current.session.check_retirement_access().await.is_err() {
        return super::services::management_error(crate::retention::ManagementError::Retiring);
    }
    let query = request.uri().query().unwrap_or("");
    if query.len() > 1024 {
        return StatusCode::PAYLOAD_TOO_LARGE.into_response();
    }
    let mut cursor = None;
    for (key, value) in url::form_urlencoded::parse(query.as_bytes()) {
        if key != "cursor" || cursor.is_some() {
            return StatusCode::BAD_REQUEST.into_response();
        }
        let Ok(value) = value.parse::<HistoryCursor>() else {
            return (StatusCode::BAD_REQUEST, "invalid history cursor").into_response();
        };
        cursor = Some(value);
    }
    let Ok(permit) = shared.queries.clone().try_acquire_owned() else {
        return StatusCode::TOO_MANY_REQUESTS.into_response();
    };
    let generation = current.generation.clone();
    // The tracker owns this task after disconnect. The query permit spans read, joined encoding and
    // response consumption, while the recorder independently owns any entered physical file read.
    let encoded = shared
        .encoders
        .spawn(async move {
            let captured = current
                .session
                .try_history_page(
                    cursor,
                    HistoryPageLimits {
                        entries: 200,
                        bytes: (max_bytes() - 4096) as u64,
                    },
                )
                .await?;
            let encoded = tokio::task::spawn_blocking(move || encode(captured, current.generation))
                .await
                .map_err(|_| RecordError::Closed)?
                .map_err(|error| RecordError::backend("encoding saved history", false, error))?;
            Ok::<_, RecordError>((encoded, permit))
        })
        .await;
    let (bytes, permit) = match encoded {
        Ok(Ok(result)) => result,
        Ok(Err(RecordError::InvalidCursor)) => {
            return (
                StatusCode::CONFLICT,
                "saved-history cursor expired; start from the beginning",
            )
                .into_response();
        }
        Ok(Err(RecordError::ReadBusy)) => {
            return (
                StatusCode::TOO_MANY_REQUESTS,
                "saved-history reader is busy; try again",
            )
                .into_response();
        }
        Ok(Err(RecordError::PageUnsupported)) => {
            return (
                StatusCode::NOT_IMPLEMENTED,
                "saved history is unavailable for this session",
            )
                .into_response();
        }
        Ok(Err(RecordError::Limit(_))) => {
            return (
                StatusCode::PAYLOAD_TOO_LARGE,
                "a history entry exceeds the browser page budget",
            )
                .into_response();
        }
        _ => {
            return (
                StatusCode::SERVICE_UNAVAILABLE,
                "saved history could not be read",
            )
                .into_response();
        }
    };
    if !shared
        .application
        .current()
        .is_ok_and(|now| now.generation == generation)
    {
        return (
            StatusCode::CONFLICT,
            "session changed while reading saved history",
        )
            .into_response();
    }
    let chunks = futures_util::stream::unfold(
        (Bytes::from(bytes), permit),
        |(mut bytes, permit)| async move {
            if bytes.is_empty() {
                None
            } else {
                let next = bytes.split_to(bytes.len().min(64 * 1024));
                Some((Ok::<_, Infallible>(next), (bytes, permit)))
            }
        },
    );
    (
        [
            (header::CONTENT_TYPE, "application/json"),
            (header::CACHE_CONTROL, "no-store"),
        ],
        Body::from_stream(chunks),
    )
        .into_response()
}
fn encode(captured: CapturedPage, generation: String) -> io::Result<Vec<u8>> {
    let page = captured.page();
    let mut entries = vec![];
    for entry in page.entries() {
        if let Some(event) = projection::history_event(
            &entry.entry,
            page.checkpoint().persistence != Persistence::Volatile,
            "",
        )? {
            entries.push(event);
        }
    }
    let value = json!({"generation":generation, "entries":entries, "next":page.next().map(|cursor| cursor.to_string()), "through":page.checkpoint().end_offset.to_string(), "unconfirmedWrites":captured.append_report().failed.to_string()});
    let mut writer = Limited(Vec::new());
    serde_json::to_writer(&mut writer, &value)?;
    Ok(writer.0)
}
struct Limited(Vec<u8>);
impl Write for Limited {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        if bytes.len() > max_bytes().saturating_sub(self.0.len()) {
            return Err(io::Error::other(
                "saved history response exceeds its byte budget",
            ));
        }
        self.0.extend_from_slice(bytes);
        Ok(bytes.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}
