//! Dataset pages use the same FIFO read admission as ordinary result reads.
use super::*;
use crate::dataset_reads::{self, Error};

pub(super) async fn read(
    Scoped(shared): Scoped,
    Path(handle): Path<String>,
    request: Request,
) -> Response {
    let input = match query(request.uri().query().unwrap_or("")) {
        Ok(input) => input,
        Err(error) => return error_response(error),
    };
    let Ok(handle) = ValueHandle::new(&handle) else {
        return failure(
            StatusCode::BAD_REQUEST,
            "DATASET_INVALID",
            "Invalid result reference.",
            false,
        );
    };
    let Ok(current) = shared.application.current() else {
        return failure(
            StatusCode::GONE,
            "DATASET_SESSION_ENDED",
            "Session has ended.",
            false,
        );
    };
    if request
        .headers()
        .get("X-Wes-Session")
        .and_then(|h| h.to_str().ok())
        != Some(current.generation.as_str())
    {
        return failure(
            StatusCode::CONFLICT,
            "DATASET_SESSION_CHANGED",
            "Read a fresh workspace snapshot.",
            false,
        );
    }
    let Ok(permit) = read_admission::acquire(&shared.reads, &shared.stopped).await else {
        return failure(
            StatusCode::SERVICE_UNAVAILABLE,
            "DATASET_READ_BUSY",
            "Dataset readers are busy; retry this read without rerunning work.",
            true,
        );
    };
    if current.session.check_retirement_access().await.is_err() {
        return failure(
            StatusCode::SERVICE_UNAVAILABLE,
            "DATASET_RETIREMENT_BUSY",
            "Work deletion is in progress.",
            false,
        );
    }
    let before = match shared.values.read(handle.clone()).await {
        Ok(Some(value)) => value,
        Ok(None) => {
            return failure(
                StatusCode::NOT_FOUND,
                "DATASET_MISSING",
                "The stored result is unavailable.",
                false,
            );
        }
        Err(error) => return error_response(Error::Storage(error)),
    };
    let reference = match dataset_reads::reference(&before.value, &input.select) {
        Ok(reference) => reference,
        Err(error) => return error_response(error),
    };
    let advanced = input.head || input.extent.is_some();
    let response =
        match dataset_reads::read(&shared.values, &before.value, input, 1024 * 1024).await {
            Ok(value) => value,
            Err(error) => return error_response(error),
        };
    if !shared
        .application
        .current()
        .is_ok_and(|now| now.generation == current.generation)
    {
        return failure(
            StatusCode::CONFLICT,
            "DATASET_SESSION_CHANGED",
            "Read a fresh workspace snapshot.",
            false,
        );
    }
    if current.session.check_retirement_access().await.is_err() {
        return failure(
            StatusCode::GONE,
            "DATASET_WITHDRAWN",
            "Result access has been withdrawn.",
            false,
        );
    }
    match shared.values.read_matches(handle, before.value).await {
        Ok(true) => {}
        _ => {
            return failure(
                StatusCode::GONE,
                "DATASET_WITHDRAWN",
                "Result access has been withdrawn.",
                false,
            );
        }
    }
    let advanced_extent = if advanced {
        match serde_json::from_value::<wes_core::DatasetRef>(response["reference"].clone()) {
            Ok(reference) => Some(reference),
            Err(_) => return error_response(Error::Encoding),
        }
    } else {
        None
    };
    let encoded = shared
        .encoders
        .spawn_blocking(move || {
            let _permit = permit;
            serde_json::to_vec(&response)
        })
        .await;
    if !shared
        .application
        .current()
        .is_ok_and(|now| now.generation == current.generation)
        || current.session.check_retirement_access().await.is_err()
    {
        return failure(
            StatusCode::GONE,
            "DATASET_WITHDRAWN",
            "Result access has been withdrawn.",
            false,
        );
    }
    if let Some(shown) = advanced_extent {
        match shared.values.dataset_head(reference.clone()).await {
            Ok(info) if dataset_reads::extends(&info.reference, &shown) => {}
            Ok(_) => return error_response(Error::Invalid),
            Err(error) => return error_response(Error::Storage(error)),
        }
    }
    if let Err(error) = shared.values.dataset_inspect(reference).await {
        return error_response(Error::Storage(error));
    }
    match encoded {
        Ok(Ok(bytes)) => ([(header::CONTENT_TYPE, "application/json")], bytes).into_response(),
        _ => failure(
            StatusCode::INTERNAL_SERVER_ERROR,
            "DATASET_ENCODING_FAILED",
            "Dataset page could not be encoded.",
            false,
        ),
    }
}
pub(super) fn error_response(error: Error) -> Response {
    let (status, code) = match &error {
        Error::Unavailable | Error::Storage(wes_engine::storage::StoreError::Restricted) => {
            (StatusCode::FORBIDDEN, "DATASET_ACCESS_REFUSED")
        }
        Error::Invalid => (StatusCode::BAD_REQUEST, "DATASET_INVALID"),
        Error::Storage(wes_engine::storage::StoreError::Conflict) => {
            return failure(
                StatusCode::CONFLICT,
                "DATASET_CONTINUITY_CHANGED",
                "The selected dataset no longer has the same committed identity or analysis attempt. Keep reading the saved snapshot or open a new result.",
                false,
            );
        }
        Error::Encoding => (StatusCode::PAYLOAD_TOO_LARGE, "DATASET_REPLY_LIMIT"),
        Error::Storage(wes_engine::storage::StoreError::DatasetWithdrawn) => {
            (StatusCode::GONE, "DATASET_WITHDRAWN")
        }
        Error::Storage(wes_engine::storage::StoreError::DatasetMissing) => {
            (StatusCode::NOT_FOUND, "DATASET_MISSING")
        }
        Error::Storage(wes_engine::storage::StoreError::Limit(_)) => {
            (StatusCode::PAYLOAD_TOO_LARGE, "DATASET_READ_LIMIT")
        }
        _ => (StatusCode::INTERNAL_SERVER_ERROR, "DATASET_READ_FAILED"),
    };
    failure(status, code, &error.to_string(), false)
}
fn failure(status: StatusCode, code: &str, message: &str, retryable: bool) -> Response {
    (
        status,
        [(header::CONTENT_TYPE, "application/json")],
        serde_json::json!({"error": {"code": code, "message": message, "retryable": retryable}})
            .to_string(),
    )
        .into_response()
}
pub(super) fn query(query: &str) -> Result<dataset_reads::Request, Error> {
    if query.len() > 8192 {
        return Err(Error::Invalid);
    }
    let mut fields = serde_json::Map::new();
    for (key, value) in url::form_urlencoded::parse(query.as_bytes()) {
        if fields.contains_key(key.as_ref()) {
            return Err(Error::Invalid);
        }
        let value = match key.as_ref() {
            "select" | "from" | "cursor" => serde_json::Value::String(value.into_owned()),
            "limit" => serde_json::json!(value.parse::<usize>().map_err(|_| Error::Invalid)?),
            "inspect" | "head" => serde_json::json!(match value.as_ref() {
                "true" => true,
                "false" => false,
                _ => return Err(Error::Invalid),
            }),
            "extent" => {
                if value.len() > 4096 {
                    return Err(Error::Invalid);
                }
                serde_json::from_str(&value).map_err(|_| Error::Invalid)?
            }
            _ => return Err(Error::Invalid),
        };
        fields.insert(key.into_owned(), value);
    }
    serde_json::from_value(fields.into()).map_err(|_| Error::Invalid)
}
