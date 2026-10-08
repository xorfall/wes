//! Single-owner declaration state over the existing runtime. Preparation performs no provider I/O.
mod name;
use crate::{
    bindings::{BindingError, Bindings},
    calls::AdmittedCommand,
    graph::{NodeId, OutputPort, OutputRef},
    imports::{CapturedImport, ImportProduct, ImportRequest, Importer, Importers},
    plan::{Input, Plan, Task},
    providers::{Invoker, Provider, Providers},
    runtime::{Deadline, Effect, Outcome, Run, Runtime, RuntimeError},
    tasks::BoundTask,
    type_sources::CapturedTypePackage,
};
use indexmap::IndexMap;
mod specs;
pub use name::{InvalidWorkspaceName, WorkspaceName};
pub use specs::ImportedSpec;
use std::{sync::Arc, time::Duration};
use thiserror::Error;
use wes_core::{
    ErrorValue, ValidationIssue,
    capability::{Catalogue, ProviderDescription, Typing},
    contracts::ContractRegistry,
};
use wes_language::{Diagnostic, Span, Statement, templates::Templates, vocabulary::MetaCommand};
mod access;
pub(crate) mod actions;
mod analysis;
mod binding_status;
pub(crate) use binding_status::BINDING_CHANGED;
mod default_environment;
mod draft;
mod pipeline;
mod replay;
pub use actions::{ControlApplied, OperationReceipt, PreparedControl};
pub use draft::{BatchApplied, DeclarationDraft, PreparedBatch};
pub use replay::ReplayWorkspace;
#[derive(Clone, Copy, Debug)]
pub(super) enum Installation {
    Live,
    Held,
}
#[derive(Clone, Debug)]
pub struct PreparedWait {
    selected: crate::runtime::OutputSelection,
    span: Span,
    diagnostics: Vec<Diagnostic>,
}
impl PreparedWait {
    pub fn diagnostics(&self) -> &[Diagnostic] {
        &self.diagnostics
    }
    pub fn selected(&self) -> &crate::runtime::OutputSelection {
        &self.selected
    }
    pub fn budget(&self) -> Duration {
        Duration::from_secs(60)
    }
    pub fn unavailable(&self) -> Diagnostic {
        Diagnostic::error(
            "MET006",
            self.span,
            "The selected output did not become available during this wait.",
        )
        .with_public_message("The selected output did not become available during this wait.")
    }
}

#[derive(Debug, Error)]
pub enum WorkspaceError {
    #[error("prepared change belongs to another workspace or an obsolete declaration revision")]
    Obsolete,
    #[error("command receipt does not contain the prepared node")]
    AdmissionMismatch,
    #[error("workspace declaration revision exhausted")]
    RevisionExhausted,
    #[error("declaration draft exceeds the maximum number of accepted changes")]
    DraftCapacity,
    #[error("workspace statement rejected")]
    Rejected {
        diagnostics: Vec<Diagnostic>,
        issues: Vec<ValidationIssue>,
    },
    #[error(
        "View queries require SAFE operations and matching typed source/adapter contracts throughout the captured query; nothing was started"
    )]
    UnsafeObservation,
    #[error("workspace preparation cancelled")]
    Cancelled,
    #[error(transparent)]
    Runtime(#[from] RuntimeError),
}
impl From<Diagnostic> for WorkspaceError {
    fn from(diagnostic: Diagnostic) -> Self {
        Self::Rejected {
            diagnostics: vec![diagnostic],
            issues: vec![],
        }
    }
}
impl WorkspaceError {
    fn preceded_by(mut self, earlier: &[Diagnostic]) -> Self {
        if let Self::Rejected { diagnostics, .. } = &mut self {
            let mut combined = earlier.to_vec();
            combined.append(diagnostics);
            *diagnostics = combined;
        }
        self
    }
}

