//! Whole-node analysis selection is shared by continuation and evidence reads.
use crate::{
    graph::{OutputPort, OutputRef},
    plan::{Input, MetaTask},
    tasks::BoundTask,
    workspace::Workspace,
};
use wes_core::flow::FlowPolicy;
use wes_language::{Diagnostic, Span};

#[derive(Clone, Debug)]
pub(super) struct OwnedAnalysis {
    source: OutputRef,
    captured: Option<Result<String, String>>,
    policy: FlowPolicy,
}
#[derive(Clone, Copy)]
pub(super) enum Selection {
    Continue,
    Read,
}
impl OwnedAnalysis {
    pub(super) fn bind(task: &MetaTask, span: Span) -> Result<Self, Diagnostic> {
        let [Input::FromNode(source)] = task.subjects.as_slice() else {
            return Err(Diagnostic::error(
                "CAL009",
                span,
                "Select one whole owned scan analysis, not a copied value, field or UUID",
            ));
        };
        if source.port != OutputPort::Data {
            return Err(Diagnostic::error(
                "CAL009",
                span,
                "Select the analysis data output",
            ));
        }
        Ok(Self {
            source: source.clone(),
            captured: None,
            policy: Default::default(),
        })
    }
    pub(super) fn policy(&self) -> FlowPolicy {
        self.policy.clone()
    }
    pub(super) fn issued(self) -> Result<String, String> {
        self.captured.unwrap_or_else(|| {
            Err("Analysis selection was not captured by the workspace owner".into())
        })
    }
    pub(super) fn capture(&mut self, workspace: &Workspace, selection: Selection) {
        self.captured = Some(
            (|| {
                let runtime = workspace.runtime();
                let node = runtime
                    .graph()
                    .node(&self.source.node)
                    .ok_or("Analysis is no longer present in this workspace")?;
                if !matches!(
                    node.payload(),
                    BoundTask::Scan(_) | BoundTask::ScanResume(_)
                ) {
                    return Err(
                        "Select an original owned scan analysis, not an arbitrary stored value",
                    );
                }
                if matches!(selection, Selection::Continue)
                    && (runtime.is_executing(&self.source.node)
                        || matches!(
                            node.state(),
                            crate::graph::NodeState::Running | crate::graph::NodeState::Pending
                        ))
                {
                    return Err(
                        "Analysis still has an active run; stop it before requesting continuation",
                    );
                }
                if let Some(value) = runtime
                    .value_of(&self.source.node)
                    .or_else(|| runtime.evidence_value(&self.source.node).map(|e| &e.value))
                {
                    self.policy = value.provenance().policy().clone();
                    if self.policy.is_private() || self.policy.is_unknown() {
                        return Err("Analysis selection is not exportable");
                    }
                }
                runtime
                    .run_of(&self.source.node)
                    .map(ToString::to_string)
                    .ok_or("Analysis has no owned run acknowledgement")
            })()
            .map_err(str::to_owned),
        );
    }
}
