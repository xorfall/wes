//! Exact-prefix dataset reads capture an owned workspace selection at run entry.
use crate::{
    driver::{CancellationToken, ExecutionFuture},
    plan::{Input, MetaTask},
    runtime::{Outcome, RuntimeCode},
    storage::{
        StoreWorker,
        datasets::{DatasetSnapshot, PageRequest},
    },
    workspace::Workspace,
};
use indexmap::IndexMap;
use std::sync::Arc;
use wes_core::{
    Data, Provenance, Shape, Value,
    contracts::{Contract, ContractField, ContractRegistry, metadata::ValueMetadata},
    flow::FlowPolicy,
};
use wes_language::{Diagnostic, Span, vocabulary::MetaCommand};
#[derive(Clone, Debug)]
pub struct BoundDataset {
    command: MetaCommand,
    input: Option<Input>,
    from: u64,
    limit: usize,
    captured: Option<Result<Value, String>>,
    policy: FlowPolicy,
    authority: crate::environments::InvocationAuthority,
    remove_references: bool,
    protected: bool,
    snapshot: Option<DatasetSnapshot>,
    basis: Option<String>,
}
impl BoundDataset {
    pub(crate) fn bind(task: MetaTask, span: Span) -> Result<Self, Diagnostic> {
        let invalid = |message| Diagnostic::error("PLN004", span, message);
        let input = match task.subjects.as_slice() {
            [] if task.spec.command == MetaCommand::DatasetCollect => None,
            [input] => Some(input.clone()),
            _ => {
                return Err(invalid(
                    "Dataset command requires one owned result selection",
                ));
            }
        };
        if input
            .as_ref()
            .is_some_and(|input| !matches!(input, Input::FromNode(_) | Input::FieldPath { .. }))
        {
            return Err(invalid(
                "Dataset read requires a workspace result reference",
            ));
        }
        let from = match task.inputs.get("from") {
            None => 0,
            Some(Input::Literal(value)) => match value.data() {
                Data::Text(text) => text
                    .parse::<u64>()
                    .ok()
                    .filter(|n| n.to_string() == text.as_ref())
                    .ok_or_else(|| invalid("from: must be a canonical unsigned decimal ordinal"))?,
                _ => return Err(invalid("from: must be a literal exact decimal ordinal")),
            },
            _ => return Err(invalid("from: must be a literal exact decimal ordinal")),
        };
        let limit = match task.inputs.get("limit") {
            None => 50,
            Some(Input::Literal(value)) => match value.data() {
                Data::Int(n) if (1..=100).contains(n) => *n as usize,
                _ => return Err(invalid("limit: must be a literal integer from 1 to 100")),
            },
            _ => return Err(invalid("limit: must be a literal integer from 1 to 100")),
        };
        let flag = |key| match task.inputs.get(key) {
            None => Ok(false),
            Some(Input::Literal(value)) => match value.data() {
                Data::Bool(value) => Ok(*value),
                _ => Err(invalid("Deletion approvals must be literal booleans")),
            },
            _ => Err(invalid("Deletion approvals must be literal booleans")),
        };
        let remove_references = flag("references")?;
        let protected = flag("protected")?;
        let text = |key: &str| match task.inputs.get(key) {
            Some(Input::Literal(value)) => match value.data() {
                Data::Text(text) => Ok(text.to_string()),
                _ => Err(invalid("Dataset identity constraints must be literal text")),
            },
            _ => Err(invalid(
                "Dataset read requires its literal identity constraints",
            )),
        };
        let basis = if matches!(
            task.spec.command,
            MetaCommand::DatasetSnapshot | MetaCommand::DatasetRetention
        ) {
            let basis = text("basis")?;
            if !valid_digest(&basis) {
                return Err(invalid("Dataset basis must be a canonical sha256 identity"));
            }
            Some(basis)
        } else {
            None
        };
        let snapshot = if task.spec.command == MetaCommand::DatasetSnapshot {
            let basis = basis.clone().expect("snapshot identity");
            let digest = text("digest")?;
            if !valid_digest(&digest) {
                return Err(invalid(
                    "Snapshot digest must be a canonical sha256 identity",
                ));
            }
            let ordinal = text("generation")?;
            let generation = ordinal
                .parse::<u64>()
                .ok()
                .filter(|n| *n > 0 && n.to_string() == ordinal)
                .ok_or_else(|| {
                    invalid("Snapshot generation must be a positive canonical decimal string")
                })?;
            Some(DatasetSnapshot {
                basis,
                generation,
                digest,
            })
        } else {
            None
        };
        Ok(Self {
            command: task.spec.command,
            input,
            from,
            limit,
            captured: None,
            policy: Default::default(),
            authority: Default::default(),
            remove_references,
            protected,
            snapshot,
            basis,
        })
    }
    pub(crate) fn set_authority(&mut self, authority: crate::environments::InvocationAuthority) {
        self.authority = authority;
    }
    pub(crate) fn observational(&self) -> bool {
        !matches!(
            self.command,
            MetaCommand::DatasetDelete | MetaCommand::DatasetCollect
        )
    }
    pub(crate) fn repeatable(&self) -> bool {
        matches!(
            self.command,
            MetaCommand::DatasetPage
                | MetaCommand::DatasetInspect
                | MetaCommand::DatasetSnapshot
                | MetaCommand::DatasetRetention
        )
    }
    pub(crate) fn predicted_typing(&self) -> wes_core::capability::Typing {
        wes_core::capability::Typing::new(if self.command == MetaCommand::DatasetPlanDelete {
            Shape::Meta(wes_core::MetaType::DatasetDeletePlan)
        } else {
            Shape::Unknown
        })
    }
    pub(crate) fn command(&self) -> MetaCommand {
        self.command
    }
    pub(crate) fn result_flow(&self) -> super::ResultFlow {
        match self.command {
            MetaCommand::DatasetDelete | MetaCommand::DatasetCollect => {
                super::ResultFlow::ControlAcknowledgement
            }
            _ => super::ResultFlow::Content,
        }
    }
    pub(crate) fn policy(&self) -> FlowPolicy {
        self.policy.clone()
    }
    pub(crate) fn capture(&mut self, workspace: &Workspace) {
        let Some(input) = &self.input else {
            return;
        };
        self.captured = Some(
            (|| {
                let output = input
                    .dependency()
                    .ok_or("Dataset selection is not an owned result")?;
                let runtime = workspace.runtime();
                let root = runtime
                    .value_of(&output.node)
                    .or_else(|| runtime.evidence_value(&output.node).map(|e| &e.value))
                    .ok_or("Dataset result is unavailable; reading does not rerun it")?;
                self.policy = root.provenance().policy().clone();
                if self.policy.is_private() || self.policy.is_unknown() {
                    return Err("Dataset selection is not exportable");
                }
                let selected = input
                    .resolve_with_limit(
                        &[(output.node.clone(), root.clone())].into(),
                        wes_budgets::get("query.bytes"),
                    )
                    .map_err(|_| "Dataset selection is unavailable or exceeds its read budget")?
                    .into_owned();
                if self.command == MetaCommand::DatasetDelete {
                    if selected.shape() != &Shape::Meta(wes_core::MetaType::DatasetDeletePlan) || selected.management_authority().is_none() {
                        return Err("Expected a live DatasetDeletePlan; restored projections grant no apply authority");
                    }
                } else if !matches!(
                    (selected.data(), selected.shape()),
                    (Data::Dataset(_), Shape::Dataset(_))
                ) {
                    return Err("Selected result is not a typed Dataset");
                }
                if let Some(basis) = &self.basis {
                    let Data::Dataset(reference) = selected.data() else { unreachable!("typed Dataset") };
                    if reference.manifest_digest() != basis.as_str() {
                        return Err("Dataset anchor changed; read the intended prefix and prepare a new capture");
                    }
                }
                Ok(selected)
            })()
            .map_err(str::to_owned),
        );
    }
    pub(crate) fn execute(
        self,
        storage: Option<StoreWorker>,
        token: CancellationToken,
    ) -> ExecutionFuture {
        Box::pin(async move {
            let failed = |message: String| {
                Outcome::Failed(RuntimeCode::ExecutionFailed.error(message, None))
            };
            if token.is_cancelled() {
                return Outcome::Cancelled(
                    RuntimeCode::Cancelled.error("Dataset read cancelled.", None),
                )
                .into();
            }
            let management = !matches!(
                self.command,
                MetaCommand::DatasetPage
                    | MetaCommand::DatasetInspect
                    | MetaCommand::DatasetSnapshot
                    | MetaCommand::DatasetRetention
            );
            if management && !self.authority.is_local_user() {
                return failed("Dataset management requires an authorized user session".into())
                    .into();
            }
            let Some(worker) = storage else {
                return failed("This storage owner has no Dataset reader".into()).into();
            };
            if self.command == MetaCommand::DatasetCollect {
                return match worker.dataset_collect().await {
                    Ok(cleanup) => cleanup_outcome(cleanup),
                    Err(e) => failed(e.to_string()).into(),
                };
            }
            let selected = match self.captured {
                Some(Ok(value)) => value,
                Some(Err(message)) => return failed(message).into(),
                None => {
                    return failed("Dataset read was not captured by its workspace owner".into())
                        .into();
                }
            };
            if self.command == MetaCommand::DatasetDelete {
                let authority = selected
                    .management_authority()
                    .expect("captured live deletion plan")
                    .to_owned();
                return match worker
                    .dataset_delete(authority, self.remove_references, self.protected)
                    .await
                {
                    Ok(cleanup) => cleanup_outcome(cleanup),
                    Err(e) => failed(e.to_string()).into(),
                };
            }
            let Data::Dataset(reference) = selected.data() else {
                unreachable!("captured typed dataset");
            };
            let reference = reference.as_ref().clone();
            if let Some(snapshot) = self.snapshot {
                let info = match worker.dataset_snapshot(reference, snapshot).await {
                    Ok(info) => info,
                    Err(e) => return failed(e.to_string()).into(),
                };
                // Revalidate the exact captured prefix before publication; never replace it with head.
                if let Err(e) = worker.dataset_inspect(info.reference.clone()).await {
                    return failed(e.to_string()).into();
                }
                if token.is_cancelled() {
                    return Outcome::Cancelled(
                        RuntimeCode::Cancelled.error("Dataset capture cancelled.", None),
                    )
                    .into();
                }
                let provenance = selected
                    .provenance()
                    .clone()
                    .with_policy(&info.policy.read_from_dataset(&info.reference));
                let value = match Value::new(
                    selected.shape().clone(),
                    Data::Dataset(Arc::new(info.reference)),
                    provenance,
                ) {
                    Ok(value) => value.with_metadata(selected.metadata().cloned()),
                    Err(e) => return failed(e.to_string()).into(),
                };
                return Outcome::Produced(value).into();
            }
            if self.command == MetaCommand::DatasetRetention {
                let cost = match worker.dataset_retention_preview(reference.clone()).await {
                    Ok(cost) => cost,
                    Err(e) => return failed(e.to_string()).into(),
                };
                let info = match worker.dataset_inspect(reference.clone()).await {
                    Ok(info) => info,
                    Err(e) => return failed(e.to_string()).into(),
                };
                if token.is_cancelled() {
                    return Outcome::Cancelled(
                        RuntimeCode::Cancelled.error("Dataset retention preview cancelled.", None),
                    )
                    .into();
                }
                let registry = ContractRegistry::new();
                let text = registry.resolve("Text").expect("retention preview text");
                let fields = [
                    ("dataset",reference.dataset().to_string()),
                    ("generation",reference.generation().to_string()),
                    ("records",reference.records().to_string()),
                    ("manifestDigest",reference.manifest_digest().to_string()),
                    ("totalBytes",cost.total_bytes.to_string()),
                    ("sharedBytes",cost.shared_bytes.to_string()),
                    ("exclusiveBytes",cost.exclusive_bytes.to_string()),
                    ("capturedSourceBytes",cost.captured_source_bytes.to_string()),
                    ("catalogRevision",cost.catalog_revision.to_string()),
                    ("notice","Exact prefix footprint including commit proofs, schemas, indexes, segments, checkpoint and captured source payloads. Shared means already independently retained now; exclusive is the remainder, not reclaimable disk. Excludes the holding result/container and catalog journal overhead. This read is not Keep or a quota reservation; later commit proofs or roots can change cost. Keep rechecks identity, authority and actual quotas.".into()),
                ].into_iter().map(|(key,value)|(key.to_string(),(text.clone(),Data::Text(value.into())))).collect();
                let provenance = selected
                    .provenance()
                    .clone()
                    .with_policy(&info.policy.read_from_dataset(&reference));
                return match build(fields, provenance) {
                    Ok(value) => Outcome::Produced(value).into(),
                    Err(e) => failed(e).into(),
                };
            }
            if self.command == MetaCommand::DatasetPlanDelete {
                return match worker.dataset_plan_delete(reference).await {
                    Ok(plan) => {
                        let projection = Data::Record([
                            ("dataset".into(),Data::Text(plan.reference.dataset().into())),
                            ("generation".into(),Data::Text(plan.reference.generation().to_string().into())),
                            ("protectedBytes".into(),Data::Text(plan.protected_bytes.to_string().into())),
                            ("activeReaders".into(),Data::Text(plan.active_readers.to_string().into())),
                            ("activeWriter".into(),Data::Bool(plan.active_writer)),
                            ("references".into(),Data::List(plan.references.into_iter().map(|r| Data::Record([
                                ("identity".into(),Data::Text(r.identity.into())),
                                ("kind".into(),Data::Text(format!("{:?}",r.kind).to_lowercase().into())),
                                ("retention".into(),Data::Text(r.retention.as_str().into())),
                            ].into())).collect())),
                            ("notice".into(),Data::Text("One live use in this workspace; expires in five minutes. Changed roots require a new plan. Active readers and writers must be stopped explicitly; restored plans have no authority.".into())),
                        ].into());
                        Outcome::Produced(Value::management(
                            wes_core::MetaType::DatasetDeletePlan,
                            projection,
                            plan.token,
                        ))
                        .into()
                    }
                    Err(e) => failed(e.to_string()).into(),
                };
            }
            let info = match worker.dataset_inspect(reference.clone()).await {
                Ok(info) => info,
                Err(e) => return failed(e.to_string()).into(),
            };
            let provenance = selected
                .provenance()
                .clone()
                .with_policy(&info.policy.read_from_dataset(&reference));
            let registry = ContractRegistry::new();
            let mut fields: IndexMap<String, (Arc<Contract>, Data)> = IndexMap::new();
            let mut text = |key: &str, value: String| {
                fields.insert(
                    key.into(),
                    (registry.resolve("Text").unwrap(), Data::Text(value.into())),
                );
            };
            text("first", self.from.to_string());
            text("records", reference.records().to_string());
            text("lifecycle", format!("{:?}", info.lifecycle).to_lowercase());
            text("schemaDigest", info.schema.digest().into());
            if self.command == MetaCommand::DatasetPage {
                let page = match worker
                    .dataset_page(
                        reference.clone(),
                        PageRequest {
                            work: None,
                            from: self.from,
                            rows: self.limit,
                            bytes: (64 * 1024).min(wes_budgets::get("dataset.page.bytes") as usize),
                            segments: (wes_budgets::get("dataset.page.segments") as usize).min(32),
                        },
                    )
                    .await
                {
                    Ok(page) => page,
                    Err(e) => return failed(e.to_string()).into(),
                };
                fields.insert(
                    "next".into(),
                    (
                        registry.resolve("Text").unwrap(),
                        Data::Text(page.next.to_string().into()),
                    ),
                );
                fields.insert(
                    "extentExhausted".into(),
                    (
                        registry.resolve("Bool").unwrap(),
                        Data::Bool(page.extent_exhausted),
                    ),
                );
                let list = match Contract::list("DatasetPageRows", page.schema.root().clone()) {
                    Ok(c) => Arc::new(c),
                    Err(e) => return failed(e.to_string()).into(),
                };
                fields.insert(
                    "rows".into(),
                    (
                        list,
                        Data::List(
                            page.rows
                                .into_iter()
                                .map(|row| row.value.into_data())
                                .collect(),
                        ),
                    ),
                );
            } else {
                fields.insert(
                    "protected".into(),
                    (
                        registry.resolve("Bool").unwrap(),
                        Data::Bool(info.protected),
                    ),
                );
                fields.insert(
                    "segmentBytes".into(),
                    (
                        registry.resolve("Text").unwrap(),
                        Data::Text(info.segment_bytes.to_string().into()),
                    ),
                );
                fields.insert(
                    "persistence".into(),
                    (
                        registry.resolve("Text").unwrap(),
                        Data::Text(format!("{:?}", info.persistence).into()),
                    ),
                );
            }
            // Revalidate after joined paging; a withdrawal also suppresses counts and schema.
            if let Err(e) = worker.dataset_inspect(reference).await {
                return failed(e.to_string()).into();
            }
            if token.is_cancelled() {
                return Outcome::Cancelled(
                    RuntimeCode::Cancelled.error("Dataset read cancelled.", None),
                )
                .into();
            }
            let built = tokio::task::spawn_blocking(move || build(fields, provenance)).await;
            match built {
                Ok(Ok(value)) => Outcome::Produced(value).into(),
                Ok(Err(message)) => failed(message).into(),
                Err(_) => failed("Dataset reply formatting failed".into()).into(),
            }
        })
    }
}
fn valid_digest(s: &str) -> bool {
    s.len() == 71
        && s.starts_with("sha256:")
        && s[7..]
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}
fn cleanup_outcome(
    cleanup: crate::storage::datasets::DatasetCleanup,
) -> crate::driver::ExecutionReport {
    let registry = ContractRegistry::new();
    let optional = registry
        .resolve("Option<Text>")
        .expect("bounded cleanup count contract");
    let mut fields: IndexMap<String, (Arc<Contract>, Data)> = [
        ("reclaimedBytes", cleanup.reclaimed_bytes),
        ("sharedBytes", cleanup.shared_bytes),
        ("pendingBytes", cleanup.pending_bytes),
    ]
    .into_iter()
    .map(|(name, count)| {
        (
            name.into(),
            (
                optional.clone(),
                Data::Option(count.map(|n| Box::new(Data::Text(n.to_string().into())))),
            ),
        )
    })
    .collect();
    fields.insert(
        "cleanupPending".into(),
        (
            registry.resolve("Bool").unwrap(),
            Data::Bool(!cleanup.complete),
        ),
    );
    match build(fields, Provenance::default()) {
        Ok(value) => Outcome::Produced(value).into(),
        Err(message) => Outcome::Failed(RuntimeCode::ExecutionFailed.error(message, None)).into(),
    }
}
fn build(
    fields: IndexMap<String, (Arc<Contract>, Data)>,
    provenance: Provenance,
) -> Result<Value, String> {
    let contract = Contract::record(
        "DatasetRead",
        fields
            .iter()
            .map(|(name, (contract, _))| {
                (
                    name.clone(),
                    ContractField {
                        contract: contract.clone(),
                        optional: false,
                    },
                )
            })
            .collect(),
    )
    .map_err(|e| e.to_string())?;
    let value = Value::new(
        contract.shape(),
        Data::Record(
            fields
                .into_iter()
                .map(|(key, (_, data))| (key, data))
                .collect(),
        ),
        provenance,
    )
    .map_err(|e| e.to_string())?
    .with_metadata(Some(ValueMetadata::capture(&contract)));
    crate::value_size::value_charge(&value, wes_budgets::get("query.bytes"))
        .ok_or("Dataset reply exceeds its materialized read budget")?;
    Ok(value)
}
