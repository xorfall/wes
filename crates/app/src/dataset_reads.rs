//! One bounded dataset read model for browser and agent transports. Reads never dispatch work.
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use serde::{Deserialize, Serialize};
use serde_json::{Value as Json, json};
use wes_core::{Data, DatasetRef, DatasetStream, Value};
use wes_engine::storage::{StoreError, StoreWorker, datasets::PageRequest};

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Request {
    #[serde(default)]
    pub stream: DatasetStream,
    #[serde(default)]
    pub select: String,
    pub from: Option<String>,
    pub cursor: Option<String>,
    pub limit: Option<usize>,
    #[serde(default)]
    pub inspect: bool,
    /// Explicitly read the acknowledged head; never implied by opening a presentation.
    #[serde(default)]
    pub head: bool,
    /// A previously read exact committed extension of the stored selection.
    /// This is an identity constraint, never independent read or control authority.
    pub extent: Option<DatasetRef>,
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Cursor {
    version: u16,
    reference: DatasetRef,
    stream: DatasetStream,
    select: String,
    next: String,
}
#[derive(Debug, thiserror::Error)]
pub(crate) enum Error {
    #[error("Dataset read requires a public typed Dataset selection.")]
    Unavailable,
    #[error(
        "Selected Dataset has no rejected-frame coverage; read stream:outputs or inspect its available streams."
    )]
    MissingStream,
    #[error(
        "Invalid dataset range or cursor; read the same committed snapshot with a canonical ordinal."
    )]
    Invalid,
    #[error(
        "Dataset reply exceeds its encoding budget; request a smaller page or bounded record inspection."
    )]
    Encoding,
    #[error(transparent)]
    Storage(#[from] StoreError),
}
fn ordinal(text: &str) -> Result<u64, Error> {
    if text.is_empty()
        || (text.len() > 1 && text.starts_with('0'))
        || !text.bytes().all(|b| b.is_ascii_digit())
    {
        return Err(Error::Invalid);
    }
    text.parse().map_err(|_| Error::Invalid)
}
pub(crate) fn reference(value: &Value, select: &str) -> Result<DatasetRef, Error> {
    if value.provenance().policy().is_private() || value.provenance().policy().is_unknown() {
        return Err(Error::Unavailable);
    }
    let (data, shape, _) =
        wes_adapters::codec::select_value(value, select).map_err(|_| Error::Invalid)?;
    match (data, shape) {
        (Data::Dataset(reference), wes_core::Shape::Dataset(_)) => Ok(reference.as_ref().clone()),
        _ => Err(Error::Unavailable),
    }
}
pub(crate) async fn read(
    worker: &StoreWorker,
    value: &Value,
    request: Request,
    budget: usize,
) -> Result<Json, Error> {
    let original = reference(value, &request.select)?;
    if request.head && (!request.inspect || request.extent.is_some()) {
        return Err(Error::Invalid);
    }
    let reference = if request.head {
        worker.dataset_head(original.clone()).await?.reference
    } else if let Some(extent) = &request.extent {
        // Both the current head and the selected extent must still belong to the
        // stored result's exact epoch/attempt, even when a cursor is replayed.
        if !extends(extent, &original) {
            return Err(Error::Invalid);
        }
        let head = worker.dataset_head(original.clone()).await?.reference;
        if !extends(&head, extent) {
            return Err(Error::Invalid);
        }
        worker.dataset_inspect(extent.clone()).await?;
        extent.clone()
    } else {
        original.clone()
    };
    let cap = wes_adapters::datasets::PageLimits::default();
    let limit = request.limit.unwrap_or(50);
    if limit == 0
        || limit > cap.rows
        || (request.from.is_some() && request.cursor.is_some())
        || (request.inspect
            && (request.from.is_some() || request.cursor.is_some() || request.limit.is_some()))
    {
        return Err(Error::Invalid);
    }
    let from = if let Some(cursor) = request.cursor {
        if cursor.len() > 4096 {
            return Err(Error::Invalid);
        }
        let bytes = URL_SAFE_NO_PAD
            .decode(&cursor)
            .map_err(|_| Error::Invalid)?;
        if URL_SAFE_NO_PAD.encode(&bytes) != cursor {
            return Err(Error::Invalid);
        }
        let cursor: Cursor = serde_json::from_slice(&bytes).map_err(|_| Error::Invalid)?;
        if cursor.version != 2
            || cursor.reference != reference
            || cursor.select != request.select
            || cursor.stream != request.stream
        {
            return Err(Error::Invalid);
        }
        ordinal(&cursor.next)?
    } else {
        ordinal(request.from.as_deref().unwrap_or("0"))?
    };
    let info = worker.dataset_inspect(reference.clone()).await?;
    if request.stream == DatasetStream::Coverage && info.coverage.is_none() {
        return Err(Error::MissingStream);
    }
    let mut response = json!({"reference": reference, "stream": request.stream, "lifecycle": format!("{:?}", info.lifecycle).to_lowercase(),
        "protected": info.protected, "persistence": format!("{:?}", info.persistence), "segmentBytes": info.segment_bytes.to_string()});
    if let Some(coverage) = &info.recording {
        response["recording"] = recording(coverage);
    }
    if let Some(coverage) = &info.coverage {
        response["coverage"] = json!({
            "records": coverage.progress.records.to_string(),
            "inputBytes": coverage.progress.input_bytes.to_string(),
            "lastOrdinal": coverage.progress.last_ordinal.map(|n| n.to_string()),
            "through": coverage.progress.through.map(|n| n.to_string()),
            "excerptBytes": coverage.progress.policy.excerpt_bytes.to_string(),
            "segmentBytes": coverage.segment_bytes.to_string(),
        });
    }
    if !request.inspect {
        let page = worker
            .dataset_stream_page(
                reference.clone(),
                request.stream,
                PageRequest {
                    charge: None,
                    work: None,
                    from,
                    rows: limit,
                    bytes: cap.bytes.min(budget / 4),
                    segments: cap.segments,
                },
            )
            .await?;
        let mut rows = Vec::with_capacity(page.rows.len());
        let mut used = 4096usize;
        for row in page.rows {
            let encoded = wes_adapters::codec::encode_display_value(
                &row.value,
                wes_adapters::codec::Limits {
                    bytes: budget.saturating_sub(used),
                    nodes: 100000,
                },
            )
            .map_err(|_| Error::Encoding)?;
            used = used
                .checked_add(encoded.len() + 256)
                .filter(|n| *n <= budget)
                .ok_or(Error::Encoding)?;
            rows.push(json!({"ordinal": row.ordinal.to_string(), "sourceStart": row.source_start.to_string(), "sourceEnd": row.source_end.to_string(), "value": serde_json::from_slice::<Json>(&encoded).map_err(|_| Error::Encoding)?}));
        }
        let cursor = if page.extent_exhausted {
            None
        } else {
            Some(
                URL_SAFE_NO_PAD.encode(
                    serde_json::to_vec(&Cursor {
                        version: 2,
                        reference: reference.clone(),
                        stream: request.stream,
                        select: request.select,
                        next: page.next.to_string(),
                    })
                    .map_err(|_| Error::Encoding)?,
                ),
            )
        };
        response["page"] = json!({"first": page.first.to_string(), "next": page.next.to_string(), "rows": rows,
            "extentExhausted": page.extent_exhausted, "limitedBy": page.limited_by, "cursor": cursor});
    }
    // Coarse withdrawal after joined I/O denies even a count or captured schema.
    worker.dataset_inspect(reference.clone()).await?;
    if request.head || request.extent.is_some() {
        let current = worker.dataset_head(original).await?.reference;
        if !extends(&current, &reference) {
            return Err(Error::Invalid);
        }
    }
    if serde_json::to_vec(&response)
        .map_err(|_| Error::Encoding)?
        .len()
        > budget
    {
        return Err(Error::Encoding);
    }
    Ok(response)
}

