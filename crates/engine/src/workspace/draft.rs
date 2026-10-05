//! A structural analysis snapshot: no runtime, leases, values, clock, worker or publication authority.
mod pipelines;
use super::{
    Applied, Change, Preparation, PreparedChange, PreparedControl, PreparedMeta, Stamp, Workspace,
    WorkspaceError,
    actions::{Control, bind_control},
    analysis::Analysis,
    binding_error,
};
use crate::{
    bindings::Bindings,
    calls::AdmittedCommand,
    graph::{DependencyGraph, NodeId, OutputRef},
    imports::{CapturedImport, Importers},
    providers::Providers,
    tasks::BoundTask,
};
use indexmap::{IndexMap, IndexSet};
use std::{collections::VecDeque, sync::Arc, time::Duration};
use wes_core::{capability::Typing, contracts::ContractRegistry};
use wes_language::{Diagnostic, Span, Statement, templates::Templates};

fn max_changes() -> usize {
    wes_budgets::get("workspace.changes") as usize
}

/// A finite declaration draft lets a later statement see earlier accepted declarations without
/// installing them in the live workspace. Runtime outcomes may continue while this snapshot exists.
/// Recorded controls are staged alongside declarations without live execution effects. Structural
/// changes make the finished batch obsolete. Other meta continuations still require dedicated
/// handlers; this is not a complete source submission or replay coordinator.
pub struct DeclarationDraft {
    reserved_names: Vec<String>,
    pub(super) access: crate::access::WriteScope,
    pub(super) environments: crate::environments::Registry,
    environment_images: crate::environments::ExecutionImages,
    environment_context: Option<wes_core::environments::EnvironmentContext>,
    environment_managed: bool,
    default_environment: Option<String>,
    default_configuration: Option<super::default_environment::DefaultEnvironment>,
    environment_history_bytes: u64,
    environment_record: Option<crate::environments::EnvironmentRecord>,
    environment_loader: Option<Arc<dyn crate::environments::EnvironmentLoader>>,
    local_default_revision: Option<wes_core::environments::Revision>,
    environment_replay: bool,
    base: Stamp,
    owner: Arc<()>,
    pub(super) graph: DependencyGraph<BoundTask>,
    pub(super) pipeline_groups: IndexMap<NodeId, Vec<NodeId>>,
    pub(super) providers: Providers,
    importers: Importers,
    pub(super) bindings: Bindings,
    typings: IndexMap<NodeId, Arc<Typing>>,
    actual_typings: IndexSet<NodeId>,
    templates: Templates,
    contracts: ContractRegistry,
    views: wes_views::Catalogue,
    calc_services: Option<Arc<dyn crate::calc::LocalServices>>,
    describe_service: Option<Arc<dyn crate::describe::DescribeService>>,
    calculation_package: Arc<wes_language::calc::Package>,
    changes: Vec<BatchChange>,
    removed: Vec<NodeId>,
    unbound: Vec<String>,
    recorded_ids: Option<VecDeque<NodeId>>,
}

#[derive(Debug)]
enum BatchChange {
    Declaration(PreparedChange),
    Control(PreparedControl),
}
impl BatchChange {
    fn node(&self) -> Option<&NodeId> {
        match self {
            Self::Declaration(change) => change.node(),
            Self::Control(_) => None,
        }
    }
    fn diagnostics(&self) -> &[Diagnostic] {
        match self {
            Self::Declaration(change) => change.diagnostics(),
            Self::Control(control) => control.diagnostics(),
        }
    }
}

/// Ordered declarations plus deferred runtime effects. Consume effects only after the entire
/// revision-checked batch has committed; no provider may enter between its structural operations.
pub struct BatchApplied {
    pub receipts: Vec<super::OperationReceipt>,
    pub changes: Vec<Applied>,
    pub effects: Vec<crate::runtime::Effect<BoundTask>>,
    pub removed: Vec<NodeId>,
    pub unbound: Vec<String>,
}

