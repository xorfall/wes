//! Prepared local control effects. Import, persistence, waits and value-producing work use other ports.
use super::{PreparedMeta, Stamp, Workspace, WorkspaceError, rejected};
use crate::{
    graph::NodeId,
    plan::{Input, Plan},
    providers::BoundCall,
    runtime::{Effect, Policy, valid_timeout},
    tasks::BoundTask,
};
use std::{sync::Arc, time::Duration};
use wes_core::{
    Data, Shape, Value,
    capability::Typing,
    contracts::boundary::{self, BoundaryError},
    literals,
};
use wes_language::{
    Diagnostic, Span,
    vocabulary::{MetaCommand, RefreshScope},
};

#[derive(Debug)]
pub struct PreparedControl {
    pub(super) stamp: Stamp,
    pub(super) operation: Control,
    pub(super) diagnostics: Vec<Diagnostic>,
    pub(super) recorded: bool,
}
impl PreparedControl {
    pub(crate) fn execution_targets(
        &self,
        graph: &crate::graph::DependencyGraph<BoundTask>,
    ) -> Vec<NodeId> {
        match &self.operation {
            Control::Refresh(node) => vec![node.clone()],
            Control::RefreshDownstream(node, _) => graph
                .downstream(node)
                .unwrap_or_default()
                .into_iter()
                .collect(),
            _ => vec![],
        }
    }
    pub fn diagnostics(&self) -> &[Diagnostic] {
        &self.diagnostics
    }
    pub fn recorded(&self) -> bool {
        self.recorded
    }
    pub fn refreshes_downstream(&self) -> bool {
        matches!(self.operation, Control::RefreshDownstream(..))
    }
}
#[derive(Debug)]
#[expect(
    clippy::large_enum_variant,
    reason = "the bounded optional trace profile slightly grows Change; keep existing inline control ownership instead of restructuring unrelated declaration/control enums"
)]
pub(crate) enum Control {
    Refresh(NodeId),
    RefreshDownstream(NodeId, Span),
    Cancel(NodeId),
    Timeout(NodeId, Duration),
    Policy(Option<NodeId>, Policy),
    DropNode(NodeId),
    DropName(String),
    Change {
        node: NodeId,
        call: BoundCall,
        typing: Arc<Typing>,
    },
}
#[derive(Clone, Debug)]
pub struct OperationReceipt {
    pub operation: &'static str,
    pub target: String,
    pub node: Option<NodeId>,
    pub requested: usize,
    pub started: usize,
    pub stale: usize,
    pub skipped: usize,
    pub removed: usize,
    pub unbound: usize,
    pub detail: String,
}
impl OperationReceipt {
    pub fn summary(&self) -> String {
        let mut parts = vec![format!(
            "{} {}{}",
            self.operation,
            self.target,
            if self.detail.is_empty() {
                String::new()
            } else {
                format!(" · {}", self.detail)
            }
        )];
        for (n, label) in [
            (
                self.requested,
                if self.requested == 1 {
                    "execution requested"
                } else {
                    "executions requested"
                },
            ),
            (self.started, "started"),
            (self.stale, "marked stale"),
            (self.skipped, "skipped"),
            (self.removed, "removed"),
            (
                self.unbound,
                if self.unbound == 1 {
                    "name unbound"
                } else {
                    "names unbound"
                },
            ),
        ] {
            if n > 0 {
                parts.push(format!("{n} {label}"));
            }
        }
        parts.join(" · ")
    }
    pub fn charge(&self) -> usize {
        256 + (self.target.len() + self.detail.len()) * 6
    }
}
pub struct ControlApplied {
    pub receipt: OperationReceipt,
    pub effects: Vec<Effect<BoundTask>>,
    pub removed: Vec<NodeId>,
    pub unbound: Vec<String>,
    pub diagnostics: Vec<Diagnostic>,
}

