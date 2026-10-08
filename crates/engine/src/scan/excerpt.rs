//! Bounded evidence reads of a scan's captured original source, never of its producer.
use super::owned::{OwnedAnalysis, Selection};
use crate::{
    driver::{CancellationToken, ExecutionFuture},
    plan::{Input, MetaTask},
    storage::{
        StoreError, StoreWorker,
        datasets::{AnalysisCheckpoint, DatasetStorage, PageRequest},
    },
    workspace::Workspace,
};
use std::sync::Arc;
use wes_core::{
    Data, Primitive, RecordShape, Shape, Value,
    contracts::{Contract, ContractField, ContractRegistry, metadata::ValueMetadata},
    flow::FlowPolicy,
};
use wes_language::{Diagnostic, Span};
const BYTES: usize = 65_536;
const ROWS: usize = 100;
#[derive(Clone, Debug)]
pub struct BoundExcerpt {
    selection: OwnedAnalysis,
    from: u64,
    limit: usize,
    span: Span,
}
impl BoundExcerpt {
    pub(crate) fn bind(task: MetaTask, span: Span) -> Result<Self, Diagnostic> {
        let selection = OwnedAnalysis::bind(&task, span)?;
        let invalid = |message| Diagnostic::error("CAL009", span, message);
        if task
            .inputs
            .keys()
            .any(|key| !matches!(key.as_str(), "from" | "limit"))
        {
            return Err(invalid(
                "Excerpt accepts only literal from: and limit:; it cannot acquire or reinterpret input",
            ));
        }
        let Some(Input::Literal(from)) = task.inputs.get("from") else {
            return Err(invalid(
                "from: is a required literal canonical unsigned decimal string",
            ));
        };
        let Data::Text(from) = from.data() else {
            return Err(invalid(
                "from: must be a literal canonical unsigned decimal string",
            ));
        };
        let from = from
            .parse::<u64>()
            .ok()
            .filter(|n| n.to_string() == from.as_ref())
            .ok_or_else(|| invalid("from: must be a canonical unsigned decimal string"))?;
        let Some(Input::Literal(limit)) = task.inputs.get("limit") else {
            return Err(invalid(
                "limit: is a required literal integer from 1 to 65536; record sources allow at most 100",
            ));
        };
        let Data::Int(limit) = limit.data() else {
            return Err(invalid("limit: must be a literal integer from 1 to 65536"));
        };
        if !(1..=BYTES as i64).contains(limit) {
            return Err(invalid("limit: must be from 1 to 65536"));
        }
        Ok(Self {
            selection,
            from,
            limit: *limit as usize,
            span,
        })
    }
    pub(crate) fn capture(&mut self, workspace: &Workspace) {
        self.selection.capture(workspace, Selection::Read);
    }
    pub(crate) fn policy(&self) -> FlowPolicy {
        self.selection.policy()
    }
    pub(crate) fn execute(
        self,
        storage: Option<StoreWorker>,
        token: CancellationToken,
    ) -> ExecutionFuture {
        Box::pin(async move {
            let failed = |message: String| {
                super::bound::failed(crate::calc::Failure::new("CAL004", self.span, message))
            };
            if token.is_cancelled() {
                return super::bound::failed(crate::calc::Failure::cancelled(self.span)).into();
            }
            let run = match self.selection.issued() {
                Ok(run) => run,
                Err(message) => return failed(message).into(),
            };
            let Some(worker) = storage else {
                return failed("Source excerpts require the analysis's owned durable store".into())
                    .into();
            };
            // Join this bounded local read even if cancellation arrives during I/O.
            let value = worker
                .dataset_source_excerpt(run, self.from, self.limit)
                .await;
            if token.is_cancelled() {
                return super::bound::failed(crate::calc::Failure::cancelled(self.span)).into();
            }
            match value {
                Ok(value) => crate::runtime::Outcome::Produced(value).into(),
                Err(error) => failed(error.to_string()).into(),
            }
        })
    }
}

