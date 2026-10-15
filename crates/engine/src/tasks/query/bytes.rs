//! Original byte coordinates over one acknowledged result; never a producer dependency.
use super::{BoundQuery, Failure};
use crate::{
    driver::{CancellationToken, ExecutionFuture},
    graph::{OutputPort, OutputRef},
    plan::{Input, MetaTask},
    runtime::{Outcome, OutputState, RuntimeCode},
    storage::{CHUNK_BYTES, SourceRange, StoreError, StoreWorker, StoredSnapshot},
    workspace::Workspace,
};
use indexmap::IndexMap;
use wes_core::{
    Data, Primitive, Provenance, RecordShape, Shape, Value,
    contracts::{Contract, ContractField, ContractRegistry, metadata::ValueMetadata},
    flow::FlowPolicy,
};

#[derive(Clone, Debug)]
struct Options {
    from: u64,
    limit: usize,
    fields: Vec<String>,
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
        if task
            .inputs
            .keys()
            .any(|k| !matches!(k.as_str(), "from" | "limit" | "select"))
        {
            return Err("Byte reads accept only from:, limit: and optional select:");
        }
        Ok(Self {
            from,
            limit: *limit as usize,
            fields,
        })
    }
}
pub(super) fn validate(task: &MetaTask) -> Result<(), &'static str> {
    Options::parse(task).map(|_| ())
}

#[derive(Clone, Debug)]
enum Source {
    Resident(Value),
    Stored(StoredSnapshot),
}
#[derive(Clone, Debug)]
pub(super) struct CapturedBytes {
    options: Options,
    source: Source,
    node: String,
    run: Option<String>,
    revision: u64,
    provenance: Provenance,
    readers: crate::provider_reads::Readers,
    epoch: String,
}
impl CapturedBytes {
    pub(super) fn capture(
        query: &BoundQuery,
        workspace: &Workspace,
        output: &OutputRef,
    ) -> Result<Self, Failure> {
        let options = Options::parse(&query.task).map_err(Failure::Selection)?;
        let (source, run, revision, provenance) = match workspace.runtime().output(output) {
            OutputState::Available(value) => {
                let provenance = value.provenance().clone();
                let run = if output.port == OutputPort::Data {
                    workspace.runtime().value_run(&output.node)
                } else {
                    workspace.runtime().run_of(&output.node)
                }.map(ToString::to_string);
                (Source::Resident(value), run, workspace.runtime().display_revision(&output.node), provenance)
            }
            OutputState::Stored(captured) => {
                let provenance = captured.snapshot.provenance.clone();
                (Source::Stored(captured.snapshot), captured.run.map(|r| r.to_string()), captured.revision, provenance)
            }
            _ if output.port == OutputPort::Data => {
                let evidence = workspace.runtime().evidence_value(&output.node).ok_or_else(|| Failure::OutputUnavailable(
                    "Byte source has no acknowledged accessible result; this read did not run its producer.".into(),
                ))?;
                let provenance = evidence.value.provenance().clone()
                    .with_fact("wes.observation.state", "stopped")
                    .with_fact("wes.observation.run", evidence.run.as_str())
                    .with_fact("wes.observation.source", evidence.source.as_str());
                (Source::Resident(evidence.value.clone()), Some(evidence.run.to_string()), workspace.runtime().display_revision(&output.node), provenance)
            }
            _ => return Err(Failure::OutputUnavailable(
                "Byte source has no acknowledged accessible result; this read did not run its producer.".into(),
            )),
        };
        // Root authority precedes field lookup, including stopped independent evidence.
        if workspace.dataset_access.blocks_policy(provenance.policy()) {
            return Err(Failure::Selection(
                "Byte-source read authority is unavailable",
            ));
        }
        let readers = workspace.dataset_access.providers.clone();
        let epoch = readers.epoch();
        Ok(Self {
            options,
            source,
            node: output.node.to_string(),
            run,
            revision,
            provenance,
            readers,
            epoch,
        })
    }
    pub(super) fn policy(&self) -> FlowPolicy {
        self.provenance.policy().clone()
    }
    fn check_authority(&self) -> Result<(), StoreError> {
        let policy = self.provenance.policy();
        if !policy.provider_reads().is_empty() && self.epoch != self.readers.epoch() {
            return Err(StoreError::ProviderReadChanged);
        }
        if self.readers.blocks_policy(policy) {
            return Err(StoreError::ProviderReadUnavailable);
        }
        Ok(())
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
            if let Err(error) = self.check_authority() {
                return Outcome::Failed(error.read_error("Byte read refused")).into();
            }
            let result = match &self.source {
                Source::Resident(value) => {
                    let source = value.clone();
                    let options = self.options.clone();
                    let read =
                        tokio::task::spawn_blocking(move || resident_range(&source, &options))
                            .await;
                    read.unwrap_or(Err(StoreError::Corrupt))
                }
                Source::Stored(snapshot) => match storage {
                    Some(worker) => {
                        worker
                            .source_range_selected(
                                snapshot.clone(),
                                self.options.fields.clone(),
                                self.options.from,
                                self.options.limit,
                            )
                            .await
                    }
                    None => Err(StoreError::Closed),
                },
            };
            // Submitted local work is joined, even when the read is cancelled or withdrawn.
            if token.is_cancelled() {
                return cancelled().into();
            }
            let result = self
                .check_authority()
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
        let (handle, digest, selection) = match &self.source {
            Source::Resident(_) => (None, None, self.options.fields.clone()),
            Source::Stored(snapshot) => {
                let mut selection = snapshot.selection.clone();
                selection.extend(self.options.fields.iter().cloned());
                (
                    Some(snapshot.handle.to_string()),
                    Some(snapshot.digest.clone()),
                    selection,
                )
            }
        };
        let optional = |s: Option<String>| Data::Option(s.map(|v| Box::new(Data::Text(v.into()))));
        let data = Data::Record(IndexMap::from([
            ("source".into(), Data::Text(self.node.clone().into())),
            ("run".into(), optional(self.run.clone())),
            (
                "revision".into(),
                Data::Text(self.revision.to_string().into()),
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
        Value::new(shape(), data, self.provenance.clone())
            .map(|v| v.with_metadata(Some(ValueMetadata::capture(&contract))))
            .map_err(|_| StoreError::Corrupt)
    }
}
fn resident_range(source: &Value, options: &Options) -> Result<SourceRange, StoreError> {
    let mut data = source.data();
    for field in &options.fields {
        let Data::Record(fields) = data else {
            return Err(StoreError::SourceRange("select: requires record fields"));
        };
        data = fields
            .get(field)
            .ok_or(StoreError::SourceRange("selected field is unavailable"))?;
    }
    let (bytes, text) = match data {
        Data::Text(s) => (s.as_bytes(), true),
        Data::Bytes(b) => (b.as_ref(), false),
        _ => {
            return Err(StoreError::SourceRange(
                "selected source must be Text or Bytes",
            ));
        }
    };
    let from = usize::try_from(options.from)
        .ok()
        .filter(|n| *n <= bytes.len())
        .ok_or(StoreError::SourceRange(
            "from: exceeds the original source extent",
        ))?;
    let next = from.saturating_add(options.limit).min(bytes.len());
    Ok(SourceRange {
        bytes: bytes[from..next].to_vec(),
        extent: bytes.len() as u64,
        text,
    })
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