impl Workspace {
    pub(crate) fn repeat_nodes(
        &mut self,
        nodes: &[NodeId],
        now: Duration,
    ) -> Result<Vec<Effect<BoundTask>>, WorkspaceError> {
        self.check_refresh_bindings(nodes, wes_language::Span::at(0))?;
        Ok(self.runtime.refresh_group(nodes, now)?)
    }
    pub(crate) fn cancel_nodes(
        &mut self,
        nodes: &[NodeId],
        now: Duration,
    ) -> Result<Vec<Effect<BoundTask>>, WorkspaceError> {
        let effects = self.runtime.cancel_group(nodes, now)?;
        self.views.cancel_origins(nodes);
        Ok(effects)
    }
    /// Resolve the supported immediate runtime/declaration controls without mutating state.
    /// A coordinator routes other meta commands to their own ports instead of calling this method.
    pub fn prepare_control(
        &self,
        prepared: PreparedMeta,
    ) -> Result<PreparedControl, WorkspaceError> {
        self.check_stamp(&prepared.stamp)?;
        let operation = bind_control(&prepared, self.runtime.graph(), &self.bindings, |node| {
            self.data_typing(node)
        })
        .map_err(|error| error.preceded_by(&prepared.diagnostics))?;
        let Plan::Action { task, .. } = &prepared.plan else {
            unreachable!("control validated an action")
        };
        if task.spec.recorded {
            self.next_revision()?;
        }
        Ok(PreparedControl {
            stamp: prepared.stamp,
            operation,
            diagnostics: prepared.diagnostics,
            recorded: task.spec.recorded,
        })
    }

