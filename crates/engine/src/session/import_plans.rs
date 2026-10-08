//! Process-local frozen input authority; successful import evidence is a separate journal fact.
use super::*;
use crate::{
    imports::{ImportRequest, Importer},
    plan::Input,
    runtime::{Outcome, RunId, RuntimeCode},
    tasks::import_plan::BoundImportPlan,
};
use std::time::Duration;
use wes_core::{Data, MetaType, Shape, Value};
#[derive(Clone)]
struct Origin {
    argument: String,
    input: Input,
    run: RunId,
    value: Value,
}
#[derive(Clone)]
pub(crate) struct FrozenImport {
    pub(crate) request: ImportRequest,
    importer: Arc<dyn Importer>,
    origins: Vec<Origin>,
    principal: (crate::environments::InvocationAuthority, String),
    environment: Option<wes_core::environments::EnvironmentContext>,
}
struct Plan {
    frozen: FrozenImport,
    node: NodeId,
    run: RunId,
    until: Duration,
    charge: u64,
}
#[derive(Default)]
pub(super) struct Plans {
    entries: IndexMap<String, Plan>,
}
impl Plans {
    fn sweep(&mut self, workspace: &Workspace, now: Duration) {
        self.entries.retain(|_, plan| {
            plan.until > now
                && workspace.runtime().run_of(&plan.node) == Some(&plan.run)
                && !matches!(
                    workspace
                        .runtime()
                        .graph()
                        .node(&plan.node)
                        .map(|n| n.state()),
                    None | Some(
                        crate::graph::NodeState::Failed
                            | crate::graph::NodeState::Cancelled
                            | crate::graph::NodeState::Skipped
                    )
                )
        });
    }
    pub(super) fn clear(&mut self) {
        self.entries.clear();
    }
}
impl FrozenImport {
    pub(super) fn validate(&self, actor: &Actor, input: &SourceInput) -> Result<(), SessionError> {
        let denied = |message: &str| SessionError::Management(message.into());
        if self.principal
            != (
                crate::environments::InvocationAuthority::from_source(input),
                input.client().to_owned(),
            )
        {
            return Err(denied(
                "Import plan belongs to another principal or client. Plan again in this session.",
            ));
        }
        let context = input
            .environments()
            .cloned()
            .or_else(|| actor.environment_clients.get(input.client()).cloned())
            .or_else(|| actor.workspace.default_environment_context());
        if context != self.environment
            || context.as_ref().is_some_and(|c| {
                c.revisions.iter().any(|(name, revision)| {
                    actor
                        .workspace
                        .environments()
                        .inspect(name)
                        .is_none_or(|e| e.revision() != *revision)
                })
            })
        {
            return Err(denied(
                "Import plan environment changed. Plan again with the current environment.",
            ));
        }
        if !actor
            .workspace
            .importer_entry(self.request.kind())
            .is_some_and(|entry| Arc::ptr_eq(&entry, &self.importer))
        {
            return Err(denied("Importer changed after planning. Plan again."));
        }
        for origin in &self.origins {
            let output = origin.input.dependencies()[0];
            if actor.workspace.runtime().value_run(&output.node) != Some(&origin.run) {
                return Err(denied(
                    "An import argument has a new producing run. Plan again, even if its value looks unchanged.",
                ));
            }
            let crate::runtime::OutputState::Available(value) =
                actor.workspace.runtime().output(output)
            else {
                return Err(denied(
                    "An import argument is no longer available. No contents were read.",
                ));
            };
            let values = IndexMap::from([(output.node.clone(), value.clone())]);
            if !value.provenance().policy().is_empty()
                || !origin
                    .input
                    .resolve(&values)
                    .is_ok_and(|v| v.as_ref() == &origin.value)
            {
                return Err(denied(
                    "Import argument visibility or value changed. Plan again.",
                ));
            }
        }
        Ok(())
    }
}
impl Actor {
    pub(super) fn freeze_import(
        &mut self,
        task: &BoundImportPlan,
        run: &crate::runtime::Run,
        inputs: &IndexMap<NodeId, Value>,
    ) -> Outcome {
        let result = self.freeze_import_value(task, run, inputs);
        match result {
            Ok(value) => Outcome::Produced(value),
            Err(message) => Outcome::Failed(RuntimeCode::InputFailed.error(message, None)),
        }
    }
    fn freeze_import_value(
        &mut self,
        task: &BoundImportPlan,
        run: &crate::runtime::Run,
        inputs: &IndexMap<NodeId, Value>,
    ) -> Result<Value, &'static str> {
        let principal = self
            .execution_principals
            .get(run.node())
            .cloned()
            .ok_or("Import planning requires an authorized session")?;
        if task.environment.as_ref().is_some_and(|c| {
            c.revisions.iter().any(|(name, revision)| {
                self.workspace
                    .environments()
                    .inspect(name)
                    .is_none_or(|e| e.revision() != *revision)
            })
        }) {
            return Err(
                "Import planning environment changed; plan again with the current environment",
            );
        }
        let importer = self
            .workspace
            .importer_entry(&task.task.tail[0])
            .ok_or("Importer is unavailable")?;
        let values = crate::plan::resolve_arguments(&task.task.inputs, inputs)
            .map_err(|_| "Import arguments could not be resolved")?;
        let mut arguments = IndexMap::new();
        let mut alias = None;
        let mut origins = vec![];
        let mut charge = 1024u64
            + task
                .environment
                .as_ref()
                .map_or(0, |c| c.revisions.len() as u64 * 1024);
        for (key, value) in values {
            if !matches!(value.shape(), Shape::Primitive(_))
                || !value.data().is_inline()
                || !value.provenance().policy().is_empty()
            {
                return Err(
                    "Import plans accept public scalar values with no resource-origin restrictions; no contents were read",
                );
            }
            if !value.shape().is_assignable_to(&task.expected(&key)) {
                return Err(
                    "Import argument type does not match its registered parameter; existing values are never coerced",
                );
            }
            charge = charge
                .checked_add(key.len() as u64 * 6 + 128)
                .ok_or("Import argument capacity exceeded")?;
            charge = charge
                .checked_add(
                    crate::value_size::value_charge(&value, 128 * 1024)
                        .ok_or("Import arguments exceed their supported budget")?,
                )
                .ok_or("Import argument capacity exceeded")?;
            if let Some(input) = task.task.inputs.get(&key)
                && !input.dependencies().is_empty()
            {
                let output = input.dependencies()[0];
                if output.port != crate::graph::OutputPort::Data {
                    return Err("Import plan references must select data fields");
                }
                let input_run = self
                    .workspace
                    .runtime()
                    .value_run(&output.node)
                    .cloned()
                    .ok_or("Import argument has no producing run")?;
                if let Input::FieldPath { fields, .. } = input {
                    for field in fields {
                        charge = charge
                            .checked_add(field.len() as u64 * 6 + 128)
                            .ok_or("Import argument capacity exceeded")?;
                    }
                }
                origins.push(Origin {
                    argument: key.clone(),
                    input: input.clone(),
                    run: input_run,
                    value: value.clone(),
                });
                charge = charge
                    .checked_add(
                        crate::value_size::value_charge(&value, 128 * 1024)
                            .ok_or("Import argument capacity exceeded")?
                            + 1024,
                    )
                    .ok_or("Import argument capacity exceeded")?;
            }
            if key == "as" {
                let Data::Text(text) = value.data() else {
                    return Err("Import alias must be Text");
                };
                alias = Some(text.to_string());
            } else {
                arguments.insert(key, value);
            }
        }
        if task
            .parameters
            .iter()
            .any(|p| p.required && !arguments.contains_key(&p.name))
        {
            return Err("Import plan is missing a required argument");
        }
        let request = ImportRequest::new(task.task.tail[0].clone(), alias, arguments)
            .map_err(|_| "Import request exceeds its name or argument budget")?;
        let now = self.io.now();
        self.import_plans.sweep(&self.workspace, now);
        if self.import_plans.entries.len() >= wes_budgets::get("imports.plans") as usize
            || self
                .import_plans
                .entries
                .values()
                .map(|p| p.charge)
                .sum::<u64>()
                .checked_add(charge)
                .is_none_or(|n| n > wes_budgets::get("imports.plans.bytes"))
        {
            return Err("Live import plan capacity exceeded; use or retire existing plans");
        }
        let lifetime = Duration::from_millis(wes_budgets::get("imports.plans.ttl"));
        let expires = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_err(|_| "Import plan clock is unavailable")?
            .checked_add(lifetime)
            .ok_or("Import plan expiry is unavailable")?;
        let expires = wes_core::Timestamp::new(
            i64::try_from(expires.as_secs()).map_err(|_| "Import plan expiry is unavailable")?,
            expires.subsec_nanos(),
        )
        .map_err(|_| "Import plan expiry is unavailable")?;
        let token = uuid::Uuid::new_v4().to_string();
        let projection=Data::Record([
            ("notice".into(),Data::Text("Restored, expired or used plans cannot be applied. Planning read no contents. Authority is usable once in this session until expiry.".into())),
            ("expires".into(),Data::Instant(expires)),
            ("kind".into(),Data::Text(request.kind().into())),
            ("alias".into(),Data::Option(request.alias().map(|s|Box::new(Data::Text(s.into()))))),
            ("arguments".into(),Data::Record(request.arguments().iter().map(|(k,v)|(k.clone(),v.data().clone())).collect())),
            ("contentsReadWhenPlanned".into(),Data::Bool(false)),
            ("argumentTypes".into(),Data::Record(request.arguments().iter().map(|(k,v)|(k.clone(),Data::Text(v.shape().to_string().into()))).collect())),
            ("origins".into(),Data::List(origins.iter().map(|origin| {
                let output=origin.input.dependencies()[0];
                Data::Record([("argument".into(),Data::Text(origin.argument.as_str().into())),("node".into(),Data::Text(output.node.as_str().into())),("port".into(),Data::Text("data".into())),("run".into(),Data::Text(origin.run.as_str().into())),("fields".into(),Data::List(match &origin.input {Input::FieldPath{fields,..}=>fields.iter().map(|s|Data::Text(s.as_str().into())).collect(),_=>vec![]}))].into())
            }).collect())),
        ].into());
        self.import_plans.entries.insert(
            token.clone(),
            Plan {
                frozen: FrozenImport {
                    request,
                    importer,
                    origins,
                    principal,
                    environment: task.environment.clone(),
                },
                node: run.node().clone(),
                run: run.id().clone(),
                until: now + lifetime,
                charge,
            },
        );
        Ok(Value::management(MetaType::ImportPlan, projection, token))
    }
    pub(super) fn admit_import_apply(
        &mut self,
        source: &mut ParsedSource,
    ) -> Result<Option<FrozenImport>, SessionError> {
        let applies=source.statements().iter().filter(|s| matches!(&s.expression,wes_language::Expression::Call(call) if wes_language::vocabulary::commands::invocation(call).is_ok_and(|i|i.spec.command==MetaCommand::ImportApply))).count();
        if applies == 0 {
            return Ok(None);
        }
        if applies != 1
            || source.statements().len() != 1
            || !source.statements()[0].annotations.is_empty()
        {
            return Err(SessionError::Management(
                "Submit import apply as one independent, unannotated statement.".into(),
            ));
        }
        let wes_language::Expression::Call(call) = &source.statements()[0].expression else {
            unreachable!()
        };
        let invocation = wes_language::vocabulary::commands::invocation(call)
            .map_err(|_| SessionError::Management("Malformed import apply".into()))?;
        if invocation.call.arguments.iter().any(|a| a.key.text!="replace" || !matches!(&a.value,wes_language::Value::Word(n) if matches!(n.text.as_str(),"true"|"false"))) {return Err(SessionError::Management("Import replacement requires literal replace:true or false.".into()));}
        let [wes_language::Value::Reference(reference)] = invocation.call.operands.as_slice()
        else {
            return Err(SessionError::Management(
                "Import apply requires a live plan reference".into(),
            ));
        };
        self.import_plans.sweep(&self.workspace, self.io.now());
        let token=self.workspace.resolve(&reference.text).and_then(|output|match self.workspace.runtime().output(&output) {crate::runtime::OutputState::Available(value) if value.shape()==&Shape::Meta(MetaType::ImportPlan)=>value.management_authority().map(str::to_owned),_=>None}).ok_or_else(||SessionError::Management("Expected a live ImportPlan; restored plans have no apply authority. Plan again.".into()))?;
        let frozen = self
            .import_plans
            .entries
            .get(&token)
            .ok_or_else(|| {
                SessionError::Management(
                    "Import plan expired, was consumed or belongs to another session. Plan again."
                        .into(),
                )
            })?
            .frozen
            .clone();
        frozen.validate(self, source.input())?;
        if !self.cells.reserve(source.input().cell(), 4096) {
            return Err(SessionError::Capacity);
        }
        self.import_plans.entries.shift_remove(&token);
        source.apply_import = frozen.request.clone().into();
        Ok(Some(frozen))
    }
}
