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
    fields: Vec<String>,
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
        let (source, fields) = match task.subjects.as_slice() {
            [Input::FromNode(source)] => (source, vec![]),
            [Input::FieldPath { output, fields }] => (output, fields.clone()),
            _ => {
                return Err(Diagnostic::error(
                    "CAL009",
                    span,
                    "Select one whole owned scan analysis or an exact saved analysis member",
                ));
            }
        };
        if fields.len() > 1 {
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
            fields,
            captured: None,
            policy: Default::default(),
        })
    }
    pub(super) fn node_id(&self) -> &str {
        self.source.node.as_str()
    }
    pub(super) fn read_sources(&self) -> Vec<OutputRef> {
        vec![self.source.clone()]
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
                if let BoundTask::SavedInput(saved) = node.payload() {
                    let member = if saved.single && self.fields.is_empty() {
                        saved.members.first()
                    } else if !saved.single && self.fields.len() == 1 {
                        saved.members.iter().find(|m| m.name == self.fields[0])
                    } else {
                        None
                    };
                    let member = member
                        .filter(|m| {
                            m.port == "data"
                                && m.fields.is_empty()
                                && m.payload.is_some()
                                && matches!(m.operation.as_str(), "scan" | "scan_attempt")
                        })
                        .ok_or("Select an exact saved whole scan result, not an arbitrary copied value")?;
                    let policy = match runtime.output(&self.source) {
                        crate::runtime::OutputState::Available(value) => {
                            let value = crate::plan::project_value(&value, &self.fields)
                                .map_err(|_| "The saved analysis member is unavailable")?;
                            if workspace.dataset_access.blocks(&value) {
                                return Err("Saved analysis access is not authorized");
                            }
                            value.provenance().policy().clone()
                        }
                        crate::runtime::OutputState::Stored(stored) => {
                            // Entry read preparation holds and revalidates this exact owner
                            // without materializing a cold saved composite. Only native
                            // whole-scan membership supplies its original run acknowledgement.
                            let payload = member.payload.as_ref().expect("checked payload");
                            if (saved.single
                                && (stored.snapshot.handle.as_str() != payload.handle
                                    || stored.snapshot.digest != payload.digest
                                    || stored.snapshot.selection != member.fields))
                                || (!saved.single
                                    && (stored.snapshot.handle.as_str() != saved.id
                                        || !stored.snapshot.selection.is_empty()))
                            {
                                return Err("Saved analysis backing identity changed");
                            }
                            if crate::plan::field_shape(&stored.snapshot.shape, &self.fields)
                                .is_none()
                            {
                                return Err("The saved analysis member is unavailable");
                            }
                            let policy = stored.snapshot.provenance.policy();
                            if workspace.dataset_access.blocks_policy(policy) {
                                return Err("Saved analysis access is not authorized");
                            }
                            policy.clone()
                        }
                        _ => return Err("The saved analysis is no longer accessible"),
                    };
                    if policy.is_private() || policy.is_unknown() {
                        return Err("Saved analysis access is not authorized");
                    }
                    self.policy = policy;
                    return member
                        .run
                        .clone()
                        .ok_or("Saved analysis has no exact owned run");
                }
                if !self.fields.is_empty() || !matches!(
                    node.payload(),
                    BoundTask::Scan(_) | BoundTask::ScanAttempt(_)
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