/// Private, already checked operations in source order. Contains no live execution state and must
/// never replace the workspace's runtime with an old snapshot at commit time.
#[derive(Debug)]
pub struct PreparedBatch {
    stamp: Stamp,
    changes: Vec<BatchChange>,
    removed: Vec<NodeId>,
    unbound: Vec<String>,
}
impl PreparedBatch {
    /// Information returned by commit, including notices stored in the prepared operations.
    pub(crate) fn success_diagnostics(&self) -> impl Iterator<Item = &Diagnostic> {
        self.changes.iter().flat_map(|change| {
            let information = match change {
                BatchChange::Declaration(PreparedChange {
                    operation:
                        Change::Define { information, .. } | Change::Types { information, .. },
                    ..
                }) => Some(information),
                _ => None,
            };
            change
                .diagnostics()
                .iter()
                .chain(information)
                .filter(|diagnostic| diagnostic.severity == wes_language::Severity::Info)
        })
    }
    pub(crate) fn receipt_charge(&self) -> usize {
        self.changes
            .iter()
            .filter(|change| matches!(change, BatchChange::Control(_)))
            .count()
            * 4096
    }
    /// Whole-batch check before any task is admitted. Unexecuted branches count too.
    pub(crate) fn safe_observation(&self, kind: super::observation::Kind) -> bool {
        use super::{actions::Control, observation::Kind};
        let nodes = self
            .changes
            .iter()
            .filter_map(BatchChange::node)
            .collect::<Vec<_>>();
        let typings = self
            .changes
            .iter()
            .filter_map(|change| match change {
                BatchChange::Declaration(PreparedChange {
                    operation: Change::Node { node, typing, .. },
                    ..
                }) => Some((node.clone(), typing.clone())),
                _ => None,
            })
            .collect::<IndexMap<_, _>>();
        let mut streams = 0;
        let safe = !nodes.is_empty()
            && self.changes.iter().all(|change| {
                let task = match change {
                    BatchChange::Declaration(PreparedChange {
                        operation: Change::Node { task, .. },
                        ..
                    }) => task,
                    BatchChange::Control(PreparedControl {
                        operation: Control::Policy(Some(node), crate::runtime::Policy::Reactive),
                        ..
                    }) => {
                        return kind == Kind::Live && nodes.contains(&node);
                    }
                    _ => return false,
                };
                match task {
                    BoundTask::Call(call) => {
                        if call.streaming() {
                            streams += 1;
                        }
                        call.invocation().capability.safety == wes_core::capability::Safety::Safe
                            && !call.interactive()
                    }
                    BoundTask::Calculation(calc) => {
                        calc.safe_observation()
                            && (kind != Kind::Live || calc.observation_inputs_match(&typings))
                    }
                    BoundTask::Input(_)
                    | BoundTask::TypeCheck(_)
                    | BoundTask::Stream(_)
                    | BoundTask::Accumulation(_) => true,
                    _ => false,
                }
            });
        safe && match kind {
            Kind::Finite => streams == 0,
            Kind::Live => streams == 1 && nodes.len() == 2,
        }
    }

    pub(crate) fn calculation_package_used(&self) -> bool {
        self.changes.iter().any(|change| {
            if let BatchChange::Declaration(PreparedChange {
                operation: Change::Define { templates, .. },
                ..
            }) = change
            {
                return templates
                    .snapshot()
                    .values()
                    .any(|d| d.calculation.is_some());
            }
            matches!(
                change,
                BatchChange::Declaration(PreparedChange {
                    operation: Change::Node {
                        task: BoundTask::Calculation(_),
                        ..
                    },
                    ..
                })
            )
        })
    }

    pub(crate) fn changed_nodes(&self) -> impl Iterator<Item = &NodeId> {
        self.changes.iter().filter_map(|change| match change {
            BatchChange::Control(PreparedControl {
                operation: Control::Change { node, .. },
                ..
            }) => Some(node),
            _ => None,
        })
    }
    pub(crate) fn written_names(&self) -> Vec<String> {
        self.changes
            .iter()
            .flat_map(|change| match change {
                BatchChange::Declaration(PreparedChange {
                    operation: Change::Node { names, .. },
                    ..
                }) => names.iter().map(|(name, _)| name.clone()).collect(),
                BatchChange::Declaration(PreparedChange {
                    operation: Change::Aliases(names),
                    ..
                }) => names.iter().map(|(name, _)| name.clone()).collect(),
                _ => Vec::new(),
            })
            .collect()
    }
    pub fn nodes(&self) -> impl Iterator<Item = &NodeId> {
        self.changes.iter().filter_map(BatchChange::node)
    }
    pub fn removed(&self) -> &[NodeId] {
        &self.removed
    }
    pub fn unbound(&self) -> &[String] {
        &self.unbound
    }
    pub fn len(&self) -> usize {
        self.changes.len()
    }
    pub fn is_empty(&self) -> bool {
        self.changes.is_empty()
    }
    /// The coordinator obtains one receipt for the accepted source and all its node identities.
    /// Check the complete set before attaching it to any call. Journal identity is also checked
    /// at the executor boundary, as with a single prepared change.
    pub fn with_admission(mut self, admission: AdmittedCommand) -> Result<Self, WorkspaceError> {
        if self.nodes().any(|node| !admission.contains(node)) {
            return Err(WorkspaceError::AdmissionMismatch);
        }
        let created = self.nodes().cloned().collect::<IndexSet<_>>();
        for prepared in &mut self.changes {
            match prepared {
                BatchChange::Declaration(PreparedChange {
                    operation: Change::Node { task, .. },
                    ..
                }) => *task = task.clone().with_admission(admission.clone()),
                BatchChange::Control(PreparedControl {
                    operation: Control::Change { node, call, .. },
                    ..
                }) if created.contains(node) => {
                    *call = call.clone().with_admission(admission.clone());
                }
                _ => {}
            }
        }
        Ok(self)
    }
}

