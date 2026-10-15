//! Original byte coordinates over one acknowledged result; never a producer dependency.
use super::{BoundQuery, Failure};
use crate::{
    byte_input::{CapturedSource, Source},
    driver::{CancellationToken, ExecutionFuture},
    graph::OutputRef,
    plan::{Input, MetaTask},
    runtime::{Outcome, RuntimeCode},
    storage::{CHUNK_BYTES, SourceRange, StoreError, StoreWorker},
    workspace::Workspace,
};
use indexmap::IndexMap;
use wes_core::{
    Data, Primitive, RecordShape, Shape, Value,
    contracts::{Contract, ContractField, ContractRegistry, metadata::ValueMetadata},
    flow::FlowPolicy,
};

#[derive(Clone, Debug)]
struct Options {
    from: u64,
    limit: usize,
    fields: Vec<String>,
    dependency: Option<String>,
}
impl Options {
    fn parse(task: &MetaTask) -> Result<Self, &'static str> {
        let text = |key| match task.inputs.get(key) {
            Some(Input::Literal(value)) => match value.data() {
                Data::Text(text) => Some(text.as_ref()),
                _ => None,
            },
            _ => None,
        };
        let from = text("from")
            .and_then(|s| s.parse::<u64>().ok().filter(|n| n.to_string() == s))
            .ok_or("from: must be a literal canonical unsigned decimal Text offset")?;
        let Some(Input::Literal(value)) = task.inputs.get("limit") else {
            return Err("limit: must be a literal Int from 1 to 65536");
        };
        let Data::Int(limit) = value.data() else {
            return Err("limit: must be a literal Int from 1 to 65536");
        };
        if !(1..=CHUNK_BYTES as i64).contains(limit) {
            return Err("limit: must be from 1 to 65536");
        }
        let fields = match (task.inputs.contains_key("select"), text("select")) {
            (false, _) => vec![],
            (true, Some(s)) if s.len() <= 4096 => {
                let fields: Vec<String> = s.split('.').map(str::to_owned).collect();
                if fields.len() > 64 || fields.iter().any(String::is_empty) {
                    return Err("select: requires at most 64 nonempty static record field names");
                }
                fields
            }
            _ => return Err("select: must be a literal static field path of at most 4096 bytes"),
        };
        let dependency = match (task.inputs.contains_key("dependency"), text("dependency")) {
            (false, _) => None,
            (true, Some(name))
                if !name.is_empty()
                    && name.len() <= 1024
                    && !task.inputs.contains_key("select") =>
            {
                Some(name.to_owned())
            }
            _ => {
                return Err(
                    "dependency: requires one literal native slot name and cannot be combined with select:",
                );
            }
        };
        if task
            .inputs
            .keys()
            .any(|k| !matches!(k.as_str(), "from" | "limit" | "select" | "dependency"))
        {
            return Err("Byte reads accept from:, limit: and optional select: or dependency:");
        }
        Ok(Self {
            from,
            limit: *limit as usize,
            fields,
            dependency,
        })
    }
}
pub(super) fn validate(task: &MetaTask) -> Result<(), &'static str> {
    Options::parse(task).map(|_| ())
}