pub(crate) fn extends(next: &DatasetRef, previous: &DatasetRef) -> bool {
    next.store() == previous.store()
        && next.dataset() == previous.dataset()
        && next.schema_digest() == previous.schema_digest()
        && next.authorization_generation() == previous.authorization_generation()
        && next.generation() >= previous.generation()
        && next.records() >= previous.records()
        && (next.generation() != previous.generation() || next == previous)
}

/// Coverage belongs to this immutable committed manifest, not a mutable source window.
/// Exact decimal strings preserve all sequence/count values across browser and MCP reads.
fn recording(coverage: &wes_engine::storage::datasets::EventLogCoverage) -> Json {
    json!({
        "run": coverage.run,
        "epoch": coverage.epoch,
        "first": coverage.first.to_string(),
        "acceptedThrough": coverage.accepted_through.to_string(),
        "committedThrough": coverage.committed_through.to_string(),
        "pending": coverage.pending.map(|n| n.to_string()),
        "rejected": coverage.rejected.to_string(),
        "termination": coverage.termination,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn ordinals_are_exact_unsigned_decimal_and_never_float_or_aliases() {
        assert_eq!(ordinal("18446744073709551615").unwrap(), u64::MAX);
        for invalid in ["", "01", "+1", "-1", "1.0", "1e3", "18446744073709551616"] {
            assert!(ordinal(invalid).is_err());
        }
    }

    #[test]
    fn recording_coverage_preserves_exact_counts_and_unknown_pending() {
        use wes_engine::storage::datasets::{EventLogCoverage, RecordingEnd};
        let mut coverage = EventLogCoverage {
            run: uuid::Uuid::new_v4().to_string(),
            epoch: uuid::Uuid::new_v4().to_string(),
            first: u64::MAX - 2,
            accepted_through: u64::MAX,
            committed_through: u64::MAX - 1,
            pending: Some(1),
            rejected: 0,
            termination: Some(RecordingEnd::Overloaded),
        };
        let value = recording(&coverage);
        assert_eq!(value["acceptedThrough"], u64::MAX.to_string());
        assert_eq!(value["pending"], "1");
        assert_eq!(value["termination"], "overloaded");
        coverage.pending = None;
        assert!(recording(&coverage)["pending"].is_null());
    }
}
