//! Durable desktop-client preferences, separate from workspace/engine state.
use super::*;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{io::Read, sync::Mutex};

// Bounded dashboard definitions share this store with other desktop preferences.
fn max_bytes() -> usize {
    wes_budgets::get("ui.preferences.bytes") as usize
}
#[derive(Clone)]
pub struct DesktopPreferences {
    path: PathBuf,
    lock: Arc<Mutex<()>>,
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Saved {
    version: u8,
    settings: Value,
}
fn decode(bytes: &[u8]) -> io::Result<Value> {
    wes_adapters::codec::decode_json_preserving(
        bytes,
        Limits {
            bytes: max_bytes(),
            nodes: 50_000,
        },
    )
    .map_err(|_| io::Error::other("invalid UI preferences"))?;
    let value: Value = serde_json::from_slice(bytes).map_err(io::Error::other)?;
    if !value.is_object() {
        return Err(io::Error::other("UI preferences must be an object"));
    }
    Ok(value)
}
impl DesktopPreferences {
    pub fn new(path: PathBuf) -> Self {
        Self {
            path,
            lock: Arc::new(Mutex::new(())),
        }
    }
    fn read(&self) -> io::Result<Value> {
        let _lock = self
            .lock
            .lock()
            .map_err(|_| io::Error::other("preferences lock unavailable"))?;
        let metadata = match std::fs::symlink_metadata(&self.path) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(Value::Null),
            Err(error) => return Err(error),
        };
        if !metadata.is_file() || metadata.file_type().is_symlink() {
            return Err(io::Error::other("invalid preferences file"));
        }
        let mut bytes = Vec::new();
        std::fs::File::open(&self.path)?
            .take((max_bytes() + 1) as u64)
            .read_to_end(&mut bytes)?;
        let saved: Saved = serde_json::from_value(decode(&bytes)?).map_err(io::Error::other)?;
        if saved.version != 1 || !saved.settings.is_object() {
            return Err(io::Error::other("unsupported preferences file"));
        }
        Ok(saved.settings)
    }
    fn write(&self, settings: Value) -> io::Result<()> {
        let _lock = self
            .lock
            .lock()
            .map_err(|_| io::Error::other("preferences lock unavailable"))?;
        let saved = Saved {
            version: 1,
            settings,
        };
        // The version envelope also consumes the read budget. Never publish a file that this
        // store would reject on the next launch, even when the bare request fits its own budget.
        decode(&serde_json::to_vec_pretty(&saved).map_err(io::Error::other)?)?;
        wes_adapters::api_library::atomic_bytes(
            &self.path,
            &serde_json::to_vec_pretty(&saved).map_err(io::Error::other)?,
            max_bytes(),
            "UI preferences exceed the configured byte budget",
        )
    }
}
pub(super) async fn read(State(shared): State<Shared>) -> Response {
    perform(shared, None).await
}
pub(super) async fn write(State(shared): State<Shared>, request: Request) -> Response {
    if !request
        .headers()
        .get(header::CONTENT_TYPE)
        .is_some_and(|h| h == "application/json")
    {
        return StatusCode::UNSUPPORTED_MEDIA_TYPE.into_response();
    }
    let bytes = match tokio::time::timeout(
        Duration::from_secs(5),
        to_bytes(request.into_body(), max_bytes()),
    )
    .await
    {
        Ok(Ok(bytes)) => bytes,
        _ => return StatusCode::PAYLOAD_TOO_LARGE.into_response(),
    };
    match decode(&bytes) {
        Ok(settings) => perform(shared, Some(settings)).await,
        Err(_) => (StatusCode::BAD_REQUEST, "invalid UI preferences").into_response(),
    }
}
async fn perform(shared: Shared, settings: Option<Value>) -> Response {
    let Some(store) = shared.services.desktop_preferences.clone() else {
        return StatusCode::NOT_FOUND.into_response();
    };
    let Ok(permit) = shared.queries.clone().try_acquire_owned() else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    match shared
        .encoders
        .spawn_blocking(move || {
            let _permit = permit;
            if let Some(settings) = settings {
                store.write(settings)?;
                Ok::<_, io::Error>(json!({"saved": true}))
            } else {
                Ok(json!({"settings": store.read()?}))
            }
        })
        .await
    {
        Ok(Ok(value)) => (
            [
                (header::CONTENT_TYPE, "application/json"),
                (header::CACHE_CONTROL, "no-store"),
            ],
            value.to_string(),
        )
            .into_response(),
        _ => (
            StatusCode::INTERNAL_SERVER_ERROR,
            "could not load or save desktop UI preferences",
        )
            .into_response(),
    }
}