#[derive(Clone, Debug)]
pub(super) struct CapturedBytes {
    options: Options,
    source: CapturedSource,
}
impl CapturedBytes {
    pub(super) fn capture(
        query: &BoundQuery,
        workspace: &Workspace,
        output: &OutputRef,
    ) -> Result<Self, Failure> {
        let options = Options::parse(&query.task).map_err(Failure::Selection)?;
        let source = CapturedSource::capture(workspace, output, options.fields.clone()).map_err(
            |error| {
                Failure::OutputUnavailable(
                    error
                        .read_error("Byte source unavailable; this read did not run its producer")
                        .message()
                        .to_owned(),
                )
            },
        )?;
        if options.dependency.is_some() && !matches!(source.source, Source::Stored(_)) {
            return Err(Failure::OutputUnavailable(
                "Native input dependency requires an acknowledged owned stored result; copies and literals confer no authority.".into(),
            ));
        }
        Ok(Self { options, source })
    }
    pub(super) fn policy(&self) -> FlowPolicy {
        self.source.provenance.policy().clone()
    }
    pub(super) fn execute(
        self,
        storage: Option<StoreWorker>,
        token: CancellationToken,
    ) -> ExecutionFuture {
        Box::pin(async move {
            let cancelled = || {
                Outcome::Cancelled(
                    RuntimeCode::Cancelled.error("The byte read was cancelled.", None),
                )
            };
            if token.is_cancelled() {
                return cancelled().into();
            }
            if let Err(error) = self.source.check() {
                return Outcome::Failed(error.read_error("Byte read refused")).into();
            }
            let result = if let Some(name) = &self.options.dependency {
                match (&self.source.source, &storage) {
                    (Source::Stored(snapshot), Some(worker)) => {
                        worker
                            .source_range_dependency(
                                snapshot.clone(),
                                name.clone(),
                                self.options.from,
                                self.options.limit,
                            )
                            .await
                    }
                    _ => Err(StoreError::MissingValue),
                }
            } else {
                self.source
                    .range(storage.as_ref(), self.options.from, self.options.limit)
                    .await
            };
            // Submitted local work is joined, even when the read is cancelled or withdrawn.
            if token.is_cancelled() {
                return cancelled().into();
            }
            let result = self
                .source
                .check()
                .and(result)
                .and_then(|range| self.value(range));
            match result {
                Ok(value) => Outcome::Produced(value).into(),
                Err(error) => Outcome::Failed(error.read_error("Byte read refused")).into(),
            }
        })
    }
    fn value(&self, range: SourceRange) -> Result<Value, StoreError> {
        if range.bytes.len() > self.options.limit {
            return Err(StoreError::Corrupt);
        }
        let next = self
            .options
            .from
            .checked_add(range.bytes.len() as u64)
            .filter(|n| *n <= range.extent)
            .ok_or(StoreError::Corrupt)?;
        if next < range.extent && range.bytes.is_empty() {
            return Err(StoreError::Corrupt);
        }
        let (handle, digest, selection) = match &self.source.source {
            Source::Resident(_) => (None, None, self.options.fields.clone()),
            Source::Stored(snapshot) => {
                let mut selection = snapshot.selection.clone();
                selection.extend(self.options.fields.iter().cloned());
                if let Some(name) = &self.options.dependency {
                    selection.push(format!("dependency:{name}"));
                }
                (
                    Some(snapshot.handle.to_string()),
                    Some(snapshot.digest.clone()),
                    selection,
                )
            }
        };
        let optional = |s: Option<String>| Data::Option(s.map(|v| Box::new(Data::Text(v.into()))));
        let data = Data::Record(IndexMap::from([
            ("source".into(), Data::Text(self.source.node.clone().into())),
            ("run".into(), optional(self.source.run.clone())),
            (
                "revision".into(),
                Data::Text(self.source.revision.to_string().into()),
            ),
            ("handle".into(), optional(handle)),
            ("digest".into(), optional(digest)),
            (
                "selection".into(),
                Data::List(
                    selection
                        .into_iter()
                        .map(|s| Data::Text(s.into()))
                        .collect(),
                ),
            ),
            (
                "from".into(),
                Data::Text(self.options.from.to_string().into()),
            ),
            ("next".into(), Data::Text(next.to_string().into())),
            ("extent".into(), Data::Text(range.extent.to_string().into())),
            ("text".into(), Data::Bool(range.text)),
            ("extentExhausted".into(), Data::Bool(next == range.extent)),
            ("data".into(), Data::Bytes(range.bytes.into())),
        ]));
        let registry = ContractRegistry::new();
        let contracts = fields()
            .into_iter()
            .map(|(name, shape)| {
                Ok((
                    name,
                    ContractField {
                        contract: registry
                            .resolve(&shape.to_string())
                            .map_err(|_| StoreError::Corrupt)?,
                        optional: false,
                    },
                ))
            })
            .collect::<Result<_, StoreError>>()?;
        let contract = Contract::record("ByteRange", contracts).map_err(|_| StoreError::Corrupt)?;
        Value::new(shape(), data, self.source.provenance.clone())
            .map(|v| v.with_metadata(Some(ValueMetadata::capture(&contract))))
            .map_err(|_| StoreError::Corrupt)
    }
}
fn fields() -> IndexMap<String, Shape> {
    ["source", "revision", "from", "next", "extent"]
        .into_iter()
        .map(|name| (name.into(), Shape::Primitive(Primitive::Text)))
        .chain(["run", "handle", "digest"].into_iter().map(|name| {
            (
                name.into(),
                Shape::Option(Box::new(Shape::Primitive(Primitive::Text))),
            )
        }))
        .chain([(
            "selection".into(),
            Shape::List(Box::new(Shape::Primitive(Primitive::Text))),
        )])
        .chain(
            ["text", "extentExhausted"]
                .into_iter()
                .map(|name| (name.into(), Shape::Primitive(Primitive::Bool))),
        )
        .chain([("data".into(), Shape::Primitive(Primitive::Bytes))])
        .collect()
}
pub(super) fn shape() -> Shape {
    Shape::Record(RecordShape::new("ByteRange", fields()).expect("native ByteRange fields"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{driver::Executor, runtime::Effect, tasks::TaskExecutor, workspace::Preparation};
    use std::time::Duration;
    use wes_core::Provenance;

    #[tokio::test]
    async fn private_native_byte_range_is_refused_by_the_shared_public_view_boundary() {
        let mut workspace = Workspace::local(crate::providers::LocalScope::new("fixture").unwrap());
        for (command, original) in [
            (":calc { return 0; } > secret", true),
            (":read bytes:$secret from:\"0\" limit:4", false),
        ] {
            let parsed = wes_language::parse(&wes_language::SourceText::new("fixture", command));
            let Preparation::Change(change) =
                workspace.prepare(&parsed.script.statements[0]).unwrap()
            else {
                panic!("native node")
            };
            workspace.commit(change).unwrap();
            let ticket = workspace
                .start(Duration::ZERO)
                .into_iter()
                .find_map(|effect| match effect {
                    Effect::Spawn(ticket) => Some(ticket),
                    _ => None,
                })
                .unwrap();
            let run = ticket.run.clone();
            let ticket = workspace.enter_ticket(ticket).unwrap().unwrap();
            if original {
                let value = Value::new(
                    Shape::Primitive(Primitive::Bytes),
                    Data::Bytes(b"synthetic private data".to_vec().into()),
                    Provenance::default().with_policy(&FlowPolicy::default().private()),
                )
                .unwrap();
                workspace.complete(&run, Outcome::Produced(value), Duration::ZERO);
            } else {
                let report = TaskExecutor::ephemeral()
                    .execute(ticket, CancellationToken::new())
                    .await;
                let Outcome::Produced(value) = report.outcome else {
                    panic!("native byte read")
                };
                assert!(value.provenance().policy().is_private());
                assert!(
                    !crate::views::public_input(&value),
                    "bytes and their captured contract never become a public SDK/frame input"
                );
            }
        }
    }
}
