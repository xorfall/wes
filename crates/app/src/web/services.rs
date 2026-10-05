//! Optional backend services beside serialized workspace command admission.
use super::Shared;
use axum::{
    extract::{Request, State},
    http::{StatusCode, header},
    response::{IntoResponse, Response},
};
use serde_json::json;
use std::{collections::BTreeMap, sync::Arc};
use wes_adapters::inventory::{FileInventory, StoragePlace};
use wes_engine::{
    completion::{Completer, CompletionError, Suggestions},
    driver::CancellationToken,
};

#[derive(Default)]
pub struct Services {
    pub telemetry: Option<Arc<crate::telemetry::Controller>>,
    pub data_home: Option<crate::data_home::host::Connection>,
    pub terminal: Option<crate::terminal::Config>,
    pub budgets: Option<Arc<crate::budgets::Store>>,
    /// The data home's own credential vault; None where a system secure store is used.
    pub credential_vault: Option<Arc<crate::credential_vault::CredentialVault>>,
    pub desktop_preferences: Option<super::DesktopPreferences>,
    pub api_library: Option<crate::api_library::ApiLibrary>,
    pub inventory: Option<FileInventory>,
    pub completers: BTreeMap<String, Arc<dyn Completer>>,
    /// Where `/edit-files` saves a named editor buffer: `<edit_home>/<context>/<name>.yaml`.
    pub edit_home: Option<std::path::PathBuf>,
    /// `<data home>/presentations`, served read-only by `GET /presentations` as transport.
    pub presentations: Option<std::path::PathBuf>,
}
#[derive(Clone)]
pub(super) struct StorageReport {
    pub generation: String,
    pub workspace: String,
    pub places: Vec<StoragePlace>,
    pub retention: wes_engine::storage::RetentionUsage,
}
impl StorageReport {
    pub fn event(&self) -> serde_json::Value {
        json!({"event":"storage","workspace":self.workspace,"places":self.places.iter().map(|p|json!({"name":p.name,"where":p.location,"holds":p.holds,"durable":p.durable,"files":p.files,"bytes":p.bytes})).collect::<Vec<_>>(),
            "retention":{"classes":self.retention.classes.iter().map(|(kind,count,bytes)|json!({"kind":kind.as_str(),"count":count,"bytes":bytes})).collect::<Vec<_>>(),"liveBytes":self.retention.live_bytes,"archiveBytes":self.retention.archive_bytes,"privateCount":self.retention.private_count,"privateBytes":self.retention.private_bytes}})
    }
}
pub(super) async fn storage(shared: &Shared, current: crate::CurrentSession) -> Response {
    if shared.services.inventory.is_none() {
        return (
            StatusCode::NOT_IMPLEMENTED,
            "storage inventory is not configured",
        )
            .into_response();
    }
    let Ok(permit) = shared.queries.clone().try_acquire_owned() else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    let services = shared.services.clone();
    let result = shared
        .encoders
        .spawn_blocking(move || {
            let _permit = permit;
            services
                .inventory
                .as_ref()
                .expect("configured inventory")
                .read()
        })
        .await;
    let places = match result {
        Ok(Ok(places)) => places,
        _ => {
            return (
                StatusCode::SERVICE_UNAVAILABLE,
                "storage inventory could not be read",
            )
                .into_response();
        }
    };
    let retention = match shared.application.retention_usage().await {
        Ok(usage) => usage,
        Err(error) => return management_error(error),
    };
    if !shared
        .application
        .current()
        .is_ok_and(|now| now.generation == current.generation)
    {
        return (
            StatusCode::CONFLICT,
            "session changed while reading storage inventory",
        )
            .into_response();
    }
    shared
        .storage_reports
        .send_replace(Some(Arc::new(StorageReport {
            generation: current.generation,
            workspace: current.name.as_str().into(),
            places,
            retention,
        })));
    (
        StatusCode::ACCEPTED,
        [(header::CONTENT_TYPE, "application/json")],
        "{\"accepted\":true}",
    )
        .into_response()
}
pub(super) fn management_error(error: crate::retention::ManagementError) -> Response {
    let status = match error {
        crate::retention::ManagementError::Stale => StatusCode::CONFLICT,
        crate::retention::ManagementError::Missing => StatusCode::NOT_FOUND,
        _ => StatusCode::SERVICE_UNAVAILABLE,
    };
    let blockers = match &error {
        crate::retention::ManagementError::Work(
            wes_engine::session::retirement::RetirementError::Blocked(blockers),
        ) => blockers
            .iter()
            .map(|blocker| {
                json!({
                    "node": blocker.node.as_ref().map(ToString::to_string),
                    "cells": blocker.cells,
                    "state": blocker.state,
                    "reason": blocker.reason,
                })
            })
            .collect::<Vec<_>>(),
        _ => Vec::new(),
    };
    management_json(
        status,
        &json!({"code":error.code(),"message":error.to_string(),"blockers":blockers,
        "mayHaveApplied":matches!(error, crate::retention::ManagementError::ProtectionUnconfirmed | crate::retention::ManagementError::Unconfirmed | crate::retention::ManagementError::WorkUnconfirmed | crate::retention::ManagementError::CleanupPending),"retryable":false}),
    )
}
pub(super) fn management_json(status: StatusCode, value: &impl serde::Serialize) -> Response {
    match serde_json::to_string(value) {
        Ok(text) => (status, [(header::CONTENT_TYPE, "application/json")], text).into_response(),
        Err(_) => StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    }
}
pub(super) async fn complete(State(shared): State<Shared>, request: Request) -> Response {
    let query = request.uri().query().unwrap_or("");
    if query.len() > 32 * 1024 {
        return StatusCode::URI_TOO_LONG.into_response();
    }
    let mut language = String::new();
    let mut written = String::new();
    let mut caret = None;
    for (key, value) in url::form_urlencoded::parse(query.as_bytes()) {
        match key.as_ref() {
            "in" => language = value.into_owned(),
            "line" => written = value.into_owned(),
            "caret" => caret = value.parse::<i64>().ok(),
            _ => {}
        }
    }
    if written.len() > 16 * 1024 || language.len() > 128 {
        return StatusCode::PAYLOAD_TOO_LARGE.into_response();
    }
    let length = written.encode_utf16().count();
    let caret = caret.map_or(length, |at| at.clamp(0, length as i64) as usize);
    let text = wes_language::SourceText::new("completion", written);
    let Ok(at) = text.byte_offset(caret) else {
        return (StatusCode::BAD_REQUEST, "caret splits an encoded character").into_response();
    };
    let Some(completer) = shared.services.completers.get(&language).cloned() else {
        return suggestions(Suggestions::default(), text.text(), at);
    };
    let Ok(permit) = shared.queries.clone().try_acquire_owned() else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    let cancelled: CancellationToken = shared.stopped.clone();
    let completed = shared
        .encoders
        .spawn_blocking(move || {
            let _permit = permit;
            let result = completer.complete(text.text(), at, &cancelled);
            (result, text)
        })
        .await;
    match completed {
        Ok((Ok(result), text)) => suggestions(result, text.text(), at),
        Ok((Err(CompletionError::Invalid), _)) => {
            (StatusCode::BAD_REQUEST, "invalid completion request").into_response()
        }
        _ => (
            StatusCode::SERVICE_UNAVAILABLE,
            "completion could not be produced",
        )
            .into_response(),
    }
}
fn suggestions(found: Suggestions, written: &str, caret: usize) -> Response {
    let Some(prefix) = written.get(..found.from).filter(|_| found.from <= caret) else {
        return StatusCode::INTERNAL_SERVER_ERROR.into_response();
    };
    let bytes = found.items.iter().fold(0usize, |sum, item| {
        sum.saturating_add(item.text.len())
            .saturating_add(item.kind.len())
            .saturating_add(item.detail.len())
    });
    if found.items.len() > 60 || bytes > 128 * 1024 {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    }
    let event = json!({"from":prefix.encode_utf16().count(),"items":found.items.iter().map(|item|json!({"text":item.text,"kind":item.kind,"detail":item.detail})).collect::<Vec<_>>()});
    (
        [(header::CONTENT_TYPE, "application/json")],
        event.to_string(),
    )
        .into_response()
}