/// The private stamp is process-local and not serialized as a substitute for command/run identity.
#[derive(Clone, Debug)]
struct Stamp {
    owner: Arc<()>,
    revision: u64,
}
#[derive(Debug)]
pub struct PreparedChange {
    stamp: Stamp,
    operation: Change,
    diagnostics: Vec<Diagnostic>,
}
impl PreparedChange {
    /// Attach acknowledged origin to a finite call before commit. The recorded executor also
    /// checks the journal identity at execution. The coordinator is responsible for recording
    /// this prepared declaration's source and for serializing preparation through commit.
    pub fn with_admission(mut self, admission: AdmittedCommand) -> Result<Self, WorkspaceError> {
        if let Change::Node { node, task, .. } = &mut self.operation {
            if !admission.contains(node) {
                return Err(WorkspaceError::AdmissionMismatch);
            }
            *task = task.clone().with_admission(admission);
        }
        Ok(self)
    }
    pub fn diagnostics(&self) -> &[Diagnostic] {
        &self.diagnostics
    }
    pub fn node(&self) -> Option<&NodeId> {
        if let Change::Node { node, .. } = &self.operation {
            Some(node)
        } else {
            None
        }
    }
}
#[derive(Debug)]
enum Change {
    Provider(Arc<ImportProduct>),
    DefaultProvider {
        product: Arc<ImportProduct>,
        publication: Box<default_environment::Publication>,
    },
    EnvironmentProvider(Box<default_environment::DocumentPublication>),
    Define {
        templates: Templates,
        information: Diagnostic,
    },
    Types {
        views: Option<wes_views::Catalogue>,
        registry: ContractRegistry,
        information: Diagnostic,
    },
    Aliases(Vec<(String, OutputRef)>),
    Node {
        node: NodeId,
        task: BoundTask,
        admission: Installation,
        activation: Option<OutputRef>,
        stream_origin: Option<NodeId>,
        pipeline: crate::runtime::PipelineStep,
        typing: Arc<Typing>,
        names: Vec<(String, OutputPort)>,
    },
}
/// L1 dispatch stays explicit. It must bind its concrete meta operation before admission; this
/// value is an analysed plan, never permission to execute it during completion or preflight.
#[derive(Debug)]
pub struct PreparedMeta {
    stamp: Stamp,
    plan: Plan,
    diagnostics: Vec<Diagnostic>,
    span: Span,
}
pub(crate) struct PreparedImport {
    stamp: Stamp,
    request: ImportRequest,
    replace: bool,
    diagnostics: Vec<Diagnostic>,
    span: Span,
}
impl PreparedMeta {
    pub(crate) fn import_step(
        self,
        request: ImportRequest,
    ) -> Result<PreparedImport, WorkspaceError> {
        let Plan::Action { task, .. } = &self.plan else {
            return Err(rejected(
                "IMP001",
                self.span,
                "Expected an explicit import action",
            ));
        };
        if !matches!(
            task.spec.command,
            MetaCommand::Import | MetaCommand::ImportApply
        ) {
            return Err(rejected(
                "IMP001",
                self.span,
                "Expected an explicit import action",
            ));
        }
        let replace = match task.inputs.get("replace") {
            None => false,
            Some(Input::Literal(value)) if matches!(value.data(), wes_core::Data::Bool(_)) => {
                value.data() == &wes_core::Data::Bool(true)
            }
            _ => {
                return Err(rejected(
                    "IMP001",
                    self.span,
                    "replace must be a literal Bool",
                ));
            }
        };
        Ok(PreparedImport {
            stamp: self.stamp,
            request,
            replace,
            diagnostics: self.diagnostics,
            span: self.span,
        })
    }
    pub fn import_request(&self) -> Result<ImportRequest, WorkspaceError> {
        let failure = || {
            rejected(
                "IMP001",
                self.span,
                "import requires an importer kind and literal arguments",
            )
            .preceded_by(&self.diagnostics)
        };
        let Plan::Action { task, .. } = &self.plan else {
            return Err(failure());
        };
        if task.spec.command != MetaCommand::Import || task.tail.len() != 1 {
            return Err(failure());
        }
        let mut arguments = IndexMap::new();
        let mut alias = None;
        for (key, input) in &task.inputs {
            let Input::Literal(value) = input else {
                return Err(failure());
            };
            if key == "as" {
                let wes_core::Data::Text(name) = value.data() else {
                    return Err(failure());
                };
                alias = Some(name.to_string());
            } else if key != "replace" {
                arguments.insert(key.clone(), value.clone());
            }
        }
        ImportRequest::new(task.tail[0].clone(), alias, arguments).map_err(|error| {
            rejected("IMP001", self.span, error.to_string()).preceded_by(&self.diagnostics)
        })
    }
    pub fn is_type_load(&self) -> bool {
        matches!(&self.plan, Plan::Action {task, ..} if task.spec.command == MetaCommand::Type && task.tail == ["load"])
    }
    pub fn type_source(
        &self,
    ) -> Result<crate::type_sources::TypeInput, crate::type_sources::TypeSourceError> {
        use crate::type_sources::{TypeInput, TypeSourceError};
        let Plan::Action { task, .. } = &self.plan else {
            return Err(TypeSourceError::InvalidInput);
        };
        if !self.is_type_load() {
            return Err(TypeSourceError::InvalidInput);
        }
        let text = |key| match task.inputs.get(key) {
            Some(Input::Literal(value)) => match value.data() {
                wes_core::Data::Text(s) => Some(s.clone()),
                _ => None,
            },
            _ => None,
        };
        match (text("path"), text("source"), text("origin")) {
            (Some(path), None, None) => Ok(TypeInput::File(path.to_string())),
            (None, Some(source), origin) => {
                let origin = crate::type_sources::text_origin(origin.as_deref(), &source)?;
                Ok(TypeInput::Text {
                    source: source.to_string(),
                    origin,
                })
            }
            _ => Err(TypeSourceError::InvalidInput),
        }
    }
    pub fn command(&self) -> MetaCommand {
        match &self.plan {
            Plan::Action { task, .. } => task.spec.command,
            Plan::NewNode(node) => match &node.task {
                Task::Meta(task) => task.spec.command,
                Task::Invoke(_) => unreachable!("prepared meta contains only meta tasks"),
            },
        }
    }
    pub fn plan(&self) -> &Plan {
        &self.plan
    }
    pub fn diagnostics(&self) -> &[Diagnostic] {
        &self.diagnostics
    }
}
#[derive(Debug)]
pub enum Preparation {
    Change(PreparedChange),
    Meta(PreparedMeta),
}
#[derive(Debug)]
pub struct Applied {
    pub node: Option<NodeId>,
    pub diagnostics: Vec<Diagnostic>,
}

