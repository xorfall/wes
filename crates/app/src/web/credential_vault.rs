//! Data-home credential vault controls. Passwords are never echoed, logged or persisted.
use super::*;
use crate::credential_vault::{VaultError, VaultStatus};
use serde_json::json;

/// Passwords are bounded at 1 KiB; the envelope adds only the action name.
const REQUEST_BYTES: usize = 4 * 1024;
/// A reset permanently discards remembered credentials, so the request names it explicitly.
const RESET_CONFIRMATION: &str = "reset";

#[derive(Deserialize)]
#[serde(tag = "action", rename_all = "camelCase", deny_unknown_fields)]
enum Action {
    Create { password: String },
    Unlock { password: String },
    Lock,
    Reset { confirm: String },
}

pub(super) async fn read(State(shared): State<Shared>) -> Response {
    perform(shared, None).await
}

pub(super) async fn change(State(shared): State<Shared>, request: Request) -> Response {
    if !request
        .headers()
        .get(header::CONTENT_TYPE)
        .is_some_and(|h| h == "application/json")
    {
        return StatusCode::UNSUPPORTED_MEDIA_TYPE.into_response();
    }
    let bytes = match tokio::time::timeout(
        Duration::from_secs(5),
        to_bytes(request.into_body(), REQUEST_BYTES),
    )
    .await
    {
        Ok(Ok(bytes)) => bytes,
        _ => return StatusCode::PAYLOAD_TOO_LARGE.into_response(),
    };
    // Deserializer errors can quote the submitted password; never return them.
    let Ok(action) = serde_json::from_slice::<Action>(&bytes) else {
        return (StatusCode::BAD_REQUEST, "Invalid credential vault request.").into_response();
    };
    if matches!(&action, Action::Reset { confirm } if confirm != RESET_CONFIRMATION) {
        return (
            StatusCode::BAD_REQUEST,
            "Confirm the reset to remove the credential vault.",
        )
            .into_response();
    }
    perform(shared, Some(action)).await
}

async fn perform(shared: Shared, action: Option<Action>) -> Response {
    let Some(vault) = shared.services.credential_vault.clone() else {
        return match action {
            None => respond(json!({"kind": "system"})),
            Some(_) => (
                StatusCode::NOT_IMPLEMENTED,
                "This platform keeps remembered credentials in its system secure store.",
            )
                .into_response(),
        };
    };
    let changes = action.is_some();
    let result = shared
        .encoders
        .spawn_blocking(move || {
            match action {
                Some(Action::Create { password }) => vault.create(&password.into())?,
                Some(Action::Unlock { password }) => vault.unlock(&password.into())?,
                Some(Action::Lock) => vault.lock()?,
                Some(Action::Reset { .. }) => vault.reset()?,
                None => {}
            }
            vault.status()
        })
        .await;
    if changes {
        // Projections re-read credential availability after any vault transition.
        shared.credential_updates.send_replace(());
    }
    match result {
        Ok(Ok(status)) => respond(state(status)),
        Ok(Err(error)) => failure(error),
        Err(_) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            "The credential vault operation could not be confirmed; reload before retrying.",
        )
            .into_response(),
    }
}

fn state(status: VaultStatus) -> serde_json::Value {
    json!({"kind": "vault", "state": status.as_str()})
}

fn respond(body: serde_json::Value) -> Response {
    (
        [
            (header::CONTENT_TYPE, "application/json"),
            (header::CACHE_CONTROL, "no-store"),
        ],
        body.to_string(),
    )
        .into_response()
}

fn failure(error: VaultError) -> Response {
    let status = match &error {
        VaultError::WeakPassword => StatusCode::BAD_REQUEST,
        VaultError::WrongPassword => StatusCode::FORBIDDEN,
        VaultError::Exists | VaultError::Missing | VaultError::Locked | VaultError::Damaged => {
            StatusCode::CONFLICT
        }
        VaultError::Io(_) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                "The credential vault could not be read or written; reload before retrying.",
            )
                .into_response();
        }
    };
    (status, capitalized(&error.to_string())).into_response()
}

fn capitalized(message: &str) -> String {
    let mut characters = message.chars();
    characters
        .next()
        .map(|first| first.to_uppercase().chain(characters).collect())
        .unwrap_or_default()
}
