//! Local recovery selects owned work, never a nominal Dataset or producer capability.
use crate::{
    driver::{CancellationToken, ExecutionFuture},
    graph::{OutputPort, OutputRef},
    plan::{Input, MetaTask},
    runtime::{Outcome, RuntimeCode},
    storage::{StoreWorker, datasets::*},
    workspace::Workspace,
};
use wes_core::{
    Data, Provenance, Value,
    contracts::{Contract, ContractField, ContractRegistry, metadata::ValueMetadata},
    flow::FlowPolicy,
};
use wes_language::{Diagnostic, Span, vocabulary::MetaCommand};
#[derive(Clone, Copy, Debug, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ReconciliationControl {
    pub command: &'static str,
    pub available: bool,
}
#[derive(Clone, Debug)]
enum ReconciliationTarget {
    Store,
    Owned(OutputRef),
}
#[derive(Clone, Debug)]
pub struct BoundReconcile {
    target: ReconciliationTarget,
    role: DatasetWriteRole,
    expected_run: Option<String>,
    captured: Option<Result<DatasetWriteSelection, String>>,
    policy: FlowPolicy,
    authority: crate::environments::InvocationAuthority,
}
impl BoundReconcile {
    pub(crate) fn bind(task: MetaTask, span: Span) -> Result<Self, Diagnostic> {
        let invalid = |s| Diagnostic::error("PLN004", span, s);
        let target = match task.subjects.as_slice() {
            [] if task.spec.command == MetaCommand::DatasetReconcile && task.inputs.is_empty() => {
                ReconciliationTarget::Store
            }
            [Input::FromNode(subject)] if subject.port == OutputPort::Data => {
                ReconciliationTarget::Owned(subject.clone())
            }
            _ => {
                return Err(invalid(
                    "Select one whole original owned analysis/recording; bare :dataset reconcile repairs only the owned local store",
                ));
            }
        };
        if task.inputs.keys().any(|key| key != "run") {
            return Err(invalid(
                "Reconcile preserves the owned run and accepts no overrides",
            ));
        }
        let role = match task.spec.command {
            MetaCommand::ScanReconcile => DatasetWriteRole::Analysis,
            MetaCommand::DatasetReconcile => DatasetWriteRole::Recording,
            _ => return Err(invalid("Unknown local recovery command")),
        };
        let expected_run = match task.inputs.get("run") {
            None => None,
            Some(Input::Literal(value)) => match value.data() {
                Data::Text(text)
                    if uuid::Uuid::parse_str(text)
                        .is_ok_and(|id| id.hyphenated().to_string() == text.as_ref()) =>
                {
                    Some(text.to_string())
                }
                _ => return Err(invalid("run: must be a literal canonical owned run UUID")),
            },
            _ => return Err(invalid("run: must be a literal canonical owned run UUID")),
        };
        Ok(Self {
            target,
            role,
            expected_run,
            captured: None,
            policy: Default::default(),
            authority: Default::default(),
        })
    }
    pub(crate) fn command(&self) -> MetaCommand {
        match self.role {
            DatasetWriteRole::Analysis => MetaCommand::ScanReconcile,
            DatasetWriteRole::Recording => MetaCommand::DatasetReconcile,
        }
    }
    pub(crate) fn set_authority(&mut self, authority: crate::environments::InvocationAuthority) {
        self.authority = authority;
    }
    pub(crate) fn policy(&self) -> FlowPolicy {
        self.policy.clone()
    }
    pub(crate) fn capture(&mut self, workspace: &Workspace) {
        let ReconciliationTarget::Owned(subject) = &self.target else {
            return;
        };
        self.captured = Some((|| {
            let runtime = workspace.runtime();
            let node = runtime.graph().node(&subject.node).ok_or("Selected work is no longer in this workspace")?;
            match (self.role, node.payload()) {
                (DatasetWriteRole::Analysis, super::BoundTask::Scan(_) | super::BoundTask::ScanAttempt(_)) => {},
                (DatasetWriteRole::Recording, super::BoundTask::Recording(r)) if r.starts_lifetime() => {},
                _ => return Err("Reconcile requires original owned analysis or recording work"),
            }
            if runtime.is_executing(&subject.node) || matches!(node.state(), crate::graph::NodeState::Pending | crate::graph::NodeState::Running) {
                return Err("Selected work has not physically joined; stop it before local reconciliation");
            }
            self.policy = workspace.typing(subject).map_or_else(FlowPolicy::default, |t| t.provenance.policy().clone());
            if let Some(value) = runtime.value_of(&subject.node).or_else(|| runtime.evidence_value(&subject.node).map(|e| &e.value)) { self.policy = self.policy.join(value.provenance().policy()); }
            if self.policy.is_private() || self.policy.is_unknown() { return Err("Selected work is not exportable"); }
            let run = runtime.run_of(&subject.node).ok_or("Selected work has no owned run identity")?;
            if self.expected_run.as_ref().is_some_and(|expected| expected != run.as_str()) { return Err("Selected run changed; review the new local-write identity before reconciling"); }
            Ok(DatasetWriteSelection { role: self.role, run: run.to_string() })
        })().map_err(str::to_owned));
    }
    pub(crate) fn execute(
        self,
        worker: Option<StoreWorker>,
        token: CancellationToken,
    ) -> ExecutionFuture {
        Box::pin(async move {
            let fail = |message: String| {
                Outcome::Failed(RuntimeCode::ExecutionFailed.error(message, None))
            };
            if !self.authority.is_local_user() {
                return fail("Local reconciliation requires an authorized user session".into())
                    .into();
            }
            if token.is_cancelled() {
                return Outcome::Cancelled(
                    RuntimeCode::Cancelled
                        .error("Local reconciliation cancelled before admission", None),
                )
                .into();
            }
            let Some(worker) = worker else {
                return fail("Local reconciliation requires the owned durable store".into()).into();
            };
            if matches!(self.target, ReconciliationTarget::Store) {
                return match worker.dataset_reconcile_store().await {
                    Ok(persistence) => Outcome::Produced(store_receipt_value(persistence)).into(),
                    Err(error) => fail(error.to_string()).into(),
                };
            }
            let owner = match self.captured {
                Some(Ok(owner)) => owner,
                Some(Err(e)) => return fail(e).into(),
                None => {
                    return fail("Reconciliation was not captured by its workspace owner".into())
                        .into();
                }
            };
            // Entered I/O is joined even if cancellation arrives; recovery never retries a producer.
            match worker.dataset_reconcile(owner).await {
                Ok(receipt) => Outcome::Produced(receipt_value(receipt, &self.policy)).into(),
                Err(e) => fail(e.to_string()).into(),
            }
        })
    }
}
fn receipt_value(receipt: DatasetReconciliation, policy: &FlowPolicy) -> Value {
    let registry = ContractRegistry::new();
    let text = registry.resolve("Text").unwrap();
    let optional = registry.resolve("Option<Text>").unwrap();
    let boolean = registry.resolve("Bool").unwrap();
    let opt = |value: Option<String>| Data::Option(value.map(|s| Box::new(Data::Text(s.into()))));
    let current = receipt
        .committed
        .as_ref()
        .or(receipt.predecessor.as_ref())
        .cloned();
    let policy = policy.join(&receipt.policy);
    let fields: Vec<(&str, Data, std::sync::Arc<Contract>)> = vec![
        (
            "ownerRole",
            Data::Text(
                match receipt.selection.role {
                    DatasetWriteRole::Analysis => "analysis",
                    DatasetWriteRole::Recording => "recording",
                }
                .into(),
            ),
            text.clone(),
        ),
        (
            "selectedRun",
            Data::Text(receipt.selection.run.into()),
            text.clone(),
        ),
        (
            "ownerRun",
            opt(receipt.owner.as_ref().map(|owner| owner.run.clone())),
            optional.clone(),
        ),
        (
            "lineage",
            opt(receipt.owner.as_ref().map(|owner| owner.lineage.clone())),
            optional.clone(),
        ),
        (
            "witnessScope",
            Data::Text("latest_admitted".into()),
            text.clone(),
        ),
        (
            "currentGeneration",
            opt(current.as_ref().map(|p| p.generation().to_string())),
            optional.clone(),
        ),
        (
            "currentRecords",
            opt(current.as_ref().map(|p| p.records().to_string())),
            optional.clone(),
        ),
        (
            "currentManifestDigest",
            opt(current.map(|p| p.manifest_digest().to_string())),
            optional.clone(),
        ),
        ("transaction", opt(receipt.transaction), optional.clone()),
        (
            "writeOutcome",
            Data::Text(
                match receipt.outcome {
                    DatasetWriteOutcome::Committed => "committed",
                    DatasetWriteOutcome::Absent => "absent",
                    DatasetWriteOutcome::Unknown => "unknown",
                }
                .into(),
            ),
            text.clone(),
        ),
        (
            "predecessorGeneration",
            opt(receipt.predecessor.map(|p| p.generation().to_string())),
            optional.clone(),
        ),
        (
            "committedGeneration",
            opt(receipt
                .committed
                .as_ref()
                .map(|p| p.generation().to_string())),
            optional.clone(),
        ),
        (
            "committedRecords",
            opt(receipt.committed.as_ref().map(|p| p.records().to_string())),
            optional.clone(),
        ),
        (
            "committedManifestDigest",
            opt(receipt.committed.map(|p| p.manifest_digest().to_string())),
            optional,
        ),
        (
            "persistence",
            Data::Text(
                match receipt.persistence {
                    crate::history::Persistence::FileAndDirectorySynced => {
                        "file_and_directory_synced"
                    }
                    crate::history::Persistence::FileSynced => "file_synced",
                    _ => "unknown",
                }
                .into(),
            ),
            text,
        ),
        (
            "executionUnknown",
            Data::Bool(receipt.execution_unknown),
            boolean,
        ),
    ];
    record_value("LocalWriteReceipt", fields, &policy)
}
fn store_receipt_value(persistence: crate::history::Persistence) -> Value {
    let registry = ContractRegistry::new();
    let text = registry.resolve("Text").unwrap();
    let boolean = registry.resolve("Bool").unwrap();
    record_value(
        "DatasetRecoveryReceipt",
        vec![
            ("scope", Data::Text("owned_store".into()), text.clone()),
            ("status", Data::Text("reconciled".into()), text.clone()),
            (
                "persistence",
                Data::Text(
                    match persistence {
                        crate::history::Persistence::FileAndDirectorySynced => {
                            "file_and_directory_synced"
                        }
                        crate::history::Persistence::FileSynced => "file_synced",
                        crate::history::Persistence::Volatile => "unknown",
                    }
                    .into(),
                ),
                text,
            ),
            ("executionUnknown", Data::Bool(true), boolean),
        ],
        &FlowPolicy::default(),
    )
}
fn record_value(
    name: &str,
    fields: Vec<(&str, Data, std::sync::Arc<Contract>)>,
    policy: &FlowPolicy,
) -> Value {
    let contract = Contract::record(
        name,
        fields
            .iter()
            .map(|(key, _, contract)| {
                (
                    (*key).into(),
                    ContractField {
                        contract: contract.clone(),
                        optional: false,
                    },
                )
            })
            .collect(),
    )
    .expect("local receipt contract");
    Value::new(
        contract.shape(),
        Data::Record(
            fields
                .into_iter()
                .map(|(key, data, _)| (key.into(), data))
                .collect(),
        ),
        Provenance::default().with_policy(policy),
    )
    .expect("bounded local receipt")
    .with_metadata(Some(ValueMetadata::capture(&contract)))
}