    /// Required command persistence precedes this operation. Returned effects must be consumed by
    /// the execution coordinator, including reactive starts and revocation of an old active run.
    pub fn apply_control(
        &mut self,
        prepared: PreparedControl,
        now: Duration,
    ) -> Result<ControlApplied, WorkspaceError> {
        self.apply_control_installation(prepared, now, super::Installation::Live)
    }
    pub(super) fn apply_control_installation(
        &mut self,
        prepared: PreparedControl,
        now: Duration,
        installation: super::Installation,
    ) -> Result<ControlApplied, WorkspaceError> {
        if matches!(installation, super::Installation::Held) && !prepared.recorded {
            return Err(WorkspaceError::Obsolete);
        }
        self.check_stamp(&prepared.stamp)?;
        if matches!(installation, super::Installation::Live) {
            self.check_refresh_bindings(
                &prepared.execution_targets(self.runtime.graph()),
                wes_language::Span::at(0),
            )?;
        }
        let next = if prepared.recorded {
            Some(self.next_revision()?)
        } else {
            None
        };
        let label = |node: &NodeId| {
            self.bindings
                .names()
                .iter()
                .find(|(_, output)| {
                    output.node == *node && output.port == crate::graph::OutputPort::Data
                })
                .map_or_else(
                    || format!("${node}"),
                    |(name, _)| format!("${}", name.chars().take(80).collect::<String>()),
                )
        };
        let (operation, target, detail) = match &prepared.operation {
            Control::Refresh(node) => (
                "refresh",
                label(node),
                self.refresh_binding_summary(std::slice::from_ref(node)),
            ),
            Control::RefreshDownstream(node, _) => (
                "refresh downstream",
                label(node),
                self.refresh_binding_summary(&prepared.execution_targets(self.runtime.graph())),
            ),
            Control::Cancel(node) => (
                "cancel",
                label(node),
                "cancellation requested; external outcome may remain unknown".into(),
            ),
            Control::Timeout(node, budget) => {
                ("timeout", label(node), format!("{} ms", budget.as_millis()))
            }
            Control::Policy(node, policy) => (
                "policy",
                node.as_ref().map_or("workspace".into(), label),
                format!("{policy:?}").to_lowercase(),
            ),
            Control::DropName(name) => (
                "unbind",
                format!("${}", name.chars().take(80).collect::<String>()),
                String::new(),
            ),
            Control::DropNode(node) => ("remove downstream", label(node), String::new()),
            Control::Change { node, .. } => ("change", label(node), "definition updated".into()),
        };
        let target_node = match &prepared.operation {
            Control::Refresh(node)
            | Control::RefreshDownstream(node, _)
            | Control::Cancel(node)
            | Control::Timeout(node, _)
            | Control::DropNode(node)
            | Control::Change { node, .. } => Some(node.clone()),
            Control::Policy(node, _) => node.clone(),
            Control::DropName(name) => self
                .bindings
                .names()
                .get(name)
                .map(|output| output.node.clone()),
        };
        let requested = prepared.execution_targets(self.runtime.graph()).len();
        let mut applied = ControlApplied {
            receipt: OperationReceipt {
                operation,
                target,
                node: target_node,
                detail,
                requested,
                started: 0,
                stale: 0,
                skipped: 0,
                removed: 0,
                unbound: 0,
            },
            effects: vec![],
            removed: vec![],
            unbound: vec![],
            diagnostics: prepared.diagnostics,
        };
        applied.effects = match prepared.operation {
            Control::Refresh(node) => self.runtime.refresh(&node, now)?,
            Control::RefreshDownstream(node, span) => {
                // Capture and validate at the serialized apply boundary, never when the
                // command is parsed. No mutation or execution precedes the complete scan.
                let closure = self
                    .runtime
                    .graph()
                    .downstream(&node)
                    .map_err(crate::runtime::RuntimeError::from)?;
                for id in &closure {
                    let selected = self.runtime.graph().node(id).expect("closure member");
                    let unsupported = |id: &NodeId| {
                        self.runtime.graph().node(id).is_some_and(|entry| {
                            entry
                                .payload()
                                .call()
                                .is_some_and(|call| call.streaming() || call.interactive())
                        })
                    };
                    if unsupported(id) || selected.dependencies().keys().any(unsupported) {
                        return Err(rejected(
                            "MET015",
                            span,
                            "downstream refresh requires finite non-interactive work and finite selected inputs",
                        ));
                    }
                    // Calculation calls were captured through Providers::bind_finite
                    // with interactive=false; they cannot hide either unsupported port.
                }
                self.runtime.refresh_downstream(&node, now)?
            }
            Control::Cancel(node) => self.cancel(&node, now),
            Control::Timeout(node, budget) => self.runtime.set_timeout(&node, budget)?,
            Control::Policy(node, policy) => {
                if let Some(node) = node {
                    self.runtime.set_policy(&node, policy)?;
                } else {
                    self.runtime.set_default_policy(policy);
                }
                vec![]
            }
            Control::DropName(name) => {
                self.bindings.unbind(&name);
                applied.unbound.push(name);
                vec![]
            }
            Control::DropNode(node) => {
                let before = self.bindings.names().keys().cloned().collect::<Vec<_>>();
                let (removed, effects) = self.remove_node(&node, installation)?;
                applied.removed = removed;
                applied.unbound = before
                    .into_iter()
                    .filter(|name| !self.bindings.names().contains_key(name))
                    .collect();
                effects
            }
            Control::Change { node, call, typing } => {
                let traits = call.traits();
                let effects = match installation {
                    super::Installation::Live => {
                        self.runtime
                            .replace_payload(&node, BoundTask::Call(call), traits, now)?
                    }
                    super::Installation::Held => {
                        self.runtime
                            .replace_held_payload(&node, BoundTask::Call(call), traits)?;
                        vec![]
                    }
                };
                self.predictions.insert(node, typing);
                effects
            }
        };
        let mut states = std::collections::BTreeMap::new();
        for effect in &applied.effects {
            match effect {
                Effect::Spawn(_) => applied.receipt.started += 1,
                Effect::Observe(observation) => {
                    states.insert(observation.node.clone(), observation.state);
                }
                _ => {}
            }
        }
        applied.receipt.stale = states
            .values()
            .filter(|state| **state == crate::graph::NodeState::Stale)
            .count();
        applied.receipt.skipped = states
            .values()
            .filter(|state| **state == crate::graph::NodeState::Skipped)
            .count();
        applied.receipt.removed = applied.removed.len();
        applied.receipt.unbound = applied.unbound.len();
        if let Some(next) = next {
            self.revision = next;
        }
        Ok(applied)
    }
}
pub(super) fn bind_control<'a>(
    prepared: &PreparedMeta,
    graph: &crate::graph::DependencyGraph<BoundTask>,
    bindings: &crate::bindings::Bindings,
    typing: impl Fn(&NodeId) -> Option<&'a Typing>,
) -> Result<Control, WorkspaceError> {
    let span = prepared.span;
    let Plan::Action { task, targets } = &prepared.plan else {
        return Err(rejected(
            "MET007",
            span,
            "a value-producing command is not an immediate control",
        ));
    };
    let target = || {
        targets
            .first()
            .cloned()
            .filter(|_| targets.len() == 1)
            .ok_or_else(|| {
                rejected(
                    "MET007",
                    span,
                    "this control requires one existing node reference",
                )
            })
    };
    match task.spec.command {
        MetaCommand::Refresh => match task.inputs.get("scope") {
            None => Ok(Control::Refresh(target()?)),
            Some(input) => match literal_text(input).and_then(RefreshScope::lookup) {
                Some(RefreshScope::Downstream) => Ok(Control::RefreshDownstream(target()?, span)),
                None => Err(rejected(
                    "MET015",
                    span,
                    "refresh scope must be the literal downstream",
                )),
            },
        },
        MetaCommand::Cancel => Ok(Control::Cancel(target()?)),
        MetaCommand::Timeout => {
            let invalid = || {
                rejected(
                    "MET013",
                    span,
                    "timeout must be a positive, representable literal duration",
                )
            };
            let Some(Input::Literal(value)) = task.inputs.get("after") else {
                return Err(invalid());
            };
            let Data::Duration(value) = value.data() else {
                return Err(invalid());
            };
            let parts = value.parts();
            let seconds = u64::try_from(parts.seconds()).map_err(|_| invalid())?;
            let budget = Duration::new(seconds, parts.nanos());
            valid_timeout(budget).map_err(|_| invalid())?;
            Ok(Control::Timeout(target()?, budget))
        }
        MetaCommand::Policy => {
            let policy = match task.inputs.get("mode").and_then(literal_text) {
                Some("automatic") => Policy::Automatic,
                Some("manual") => Policy::Manual,
                Some("reactive") => Policy::Reactive,
                _ => {
                    return Err(
                        Diagnostic::error("MET005", span, "unknown execution policy")
                            .with_hint("allowed: automatic, manual, reactive")
                            .into(),
                    );
                }
            };
            let node = if task.subjects.is_empty() {
                None
            } else {
                Some(target()?)
            };
            Ok(Control::Policy(node, policy))
        }
        MetaCommand::Drop => {
            if !task.subjects.is_empty() && !task.tail.is_empty() {
                return Err(rejected(
                    "MET007",
                    span,
                    "specify either a binding name or a node reference, not both",
                ));
            }
            if !targets.is_empty() {
                return Ok(Control::DropNode(target()?));
            }
            let Some(name) = task.tail.first().filter(|_| task.tail.len() == 1) else {
                return Err(rejected(
                    "MET007",
                    span,
                    "drop requires a binding name or node reference",
                ));
            };
            if !bindings.names().contains_key(name) {
                return Err(rejected(
                    "MET007",
                    span,
                    format!("there is no binding named '{name}'"),
                ));
            }
            Ok(Control::DropName(name.clone()))
        }
        MetaCommand::Change => {
            let node = target()?;
            let existing = graph
                .node(&node)
                .ok_or_else(|| rejected("MET004", span, "the target call no longer exists"))?;
            let old = existing
                .payload()
                .call()
                .ok_or_else(|| rejected("MET004", span, "the target is not a provider call"))?;
            let invocation = old.invocation();
            let mut inputs = invocation.inputs.clone();
            for (name, input) in &task.inputs {
                let mut input = input.clone();
                if let Input::Literal(value) = &mut input {
                    let expected = invocation
                        .capability
                        .parameter(name)
                        .map_or(&Shape::Unknown, |parameter| &parameter.shape);
                    // The checker's :change analysis uses the original signature. Planning the
                    // open meta arguments must use that same signature, not silently keep Text.
                    if let Some(contracts) = invocation.guards.get(name) {
                        *value = boundary::literal(name, contracts, expected, value)
                            .map_err(|error| boundary_failure(error, span))?;
                    } else if let Data::Text(text) = value.data() {
                        let data = literals::read(text, expected).ok_or_else(|| {
                            rejected(
                                "PLN001",
                                span,
                                "changed literal does not satisfy the original parameter shape",
                            )
                        })?;
                        *value = Value::new(expected.clone(), data, value.provenance().clone())
                            .expect("contextual literal matches expected shape");
                    }
                }
                inputs.insert(name.clone(), input);
            }
            for input in inputs.values() {
                for output in input.dependencies() {
                    if existing.dependencies().get(&output.node) != Some(&output.port) {
                        return Err(rejected(
                            "MET014",
                            span,
                            "changing a dependency or its selected output requires a new command",
                        ));
                    }
                }
            }
            let call = old.clone().with_inputs(inputs);
            let typing = Arc::new(call.invocation().predicted_typing(typing));
            Ok(Control::Change { node, call, typing })
        }
        _ => Err(rejected(
            "MET007",
            span,
            "this command requires another execution port",
        )),
    }
}

fn literal_text(input: &Input) -> Option<&str> {
    if let Input::Literal(value) = input
        && let Data::Text(text) = value.data()
    {
        Some(text)
    } else {
        None
    }
}
fn boundary_failure(error: BoundaryError, span: Span) -> WorkspaceError {
    let message = error.to_string();
    let (code, issues) = match error {
        BoundaryError::Cancelled(_) => return WorkspaceError::Cancelled,
        BoundaryError::Invalid { issues, .. } => ("TYP005", issues),
        BoundaryError::Declaration(error) => (error.code, vec![]),
    };
    WorkspaceError::Rejected {
        diagnostics: vec![Diagnostic::error(code, span, message)],
        issues,
    }
}
