//! Query configuration is durable; request generations and caller authority are live owner state.
use super::*;
use crate::source::SourceInput;
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum QueryTrigger {
    Manual,
    Commit,
}
impl QueryTrigger {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Manual => "manual",
            Self::Commit => "commit",
        }
    }
}
#[derive(Clone, Debug)]
pub struct QueryAdapter {
    pub template: String,
    pub revision: String,
}
#[derive(Clone, Debug)]
pub struct QueryBinding {
    pub environment: Option<wes_core::environments::EnvironmentContext>,
    pub source: Handle,
    pub port: String,
    pub template: String,
    pub template_revision: String,
    pub adapter: Option<QueryAdapter>,
    pub trigger: QueryTrigger,
}
#[derive(Debug, Default)]
pub(super) struct QueryRuntime {
    generation: u64,
    pending: bool,
    last_input: Option<String>,
    pending_input: Option<Value>,
    caller: Option<SourceInput>,
    origin: Option<crate::runtime::Run>,
}
impl QueryRuntime {
    pub(super) fn invalidated(&self) -> Result<Self, Error> {
        Ok(Self {
            generation: self.generation.checked_add(1).ok_or(Error::Capacity)?,
            ..Self::default()
        })
    }
}
#[derive(Clone)]
pub(crate) struct QueryRequest {
    pub target: Handle,
    pub generation: u64,
    pub binding: QueryBinding,
    pub input: Value,
    pub caller: SourceInput,
    pub origin: crate::runtime::Run,
}
impl Store {
    pub(crate) fn configure_query(
        &mut self,
        target: &Handle,
        binding: QueryBinding,
        revision: u64,
    ) -> Result<(), Error> {
        self.editable(target, revision)?;
        let source = self.instance(&binding.source)?;
        if !source
            .snapshot
            .definition
            .manifest
            .outputs
            .get(&binding.port)
            .is_some_and(|p| p.shared && p.mode == wes_views::Mode::State)
        {
            return Err(Error::Interaction);
        }
        if self
            .instances
            .values()
            .filter(|i| i.snapshot.query.is_some())
            .count()
            >= wes_budgets::get("view.queries") as usize
            && self.instance(target)?.snapshot.query.is_none()
        {
            return Err(Error::Capacity);
        }
        if self.binding_reaches(&target.id, &binding.source.id)
            || self.reaches(&target.id, &binding.source.id)
        {
            return Err(Error::Cycle);
        }
        let instance = self.instances.get_mut(&target.id).expect("checked target");
        instance.query_runtime.generation = instance
            .query_runtime
            .generation
            .checked_add(1)
            .ok_or(Error::Capacity)?;
        instance.query_runtime.pending = false;
        instance.query_runtime.caller = None;
        instance.query_runtime.last_input = None;
        instance.query_runtime.pending_input = None;
        instance.query_runtime.origin = None;
        instance.snapshot.query = Some(binding);
        instance.snapshot.query_running = false;
        instance.snapshot.observing = false;
        instance.input_observation_requested = false;
        instance.snapshot.input_problem = None;
        instance.snapshot.input = Some(Input {
            binding: InputBinding::Unlinked,
            value: None,
        });
        self.charged -= instance.charge;
        instance.charge = 0;
        instance.snapshot.revision += 1;
        instance.snapshot.input_revision = instance.snapshot.input_revision.saturating_add(1);
        Ok(())
    }
    pub(crate) fn apply_query(
        &mut self,
        target: &Handle,
        caller: SourceInput,
        origin: crate::runtime::Run,
    ) -> Result<(), Error> {
        let instance = self.instance(target)?;
        let binding = instance.snapshot.query.as_ref().ok_or(Error::Input)?;
        let value = self.output(&binding.source, &binding.port)?;
        let digest = persistence::input_digest(&value);
        let next = instance
            .query_runtime
            .generation
            .checked_add(1)
            .ok_or(Error::Capacity)?;
        let instance = self.instances.get_mut(&target.id).expect("checked target");
        instance.query_runtime = QueryRuntime {
            generation: next,
            pending: true,
            last_input: Some(digest),
            pending_input: Some(value),
            caller: Some(caller),
            origin: Some(origin),
        };
        instance.snapshot.observing = true;
        instance.snapshot.query_running = true;
        instance.snapshot.input_problem = None;
        instance.snapshot.input_revision = instance.snapshot.input_revision.saturating_add(1);
        Ok(())
    }
    pub(crate) fn update_query_intents(&mut self) {
        let changes = self
            .instances
            .values()
            .filter_map(|i| {
                let binding = i.snapshot.query.as_ref()?;
                if !i.snapshot.observing || binding.trigger != QueryTrigger::Commit {
                    return None;
                }
                let value = self.output(&binding.source, &binding.port).ok();
                let digest = value.as_ref().map(persistence::input_digest);
                (digest != i.query_runtime.last_input).then_some((
                    i.handle.id.clone(),
                    digest,
                    value,
                ))
            })
            .collect::<Vec<_>>();
        for (id, digest, value) in changes {
            let instance = self.instances.get_mut(&id).expect("captured instance");
            let Some(generation) = instance.query_runtime.generation.checked_add(1) else {
                instance.snapshot.observing = false;
                instance.snapshot.input_problem = Some("Query generation capacity reached".into());
                continue;
            };
            instance.query_runtime.generation = generation;
            instance.query_runtime.pending = digest.is_some();
            instance.query_runtime.last_input = digest;
            instance.query_runtime.pending_input = value;
            instance.snapshot.query_running = instance.query_runtime.pending;
            instance.snapshot.input_revision = instance.snapshot.input_revision.saturating_add(1);
            if !instance.query_runtime.pending {
                instance.snapshot.input_problem = Some("Query source output is unavailable".into());
                instance.snapshot.observing = false;
            }
        }
    }
    pub(crate) fn query_requests(&self) -> Vec<QueryRequest> {
        self.instances
            .values()
            .filter_map(|i| {
                if !i.snapshot.observing || !i.query_runtime.pending {
                    return None;
                }
                let binding = i.snapshot.query.clone()?;
                Some(QueryRequest {
                    target: i.handle.clone(),
                    generation: i.query_runtime.generation,
                    input: i.query_runtime.pending_input.clone()?,
                    binding,
                    caller: i.query_runtime.caller.clone()?,
                    origin: i.query_runtime.origin.clone()?,
                })
            })
            .collect()
    }
    pub(crate) fn current_query(&self, target: &Handle, generation: u64) -> bool {
        self.instance(target).is_ok_and(|i| {
            if !i.snapshot.observing || i.query_runtime.generation != generation {
                return false;
            }
            i.snapshot.query.as_ref().is_some_and(|q| {
                q.trigger == QueryTrigger::Manual
                    || self.output(&q.source, &q.port).is_ok_and(|v| {
                        i.query_runtime.last_input.as_ref() == Some(&persistence::input_digest(&v))
                    })
            })
        })
    }
    pub(crate) fn stop_query(&mut self, target: &Handle) {
        if self.instance(target).is_ok() {
            let i = self.instances.get_mut(&target.id).expect("checked target");
            i.snapshot.observing = false;
            i.snapshot.query_running = false;
            i.snapshot.input_revision = i.snapshot.input_revision.saturating_add(1);
            i.query_runtime.pending = false;
            i.query_runtime.caller = None;
            i.query_runtime.pending_input = None;
        }
    }
    pub(crate) fn cancel_origins(&mut self, nodes: &[NodeId]) {
        let targets: Vec<_> = self
            .instances
            .values()
            .filter(|i| {
                i.query_runtime
                    .origin
                    .as_ref()
                    .is_some_and(|run| nodes.contains(run.node()))
            })
            .map(|i| i.handle.clone())
            .collect();
        for target in targets {
            self.stop_query(&target);
        }
    }
    pub(crate) fn has_queries(&self) -> bool {
        self.instances
            .values()
            .any(|i| i.snapshot.query.is_some() && i.snapshot.observing)
    }
    pub(crate) fn stop_queries(&mut self) {
        for i in self
            .instances
            .values_mut()
            .filter(|i| i.snapshot.query.is_some())
        {
            i.snapshot.observing = false;
            i.snapshot.query_running = false;
            i.snapshot.input_revision = i.snapshot.input_revision.saturating_add(1);
            i.query_runtime.pending = false;
            i.query_runtime.caller = None;
            i.query_runtime.pending_input = None;
        }
    }
    pub(crate) fn query_started(&mut self, target: &Handle, generation: u64) {
        if self.current_query(target, generation) {
            let runtime = &mut self
                .instances
                .get_mut(&target.id)
                .expect("current query")
                .query_runtime;
            runtime.pending = false;
            runtime.pending_input = None;
        }
    }
    pub(crate) fn query_finished(
        &mut self,
        target: &Handle,
        generation: u64,
        problem: Option<String>,
    ) {
        if !self
            .instance(target)
            .is_ok_and(|i| i.query_runtime.generation == generation)
        {
            return;
        }
        let i = self.instances.get_mut(&target.id).expect("current query");
        i.snapshot.query_running = false;
        i.query_runtime.pending = false;
        if problem.is_some()
            || i.snapshot
                .query
                .as_ref()
                .is_some_and(|q| q.trigger == QueryTrigger::Manual)
        {
            i.snapshot.observing = false;
            i.query_runtime.caller = None;
            i.query_runtime.pending_input = None;
        }
        i.snapshot.input_problem = problem;
        i.snapshot.input_revision = i.snapshot.input_revision.saturating_add(1);
    }
}
impl crate::workspace::Workspace {
    pub(crate) fn validate_query_contracts(
        &self,
        binding: &QueryBinding,
        source: &wes_views::Package,
        target: &wes_views::Package,
    ) -> Result<(), String> {
        let revision = self.query_revision(&binding.template, binding.adapter.is_some())?;
        if revision != binding.template_revision {
            return Err("Query template changed; configure its binding again".into());
        }
        let parameter = self.templates().snapshot()[&binding.template]
            .contracts
            .get("input")
            .ok_or("Query input must have an explicit type")?;
        let output = source
            .manifest
            .outputs
            .get(&binding.port)
            .filter(|p| p.shared && p.mode == wes_views::Mode::State)
            .ok_or("Query source must be a declared shared state output")?;
        let source_contract = source
            .contracts
            .resolve(&output.r#type)
            .map_err(|e| e.to_string())?;
        let output = if let Some(adapter) = &binding.adapter {
            let definition = self.query_template(&adapter.template)?;
            if definition.revision != adapter.revision {
                return Err("Query adapter changed; configure its binding again".into());
            }
            definition.output.clone()
        } else {
            self.query_template(&binding.template)?.output.clone()
        };
        if !source_contract.is_subtype_of(parameter) || !output.is_subtype_of(&target.input()) {
            return Err(
                "Query input/output contracts do not match the source port and target view".into(),
            );
        }
        Ok(())
    }
    pub(crate) fn query_revision(&self, name: &str, live: bool) -> Result<String, String> {
        let definition = self
            .templates()
            .snapshot()
            .get(name)
            .filter(|t| {
                t.parameters.len() == 1
                    && t.parameters.contains("input")
                    && t.contracts.contains_key("input")
            })
            .ok_or("Query source requires exactly one typed input parameter")?;
        if live == definition.calculation.is_some() {
            return Err("Finite queries require a calc template; live queries require a provider template and typed adapter".into());
        }
        Ok(definition.revision())
    }
    pub(crate) fn query_template(
        &self,
        name: &str,
    ) -> Result<std::sync::Arc<wes_language::templates::CalculationDefinition>, String> {
        self.templates().snapshot().get(name).and_then(|t| t.calculation.clone())
            .filter(|t| t.compiled.parameters.len() == 1 && t.compiled.parameters.contains_key("input"))
            .ok_or_else(|| "A view query requires a typed calc template with exactly one input parameter and an explicit return type".into())
    }
}

impl crate::workspace::Workspace {
    pub(crate) fn query_access(&self, binding: &QueryBinding) -> Result<(), String> {
        if let Some(context) = &binding.environment {
            context.validate().map_err(|e| e.to_string())?;
            if let Some(name) = &context.selected {
                if self
                    .environments()
                    .inspect(name)
                    .is_none_or(|e| Some(&e.revision()) != context.revisions.get(name))
                {
                    return Err("Query environment changed; inspect it and configure the query binding again".into());
                }
            }
        }
        let source = self
            .views
            .read(&binding.source)
            .map_err(|e| e.to_string())?;
        self.view_interaction(binding.source.id(), &source.identity)?;
        if self.query_revision(&binding.template, binding.adapter.is_some())?
            != binding.template_revision
        {
            return Err("Query template changed; configure its binding again".into());
        }
        if let Some(adapter) = &binding.adapter {
            if self.query_template(&adapter.template)?.revision != adapter.revision {
                return Err("Query adapter changed; configure its binding again".into());
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn setup(trigger: QueryTrigger) -> (Store, Handle, Handle) {
        let package = Arc::new(wes_views::Package::parse(
            r#"{"name":"Selector","id":"selector","summary":"Typed selection","renderer":"View.tsx","input":"Input","outputs":{"selection":{"type":"Int","mode":"state","shared":true}},"interaction":{"protocol":"Selection","state":"Input","event":"Input","sharedFields":["selection"]}}"#,
            "types: {Input: {base: Record, fields: {selection: Int}}}",
        ).unwrap());
        let mut store = Store::new([package.clone()], Limits::default()).unwrap();
        let source = store
            .create(
                NodeId::new("source").unwrap(),
                "Selector",
                &package.digest,
                None,
            )
            .unwrap();
        let target = store
            .create(
                NodeId::new("target").unwrap(),
                "Selector",
                &package.digest,
                None,
            )
            .unwrap();
        store
            .configure_query(
                &target,
                QueryBinding {
                    environment: None,
                    source: source.clone(),
                    port: "selection".into(),
                    template: "Observe".into(),
                    template_revision: "revision".into(),
                    adapter: None,
                    trigger,
                },
                0,
            )
            .unwrap();
        select(&mut store, &source, 1);
        (store, source, target)
    }
    fn select(store: &mut Store, source: &Handle, n: i64) {
        let (owner, state) = store.interaction(source).unwrap();
        store
            .commit_interaction(
                source,
                InteractionEdit {
                    events: vec![],
                    owner: owner.id,
                    identity: owner.identity.to_string(),
                    definition_revision: owner.revision,
                    revision: state.revision,
                    fields: [("selection".into(), Data::Int(n))].into(),
                    outputs: [("selection".into(), Data::Int(n))].into(),
                },
            )
            .unwrap();
    }
    fn apply(store: &mut Store, target: &Handle) {
        use crate::runtime::{Effect, ExecutionTraits, Runtime};
        let mut runtime = Runtime::new();
        runtime
            .add(
                (),
                [],
                ExecutionTraits {
                    pure: false,
                    repeatable: true,
                    bounded: true,
                },
            )
            .unwrap();
        let run = runtime
            .start(std::time::Duration::ZERO)
            .into_iter()
            .find_map(|e| match e {
                Effect::Spawn(ticket) => Some(ticket.run.clone()),
                _ => None,
            })
            .unwrap();
        store
            .apply_query(
                target,
                SourceInput::new("request".into(), ":view apply $target".into()).unwrap(),
                run,
            )
            .unwrap();
    }
    #[test]
    fn manual_apply_captures_the_value_at_request_time_and_rebind_revokes_old_generation() {
        let (mut store, source, target) = setup(QueryTrigger::Manual);
        apply(&mut store, &target);
        select(&mut store, &source, 2);
        let request = store.query_requests().pop().unwrap();
        assert_eq!(request.input.data(), &Data::Int(1));
        assert!(store.current_query(&target, request.generation));
        store.bind(&target, 1, None).unwrap();
        assert!(!store.current_query(&target, request.generation));
        let mut binding = request.binding;
        binding.trigger = QueryTrigger::Commit;
        store.configure_query(&target, binding, 2).unwrap();
        apply(&mut store, &target);
        assert!(store.query_requests()[0].generation > request.generation);
        store.query_finished(&target, request.generation, Some("obsolete error".into()));
        assert!(store.read(&target).unwrap().input_problem.is_none());
    }
    #[test]
    fn reopening_a_query_presentation_does_not_restore_execution_authority() {
        let (mut store, source, target) = setup(QueryTrigger::Commit);
        let mount = store.open_mount(&target).unwrap();
        apply(&mut store, &target);
        let request = store.query_requests().pop().unwrap();
        store.close_mount(&target, &mount).unwrap();
        assert!(!store.current_query(&target, request.generation));
        store.open_mount(&target).unwrap();
        select(&mut store, &source, 2);
        assert!(!store.read(&target).unwrap().observing);
        assert!(store.query_requests().is_empty());
        assert!(!store.current_query(&target, request.generation));
    }
    #[test]
    fn committed_values_coalesce_but_equal_values_do_not_restart_and_stop_revokes_pending_work() {
        let (mut store, source, target) = setup(QueryTrigger::Commit);
        apply(&mut store, &target);
        let first = store.query_requests().pop().unwrap();
        store.query_started(&target, first.generation);
        select(&mut store, &source, 1);
        assert!(store.query_requests().is_empty());
        select(&mut store, &source, 2);
        select(&mut store, &source, 3);
        let latest = store.query_requests().pop().unwrap();
        assert_eq!(latest.input.data(), &Data::Int(3));
        assert!(!store.current_query(&target, first.generation));
        store.set_observing(&target, false).unwrap();
        assert!(store.query_requests().is_empty());
        assert!(!store.current_query(&target, latest.generation));
        store.query_finished(&target, latest.generation, None);
        assert!(!store.read(&target).unwrap().query_running);
    }
    #[test]
    fn query_and_field_links_share_cycle_detection_and_rejected_edits_are_atomic() {
        let (mut store, source, target) = setup(QueryTrigger::Manual);
        assert_eq!(
            store.link(&target, "selection", &source, "selection", 0),
            Err(Error::Cycle)
        );
        let before = store.read(&source).unwrap();
        let mut reverse = store.read(&target).unwrap().query.unwrap();
        reverse.source = target.clone();
        assert_eq!(
            store.configure_query(&source, reverse, 0),
            Err(Error::Cycle)
        );
        assert_eq!(store.read(&source).unwrap().revision, before.revision);
        assert!(store.read(&source).unwrap().query.is_none());
    }
}
