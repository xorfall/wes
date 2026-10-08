//! Bounded read-only node/name captures; this module never owns a runtime or reads result bodies.
use super::{Budget, Failure, fields, max_bytes, max_rows, max_work};
use crate::{
    graph::{NodeId, NodeState, OutputPort},
    tasks::BoundTask,
    workspace::Workspace,
};
use indexmap::IndexMap;
use std::sync::Arc;
use wes_core::{
    Data, ErrorValue,
    capability::{Capability, ProviderDescription, Typing},
};
use wes_language::vocabulary::MetaCommand;

#[derive(Clone, Debug)]
enum TaskLabel {
    Input,
    Call(Arc<ProviderDescription>, Arc<Capability>),
    Meta(MetaCommand),
    Calculation(IndexMap<String, Data>),
}
#[derive(Clone, Debug)]
pub(super) struct NodeDescription {
    captured_bindings: Vec<String>,
    stale_reason: Option<crate::runtime::StaleReason>,
    update_pending: bool,
    dependency_lifetime: crate::runtime::DependencyLifetime,
    construction_complete: bool,
    id: NodeId,
    names: Vec<String>,
    state: NodeState,
    policy: crate::runtime::Policy,
    traits: crate::runtime::ExecutionTraits,
    task: TaskLabel,
    dependencies: Vec<NodeId>,
    typing: Option<Arc<Typing>>,
    failure: Option<String>,
    view: Option<Data>,
    progress: Option<crate::driver::progress::ExecutionProgress>,
}
#[derive(Clone, Debug)]
pub(super) struct BoundName {
    name: String,
    node: NodeId,
    port: OutputPort,
    state: NodeState,
    typing: Option<Arc<Typing>>,
}
struct Charge {
    bytes: usize,
    work: usize,
}
impl Charge {
    fn work(&mut self, count: usize) -> Result<(), Failure> {
        self.work = self.work.checked_add(count).ok_or(Failure::Limit)?;
        if self.work > max_work() {
            return Err(Failure::Limit);
        }
        Ok(())
    }
    fn copy(&mut self, text: &str) -> Result<String, Failure> {
        self.work(1)?;
        self.bytes = self.bytes.checked_add(text.len()).ok_or(Failure::Limit)?;
        if self.bytes > max_bytes() {
            return Err(Failure::Limit);
        }
        Ok(text.into())
    }
}

