//! Process-local bindings retain weak ports of exact original runs/connections.
use super::*;
use crate::{
    runtime::{OutputState, RuntimeCode},
    tasks::source_control::{BoundSourceControl, Captured},
};
use std::time::Duration;
use wes_core::{Data, MetaType, Shape, Value};
use wes_language::vocabulary::MetaCommand;
struct Binding {
    port: crate::streams::LiveControls,
    node: NodeId,
    run: crate::runtime::RunId,
    principal: crate::environments::InvocationAuthority,
}
#[derive(Default)]
pub(super) struct Bindings {
    entries: IndexMap<String, Binding>,
}
impl Bindings {
    pub(super) fn clear(&mut self) {
        self.entries.clear();
    }
}
impl Actor {
    pub(super) fn capture_source_control(
        &mut self,
        task: &mut BoundSourceControl,
        run: &crate::runtime::Run,
        prepared: &IndexMap<NodeId, Value>,
    ) {
        let principal = self
            .execution_principals
            .get(run.node())
            .map(|(p, _)| p.clone())
            .unwrap_or_default();
        let result = (|| {
            let denied = |message| RuntimeCode::ExecutionFailed.error(message, None);
            if !principal.is_local_user() {
                return Err(denied(
                    "External source controls require an explicit native user submission; acquisition/read grants are not write authority",
                ));
            }
            self.source_controls.entries.retain(|_, binding| {
                self.workspace.runtime().run_of(&binding.node) == Some(&binding.run)
                    && binding.port.description().is_ok()
            });
            let provenance = self
                .workspace
                .typing(&task.subject)
                .map_or_else(wes_core::Provenance::default, |t| t.provenance);
            task.policy = provenance.policy().clone();
            if task.task.spec.command == MetaCommand::SourceBind {
                let (port,provenance)=self.io.source_controls(self.workspace.runtime(),&task.subject.node,principal.clone()).map_err(|_|denied("Binding requires a currently open owned source with an installed typed control port; no source was acquired"))?;
                task.policy = provenance.policy().clone();
                let (source, epoch, input, output) = port
                    .description()
                    .map_err(|_| denied("Source connection changed before control binding"))?;
                if self.source_controls.entries.len()
                    >= wes_budgets::get("source.controls.bindings") as usize
                {
                    return Err(denied("Live source binding capacity is exhausted"));
                }
                let projection = Data::Record(
                    [
                        (
                            "source".into(),
                            Data::Text(source.node().to_string().into()),
                        ),
                        ("run".into(), Data::Text(source.id().to_string().into())),
                        ("connection".into(), Data::Text(epoch.to_string().into())),
                        ("input".into(), Data::Text(input.name().into())),
                        ("inputDigest".into(), Data::Text(input.digest().into())),
                        ("output".into(), Data::Text(output.name().into())),
                        ("outputDigest".into(), Data::Text(output.digest().into())),
                        ("restorable".into(), Data::Bool(false)),
                    ]
                    .into_iter()
                    .collect(),
                );
                let token = uuid::Uuid::new_v4().to_string();
                let value = Value::management(MetaType::SourceControls, projection, token.clone())
                    .with_provenance(provenance);
                self.source_controls.entries.insert(
                    token,
                    Binding {
                        port: port.clone(),
                        node: run.node().clone(),
                        run: run.id().clone(),
                        principal,
                    },
                );
                return Ok(Captured::Binding { value, port });
            }
            let OutputState::Available(receiver) = self.workspace.runtime().output(&task.subject)
            else {
                return Err(denied(
                    "SourceControls binding is no longer resident; bind again explicitly",
                ));
            };
            if receiver.shape() != &Shape::Meta(MetaType::SourceControls) {
                return Err(denied(
                    "Source Send requires an original live SourceControls binding",
                ));
            }
            let binding=receiver.management_authority().and_then(|key|self.source_controls.entries.get(key)).ok_or_else(||denied("Stored/restored SourceControls have no live authority; bind the current source explicitly"))?;
            if binding.principal != principal
                || self.workspace.runtime().run_of(&binding.node) != Some(&binding.run)
            {
                return Err(denied(
                    "SourceControls binding ownership or producing Run changed",
                ));
            }
            let mut values = IndexMap::new();
            for source in task.read_sources() {
                let value = if let Some(value) = prepared.get(&source.node) {
                    value.clone()
                } else {
                    let OutputState::Available(value) = self.workspace.runtime().output(&source)
                    else {
                        return Err(denied(
                            "Source control input must already be completed and readable; no producer was started",
                        ));
                    };
                    value
                };
                task.policy = task.policy.join(value.provenance().policy());
                values.insert(source.node, value);
            }
            let action = task.task.inputs["action"]
                .resolve_with_limit(&values, 64 * 1024)
                .map_err(|e| e.error(RuntimeCode::InputFailed, "action"))?
                .into_owned();
            for (key, checks) in &task.task.guards {
                let value = task
                    .task
                    .inputs
                    .get(key)
                    .ok_or_else(|| denied("Source control template argument is missing"))?
                    .resolve_with_limit(&values, 64 * 1024)
                    .map_err(|e| e.error(RuntimeCode::InputFailed, key))?;
                let expected = task
                    .task
                    .spec
                    .parameter(key)
                    .map_or(&Shape::Unknown, |p| &p.shape);
                wes_core::contracts::boundary::require(key, checks, expected, &value, &|| false)
                    .map_err(|_| denied("Source control template argument violates its declared contract; nothing was dispatched"))?;
            }
            if crate::value_size::value_charge(&action, 64 * 1024).is_none() {
                return Err(denied(
                    "Source control action exceeds its captured byte budget; nothing was dispatched",
                ));
            }
            let (_, _, contract, _) = binding.port.description().map_err(|_| {
                denied("Original source Run/connection is closed; bind the new source explicitly")
            })?;
            let action = if matches!(task.task.inputs["action"], crate::plan::Input::Literal(_)) {
                wes_core::contracts::boundary::literal(
                    "action",
                    std::slice::from_ref(&contract),
                    &contract.shape(),
                    &action,
                )
            } else {
                wes_core::contracts::boundary::require(
                    "action",
                    std::slice::from_ref(&contract),
                    &contract.shape(),
                    &action,
                    &|| false,
                )
            }
            .map_err(|_| {
                denied(
                    "Source control action violates its captured contract; nothing was dispatched",
                )
            })?;
            let budget = match task.task.inputs.get("budget") {
                None => Duration::from_secs(5),
                Some(input) => match input
                    .resolve(&values)
                    .map_err(|e| e.error(RuntimeCode::InputFailed, "budget"))?
                    .data()
                {
                    Data::Duration(ns)
                        if ns.total_nanos() > 0 && ns.total_nanos() <= 60_000_000_000 =>
                    {
                        Duration::from_nanos(ns.total_nanos() as u64)
                    }
                    _ => {
                        return Err(denied(
                            "Source control budget must be greater than zero and at most 60 seconds",
                        ));
                    }
                },
            };
            binding.port.description().map_err(|_| {
                denied("Original source Run/connection is closed; bind the new source explicitly")
            })?;
            Ok(Captured::Action {
                port: binding.port.clone(),
                action: action
                    .with_provenance(action.provenance().clone().with_policy(&task.policy)),
                budget,
            })
        })();
        task.captured = Some(result.map_err(|error| error.with_policy(&task.policy)));
    }
}