impl Workspace {
    pub fn draft(&self) -> Result<DeclarationDraft, WorkspaceError> {
        self.next_revision()?;
        Ok(DeclarationDraft {
            reserved_names: vec![],
            access: Default::default(),
            environments: self.environments.planning_snapshot(),
            environment_images: self.environment_images.clone(),
            environment_context: None,
            environment_managed: self.environment_loader.is_some() && self.environment_managed,
            default_environment: self.default_environment.clone(),
            default_configuration: self.default_configuration.clone(),
            environment_record: self.environment_record.clone(),
            environment_history_bytes: self.environment_records.iter().map(|r| r.charge()).sum(),
            environment_loader: self.environment_loader.clone(),
            local_default_revision: None,
            environment_replay: false,
            base: self.stamp(),
            owner: Arc::new(()),
            graph: self.runtime.graph().clone(),
            pipeline_groups: self.runtime.pipeline_groups(),
            providers: self.providers.clone(),
            importers: self.importers.clone(),
            bindings: self.bindings.clone(),
            typings: self
                .runtime
                .graph()
                .nodes()
                .filter_map(|node| {
                    self.runtime
                        .actual_typing(node.id())
                        .or_else(|| self.predictions.get(node.id()))
                        .map(|typing| (node.id().clone(), typing.clone()))
                })
                .collect(),
            actual_typings: self
                .runtime
                .graph()
                .nodes()
                .filter(|node| self.runtime.actual_typing(node.id()).is_some())
                .map(|node| node.id().clone())
                .collect(),
            templates: self.templates.clone(),
            contracts: self.contracts.clone(),
            views: self.views.catalogue().clone(),
            calc_services: self.calc_services.clone(),
            describe_service: self.describe_service.clone(),
            calculation_package: wes_language::calc::Package::standard(),
            changes: vec![],
            removed: vec![],
            unbound: vec![],
            recorded_ids: None,
        })
    }

    /// Validate the original structural revision and total revision budget before installing any
    /// changes. No await, provider call or external I/O occurs within this operation. Recorded
    /// controls may return runtime effects; consume them only after the complete batch commits.
    pub fn commit_batch(
        &mut self,
        batch: PreparedBatch,
        now: Duration,
    ) -> Result<BatchApplied, WorkspaceError> {
        self.commit_batch_installation(batch, now, super::Installation::Live)
    }
    pub(super) fn commit_batch_installation(
        &mut self,
        batch: PreparedBatch,
        now: Duration,
        installation: super::Installation,
    ) -> Result<BatchApplied, WorkspaceError> {
        self.check_stamp(&batch.stamp)?;
        let count =
            u64::try_from(batch.changes.len()).map_err(|_| WorkspaceError::RevisionExhausted)?;
        self.revision
            .checked_add(count)
            .ok_or(WorkspaceError::RevisionExhausted)?;
        let mut timeouts = IndexMap::<NodeId, u64>::new();
        for change in &batch.changes {
            if let BatchChange::Control(PreparedControl {
                operation: Control::Timeout(node, _),
                ..
            }) = change
            {
                *timeouts.entry(node.clone()).or_default() += 1;
            }
        }
        for (node, count) in timeouts {
            self.runtime.validate_timeout_updates(&node, count)?;
        }
        let mut result = BatchApplied {
            receipts: vec![],
            changes: vec![],
            effects: vec![],
            removed: vec![],
            unbound: vec![],
        };
        for change in batch.changes {
            // Only this consuming, revision-checked boundary can rebase private draft stamps.
            match change {
                BatchChange::Declaration(mut prepared) => {
                    prepared.stamp = self.stamp();
                    result
                        .changes
                        .push(self.commit_installation(prepared, installation).expect(
                            "unchanged declaration revision preserves draft commit invariants",
                        ));
                }
                BatchChange::Control(mut control) => {
                    control.stamp = self.stamp();
                    let applied = self
                        .apply_control_installation(control, now, installation)
                        .expect("declarations and timeout revision budget were preflighted");
                    result.changes.push(Applied {
                        node: None,
                        diagnostics: applied.diagnostics,
                    });
                    result.receipts.push(applied.receipt);
                    result.effects.extend(applied.effects);
                    result.removed.extend(applied.removed);
                    result.unbound.extend(applied.unbound);
                }
            }
        }
        debug_assert_eq!(result.removed, batch.removed);
        debug_assert_eq!(result.unbound, batch.unbound);
        Ok(result)
    }
}

