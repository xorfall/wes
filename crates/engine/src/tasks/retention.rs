//! Explicit retention of one acknowledged immutable output; no producer dependency or replay.
use super::*;
use crate::{
    byte_input::{CapturedSource, Source},
    environments::InvocationAuthority,
    storage::StoreWorker,
    workspace::Workspace,
};

#[derive(Clone, Debug)]
pub struct BoundRetention {
    subject: OutputRef,
    authority: InvocationAuthority,
    captured: Option<Result<CapturedSource, ErrorValue>>,
    policy: wes_core::flow::FlowPolicy,
}
impl BoundRetention {
    pub(crate) fn bind(task: MetaTask) -> Result<Self, &'static str> {
        let [Input::FromNode(subject)] = task.subjects.as_slice() else {
            return Err(
                "Keep requires one whole acknowledged result reference; no producer is run.",
            );
        };
        if subject.port != crate::graph::OutputPort::Data || !task.inputs.is_empty() {
            return Err(
                "Keep requires a whole data result without options; select a finite value explicitly first.",
            );
        }
        Ok(Self {
            subject: subject.clone(),
            authority: Default::default(),
            captured: None,
            policy: Default::default(),
        })
    }
    pub(crate) fn set_authority(&mut self, authority: InvocationAuthority) {
        self.authority = authority;
    }
    pub(crate) fn source(&self) -> &OutputRef {
        &self.subject
    }
    pub(crate) fn policy(&self) -> wes_core::flow::FlowPolicy {
        self.policy.clone()
    }
    pub(crate) fn capture(&mut self, workspace: &Workspace) {
        self.policy = workspace
            .runtime()
            .result_provenance(&self.subject.node)
            .map(|p| p.policy().clone())
            .unwrap_or_default();
        self.captured = Some((|| {
            let source=CapturedSource::capture(workspace,&self.subject,vec![])
                .map_err(|e|e.read_error("Keep requires an acknowledged original output; no producer was run"))?;
            self.policy=source.provenance.policy().clone();
            if !self.policy.allows_retention() {
                return Err(RuntimeCode::ResultForbidden.error("This result's private, temporary or unknown policy forbids durable retention; no policy was promoted.",None));
            }
            if !matches!(source.source,Source::Stored(_)) {
                return Err(RuntimeCode::ResultUnavailable.error("Keep requires an acknowledged storage owner; no value was regenerated.",None));
            }
            Ok(source)
        })().map_err(|e|e.with_policy(&self.policy)));
    }
    pub(crate) fn execute(
        self,
        worker: Option<StoreWorker>,
        cancellation: CancellationToken,
    ) -> ExecutionFuture {
        Box::pin(async move {
            let fail = |error: ErrorValue| Outcome::Failed(error.with_policy(&self.policy)).into();
            if !self.authority.is_local_user() {
                return fail(RuntimeCode::ResultForbidden.error("Retaining an ordinary result requires an explicit native user invocation; read permission alone grants no retention authority.",None));
            }
            let source = match self.captured {
                Some(Ok(source)) => source,
                Some(Err(error)) => return fail(error),
                None => {
                    return fail(RuntimeCode::ExecutionFailed.error(
                        "Keep was not captured at native entry; nothing was retained.",
                        None,
                    ));
                }
            };
            let Some(worker) = worker else {
                return fail(
                    RuntimeCode::RecordingFailed
                        .error("Keep requires the original storage owner.", None),
                );
            };
            let Source::Stored(snapshot) = &source.source else {
                unreachable!("captured stored owner")
            };
            if cancellation.is_cancelled() {
                return Outcome::Cancelled(
                    RuntimeCode::Cancelled
                        .error("Keep was cancelled before retention.", None)
                        .with_policy(&self.policy),
                )
                .into();
            }
            if let Err(error) = source.check() {
                return fail(error.read_error("Keep refused before retention"));
            }
            let holds = match worker.hold_snapshots(vec![snapshot.clone()]).await {
                Ok(holds) => holds,
                Err(error) => return fail(error.read_error("Original Keep owner is unavailable")),
            };
            if cancellation.is_cancelled() {
                return Outcome::Cancelled(
                    RuntimeCode::Cancelled
                        .error("Keep was cancelled before retention.", None)
                        .with_policy(&self.policy),
                )
                .into();
            }
            if let Err(error) = source.check() {
                return fail(error.read_error("Keep refused before retention"));
            }
            // Join an admitted retention request even if cancellation or read withdrawal follows.
            let mut output = match worker.retain(snapshot.handle.clone()).await {
                Ok(output) => output,
                Err(error) => return fail(error.read_error(
                    "Keep was not acknowledged and may have applied; no automatic retry was made",
                )),
            };
            output.snapshot = Some(snapshot.clone());
            let output=match worker.acknowledge_acquired(output).await {
                Ok(output)=>output,
                Err(error)=>return fail(error.read_error("Retention was confirmed but output acknowledgement failed; the protected root remains and no retry was made")),
            };
            if cancellation.is_cancelled() {
                return fail(RuntimeCode::RecordingFailed.error("Retention completed but result publication was cancelled; the protected root remains and no retry was made.",None));
            }
            if let Err(error) = source.check() {
                return fail(error.read_error("Retention completed but result publication was refused; the protected root remains"));
            }
            let mut report: crate::driver::ExecutionReport = Outcome::Stored(output).into();
            report
                .holds
                .push(crate::driver::ExecutionHold::retain(holds));
            report.publication_guard = Some(crate::driver::publication::Guard::new(move || {
                source.check().is_ok()
            }));
            report
        })
    }
}