pub(super) fn capture_nodes(
    workspace: &Workspace,
    selected: Option<&NodeId>,
) -> Result<Vec<NodeDescription>, Failure> {
    let graph = workspace.runtime().graph();
    if (selected.is_none() && graph.len() > max_rows())
        || workspace.bindings().names().len() > max_rows()
    {
        return Err(Failure::Limit);
    }
    if let Some(node) = selected.filter(|node| graph.node(node).is_none()) {
        return Err(Failure::missing("node in this workspace", node.to_string()));
    }
    let mut charge = Charge { bytes: 0, work: 0 };
    let mut names = IndexMap::<NodeId, Vec<String>>::new();
    // Index once instead of rescanning all bindings for each node.
    for (name, output) in workspace.bindings().names() {
        charge.work(1)?;
        if selected.is_none_or(|node| node == &output.node) {
            names
                .entry(output.node.clone())
                .or_default()
                .push(charge.copy(name)?);
        }
    }
    let mut rows = vec![];
    let mut capture = |node: &crate::graph::Node<BoundTask>| -> Result<(), Failure> {
        charge.work(1)?;
        charge.work(node.dependencies().len())?;
        let task = match node.payload() {
            BoundTask::SourceLaunch(launch) => TaskLabel::Call(
                launch.source.invocation().provider.clone(),
                launch.source.invocation().capability.clone(),
            ),
            BoundTask::Input(_) => TaskLabel::Input,
            BoundTask::Call(call) => TaskLabel::Call(
                call.invocation().provider.clone(),
                call.invocation().capability.clone(),
            ),
            BoundTask::Calculation(calc) => {
                let mut metadata = crate::calc::describe_purity(&calc.compiled);
                if let Some(definition) = calc.definition() {
                    metadata.insert(
                        "definitionRevision".into(),
                        Data::Text(definition.revision.as_str().into()),
                    );
                    metadata.insert(
                        "outputType".into(),
                        Data::Text(definition.output.name().into()),
                    );
                }
                TaskLabel::Calculation(metadata)
            }
            BoundTask::Stream(_) => TaskLabel::Calculation(
                [("execution".into(), Data::Text("native-stream".into()))].into(),
            ),
            BoundTask::Describe(_) => TaskLabel::Meta(MetaCommand::Describe),
            BoundTask::TypeCheck(_) => TaskLabel::Meta(MetaCommand::Type),
            BoundTask::Accumulation(_) => TaskLabel::Meta(MetaCommand::Accumulate),
            BoundTask::Scan(_) => TaskLabel::Meta(MetaCommand::Scan),
            BoundTask::ScanAttempt(attempt) => TaskLabel::Meta(attempt.command()),
            BoundTask::ScanContinuation(_) => TaskLabel::Meta(MetaCommand::ScanContinuation),
            BoundTask::ScanExcerpt(_) => TaskLabel::Meta(MetaCommand::ScanExcerpt),
            BoundTask::Reconcile(reconcile) => TaskLabel::Meta(reconcile.command()),
            BoundTask::Dataset(read) => TaskLabel::Meta(read.command()),
            BoundTask::Recording(recording) => TaskLabel::Meta(recording.command()),
            BoundTask::Help(_) => TaskLabel::Meta(MetaCommand::Help),
            BoundTask::Management(_) => {
                TaskLabel::Meta(wes_language::vocabulary::MetaCommand::WorkspacePlan)
            }
            BoundTask::Query(query) => TaskLabel::Meta(query.task.spec.command),
            BoundTask::ImportPlan(_) => TaskLabel::Meta(MetaCommand::ImportPlan),
            BoundTask::View(view) => TaskLabel::Meta(view.command()),
        };
        let captured_bindings = workspace
            .captured_bindings(node.payload())
            .into_iter()
            .map(|text| charge.copy(&text))
            .collect::<Result<_, _>>()?;
        rows.push(NodeDescription {
            progress: workspace.runtime().execution_progress(node.id()).cloned(),
            captured_bindings,
            view: if selected.is_some() {
                workspace
                    .runtime()
                    .value_of(node.id())
                    .and_then(|value| workspace.views.inspect(value).ok())
            } else {
                None
            },
            stale_reason: workspace.runtime().stale_reason(node.id()),
            update_pending: workspace.runtime().input_update_pending(node.id()),
            dependency_lifetime: workspace
                .runtime()
                .dependency_lifetime(node.id())
                .expect("captured node"),
            construction_complete: workspace.runtime().construction_complete(node.id()),
            id: node.id().clone(),
            names: names.swap_remove(node.id()).unwrap_or_default(),
            state: node.state(),
            task,
            policy: workspace
                .runtime()
                .policy_of(node.id())
                .expect("captured graph node"),
            traits: node.payload().traits(),
            dependencies: node.dependencies().keys().cloned().collect(),
            typing: workspace.data_typing_handle(node.id()).cloned(),
            failure: workspace
                .runtime()
                .error_of(node.id())
                .map(|error| {
                    charge.copy(
                        if error.policy().is_private() || error.policy().is_unknown() {
                            "Restricted failure; details are unavailable."
                        } else {
                            error.message()
                        },
                    )
                })
                .transpose()?,
        });
        Ok(())
    };
    if let Some(node) = selected {
        capture(
            graph
                .node(node)
                .ok_or_else(|| Failure::missing("node in this workspace", node.to_string()))?,
        )?;
    } else {
        for node in graph.nodes() {
            capture(node)?;
        }
    }
    Ok(rows)
}
pub(super) fn capture_names(workspace: &Workspace) -> Result<Vec<BoundName>, Failure> {
    let names = workspace.bindings().names();
    if names.len() > max_rows() {
        return Err(Failure::Limit);
    }
    let mut charge = Charge { bytes: 0, work: 0 };
    let mut rows = vec![];
    let error_typing = Arc::new(Typing::new(ErrorValue::shape()));
    let cancel_typing = Arc::new(Typing::new(ErrorValue::cancellation_shape()));
    for (name, output) in names {
        let node = workspace
            .runtime()
            .graph()
            .node(&output.node)
            .ok_or_else(|| Failure::missing("node for bound name", name))?;
        let typing = match output.port {
            OutputPort::Data => workspace.data_typing_handle(node.id()).cloned(),
            OutputPort::Error => Some(error_typing.clone()),
            OutputPort::Cancel => Some(cancel_typing.clone()),
        };
        rows.push(BoundName {
            name: charge.copy(name)?,
            node: node.id().clone(),
            port: output.port,
            state: node.state(),
            typing,
        });
    }
    Ok(rows)
}
impl NodeDescription {
    pub(super) fn describe(&self, budget: &mut Budget<'_>) -> Result<Data, Failure> {
        budget.step()?;
        let mut task = String::new();
        match &self.task {
            TaskLabel::Input => budget.append(&mut task, "input")?,
            TaskLabel::Call(provider, capability) => {
                budget.append(&mut task, provider.name())?;
                budget.append(&mut task, " ")?;
                budget.joined(capability.path.iter().map(String::as_str), " ", &mut task)?;
            }
            TaskLabel::Calculation(_) => budget.append(&mut task, ":calc")?,
            TaskLabel::Meta(command) => {
                budget.append(&mut task, ":")?;
                budget.append(&mut task, command.name())?;
            }
        }
        let Data::Record(mut fields) = fields([
            ("id", budget.text(self.id.as_str())?),
            (
                "names",
                Data::List(
                    self.names
                        .iter()
                        .map(|name| budget.text(name))
                        .collect::<Result<_, _>>()?,
                ),
            ),
            ("state", budget.text(state(self.state))?),
            ("policy", budget.text(self.policy.as_str())?),
            ("pure", Data::Bool(self.traits.pure)),
            ("bounded", Data::Bool(self.traits.bounded)),
            ("task", Data::Text(task.into())),
            (
                "dependsOn",
                Data::List(
                    self.dependencies
                        .iter()
                        .map(|node| budget.text(node.as_str()))
                        .collect::<Result<_, _>>()?,
                ),
            ),
        ]) else {
            unreachable!()
        };
        if !self.captured_bindings.is_empty() {
            fields.insert(
                "capturedBindings".into(),
                Data::List(
                    self.captured_bindings
                        .iter()
                        .map(|binding| budget.text(binding))
                        .collect::<Result<_, _>>()?,
                ),
            );
        }
        fields.insert("updatePending".into(), Data::Bool(self.update_pending));
        fields.insert(
            "dependencyLifetime".into(),
            budget.text(self.dependency_lifetime.name())?,
        );
        fields.insert(
            "constructionComplete".into(),
            Data::Bool(self.construction_complete),
        );
        if let Some(reason) = self.stale_reason {
            fields.insert(
                "staleReason".into(),
                super::fields([
                    ("code", budget.text(reason.code())?),
                    ("message", budget.text(reason.message())?),
                ]),
            );
        }
        if let TaskLabel::Calculation(metadata) = &self.task {
            for (key, value) in metadata {
                let value = match value {
                    Data::Text(text) => budget.text(text)?,
                    Data::Bool(b) => Data::Bool(*b),
                    _ => unreachable!(),
                };
                fields.insert(key.clone(), value);
            }
        }
        if let Some(typing) = &self.typing {
            fields.insert("type".into(), budget.shape(&typing.shape)?);
            let mut facts = IndexMap::new();
            for (key, value) in typing.provenance.facts() {
                let Data::Text(key) = budget.text(key)? else {
                    unreachable!()
                };
                facts.insert(key.to_string(), budget.text(value)?);
            }
            fields.insert("provenance".into(), Data::Record(facts));
        }
        if let Some(failure) = &self.failure {
            fields.insert("failure".into(), budget.text(failure)?);
        }
        if let Some(view) = &self.view {
            let charge = crate::value_size::data_charge(
                view,
                max_bytes().saturating_sub(budget.bytes) as u64,
            )
            .ok_or(Failure::Limit)?;
            budget.bytes += charge as usize;
            fields.insert("view".into(), view.clone());
        }
        if let Some(progress) = &self.progress {
            let data = progress.description();
            let charge = crate::value_size::data_charge(
                &data,
                max_bytes().saturating_sub(budget.bytes) as u64,
            )
            .ok_or(Failure::Limit)?;
            budget.bytes += charge as usize;
            fields.insert("progress".into(), data);
        }
        Ok(Data::Record(fields))
    }
}
impl BoundName {
    pub(super) fn describe(&self, budget: &mut Budget<'_>) -> Result<Data, Failure> {
        budget.step()?;
        let mut name = String::new();
        budget.append(&mut name, "$")?;
        budget.append(&mut name, &self.name)?;
        let Data::Record(mut fields) = fields([
            ("name", Data::Text(name.into())),
            ("node", budget.text(self.node.as_str())?),
        ]) else {
            unreachable!()
        };
        if let Some(typing) = &self.typing {
            fields.insert("type".into(), budget.shape(&typing.shape)?);
        }
        fields.insert("state".into(), budget.text(state(self.state))?);
        fields.insert("output".into(), budget.text(self.port.selector())?);
        Ok(Data::Record(fields))
    }
}
fn state(state: NodeState) -> &'static str {
    match state {
        NodeState::Pending => "PENDING",
        NodeState::Running => "RUNNING",
        NodeState::Ready => "READY",
        NodeState::Stale => "STALE",
        NodeState::Failed => "FAILED",
        NodeState::Cancelled => "CANCELLED",
        NodeState::Skipped => "SKIPPED",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::workspace::Preparation;
    use wes_language::{SourceText, parse};
    fn commit(workspace: &mut Workspace, text: &str) {
        let parsed = parse(&SourceText::new("test", text));
        let Preparation::Change(change) = workspace.prepare(&parsed.script.statements[0]).unwrap()
        else {
            panic!("declaration")
        };
        workspace.commit(change).unwrap();
    }
    #[test]
    fn capture_row_limits_do_not_start_work_and_single_node_inspection_remains_available() {
        let mut workspace = Workspace::local(crate::providers::LocalScope::new("fixture").unwrap());
        for _ in 0..=max_rows() {
            commit(&mut workspace, ":help");
        }
        assert!(matches!(
            capture_nodes(&workspace, None),
            Err(Failure::Limit)
        ));
        let one = NodeId::new("id1000").unwrap();
        assert_eq!(capture_nodes(&workspace, Some(&one)).unwrap().len(), 1);
        assert!(workspace.runtime().is_idle());
        assert!(
            workspace
                .runtime()
                .graph()
                .nodes()
                .all(|node| node.state() == NodeState::Pending)
        );
    }
    #[test]
    fn snapshot_copy_budget_checks_aggregate_bytes_and_work() {
        let mut charge = Charge {
            bytes: max_bytes() - 1,
            work: 0,
        };
        assert_eq!(charge.copy("x").unwrap(), "x");
        assert!(matches!(charge.copy("y"), Err(Failure::Limit)));
        let mut charge = Charge {
            bytes: 0,
            work: max_work() - 1,
        };
        charge.work(1).unwrap();
        assert!(matches!(charge.copy("unused"), Err(Failure::Limit)));
        assert_eq!(charge.bytes, 0);
    }
}