impl DeclarationDraft {
    pub(crate) fn with_reserved_names(mut self, names: impl Iterator<Item = String>) -> Self {
        self.reserved_names = names.collect();
        self
    }
    pub(crate) fn with_access(mut self, access: crate::access::WriteScope) -> Self {
        self.access = access;
        self
    }
    pub(crate) fn reactive_nodes(&mut self) -> Result<String, WorkspaceError> {
        let nodes: Vec<_> = self
            .changes
            .iter()
            .filter_map(BatchChange::node)
            .cloned()
            .collect();
        let mut replay = String::new();
        for node in nodes {
            // Numeric/opaque node references use the ordinary parser and durable control path.
            let text = format!("\n:policy ${node} mode:reactive");
            let parsed = wes_language::parse(&wes_language::SourceText::new("policy", &text));
            let Preparation::Meta(meta) = self.prepare(&parsed.script.statements[0])? else {
                unreachable!()
            };
            let control = self.prepare_control(meta)?;
            self.stage_control(control)?;
            replay.push_str(&text);
        }
        Ok(replay)
    }
    pub(crate) fn with_environment_context(
        mut self,
        context: Option<wes_core::environments::EnvironmentContext>,
        replay: bool,
    ) -> Self {
        self.environment_replay = replay;
        self.environment_context = if replay || context.is_some() {
            context
        } else if self.environment_managed {
            Some(wes_core::environments::EnvironmentContext {
                selected: self.default_environment.clone(),
                revisions: self.environments.revisions(),
            })
        } else {
            None
        };
        self
    }
    pub(crate) fn environment_context(
        &self,
    ) -> Option<&wes_core::environments::EnvironmentContext> {
        self.environment_context.as_ref()
    }
    pub(crate) fn calculation_package(&self) -> Arc<wes_language::calc::Package> {
        self.calculation_package.clone()
    }
    pub(crate) fn with_calculation_package(
        mut self,
        package: Arc<wes_language::calc::Package>,
    ) -> Self {
        self.calculation_package = package;
        self
    }
    pub(crate) fn importers(&self) -> &Importers {
        &self.importers
    }
    pub fn prepare_import(
        &self,
        prepared: PreparedMeta,
        captured: &CapturedImport,
    ) -> Result<PreparedChange, WorkspaceError> {
        let request = prepared.import_request()?;
        self.prepare_import_step(prepared.import_step(request)?, captured)
    }
    pub(crate) fn prepare_import_step(
        &self,
        prepared: super::PreparedImport,
        captured: &CapturedImport,
    ) -> Result<PreparedChange, WorkspaceError> {
        let stamp = self.stamp();
        if !Arc::ptr_eq(&stamp.owner, &prepared.stamp.owner)
            || stamp.revision != prepared.stamp.revision
        {
            return Err(WorkspaceError::Obsolete);
        }
        let mut change = super::analysis::prepare_import(
            prepared,
            captured,
            &self.importers,
            &self.providers,
            &self.templates,
        )?;
        if let Some(configuration) = &self.default_configuration {
            change.operation = Change::DefaultProvider {
                product: captured.product().clone(),
                publication: Box::new(configuration.imported(
                    captured,
                    &self.environments,
                    &self.environment_images,
                )?),
            };
        } else if let (Some(name), Some(current), Some(loader)) = (
            &self.default_environment,
            &self.environment_record,
            &self.environment_loader,
        ) {
            change.operation =
                Change::EnvironmentProvider(Box::new(super::default_environment::import_document(
                    current,
                    loader.as_ref(),
                    name,
                    captured,
                    &self.environments,
                    &self.environment_images,
                    self.environment_history_bytes,
                )?));
        }
        Ok(change)
    }
    pub(crate) fn with_recorded_ids(mut self, ids: &[NodeId]) -> Result<Self, WorkspaceError> {
        if ids.iter().collect::<IndexSet<_>>().len() != ids.len()
            || ids.iter().any(|id| self.graph.node(id).is_some())
        {
            return Err(super::rejected(
                "ENG008",
                Span::at(0),
                "recorded node identities must be unique and unused",
            ));
        }
        self.recorded_ids = Some(ids.iter().cloned().collect());
        Ok(self)
    }
    fn stamp(&self) -> Stamp {
        Stamp {
            owner: self.owner.clone(),
            revision: self.changes.len() as u64,
        }
    }
    /// Only the source coordinator calls this after live capsule admission or recorded Applied evidence.
    /// The plan operand is process-local and must never be resolved during held reconstruction.
    pub(crate) fn prepare_import_apply(
        &self,
        statement: &Statement,
    ) -> Result<Preparation, WorkspaceError> {
        let wes_language::Expression::Call(call) = &statement.expression else {
            return Err(super::rejected(
                "IMP001",
                statement.span,
                "Expected import apply",
            ));
        };
        let invocation =
            wes_language::vocabulary::commands::invocation(call).map_err(WorkspaceError::from)?;
        if invocation.spec.command != wes_language::vocabulary::MetaCommand::ImportApply
            || statement.binding.is_some()
            || statement.error_binding.is_some()
            || !statement.annotations.is_empty()
        {
            return Err(super::rejected(
                "IMP001",
                statement.span,
                "Import apply is an independent action with no binding or annotations",
            ));
        }
        let mut inputs = IndexMap::new();
        for argument in &invocation.call.arguments {
            if argument.key.text != "replace"
                || !matches!(&argument.value,wes_language::Value::Word(n) if matches!(n.text.as_str(),"true"|"false"))
            {
                return Err(super::rejected(
                    "IMP001",
                    argument.span,
                    "replace must be a literal Bool",
                ));
            }
            let value = crate::plan::input(
                &argument.value,
                &wes_core::Shape::Primitive(wes_core::Primitive::Bool),
                &|_| None,
            )
            .map_err(|_| super::rejected("IMP001", argument.span, "Invalid replacement option"))?;
            if inputs.insert("replace".into(), value).is_some() {
                return Err(super::rejected(
                    "IMP001",
                    argument.span,
                    "Specify replace once",
                ));
            }
        }
        Ok(Preparation::Meta(PreparedMeta {
            stamp: self.stamp(),
            plan: crate::plan::Plan::Action {
                task: crate::plan::MetaTask {
                    spec: invocation.spec,
                    target: None,
                    tail: invocation.tail,
                    subjects: vec![],
                    inputs,
                },
                targets: vec![],
            },
            diagnostics: vec![],
            span: statement.span,
        }))
    }
    pub fn prepare(&self, statement: &Statement) -> Result<Preparation, WorkspaceError> {
        self.prepare_with_pipe(statement, None)
    }
    pub(super) fn prepare_with_pipe(
        &self,
        statement: &Statement,
        pipe_input: Option<&OutputRef>,
    ) -> Result<Preparation, WorkspaceError> {
        let annotations: Vec<_> = statement
            .annotations
            .iter()
            .filter(|a| a.name.text == "env")
            .collect();
        if annotations.len() > 1 || annotations.first().is_some_and(|a| a.targets.len() != 1) {
            return Err(super::rejected(
                "ENV008",
                statement.span,
                "use exactly one literal @env{name} selector",
            ));
        }
        if !annotations.is_empty()
            && !matches!(
                &statement.expression,
                wes_language::Expression::Calculation(_)
                    | wes_language::Expression::Call(wes_language::Call { marker: None, .. })
            )
        {
            return Err(super::rejected(
                "ENV008",
                statement.span,
                "environment selectors apply only to new provider calls or calculations",
            ));
        }
        let empty = Providers::new();
        // The source's acknowledged context remains unchanged in its journal record. Within
        // this draft only, subsequent statements see imports admitted earlier in source order.
        let mut effective_context = self.environment_context.clone();
        if let (Some(context), Some(name), Some(revision)) = (
            &mut effective_context,
            &self.default_environment,
            self.local_default_revision,
        ) {
            context.revisions.insert(name.clone(), revision);
        }
        let providers = if let Some(context) = &effective_context {
            let selected = annotations
                .first()
                .map(|a| a.targets[0].text.as_str())
                .or(context.selected.as_deref());
            if (self.default_environment.is_none()
                || selected != self.default_environment.as_deref())
                && matches!(&statement.expression, wes_language::Expression::Call(c) if c.marker.is_some() && c.path.first().is_some_and(|n| n.text == "import"))
            {
                return Err(super::rejected(
                    "ENV011",
                    statement.span,
                    "managed imports require an environment definition plan",
                ));
            }
            match selected {
                None => &empty,
                Some(name) => {
                    let revision = context.revisions.get(name).ok_or_else(|| {
                        super::rejected(
                            "ENV008",
                            statement.span,
                            "environment revision was not acknowledged",
                        )
                    })?;
                    let new_binding = matches!(&statement.expression, wes_language::Expression::Call(call) if call.marker.is_none() || call.path.first().is_some_and(|n| n.text == "import"))
                        || matches!(
                            &statement.expression,
                            wes_language::Expression::Calculation(_)
                        );
                    let environment = if self.environment_replay || !new_binding {
                        self.environments.retained_revision(name, *revision)
                    } else {
                        self.environments
                            .inspect(name)
                            .filter(|e| e.revision() == *revision)
                    }
                    .ok_or_else(|| {
                        super::rejected(
                            "ENV008",
                            statement.span,
                            "selected environment is missing or changed; inspect and select again",
                        )
                    })?;
                    if environment.is_abstract() || environment.is_retired() {
                        &empty
                    } else {
                        self.environment_images
                            .get(name, *revision)
                            .ok_or_else(|| {
                                super::rejected(
                                    "ENV010",
                                    statement.span,
                                    "captured environment adapters are unavailable",
                                )
                            })?
                    }
                }
            }
        } else {
            if !annotations.is_empty() {
                return Err(super::rejected(
                    "ENV008",
                    statement.span,
                    "no environment definitions are installed",
                ));
            }
            &self.providers
        };
        let mut statement = statement.clone();
        statement.annotations.retain(|a| a.name.text != "env");
        Analysis {
            reserved_names: &self.reserved_names,
            pipe_input,
            environment_context: effective_context.as_ref(),
            stamp: self.stamp(),
            graph: &self.graph,
            providers,
            importers: &self.importers,
            bindings: &self.bindings,
            templates: &self.templates,
            contracts: &self.contracts,
            views: &self.views,
            calc_services: &self.calc_services,
            describe_service: &self.describe_service,
            calculation_package: self.calculation_package.clone(),
            data_typing: &|node| self.typings.get(node).map(Arc::as_ref),
            allocation: match &self.recorded_ids {
                None => super::analysis::NodeAllocation::Automatic,
                Some(ids) => super::analysis::NodeAllocation::Recorded(ids.front()),
            },
        }
        .prepare(&statement)
    }
    pub fn resolve(&self, name: &str) -> Option<OutputRef> {
        self.bindings.resolve(name, &self.graph)
    }
    /// Prepare already captured package text. Opening files and recording exact source content
    /// remain the coordinator's responsibility, as for Workspace::prepare_type_package.
    pub fn prepare_type_package(
        &self,
        yaml: &str,
        span: Span,
    ) -> Result<PreparedChange, WorkspaceError> {
        super::analysis::prepare_types(self.stamp(), &self.contracts, yaml, span)
    }
    pub fn prepare_type_load(
        &self,
        prepared: super::PreparedMeta,
        package: &crate::type_sources::CapturedTypePackage,
    ) -> Result<PreparedChange, WorkspaceError> {
        let stamp = self.stamp();
        if !Arc::ptr_eq(&stamp.owner, &prepared.stamp.owner)
            || stamp.revision != prepared.stamp.revision
        {
            return Err(WorkspaceError::Obsolete);
        }
        super::analysis::prepare_type_load(prepared, &self.contracts, &self.views, package)
    }
    /// Accept one successfully prepared declaration. Rejected statements do not mutate the draft;
    /// callers can continue analysing later statements and retain only accepted source for replay.
    /// Information such as TMP000 is withheld until the live batch actually commits.
    pub fn stage(&mut self, prepared: PreparedChange) -> Result<(), WorkspaceError> {
        if !Arc::ptr_eq(&self.owner, &prepared.stamp.owner)
            || self.stamp().revision != prepared.stamp.revision
        {
            return Err(WorkspaceError::Obsolete);
        }
        if self.changes.len() >= max_changes() {
            return Err(WorkspaceError::DraftCapacity);
        }
        self.base
            .revision
            .checked_add(self.changes.len() as u64 + 1)
            .ok_or(WorkspaceError::RevisionExhausted)?;
        self.check_change_access(&prepared.operation)?;
        match &prepared.operation {
            Change::Provider(product) => {
                self.providers.register_all_ports(
                    product.description().clone(),
                    product.invoker().clone(),
                    product.streams().cloned(),
                    product.conversations().cloned(),
                );
            }
            Change::DefaultProvider {
                product,
                publication,
            } => {
                self.providers.register_all_ports(
                    product.description().clone(),
                    product.invoker().clone(),
                    product.streams().cloned(),
                    product.conversations().cloned(),
                );
                self.environments = publication.registry.planning_snapshot();
                self.environment_images = publication.images.clone();
                self.default_configuration = Some(publication.configuration.clone());
                self.local_default_revision = Some(
                    self.environments
                        .inspect(&publication.configuration.name)
                        .expect("configured")
                        .revision(),
                );
            }
            Change::EnvironmentProvider(publication) => {
                self.environment_history_bytes += publication.record.charge();
                self.environments = publication.registry.planning_snapshot();
                self.environment_images = publication.images.clone();
                self.environment_record = Some(publication.record.clone());
                self.local_default_revision = self
                    .default_environment
                    .as_ref()
                    .and_then(|name| self.environments.inspect(name))
                    .map(|env| env.revision());
            }
            Change::Define { templates, .. } => self.templates = templates.clone(),
            Change::Types {
                registry, views, ..
            } => {
                self.contracts = registry.clone();
                if let Some(views) = views {
                    self.views = views.clone();
                }
            }
            Change::Aliases(aliases) => {
                let mut bindings = self.bindings.clone();
                for (name, output) in aliases {
                    bindings
                        .bind(name, output.clone(), &self.graph)
                        .map_err(binding_error)?;
                }
                self.bindings = bindings;
            }
            Change::Node {
                node,
                task,
                typing,
                names,
                stream_origin,
                ..
            } => {
                self.graph
                    .restore(node.clone(), task.clone(), task.dependencies())
                    .map_err(|error| WorkspaceError::Runtime(error.into()))?;
                self.typings.insert(node.clone(), typing.clone());
                if let Some(root) = stream_origin {
                    self.pipeline_groups
                        .entry(root.clone())
                        .or_default()
                        .push(node.clone());
                }
                for (name, port) in names {
                    self.bindings
                        .bind(
                            name,
                            OutputRef {
                                node: node.clone(),
                                port: *port,
                            },
                            &self.graph,
                        )
                        .expect("prepared names exclude the staged ID");
                }
            }
        }
        if prepared.node().is_some()
            && let Some(ids) = &mut self.recorded_ids
        {
            debug_assert_eq!(ids.front(), prepared.node());
            ids.pop_front();
        }
        match &prepared.operation {
            Change::Node { names, .. } => {
                for (name, _) in names {
                    self.access.include_name(name.clone());
                }
            }
            Change::Aliases(names) => {
                for (name, _) in names {
                    self.access.include_name(name.clone());
                }
            }
            _ => (),
        }
        if let Some(node) = prepared.node() {
            self.access.include(node.clone());
        }
        self.changes.push(BatchChange::Declaration(prepared));
        Ok(())
    }
    pub fn prepare_control(
        &self,
        prepared: PreparedMeta,
    ) -> Result<PreparedControl, WorkspaceError> {
        let stamp = self.stamp();
        if !Arc::ptr_eq(&stamp.owner, &prepared.stamp.owner)
            || stamp.revision != prepared.stamp.revision
        {
            return Err(WorkspaceError::Obsolete);
        }
        let operation = bind_control(&prepared, &self.graph, &self.bindings, |node| {
            self.typings.get(node).map(Arc::as_ref)
        })
        .map_err(|error| error.preceded_by(&prepared.diagnostics))?;
        let crate::plan::Plan::Action { task, .. } = &prepared.plan else {
            unreachable!("control is an action")
        };
        if !task.spec.recorded {
            return Err(super::rejected(
                "ENG005",
                prepared.span,
                "immediate controls cannot be staged in a declaration batch",
            ));
        }
        Ok(PreparedControl {
            stamp: prepared.stamp,
            operation,
            diagnostics: prepared.diagnostics,
            recorded: true,
        })
    }
    pub fn stage_control(&mut self, prepared: PreparedControl) -> Result<(), WorkspaceError> {
        if !prepared.recorded
            || !Arc::ptr_eq(&self.owner, &prepared.stamp.owner)
            || self.stamp().revision != prepared.stamp.revision
        {
            return Err(WorkspaceError::Obsolete);
        }
        if self.changes.len() >= max_changes() {
            return Err(WorkspaceError::DraftCapacity);
        }
        self.base
            .revision
            .checked_add(self.changes.len() as u64 + 1)
            .ok_or(WorkspaceError::RevisionExhausted)?;
        self.access
            .check_control(&prepared.operation, &self.graph, &self.bindings)?;
        self.check_pipeline_control_access(&prepared.operation)?;
        match &prepared.operation {
            Control::DropName(name) => {
                self.bindings.unbind(name);
                self.unbound.push(name.clone());
            }
            Control::DropNode(node) => {
                let before = self.bindings.names().keys().cloned().collect::<Vec<_>>();
                let removed = self
                    .graph
                    .remove(node)
                    .map_err(|error| WorkspaceError::Runtime(error.into()))?;
                self.bindings.retain_nodes(&self.graph);
                self.typings.retain(|id, _| !removed.contains(id));
                self.actual_typings.retain(|id| !removed.contains(id));
                self.removed.extend(removed);
                self.unbound.extend(
                    before
                        .into_iter()
                        .filter(|name| !self.bindings.names().contains_key(name)),
                );
            }
            Control::Change { node, call, typing } => {
                self.graph
                    .replace_payload(node, BoundTask::Call(call.clone()))
                    .map_err(|error| WorkspaceError::Runtime(error.into()))?;
                self.graph.mark_stale(node).expect("prepared target exists");
                if !self.actual_typings.contains(node) {
                    self.typings.insert(node.clone(), typing.clone());
                }
            }
            Control::Timeout(..) | Control::Policy(..) => {}
            Control::Refresh(_) | Control::RefreshDownstream(..) | Control::Cancel(_) => {
                unreachable!("non-recorded controls cannot enter a draft")
            }
        }
        self.changes.push(BatchChange::Control(prepared));
        Ok(())
    }
    /// Analysis diagnostics only; success announcements belong to commit, not admission preflight.
    pub fn diagnostics(&self) -> impl Iterator<Item = &Diagnostic> {
        self.changes.iter().flat_map(BatchChange::diagnostics)
    }
    pub fn finish(self) -> PreparedBatch {
        PreparedBatch {
            stamp: self.base,
            changes: self.changes,
            removed: self.removed,
            unbound: self.unbound,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::runtime::RuntimeError;

    fn alias(draft: &DeclarationDraft) -> PreparedChange {
        PreparedChange {
            stamp: draft.stamp(),
            operation: Change::Aliases(vec![]),
            diagnostics: vec![],
        }
    }

    #[test]
    fn capacity_and_revision_failure_do_not_install_a_partial_change() {
        let mut workspace = Workspace::local(crate::providers::LocalScope::new("fixture").unwrap());
        let mut draft = workspace.draft().unwrap();
        for _ in 0..max_changes() {
            draft.stage(alias(&draft)).unwrap();
        }
        assert!(matches!(
            draft.stage(alias(&draft)),
            Err(WorkspaceError::DraftCapacity)
        ));
        assert_eq!(
            workspace
                .commit_batch(draft.finish(), std::time::Duration::ZERO)
                .unwrap()
                .changes
                .len(),
            max_changes()
        );
        workspace.revision = u64::MAX - 1;
        let mut draft = workspace.draft().unwrap();
        draft.stage(alias(&draft)).unwrap();
        assert!(matches!(
            draft.stage(alias(&draft)),
            Err(WorkspaceError::RevisionExhausted)
        ));
        workspace
            .commit_batch(draft.finish(), std::time::Duration::ZERO)
            .unwrap();
        assert_eq!(workspace.revision, u64::MAX);
        assert!(matches!(
            workspace.draft(),
            Err(WorkspaceError::RevisionExhausted)
        ));
    }

    #[test]
    fn closed_or_foreign_workspace_rejects_even_an_empty_batch() {
        let mut workspace = Workspace::local(crate::providers::LocalScope::new("fixture").unwrap());
        let batch = workspace.draft().unwrap().finish();
        assert!(matches!(
            Workspace::local(crate::providers::LocalScope::new("fixture").unwrap())
                .commit_batch(batch, std::time::Duration::ZERO),
            Err(WorkspaceError::Obsolete)
        ));
        let batch = workspace.draft().unwrap().finish();
        workspace.runtime.close();
        assert!(matches!(
            workspace.commit_batch(batch, std::time::Duration::ZERO),
            Err(WorkspaceError::Runtime(RuntimeError::Closed))
        ));
        assert!(matches!(
            workspace.draft(),
            Err(WorkspaceError::Runtime(RuntimeError::Closed))
        ));
    }
}