mod observation;

pub struct Workspace {
    pub(crate) recordings: crate::eventlog::owner::Owner,
    pub(crate) dataset_access: crate::storage::datasets::DatasetAccess,
    pub(crate) views: crate::views::Store,
    pub(crate) sandbox_store: Option<Arc<dyn crate::session::sandbox::DefinitionStore>>,
    pub(crate) sandbox_runtime: bool,
    pub(crate) safe_observation: Option<observation::Kind>,
    sandbox_leases: Vec<Arc<()>>,
    pub(crate) target_leases: crate::execution::TargetLeases,
    pub(crate) traces: crate::trace::Traces,
    environments: crate::environments::Registry,
    pub(crate) environment_managed: bool,
    pub(crate) default_environment: Option<String>,
    default_configuration: Option<default_environment::DefaultEnvironment>,
    pub(crate) environment_record: Option<crate::environments::EnvironmentRecord>,
    pub(crate) environment_records: Vec<crate::environments::EnvironmentRecord>,
    pub(crate) environment_loader: Option<Arc<dyn crate::environments::EnvironmentLoader>>,
    pub(crate) environment_images: crate::environments::ExecutionImages,
    owner: Arc<()>,
    revision: u64,
    runtime: Runtime<BoundTask>,
    providers: Providers,
    importers: Importers,
    bindings: Bindings,
    predictions: IndexMap<NodeId, Arc<Typing>>,
    templates: Templates,
    contracts: ContractRegistry,
    calc_services: Option<Arc<dyn crate::calc::LocalServices>>,
    describe_service: Option<Arc<dyn crate::describe::DescribeService>>,
    home_identity: Option<String>,
    saved_names: Option<tokio::sync::watch::Receiver<Arc<[WorkspaceName]>>>,
}
impl Default for Workspace {
    fn default() -> Self {
        Self::new()
    }
}
impl Workspace {
    pub fn with_describe_service(
        mut self,
        service: Arc<dyn crate::describe::DescribeService>,
    ) -> Self {
        self.describe_service = Some(service);
        self
    }
    pub(crate) fn default_document(
        &self,
    ) -> Result<
        Option<crate::environments::LoadedDefinitions>,
        wes_core::environments::EnvironmentError,
    > {
        match (&self.default_configuration, &self.environment_loader) {
            (Some(configuration), Some(loader)) => {
                configuration.document(loader.as_ref()).map(Some)
            }
            _ => Ok(None),
        }
    }
    /// Install the initial namespace before admitting source. Captured imports may extend it;
    /// selecting another environment never consults it.
    pub fn with_default_environment(
        mut self,
        name: &str,
        products: Vec<Arc<crate::imports::ImportProduct>>,
    ) -> Result<Self, WorkspaceError> {
        let authority = self.environment_loader.as_ref().and_then(|l| l.authority());
        for product in &products {
            self.providers.register_all_ports(
                product.description().clone(),
                product.invoker().clone(),
                product.streams().cloned(),
                product.conversations().cloned(),
            );
        }
        let configuration =
            default_environment::DefaultEnvironment::new(name, products, authority.clone())?;
        let publication = configuration.prepare(&self.environments, &self.environment_images)?;
        if let Some(authority) = &authority {
            authority
                .enable(
                    publication
                        .registry
                        .inspect(name)
                        .expect("configured")
                        .identity(),
                )
                .map_err(|_| {
                    rejected(
                        "ENV010",
                        Span::at(0),
                        "built-in environment authority is unavailable",
                    )
                })?;
        }
        self.environments = publication.registry;
        self.environment_images = publication.images;
        self.default_configuration = Some(publication.configuration);
        self.environment_managed = true;
        self.default_environment = Some(name.into());
        Ok(self)
    }
    pub(crate) fn default_environment_context(
        &self,
    ) -> Option<wes_core::environments::EnvironmentContext> {
        self.default_environment
            .as_ref()
            .map(|name| wes_core::environments::EnvironmentContext {
                selected: Some(name.clone()),
                revisions: self.environments.revisions(),
            })
    }
    pub fn with_environment_loader(
        mut self,
        loader: Arc<dyn crate::environments::EnvironmentLoader>,
    ) -> Self {
        self.environment_loader = Some(loader);
        self
    }
    pub fn environments(&self) -> &crate::environments::Registry {
        &self.environments
    }
    pub(crate) fn check_environment_plan(
        &self,
        plan: &crate::environments::Plan,
    ) -> Result<(), WorkspaceError> {
        self.next_revision()?;
        if self.runtime.is_closed() {
            return Err(RuntimeError::Closed.into());
        }
        if self.environment_loader.is_some() {
            for removed in plan.changes().iter().filter(|c| c.after.is_none()) {
                let Some(old) = self.environments.inspect(&removed.name) else {
                    continue;
                };
                let renamed = plan
                    .changes()
                    .iter()
                    .filter_map(|c| plan.inspect(&c.name))
                    .any(|e| e.identity() == old.identity());
                if renamed {
                    continue;
                }
                if plan.environments().any(|environment| {
                    let mut parent = environment.parent();
                    while let Some(current) = parent {
                        if current.identity() == old.identity() {
                            return true;
                        }
                        parent = current.parent();
                    }
                    false
                }) {
                    return Err(rejected(
                        "ENV025",
                        Span::at(0),
                        "environment still has captured child references; delete never cascades",
                    ));
                }
                if !old.is_retired()
                    || self.target_leases.references(old.identity())
                    || self.runtime.graph().nodes().any(|node| {
                        node.payload()
                            .environments()
                            .any(|b| b.environment().identity() == old.identity())
                    })
                {
                    return Err(rejected(
                        "ENV025",
                        Span::at(0),
                        "delete requires a retired environment with no bound graph nodes or active execution sessions; removal never cascades",
                    ));
                }
            }
        }
        self.environments
            .validate_plan(plan)
            .map_err(|e| rejected(e.code, Span::at(0), e.message))
    }
    pub(crate) fn apply_environment_plan(
        &mut self,
        plan: crate::environments::Plan,
    ) -> Result<Vec<crate::environments::Change>, WorkspaceError> {
        self.check_environment_plan(&plan)?;
        self.environment_managed = true;
        let next = self.next_revision()?;
        let changes = self
            .environments
            .apply(plan)
            .map_err(|e| rejected(e.code, Span::at(0), e.message))?;
        if self
            .default_environment
            .as_ref()
            .is_some_and(|name| !self.environments.is_configured(name))
        {
            self.default_configuration = None;
        }
        if !changes.is_empty() {
            self.revision = next;
        }
        Ok(changes)
    }
    pub fn new() -> Self {
        Self {
            recordings: Default::default(),
            dataset_access: Default::default(),
            views: Default::default(),
            sandbox_store: None,
            sandbox_runtime: false,
            safe_observation: None,
            sandbox_leases: vec![],
            target_leases: Default::default(),
            traces: crate::trace::Traces::default(),
            environments: crate::environments::Registry::default(),
            environment_managed: false,
            default_environment: None,
            default_configuration: None,
            environment_record: None,
            environment_records: vec![],
            environment_loader: None,
            environment_images: crate::environments::ExecutionImages::default(),
            owner: Arc::new(()),
            revision: 0,
            runtime: Runtime::new(),
            providers: Providers::new(),
            importers: Importers::default(),
            bindings: Bindings::new(),
            predictions: IndexMap::new(),
            templates: Templates::new(),
            contracts: ContractRegistry::new(),
            calc_services: None,
            describe_service: None,
            saved_names: None,
            home_identity: None,
        }
    }
    /// Explicit in-process execution domain, supplied again by the embedding owner on restore.
    /// Managed desktop workspaces use environment bindings instead.
    pub fn local(scope: crate::providers::LocalScope) -> Self {
        Self {
            providers: Providers::local(scope),
            ..Self::new()
        }
    }
    /// Composition-time default for future finite starts; held restoration still never executes.
    pub fn with_default_timeout(mut self, budget: Duration) -> Result<Self, WorkspaceError> {
        self.runtime.set_default_timeout(budget)?;
        Ok(self)
    }
    pub fn with_sandbox_store(
        mut self,
        store: Arc<dyn crate::session::sandbox::DefinitionStore>,
    ) -> Self {
        self.sandbox_store = Some(store);
        self
    }
    /// Capture immutable language/provider configuration, never parent nodes, values or history.
    /// Authority ports remain shared so revocation affects already running sandboxes.
    pub(crate) fn sandbox_workspace(&self) -> Result<Self, WorkspaceError> {
        let mut child = Self::new();
        child.sandbox_runtime = true;
        child.providers = self.providers.clone();
        child.importers = self.importers.clone();
        child.templates = self.templates.clone();
        child.contracts = self.contracts.clone();
        child
            .views
            .install_catalogue(self.views.catalogue().clone());
        child.calc_services = self.calc_services.clone();
        child.environments = self.environments.planning_snapshot();
        child.environment_managed = self.environment_managed;
        child.default_environment = self.default_environment.clone();
        child.environment_images = self.environment_images.clone();
        child.environment_loader = self.environment_loader.clone();
        child.target_leases = self.target_leases.clone();
        child.sandbox_leases = self
            .environments
            .names()
            .filter_map(|name| self.environments.inspect(name))
            .map(|environment| {
                self.target_leases
                    .acquire(environment.identity())
                    .map_err(|message| rejected("SBX003", Span::at(0), message))
            })
            .collect::<Result<_, _>>()?;
        child.home_identity = self.home_identity.clone();
        child.saved_names = self.saved_names.clone();
        Ok(child)
    }
    /// Application-owned immutable metadata. Query capture clones the current image without I/O.
    pub fn with_home_identity(mut self, identity: String) -> Self {
        self.home_identity = Some(identity);
        self
    }
    pub fn home_identity(&self) -> Option<&str> {
        self.home_identity.as_deref()
    }
    pub fn set_saved_workspace_names(
        &mut self,
        names: tokio::sync::watch::Receiver<Arc<[WorkspaceName]>>,
    ) {
        self.saved_names = Some(names);
    }
    pub fn saved_workspace_names(&self) -> Arc<[WorkspaceName]> {
        self.saved_names
            .as_ref()
            .map(|names| names.borrow().clone())
            .unwrap_or_default()
    }
    /// Composition-time boundary port, captured by newly prepared calculations.
    pub fn with_calculation_services(
        mut self,
        reader: Arc<dyn crate::calc::LocalServices>,
    ) -> Self {
        self.calc_services = Some(reader);
        self
    }
    pub fn runtime(&self) -> &Runtime<BoundTask> {
        &self.runtime
    }
    pub(crate) fn update_execution_progress(
        &mut self,
        run: &Run,
        progress: crate::driver::progress::ExecutionProgress,
    ) -> bool {
        let progress = if progress.blocked_by(&self.dataset_access) {
            progress.restricted(&wes_core::flow::FlowPolicy::default().private())
        } else {
            progress
        };
        self.runtime.update_progress(run, progress)
    }
    pub fn catalogue(&self) -> &Catalogue {
        self.providers.catalogue()
    }
    pub(crate) fn importer_entry(&self, kind: &str) -> Option<Arc<dyn Importer>> {
        self.importers.entry(kind)
    }
    pub fn importer_names(&self) -> impl Iterator<Item = &str> {
        self.importers.names()
    }
    pub fn importer_metadata(&self) -> &IndexMap<String, crate::imports::ImporterMetadata> {
        self.importers.metadata()
    }
    pub fn importer_parameters(&self) -> &IndexMap<String, Vec<wes_core::capability::Parameter>> {
        self.importers.parameters()
    }
    pub fn register_importer(
        &mut self,
        kind: String,
        importer: Arc<dyn Importer>,
    ) -> Result<Option<Arc<dyn Importer>>, WorkspaceError> {
        let next = self.next_revision()?;
        let previous = self
            .importers
            .register(kind, importer)
            .map_err(|error| rejected("IMP001", Span::at(0), error.to_string()))?;
        self.revision = next;
        Ok(previous)
    }
    pub fn prepare_import(
        &self,
        prepared: PreparedMeta,
        captured: &CapturedImport,
    ) -> Result<PreparedChange, WorkspaceError> {
        self.check_stamp(&prepared.stamp)?;
        self.next_revision()?;
        let request = prepared.import_request()?;
        let mut change = analysis::prepare_import(
            prepared.import_step(request)?,
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
                Change::EnvironmentProvider(Box::new(default_environment::import_document(
                    current,
                    loader.as_ref(),
                    name,
                    captured,
                    &self.environments,
                    &self.environment_images,
                    self.environment_records.iter().map(|r| r.charge()).sum(),
                )?));
        }
        Ok(change)
    }
    pub fn bindings(&self) -> &Bindings {
        &self.bindings
    }
    pub fn templates(&self) -> &Templates {
        &self.templates
    }
    pub fn view_catalogue(&self) -> &wes_views::Catalogue {
        self.views.catalogue()
    }
    pub fn contracts(&self) -> &ContractRegistry {
        &self.contracts
    }
    pub fn resolve(&self, name: &str) -> Option<OutputRef> {
        self.bindings.resolve(name, self.runtime.graph())
    }
    pub fn data_typing(&self, node: &NodeId) -> Option<&Typing> {
        self.data_typing_handle(node).map(Arc::as_ref)
    }
    pub(crate) fn data_typing_handle(&self, node: &NodeId) -> Option<&Arc<Typing>> {
        self.runtime.graph().node(node)?;
        self.runtime
            .actual_typing(node)
            .or_else(|| self.predictions.get(node))
    }
    pub fn typing(&self, output: &OutputRef) -> Option<Typing> {
        self.runtime.graph().node(&output.node)?;
        match output.port {
            OutputPort::Data => self.data_typing(&output.node).cloned(),
            OutputPort::Error => Some(Typing::new(ErrorValue::shape())),
            OutputPort::Cancel => Some(Typing::new(ErrorValue::cancellation_shape())),
        }
    }
    /// Construction/import boundary. Callers must coordinate any descriptor I/O and durable command
    /// admission before installing imported providers. Existing nodes retain their captured handles.
    pub(crate) fn interactive_provider_names(&self) -> Vec<String> {
        self.providers.interactive_names()
    }
    pub fn register_provider(
        &mut self,
        description: ProviderDescription,
        invoker: Arc<dyn Invoker>,
    ) -> Result<Option<Arc<Provider>>, WorkspaceError> {
        self.register_provider_ports(description, invoker, None)
    }
    pub fn register_provider_ports(
        &mut self,
        description: ProviderDescription,
        invoker: Arc<dyn Invoker>,
        streams: Option<Arc<dyn crate::streams::StreamingInvoker>>,
    ) -> Result<Option<Arc<Provider>>, WorkspaceError> {
        self.register_provider_all_ports(description, invoker, streams, None)
    }
    pub fn register_provider_all_ports(
        &mut self,
        description: ProviderDescription,
        invoker: Arc<dyn Invoker>,
        streams: Option<Arc<dyn crate::streams::StreamingInvoker>>,
        conversations: Option<Arc<dyn crate::conversations::InteractiveInvoker>>,
    ) -> Result<Option<Arc<Provider>>, WorkspaceError> {
        if self.templates.contains(description.name()) {
            return Err(rejected(
                "TMP002",
                Span::at(0),
                "provider name is already claimed by a template",
            ));
        }
        let next = self.next_revision()?;
        let previous =
            self.providers
                .register_all_ports(description, invoker, streams, conversations);
        self.revision = next;
        Ok(previous)
    }
    pub fn prepare_type_package(
        &self,
        yaml: &str,
        span: Span,
    ) -> Result<PreparedChange, WorkspaceError> {
        self.next_revision()?;
        analysis::prepare_types(self.stamp(), &self.contracts, yaml, span)
    }
    pub fn prepare(&self, statement: &Statement) -> Result<Preparation, WorkspaceError> {
        if self.runtime.is_closed() {
            return Err(RuntimeError::Closed.into());
        }
        analysis::Analysis {
            reserved_names: &[],
            pipe_input: None,
            environment_context: None,
            stamp: self.stamp(),
            graph: self.runtime.graph(),
            providers: &self.providers,
            importers: &self.importers,
            bindings: &self.bindings,
            templates: &self.templates,
            contracts: &self.contracts,
            views: self.views.catalogue(),
            calc_services: &self.calc_services,
            describe_service: &self.describe_service,
            calculation_package: wes_language::calc::Package::standard(),
            data_typing: &|node| self.data_typing(node),
            allocation: analysis::NodeAllocation::Automatic,
        }
        .prepare(statement)
    }
    pub fn prepare_type_load(
        &self,
        prepared: PreparedMeta,
        package: &CapturedTypePackage,
    ) -> Result<PreparedChange, WorkspaceError> {
        self.check_stamp(&prepared.stamp)?;
        self.next_revision()?;
        analysis::prepare_type_load(prepared, &self.contracts, self.views.catalogue(), package)
    }
    /// A coordinator must obtain any required journal acknowledgement before this operation and
    /// before starting the returned node. Preparation alone never changes the workspace.
    pub fn commit(&mut self, prepared: PreparedChange) -> Result<Applied, WorkspaceError> {
        self.commit_installation(prepared, Installation::Live)
    }
    fn commit_installation(
        &mut self,
        prepared: PreparedChange,
        installation: Installation,
    ) -> Result<Applied, WorkspaceError> {
        self.check_stamp(&prepared.stamp)?;
        let next = self.next_revision()?;
        let mut diagnostics = prepared.diagnostics;
        let node = match prepared.operation {
            Change::Provider(product) => {
                self.providers.register_all_ports(
                    product.description().clone(),
                    product.invoker().clone(),
                    product.streams().cloned(),
                    product.conversations().cloned(),
                );
                None
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
                self.environments = publication.registry;
                self.environment_images = publication.images;
                self.default_configuration = Some(publication.configuration);
                None
            }
            Change::EnvironmentProvider(publication) => {
                self.environments = publication.registry;
                self.environment_images = publication.images;
                self.environment_records.push(publication.record.clone());
                self.environment_record = Some(publication.record);
                None
            }
            Change::Define {
                templates,
                information,
            } => {
                self.templates = templates;
                diagnostics.push(information);
                None
            }
            Change::Types {
                views,
                registry,
                information,
            } => {
                self.contracts = registry;
                if let Some(views) = views {
                    self.views.install_catalogue(views);
                }
                diagnostics.push(information);
                None
            }
            Change::Aliases(aliases) => {
                // Stage the complete binding set so a failure cannot install only its first name.
                let mut bindings = self.bindings.clone();
                for (name, output) in aliases {
                    bindings
                        .bind(name, output, self.runtime.graph())
                        .map_err(binding_error)?;
                }
                self.bindings = bindings;
                None
            }
            Change::Node {
                node,
                task,
                admission,
                activation,
                stream_origin,
                pipeline,
                typing,
                names,
            } => {
                let dependencies = task.dependencies().collect::<Vec<_>>();
                let traits = task.traits();
                let dependency_lifetime = task.dependency_lifetime();
                match (installation, admission) {
                    (Installation::Live, Installation::Live) => {
                        self.runtime
                            .add_at(node.clone(), task, dependencies, traits)?
                    }
                    _ => self.runtime.restore(
                        node.clone(),
                        task,
                        dependencies,
                        traits,
                        crate::runtime::RestoredState::Stale,
                        None,
                    )?,
                }
                self.runtime
                    .install_dependency_lifetime(&node, dependency_lifetime);
                self.runtime.install_activation(&node, activation);
                self.runtime.install_pipeline_step(&node, pipeline);
                if let Some(root) = stream_origin {
                    self.runtime.install_ordered_stage(&node, &root);
                }
                self.predictions.insert(node.clone(), typing);
                // Namespace/revision checks and ID selection guarantee these bindings cannot fail.
                for (name, port) in names {
                    self.bindings
                        .bind(
                            name,
                            OutputRef {
                                node: node.clone(),
                                port,
                            },
                            self.runtime.graph(),
                        )
                        .expect("prepared names exclude the admitted ID");
                }
                Some(node)
            }
        };
        self.revision = next;
        Ok(Applied { node, diagnostics })
    }
    pub fn validate_meta(&self, prepared: &PreparedMeta) -> Result<(), WorkspaceError> {
        self.check_stamp(&prepared.stamp)
    }
    /// Waiting captures output references once, not binding names to re-resolve later. It neither
    /// starts nodes nor retains execution authority, so later aliases and declarations may proceed.
    pub fn prepare_wait(&self, prepared: PreparedMeta) -> Result<PreparedWait, WorkspaceError> {
        self.check_stamp(&prepared.stamp)?;
        let invalid = || {
            rejected(
                "MET006",
                prepared.span,
                "wait requires compatible existing output references",
            )
            .preceded_by(&prepared.diagnostics)
        };
        let Plan::Action { task, .. } = &prepared.plan else {
            return Err(invalid());
        };
        if task.spec.command != MetaCommand::Wait {
            return Err(invalid());
        }
        let outputs = task
            .subjects
            .iter()
            .map(|input| match input {
                Input::FromNode(output) => Ok(output.clone()),
                Input::Literal(_)
                | Input::FieldPath { .. }
                | Input::Record(_)
                | Input::List { .. } => Err(invalid()),
            })
            .collect::<Result<Vec<_>, _>>()?;
        if outputs.is_empty() {
            return Err(invalid());
        }
        let selected = crate::runtime::OutputSelection::new(outputs).map_err(|_| invalid())?;
        Ok(PreparedWait {
            selected,
            span: prepared.span,
            diagnostics: prepared.diagnostics,
        })
    }
    pub fn start(&mut self, now: Duration) -> Vec<Effect<BoundTask>> {
        self.runtime.start(now)
    }
    pub fn close(&mut self) -> Vec<Effect<BoundTask>> {
        self.runtime.close()
    }
    pub fn enter(&mut self, run: &Run) -> bool {
        self.runtime.enter(run)
    }
    /// Authorize a physical worker and capture query metadata at that same serialized boundary.
    /// Only the returned ticket may be executed. This never changes a bound provider or its inputs.
    pub fn enter_ticket(
        &mut self,
        mut ticket: crate::runtime::RunTicket<BoundTask>,
    ) -> Result<Option<crate::runtime::RunTicket<BoundTask>>, wes_core::ErrorValue> {
        let entered = if ticket
            .payload
            .call()
            .is_some_and(crate::providers::BoundCall::streaming)
        {
            self.runtime.enter_stream(&ticket.run)
        } else if ticket.payload.lifetime() {
            self.runtime.enter_lifetime(&ticket.run)
        } else {
            self.runtime.enter(&ticket.run)
        };
        if !entered {
            return Ok(None);
        }
        self.recordings.retire_obsolete(&self.runtime);
        self.check_task_bindings(&ticket.payload).map_err(|error| {
            wes_core::ErrorValue::new(
                wes_core::ErrorId::new(uuid::Uuid::new_v4().to_string()).expect("error identity"),
                error.code,
                error.message,
                vec![],
                None,
            )
            .expect("binding refusal")
        })?;
        if let BoundTask::SourceLaunch(launch) = &mut ticket.payload {
            launch.capture(self);
        }
        let call = match &mut ticket.payload {
            BoundTask::Call(call) => Some(call),
            BoundTask::SourceLaunch(launch) => Some(&mut launch.source),
            _ => None,
        };
        if let Some(call) = call {
            if let Some(node) = self.runtime.graph().node(ticket.run.node()) {
                call.set_source_definition(node.definition());
            }
            call.set_traces(self.traces.clone());
            call.set_stream_budget(self.runtime.stream_delivery_budget(ticket.run.node()));
        }
        if let BoundTask::View(view) = &mut ticket.payload {
            view.capture(self, ticket.run.node());
        }
        if let BoundTask::Query(query) = &mut ticket.payload {
            query.capture(self);
        }
        if let BoundTask::Recording(recording) = &mut ticket.payload {
            recording.capture(self);
        }
        if let BoundTask::Dataset(read) = &mut ticket.payload {
            read.capture(self);
        }
        if let BoundTask::Reconcile(reconcile) = &mut ticket.payload {
            reconcile.capture(self);
        }
        if let BoundTask::ScanExcerpt(excerpt) = &mut ticket.payload {
            excerpt.capture(self);
        }
        if let BoundTask::ScanResume(resume) = &mut ticket.payload {
            resume.capture(self);
        }
        if let BoundTask::Stream(op) = &mut ticket.payload {
            op.delivery = self
                .runtime
                .ordered_delivery(ticket.run.node())
                .map(|(_, _, seq)| seq);
        }
        if let BoundTask::Accumulation(accumulation) = &mut ticket.payload {
            accumulation.capture(&self.runtime, &ticket.run);
        }
        Ok(Some(ticket))
    }
    pub(crate) fn lifetime_value(
        &mut self,
        run: &Run,
        value: wes_core::Value,
        now: Duration,
    ) -> Option<Vec<Effect<BoundTask>>> {
        if self.dataset_access.blocks(&value) {
            return None;
        }
        self.runtime.lifetime_value(run, value, now)
    }
    pub(crate) fn stream_update(
        &mut self,
        update: crate::driver::streaming::Update,
        now: Duration,
    ) -> Vec<Effect<BoundTask>> {
        update.apply(&mut self.runtime, now)
    }
    pub fn complete(
        &mut self,
        run: &Run,
        outcome: Outcome,
        now: Duration,
    ) -> Vec<Effect<BoundTask>> {
        let outcome = match outcome {
            Outcome::Produced(value) | Outcome::Incomplete { value, .. }
                if self.dataset_access.blocks(&value) =>
            {
                Outcome::Failed(crate::runtime::RuntimeCode::InputFailed.error(
                    "Result access was withdrawn before publication; no source was replayed.",
                    None,
                ))
            }
            outcome => outcome,
        };
        let effects = self.runtime.complete(run, outcome, now);
        if self.views.contains(run.node())
            && self.runtime.value_of(run.node()).is_none()
            && self.runtime.graph().node(run.node()).is_some_and(|node| {
                matches!(
                    node.state(),
                    crate::graph::NodeState::Failed
                        | crate::graph::NodeState::Cancelled
                        | crate::graph::NodeState::Skipped
                )
            })
        {
            self.views.retire_node(run.node());
        }
        effects
    }
    pub(crate) fn apply_dataset_access(
        &mut self,
        access: crate::storage::datasets::DatasetAccess,
    ) -> (Vec<NodeId>, Vec<Effect<BoundTask>>) {
        let blocked = self
            .runtime
            .graph()
            .nodes()
            .filter_map(|node| {
                let value = self
                    .runtime
                    .value_of(node.id())
                    .or_else(|| self.runtime.evidence_value(node.id()).map(|e| &e.value));
                (value.is_some_and(|value| access.blocks(value))
                    || self
                        .runtime
                        .execution_progress(node.id())
                        .is_some_and(|progress| progress.blocked_by(&access)))
                .then(|| node.id().clone())
            })
            .collect::<Vec<_>>();
        self.dataset_access = access;
        let mut effects = Vec::new();
        for node in &blocked {
            effects.extend(self.runtime.withdraw(node));
        }
        self.views.withdraw_dataset_inputs(&self.dataset_access);
        (blocked, effects)
    }
    pub(crate) fn complete_report(
        &mut self,
        run: &Run,
        report: crate::driver::ExecutionReport,
        now: Duration,
    ) -> Vec<Effect<BoundTask>> {
        if let Some(start) = report.stream_start {
            self.runtime.set_stream_start(run, start);
        }
        if let Some(progress) = report.progress {
            self.update_execution_progress(run, progress);
        }
        self.complete(run, report.outcome, now)
    }
    pub fn cancel(&mut self, node: &NodeId, now: Duration) -> Vec<Effect<BoundTask>> {
        self.views.cancel_origins(std::slice::from_ref(node));
        self.runtime.cancel(node, now)
    }
    /// The publication owner has already matched an evicted handle. Guard the run again here:
    /// an old storage receipt must never remove a newer run's in-memory output or actual typing.
    pub(crate) fn forget_output(
        &mut self,
        node: &NodeId,
        run: Option<&crate::runtime::RunId>,
    ) -> Vec<Effect<BoundTask>> {
        if self.runtime.run_of(node) != run
            && self.runtime.evidence_value(node).map(|v| &v.run) != run
        {
            return vec![];
        }
        self.runtime.forget(node)
    }
    pub fn expire(&mut self, deadline: &Deadline, now: Duration) -> Vec<Effect<BoundTask>> {
        self.runtime.expire(deadline, now)
    }
    pub fn drop_node(
        &mut self,
        node: &NodeId,
    ) -> Result<(Vec<NodeId>, Vec<Effect<BoundTask>>), WorkspaceError> {
        let next = self.next_revision()?;
        let result = self.remove_node(node, Installation::Live)?;
        self.revision = next;
        Ok(result)
    }
    fn remove_node(
        &mut self,
        node: &NodeId,
        installation: Installation,
    ) -> Result<(Vec<NodeId>, Vec<Effect<BoundTask>>), WorkspaceError> {
        let (removed, effects) = match installation {
            Installation::Live => self.runtime.drop_node(node)?,
            Installation::Held => (self.runtime.drop_held_node(node)?, vec![]),
        };
        self.bindings.retain_nodes(self.runtime.graph());
        for node in &removed {
            self.predictions.shift_remove(node);
            self.views.retire_node(node);
        }
        Ok((removed.into_iter().collect(), effects))
    }
    fn stamp(&self) -> Stamp {
        Stamp {
            owner: self.owner.clone(),
            revision: self.revision,
        }
    }
    fn check_stamp(&self, stamp: &Stamp) -> Result<(), WorkspaceError> {
        if self.runtime.is_closed() {
            return Err(RuntimeError::Closed.into());
        }
        if !Arc::ptr_eq(&self.owner, &stamp.owner) || self.revision != stamp.revision {
            return Err(WorkspaceError::Obsolete);
        }
        Ok(())
    }
    fn next_revision(&self) -> Result<u64, WorkspaceError> {
        if self.runtime.is_closed() {
            return Err(RuntimeError::Closed.into());
        }
        self.revision
            .checked_add(1)
            .ok_or(WorkspaceError::RevisionExhausted)
    }
}
fn rejected(code: &'static str, span: Span, message: impl Into<String>) -> WorkspaceError {
    Diagnostic::error(code, span, message).into()
}
fn binding_error(error: BindingError) -> WorkspaceError {
    rejected("ENG004", Span::at(0), error.to_string())
}
