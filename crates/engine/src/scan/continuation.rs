//! A preview reads the owned checkpoint; the returned value grants no controls.
use super::{
    ContinuationReview, Settings,
    bounds::RequestedBounds,
    owned::{OwnedAnalysis, Selection},
};
use crate::{
    driver::{CancellationToken, ExecutionFuture},
    plan::MetaTask,
    storage::StoreWorker,
    workspace::Workspace,
};
use wes_core::{
    Data, Value,
    contracts::{ContractRegistry, metadata::ValueMetadata},
    flow::FlowPolicy,
};
use wes_language::{Diagnostic, Span};

#[derive(Clone, Debug)]
pub struct BoundContinuation {
    selection: OwnedAnalysis,
    requested: RequestedBounds,
    span: Span,
}
impl BoundContinuation {
    pub(crate) fn bind(task: MetaTask, span: Span) -> Result<Self, Diagnostic> {
        if task
            .inputs
            .keys()
            .any(|key| !RequestedBounds::NAMES.contains(&key.as_str()))
        {
            return Err(Diagnostic::error(
                "CAL009",
                span,
                "Continuation preview accepts only six literal cumulative totals",
            ));
        }
        Ok(Self {
            selection: OwnedAnalysis::bind(&task, span)?,
            requested: RequestedBounds::parse(&task, span)?,
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
            let fail = |message: String| {
                super::bound::failed(crate::calc::Failure::new("CAL004", self.span, message))
            };
            if token.is_cancelled() {
                return super::bound::failed(crate::calc::Failure::cancelled(self.span)).into();
            }
            let node = self.selection.node_id().to_owned();
            let run = match self.selection.issued() {
                Ok(run) => run,
                Err(e) => return fail(e).into(),
            };
            let Some(worker) = storage else {
                return fail("Continuation preview requires an owned durable analysis".into())
                    .into();
            };
            let captured = match worker.dataset_analysis_read(run).await {
                Ok(c) => c,
                Err(e) => return fail(e.to_string()).into(),
            };
            if token.is_cancelled() {
                return super::bound::failed(crate::calc::Failure::cancelled(self.span)).into();
            }
            let requested = self.requested.requested(captured.checkpoint.budget.totals);
            let review = match ContinuationReview::new(
                &captured.reference,
                &captured.checkpoint,
                &captured.status,
                requested,
                Settings::capture(),
                self.span,
            ) {
                Ok(r) => r,
                Err(e) => return super::bound::failed(e).into(),
            };
            crate::runtime::Outcome::Produced(preview_value(&node, &captured, &review)).into()
        })
    }
}
fn preview_value(
    node: &str,
    captured: &crate::storage::datasets::DatasetContinuation,
    review: &ContinuationReview,
) -> Value {
    use crate::storage::datasets::AnalysisStop;
    let cp = &captured.checkpoint;
    let text = |s: &str| Data::Text(s.to_owned().into());
    let n = |n: u64| Data::Int(n as i64);
    let optional = |s: Option<&str>| Data::Option(s.map(|s| Box::new(text(s))));
    let old = cp.budget.totals.values();
    let active = review.active.totals().values();
    let issuance = cp.budget.ceilings.values();
    let requested = review.requested.values();
    let rows = RequestedBounds::NAMES
        .iter()
        .enumerate()
        .map(|(i, key)| {
            let status = if requested[i] > active[i] {
                "above_ceiling"
            } else if requested[i] < old[i] {
                "lowered"
            } else if requested[i] == old[i] {
                "unchanged"
            } else {
                "raised"
            };
            Data::Record(
                [
                    ("key".into(), text(key)),
                    ("current".into(), n(old[i])),
                    ("issuanceCeiling".into(), n(issuance[i])),
                    ("activeCeiling".into(), n(active[i])),
                    ("requested".into(), n(requested[i])),
                    ("status".into(), text(status)),
                ]
                .into(),
            )
        })
        .collect();
    let stop = cp.stop.map(|s| match s {
        AnalysisStop::Cumulative(d) => d.name(),
        AnalysisStop::Cancelled => "cancelled",
        AnalysisStop::Deterministic => "deterministic",
        AnalysisStop::IncompleteSource => "incomplete_source",
    });
    let settings = review.frozen_settings();
    let frozen = Data::Record(
        [
            ("stepRevision".into(), text(&cp.step_revision)),
            (
                "finishRevision".into(),
                optional(cp.finish_revision.as_deref()),
            ),
            ("profileRevision".into(), text(&cp.profile_digest)),
            ("sourceDigest".into(), text(&cp.source.digest)),
            ("memory".into(), n(settings.limits.memory_bytes)),
            ("recordWork".into(), n(settings.scratch.work)),
            ("scratch".into(), n(settings.scratch.bytes)),
            ("recordOutputs".into(), n(settings.outputs_per_record)),
            ("startup".into(), n(settings.startup_work)),
            ("rate".into(), n(settings.work_per_input_unit)),
        ]
        .into(),
    );
    let work_grant = review.requested.work.saturating_sub(cp.budget.totals.work);
    let fields = [
        ("version", n(1)),
        ("node", text(node)),
        ("analysis", text(&cp.analysis)),
        ("run", text(&cp.run)),
        ("attempt", text(&cp.attempt)),
        ("basis", text(&review.basis)),
        ("budgetDigest", text(&cp.budget.digest())),
        ("budgetIssuedAttempt", text(&cp.budget.issued_attempt)),
        ("latest", Data::Bool(captured.status.latest)),
        ("activeWriter", Data::Bool(captured.status.active_writer)),
        (
            "lifecycle",
            text(match captured.status.lifecycle {
                crate::storage::datasets::DatasetLifecycle::Open => "open",
                crate::storage::datasets::DatasetLifecycle::Sealed => "sealed",
                crate::storage::datasets::DatasetLifecycle::Cancelled => "cancelled",
                crate::storage::datasets::DatasetLifecycle::Interrupted => "interrupted",
                crate::storage::datasets::DatasetLifecycle::Incomplete => "incomplete",
                _ => "prefix",
            }),
        ),
        ("stop", optional(stop)),
        ("canResume", Data::Bool(review.resume_reason.is_none())),
        (
            "resumeReason",
            optional(review.resume_reason.map(|r| r.name())),
        ),
        ("canContinue", Data::Bool(review.continue_reason.is_none())),
        (
            "continueReason",
            optional(review.continue_reason.map(|r| r.name())),
        ),
        ("measuredWork", n(cp.work.completed)),
        ("chargedWork", n(cp.work.charged)),
        ("outstandingWork", n(cp.work.outstanding)),
        ("chargedAfterInterruption", n(review.charged_work)),
        ("durationChargedMs", n(cp.duration.spent_ms)),
        ("durationOutstandingMs", n(cp.duration.outstanding_ms)),
        ("durationAfterInterruptionMs", n(review.charged_duration_ms)),
        ("allowanceBefore", n(cp.usage.work_allowance)),
        ("allowanceAfter", n(review.allowance_after)),
        ("authorizedWork", n(cp.budget.authorized_work)),
        ("newWorkGrant", n(work_grant)),
        ("position", text(&cp.next_position.to_string())),
        (
            "outputCount",
            text(&captured.reference.records().to_string()),
        ),
        ("bounds", Data::List(rows)),
        ("frozen", frozen),
    ];
    let mut registry = ContractRegistry::new();
    registry
        .load(CONTRACT)
        .expect("native continuation contract");
    let contract = registry
        .resolve("ScanContinuationPreview")
        .expect("native continuation contract");
    Value::new(
        contract.shape(),
        Data::Record(fields.into_iter().map(|(k, v)| (k.into(), v)).collect()),
        captured.source.provenance().clone().with_policy(
            &captured
                .source
                .provenance()
                .policy()
                .clone()
                .read_from_dataset(&captured.reference),
        ),
    )
    .expect("native continuation facts match their contract")
    .with_metadata(Some(ValueMetadata::capture(&contract)))
}
const CONTRACT: &str = r#"types:
  ScanContinuationBound:
    base: Record
    fields: {key: Text, current: Int, issuanceCeiling: Int, activeCeiling: Int, requested: Int, status: Text}
  ScanContinuationFrozen:
    base: Record
    fields: {stepRevision: Text, finishRevision: 'Option<Text>', profileRevision: Text, sourceDigest: Text, memory: Int, recordWork: Int, scratch: Int, recordOutputs: Int, startup: Int, rate: Int}
  ScanContinuationPreview:
    base: Record
    fields:
      version: Int
      node: Text
      analysis: Text
      run: Text
      attempt: Text
      basis: Text
      budgetDigest: Text
      budgetIssuedAttempt: Text
      latest: Bool
      activeWriter: Bool
      lifecycle: Text
      stop: Option<Text>
      canResume: Bool
      resumeReason: Option<Text>
      canContinue: Bool
      continueReason: Option<Text>
      measuredWork: Int
      chargedWork: Int
      outstandingWork: Int
      chargedAfterInterruption: Int
      durationChargedMs: Int
      durationOutstandingMs: Int
      durationAfterInterruptionMs: Int
      allowanceBefore: Int
      allowanceAfter: Int
      authorizedWork: Int
      newWorkGrant: Int
      position: Text
      outputCount: Text
      bounds: List<ScanContinuationBound>
      frozen: ScanContinuationFrozen
"#;