/// Runs inside the credited store owner. Source bytes cannot escape in an unbounded reply.
pub(crate) fn read_source_excerpt(
    storage: &dyn DatasetStorage,
    source: &Value,
    checkpoint: &AnalysisCheckpoint,
    from: u64,
    limit: usize,
) -> Result<Value, StoreError> {
    if !(1..=BYTES).contains(&limit) {
        return Err(StoreError::SourceRange("limit must be from 1 to 65536"));
    }
    let registry = ContractRegistry::new();
    let mut provenance = source.provenance().clone();
    let (unit, extent, next, data, shape, contract, limited) = match source.data() {
        Data::Text(text) => {
            let (start, end) = byte_range(text.len(), from, limit)?;
            if !text.is_char_boundary(start) || !text.is_char_boundary(end) {
                return Err(StoreError::SourceRange(
                    "Text ranges must start and end at original UTF-8 byte boundaries; use a boundary-aligned range",
                ));
            }
            (
                "bytes",
                text.len() as u64,
                end as u64,
                Data::Text(text[start..end].into()),
                Shape::Primitive(Primitive::Text),
                Some(registry.resolve("Text").unwrap()),
                None,
            )
        }
        Data::Bytes(bytes) => {
            let (start, end) = byte_range(bytes.len(), from, limit)?;
            (
                "bytes",
                bytes.len() as u64,
                end as u64,
                Data::Bytes(bytes[start..end].into()),
                Shape::Primitive(Primitive::Bytes),
                Some(registry.resolve("Bytes").unwrap()),
                None,
            )
        }
        Data::List(items) => {
            record_range(items.len() as u64, from, limit)?;
            let Shape::List(item) = source.shape() else {
                return Err(StoreError::DatasetCorrupt);
            };
            let mut rows = Vec::new();
            let mut used = 0u64;
            for data in items.iter().skip(from as usize).take(limit) {
                let charge = crate::value_size::data_charge(data, BYTES as u64)
                    .ok_or(StoreError::Limit("excerpt record"))?;
                if used.checked_add(charge).is_none_or(|n| n > BYTES as u64) {
                    if rows.is_empty() {
                        return Err(StoreError::Limit("excerpt record"));
                    }
                    break;
                }
                used += charge;
                rows.push(data.clone());
            }
            let next = from + rows.len() as u64;
            let limited = (next < (from + limit as u64).min(items.len() as u64)).then_some("bytes");
            // Original source records may precede the scan's validation failure. Preserve their
            // actual structural shape; do not assert the transition's contract over unread data.
            (
                "records",
                items.len() as u64,
                next,
                Data::List(rows),
                Shape::List(item.clone()),
                None,
                limited,
            )
        }
        Data::Dataset(initial) => {
            let reference = checkpoint
                .followed_source
                .as_ref()
                .map_or(initial.as_ref(), |followed| &followed.prefix);
            record_range(reference.records(), from, limit)?;
            let page = storage.page(
                reference,
                PageRequest {
                    charge: None,
                    from,
                    rows: limit.min(wes_budgets::get("dataset.page.rows") as usize),
                    bytes: BYTES.min(wes_budgets::get("dataset.page.bytes") as usize),
                    segments: 32.min(wes_budgets::get("dataset.page.segments") as usize),
                    work: None,
                },
            )?;
            if page.reference != *reference
                || page.first != from
                || page.next != from + page.rows.len() as u64
            {
                return Err(StoreError::Conflict);
            }
            let info = storage.inspect(reference)?;
            provenance = provenance.with_policy(&info.policy.read_from_dataset(reference));
            let list = Arc::new(
                Contract::list("SourceExcerptRecords", page.schema.root().clone())
                    .map_err(|_| StoreError::DatasetCorrupt)?,
            );
            let limited = if page.next < from + (limit as u64).min(reference.records() - from) {
                page.limited_by
            } else {
                None
            };
            (
                "records",
                reference.records(),
                page.next,
                Data::List(
                    page.rows
                        .into_iter()
                        .map(|row| row.value.into_data())
                        .collect(),
                ),
                list.shape(),
                Some(list),
                limited,
            )
        }
        _ => return Err(StoreError::NonMaterialized),
    };
    let mut fields: indexmap::IndexMap<String, (Shape, Data)> = [
        ("unit", Data::Text(unit.into())),
        ("from", Data::Text(from.to_string().into())),
        ("next", Data::Text(next.to_string().into())),
        ("extent", Data::Text(extent.to_string().into())),
        (
            "captureDigest",
            Data::Text(checkpoint.source.digest.clone().into()),
        ),
        ("extentExhausted", Data::Bool(next == extent)),
        (
            "limitedBy",
            Data::Option(limited.map(|reason| Box::new(Data::Text(reason.into())))),
        ),
    ]
    .into_iter()
    .map(|(key, data)| {
        (
            key.into(),
            (
                match data {
                    Data::Bool(_) => Shape::Primitive(Primitive::Bool),
                    Data::Option(_) => Shape::Option(Box::new(Shape::Primitive(Primitive::Text))),
                    _ => Shape::Primitive(Primitive::Text),
                },
                data,
            ),
        )
    })
    .collect();
    fields.insert("data".into(), (shape, data));
    let shape = Shape::Record(
        RecordShape::new(
            "SourceExcerpt",
            fields
                .iter()
                .map(|(key, (shape, _))| (key.clone(), shape.clone())),
        )
        .map_err(|_| StoreError::DatasetCorrupt)?,
    );
    let metadata = contract
        .map(|payload| {
            let declarations = fields
                .iter()
                .map(|(key, (shape, _))| {
                    let contract = if key == "data" {
                        payload.clone()
                    } else {
                        registry
                            .resolve(&shape.to_string())
                            .expect("native scalar excerpt field")
                    };
                    (
                        key.clone(),
                        ContractField {
                            contract,
                            optional: false,
                        },
                    )
                })
                .collect();
            Contract::record("SourceExcerpt", declarations)
                .map(|c| ValueMetadata::capture(&c))
                .map_err(|_| StoreError::DatasetCorrupt)
        })
        .transpose()?;
    let value = Value::new(
        shape,
        Data::Record(
            fields
                .into_iter()
                .map(|(key, (_, data))| (key, data))
                .collect(),
        ),
        provenance,
    )
    .map_err(|_| StoreError::DatasetCorrupt)?
    .with_metadata(metadata);
    crate::value_size::value_charge(&value, wes_budgets::get("query.bytes"))
        .ok_or(StoreError::Limit("source excerpt reply"))?;
    Ok(value)
}
fn byte_range(extent: usize, from: u64, limit: usize) -> Result<(usize, usize), StoreError> {
    if from > extent as u64 {
        return Err(StoreError::SourceRange(
            "from is beyond the captured byte extent",
        ));
    }
    let start = from as usize;
    Ok((start, start + limit.min(extent - start)))
}
fn record_range(extent: u64, from: u64, limit: usize) -> Result<(), StoreError> {
    if limit > ROWS {
        return Err(StoreError::SourceRange(
            "record sources allow at most 100 records per excerpt",
        ));
    }
    if from > extent {
        return Err(StoreError::SourceRange(
            "from is beyond the captured record extent",
        ));
    }
    Ok(())
}
