//! Shared read-only semantic preparation for live declarations and non-executing drafts.
mod stream;
use super::{Change, Preparation, PreparedChange, PreparedMeta, Stamp, WorkspaceError, rejected};
use crate::{
    bindings::{BindingError, Bindings},
    graph::{DependencyGraph, NodeId, OutputPort, OutputRef},
    plan::{self, Plan, Task},
    providers::Providers,
    tasks::{BoundHelp, BoundQuery, BoundTask, BoundTypeCheck},
};
use indexmap::IndexMap;
use std::sync::Arc;
use wes_core::{
    ErrorValue, Shape,
    capability::{Catalogue, Typing},
    contracts::ContractRegistry,
};
use wes_language::{
    Diagnostic, Expression, Severity, Span, Statement,
    check::{self, Environment, ExistingCall},
    resolve::{self, Resolution},
    templates::Templates,
    vocabulary::MetaCommand,
};
pub(super) fn prepare_import(
    prepared: super::PreparedImport,
    captured: &crate::imports::CapturedImport,
    importers: &crate::imports::Importers,
    providers: &Providers,
    templates: &Templates,
) -> Result<PreparedChange, WorkspaceError> {
    if !captured.matches(&prepared.request, importers) {
        return Err(rejected(
            "IMP003",
            prepared.span,
            "captured import does not match this request and importer revision",
        )
        .preceded_by(&prepared.diagnostics));
    }
    let product = captured.product();
    let name = product.description().name();
    if templates.contains(name) {
        return Err(rejected(
            "TMP002",
            prepared.span,
            "provider name is already claimed by a template",
        )
        .preceded_by(&prepared.diagnostics));
    }
    // Replacement requires explicit authorization in every import entry point.
    if providers.catalogue().provider(name).is_some() && !prepared.replace {
        return Err(rejected(
            "IMP004",
            prepared.span,
            "Provider already exists; choose another alias or explicitly use replace:true.",
        ));
    }
    let mut diagnostics = prepared.diagnostics;
    for warning in product.warnings() {
        diagnostics.push(
            Diagnostic::error("IMP002", prepared.span, warning).with_severity(Severity::Warning),
        );
    }
    if providers.catalogue().provider(name).is_some() {
        diagnostics.push(Diagnostic::error("IMP006",prepared.span,format!("provider '{name}' was replaced; existing nodes retain their captured invocation handles")).with_severity(Severity::Warning));
    }
    if wes_language::vocabulary::commands::is_command_root(name) {
        diagnostics.push(Diagnostic::error("IMP005",prepared.span,format!("provider '{name}' conflicts with a meta-command name; import under an alias to use it")).with_severity(Severity::Warning));
    }
    Ok(PreparedChange {
        stamp: prepared.stamp,
        operation: Change::Provider(product.clone()),
        diagnostics,
    })
}

pub(super) fn prepare_types(
    stamp: Stamp,
    contracts: &ContractRegistry,
    yaml: &str,
    span: Span,
) -> Result<PreparedChange, WorkspaceError> {
    let mut registry = contracts.clone();
    let previous_recipes = registry.iterators().len();
    let loaded = registry
        .load(yaml)
        .map_err(|e| rejected(e.code, span, e.message))?;
    let recipes = registry.iterators().len() - previous_recipes;
    let message = format!(
        "loaded {} type definitions{}",
        loaded.len(),
        if recipes > 0 {
            format!(" and {recipes} iterator recipes")
        } else {
            String::new()
        }
    );
    Ok(PreparedChange {
        stamp,
        operation: Change::Types {
            views: None,
            registry,
            information: Diagnostic::error("TYP000", span, message).with_severity(Severity::Info),
        },
        diagnostics: vec![],
    })
}

pub(super) fn prepare_type_load(
    prepared: PreparedMeta,
    contracts: &ContractRegistry,
    views: &wes_views::Catalogue,
    package: &crate::type_sources::CapturedTypePackage,
) -> Result<PreparedChange, WorkspaceError> {
    let requested = prepared
        .type_source()
        .map_err(|e| rejected("TYP002", prepared.span, e.to_string()))?;
    let matches = match requested {
        crate::type_sources::TypeInput::File(path) => path == package.path(),
        crate::type_sources::TypeInput::Text { source, origin } => {
            origin == package.path() && source == package.source()
        }
    };
    if !matches {
        return Err(rejected(
            "TYP007",
            prepared.span,
            "captured type source does not match requested input",
        )
        .preceded_by(&prepared.diagnostics));
    }
    if wes_views::Artifact::recognizes(package.source()) {
        let artifact = Arc::new(
            wes_views::Artifact::parse(package.source().as_bytes())
                .map_err(|e| rejected("VIE004", prepared.span, e))?,
        );
        let catalogue = views
            .installed(artifact.clone())
            .map_err(|e| rejected("VIE004", prepared.span, e))?;
        let mut registry = contracts.clone();
        if views.artifact(&artifact.digest).is_none() {
            registry
                .import_package(&artifact.source.types)
                .map_err(|e| rejected(e.code, prepared.span, e.message))?;
        }
        return Ok(PreparedChange {
            stamp: prepared.stamp,
            operation: Change::Types {
                views: Some(catalogue),
                registry,
                information: Diagnostic::error(
                    "VIE000",
                    prepared.span,
                    format!(
                        "installed view {} · {}",
                        artifact.package.manifest.name, artifact.digest
                    ),
                )
                .with_severity(Severity::Info),
            },
            diagnostics: prepared.diagnostics,
        });
    }
    let mut change = prepare_types(prepared.stamp, contracts, package.source(), prepared.span)
        .map_err(|error| error.preceded_by(&prepared.diagnostics))?;
    change.diagnostics = prepared.diagnostics;
    Ok(change)
}

pub(super) struct Analysis<'a> {
    pub reserved_names: &'a [String],
    pub pipe_input: Option<&'a OutputRef>,
    pub environment_context: Option<&'a wes_core::environments::EnvironmentContext>,
    pub stamp: Stamp,
    pub graph: &'a DependencyGraph<BoundTask>,
    pub providers: &'a Providers,
    pub importers: &'a crate::imports::Importers,
    pub bindings: &'a Bindings,
    pub templates: &'a Templates,
    pub contracts: &'a ContractRegistry,
    pub views: &'a wes_views::Catalogue,
    pub calc_services: &'a Option<Arc<dyn crate::calc::LocalServices>>,
    pub describe_service: &'a Option<Arc<dyn crate::describe::DescribeService>>,
    pub calculation_package: Arc<wes_language::calc::Package>,
    pub data_typing: &'a dyn Fn(&NodeId) -> Option<&'a Typing>,
    pub allocation: NodeAllocation<'a>,
}
pub(super) enum NodeAllocation<'a> {
    Automatic,
    Recorded(Option<&'a NodeId>),
}
impl Analysis<'_> {
    fn stamp(&self) -> Stamp {
        self.stamp.clone()
    }
    fn catalogue(&self) -> &Catalogue {
        self.providers.catalogue()
    }
    fn resolve(&self, name: &str) -> Option<OutputRef> {
        self.bindings.resolve(name, self.graph)
    }
    fn data_typing(&self, node: &NodeId) -> Option<&Typing> {
        (self.data_typing)(node)
    }
    fn typing(&self, output: &OutputRef) -> Option<Typing> {
        self.graph.node(&output.node)?;
        match output.port {
            OutputPort::Data => self.data_typing(&output.node).cloned(),
            OutputPort::Error => Some(Typing::new(ErrorValue::shape())),
            OutputPort::Cancel => Some(Typing::new(ErrorValue::cancellation_shape())),
        }
    }
    fn change(&self, operation: Change, diagnostics: Vec<Diagnostic>) -> PreparedChange {
        PreparedChange {
            stamp: self.stamp(),
            operation,
            diagnostics,
        }
    }
    pub fn prepare(&self, statement: &Statement) -> Result<Preparation, WorkspaceError> {
        if statement.annotations.iter().any(|a| a.name.text == "trace")
            && !matches!(statement.expression, Expression::Call(_))
        {
            return Err(rejected(
                "TRC001",
                statement.span,
                "Use @trace(profile) on a finite provider call.",
            ));
        }
        match &statement.expression {
            Expression::Sandbox(_) => Err(rejected(
                "SBX002",
                statement.span,
                "sandbox definitions require the session owner and cannot be nested in recorded work",
            )),
            Expression::Fork(_) | Expression::Pipeline(_) => Err(rejected(
                "PIP001",
                statement.span,
                "pipelines require source declaration preparation",
            )),
            Expression::Calculation(source) => self.calculation(statement, source),
            Expression::Definition(definition) => {
                if statement.binding.is_some()
                    || statement.error_binding.is_some()
                    || !statement.annotations.is_empty()
                {
                    return Err(rejected(
                        "TMP001",
                        statement.span,
                        "template definitions cannot have output bindings or annotations",
                    ));
                }
                if self.occupied(&definition.name.text) {
                    return Err(rejected(
                        "TMP002",
                        definition.name.span,
                        "template name is already claimed",
                    ));
                }
                if let wes_language::TemplateBody::Call(body) = &definition.body
                    && body.path.first().is_some_and(|head| {
                        wes_language::vocabulary::commands::is_command_root(&head.text)
                    })
                {
                    return Err(rejected(
                        "TMP001",
                        body.span,
                        "template bodies cannot execute meta commands",
                    ));
                }
                let mut templates = self.templates.clone();
                templates
                    .define_calculation(
                        definition.clone(),
                        self.contracts,
                        self.catalogue(),
                        self.calculation_package.clone(),
                    )
                    .map_err(WorkspaceError::from)?;
                Ok(Preparation::Change(
                    self.change(
                        Change::Define {
                            templates,
                            information: Diagnostic::error(
                                "TMP000",
                                statement.span,
                                format!("defined template {}", definition.name.text),
                            )
                            .with_severity(Severity::Info),
                        },
                        vec![],
                    ),
                ))
            }
            Expression::Reference(name) => {
                let output = self
                    .resolve(&name.text)
                    .ok_or_else(|| rejected("ENG001", name.span, "unknown reference"))?;
                self.validate_names(statement)?;
                let mut aliases = vec![];
                if let Some(binding) = &statement.binding {
                    aliases.push((binding.name.text.clone(), output.clone()));
                }
                if let Some(binding) = &statement.error_binding {
                    aliases.push((
                        binding.name.text.clone(),
                        OutputRef {
                            node: output.node,
                            port: OutputPort::Error,
                        },
                    ));
                }
                Ok(Preparation::Change(
                    self.change(Change::Aliases(aliases), vec![]),
                ))
            }
            Expression::Call(call) => {
                if call.marker.is_some() && call.path.first().is_some_and(|n| n.text == "stream") {
                    return self.stream_operator(statement, call);
                }
                let was_template = call.marker.is_none()
                    && call
                        .path
                        .first()
                        .is_some_and(|head| self.templates.contains(&head.text));
                let call =
                    super::pipeline::bind_definition_input(call, self.pipe_input, self.templates);
                let expanded = self
                    .templates
                    .expand(&call, |name| self.occupied(name))
                    .map_err(WorkspaceError::from)?;
                if let Some(definition) = &expanded.calculation {
                    return self.calculation_invocation(statement, &expanded, definition.clone());
                }
                let resolution = resolve::resolve(&expanded.call, self.catalogue())
                    .map_err(WorkspaceError::from)?;
                if was_template && matches!(resolution, Resolution::Meta { .. }) {
                    return Err(rejected(
                        "TMP001",
                        statement.span,
                        "template expansion cannot execute a meta command",
                    ));
                }
                let validated = match &resolution {
                    Resolution::Capability { capability, .. } => {
                        plan::guarded_shapes(&expanded.contracts, capability)
                            .map_err(|e| rejected(e.code, statement.span, e.message))?
                    }
                    Resolution::Meta { .. } => IndexMap::new(),
                };
                let environment = self.environment(resolution.call());
                // Replay installs held declarations before values are hydrated. Unknown runtime
                // input shapes are checked at dispatch, not guessed from today's missing values.
                // Only the checker view is refined; no type/provenance evidence is persisted.
                let mut checked_shapes = validated.clone();
                if (matches!(self.allocation, NodeAllocation::Recorded(_))
                    || self.pipe_input.is_some())
                    && let Resolution::Capability { capability, .. } = &resolution
                {
                    for argument in &resolution.call().arguments {
                        if argument.value.references().iter().any(|name| {
                            environment
                                .bindings
                                .get(&name.text)
                                .is_some_and(|typing| typing.shape == Shape::Unknown)
                        }) && let Some(parameter) = capability.parameter(&argument.key.text)
                        {
                            checked_shapes
                                .entry(argument.key.text.clone())
                                .or_insert_with(|| parameter.shape.clone());
                        }
                    }
                }
                let diagnostics = check::check(
                    &resolution,
                    &statement.annotations,
                    &checked_shapes,
                    &environment,
                );
                if diagnostics.iter().any(|d| d.severity == Severity::Error) {
                    return Err(WorkspaceError::Rejected {
                        diagnostics,
                        issues: vec![],
                    });
                }
                self.validate_names(statement)
                    .map_err(|error| error.preceded_by(&diagnostics))?;
                let expanded_statement = Statement {
                    expression: Expression::Call(expanded.call),
                    ..statement.clone()
                };
                let expectations = match &resolution {
                    Resolution::Meta { spec, call, .. } if spec.command == MetaCommand::Change => {
                        call.operands
                            .first()
                            .and_then(|v| {
                                if let wes_language::Value::Reference(name) = v {
                                    environment.calls.get(&name.text)
                                } else {
                                    None
                                }
                            })
                            .map(|existing| {
                                existing
                                    .capability
                                    .parameters
                                    .iter()
                                    .map(|p| (p.name.clone(), p.shape.clone()))
                                    .collect()
                            })
                            .unwrap_or_default()
                    }
                    Resolution::Meta { spec, tail, .. }
                        if spec.command == MetaCommand::ImportPlan =>
                    {
                        tail.first()
                            .and_then(|kind| self.importers.parameters().get(kind))
                            .map(|parameters| {
                                parameters
                                    .iter()
                                    .map(|p| (p.name.clone(), p.shape.clone()))
                                    .collect()
                            })
                            .unwrap_or_default()
                    }
                    Resolution::Meta { spec, tail, .. }
                        if spec.command == MetaCommand::ViewCreate =>
                    {
                        tail.first()
                            .and_then(|name| self.views.get(name))
                            .map(|package| [("input".into(), package.input().shape())].into())
                            .unwrap_or_default()
                    }
                    Resolution::Meta { spec, call, .. }
                        if spec.command == MetaCommand::ViewBind =>
                    {
                        call.operands
                            .first()
                            .and_then(|v| {
                                if let wes_language::Value::Reference(name) = v {
                                    self.resolve(&name.text)
                                } else {
                                    None
                                }
                            })
                            .and_then(|output| self.graph.node(&output.node))
                            .and_then(|node| {
                                if let BoundTask::View(view) = node.payload() {
                                    view.input_shape()
                                } else {
                                    None
                                }
                            })
                            .map(|shape| [("input".into(), shape)].into())
                            .unwrap_or_default()
                    }
                    _ => IndexMap::new(),
                };
                let plan = plan::plan_with_expectations(
                    &resolution,
                    &expanded_statement,
                    &expanded.contracts,
                    &|name| self.resolve(name),
                    &expectations,
                )
                .map_err(|e| {
                    (match e {
                        plan::PlanError::Rejected { diagnostic, issues } => {
                            WorkspaceError::Rejected {
                                diagnostics: vec![diagnostic],
                                issues,
                            }
                        }
                        plan::PlanError::Cancelled => WorkspaceError::Cancelled,
                    })
                    .preceded_by(&diagnostics)
                })?;
                let Plan::NewNode(node) = plan else {
                    return Ok(Preparation::Meta(PreparedMeta {
                        stamp: self.stamp(),
                        plan,
                        diagnostics,
                        span: statement.span,
                    }));
                };
                let (task, typing) = match node.task {
                    Task::Invoke(invocation) => {
                        let typing =
                            Arc::new(invocation.predicted_typing(|node| self.data_typing(node)));
                        let call = if invocation.capability.streaming {
                            self.providers.bind_stream(invocation)
                        } else if invocation.interactive {
                            self.providers.bind_interactive(invocation)
                        } else {
                            self.providers.bind_finite(invocation)
                        }
                        .map_err(|e| {
                            rejected("ENG006", statement.span, e.to_string())
                                .preceded_by(&diagnostics)
                        })?;
                        (
                            BoundTask::Call(call.with_pipe_input(self.pipe_input.cloned())),
                            typing,
                        )
                    }
                    Task::Meta(task)
                        if crate::tasks::view::BoundView::supports(task.spec.command) =>
                    {
                        if task.inputs.values().any(|input| {
                            !input.dependencies().is_empty()
                                && matches!(
                                    input,
                                    plan::Input::Record(_) | plan::Input::List { .. }
                                )
                        }) {
                            return Err(rejected(
                                "VIE003",
                                statement.span,
                                "Compose referenced values in a named pure :calc or adapter, then bind that result; a view follows one source.",
                            ));
                        }
                        if (self.pipe_input.is_some()
                            && task.spec.command != MetaCommand::ViewCreate)
                            || !statement.annotations.is_empty()
                        {
                            return Err(rejected(
                                "VIE003",
                                statement.span,
                                "View edits use explicit references, without execution annotations or pipeline input",
                            ));
                        }
                        if task.spec.command == MetaCommand::ViewPin
                            && statement
                                .binding
                                .as_ref()
                                .is_some_and(|binding| self.resolve(&binding.name.text).is_some())
                        {
                            return Err(rejected(
                                "NAM003",
                                statement.span,
                                "Pin output name is already bound; choose an unused result name",
                            ));
                        }
                        let mut view =
                            crate::tasks::view::BoundView::bind(task, statement.span, self.views)
                                .map_err(WorkspaceError::from)?;
                        view.pipe_input = self.pipe_input.cloned();
                        view.environment = self.environment_context.cloned();
                        let shape = if matches!(
                            view.command(),
                            MetaCommand::ViewOutput
                                | MetaCommand::ViewCapture
                                | MetaCommand::ViewPin
                        ) {
                            Shape::Unknown
                        } else {
                            Shape::Meta(wes_core::MetaType::ViewInstance)
                        };
                        (BoundTask::View(view), Arc::new(Typing::new(shape)))
                    }
                    Task::Meta(task) if task.spec.command == MetaCommand::WorkspacePlan => {
                        let command = wes_language::vocabulary::workspace_management(statement)
                            .map_err(WorkspaceError::from)?;
                        let wes_language::vocabulary::WorkspaceManagementCommand::Plan {
                            workspace,
                            ..
                        } = command
                        else {
                            unreachable!()
                        };
                        let workspace = workspace
                            .map(super::WorkspaceName::new)
                            .transpose()
                            .map_err(|_| {
                                rejected("MET012", statement.span, "Invalid workspace name")
                            })?;
                        (
                            BoundTask::Management(crate::tasks::management::BoundManagement {
                                workspace,
                                context: None,
                            }),
                            Arc::new(Typing::new(Shape::Meta(
                                wes_core::MetaType::WorkspaceDeletePlan,
                            ))),
                        )
                    }
                    Task::Meta(task) if task.spec.command == MetaCommand::ImportPlan => {
                        if self.pipe_input.is_some() || !statement.annotations.is_empty() {
                            return Err(rejected(
                                "IMP001",
                                statement.span,
                                "Import planning cannot be a pipeline stage or carry annotations",
                            ));
                        }
                        let parameters = task
                            .tail
                            .first()
                            .and_then(|kind| self.importers.parameters().get(kind))
                            .cloned()
                            .ok_or_else(|| {
                                rejected("IMP001", statement.span, "Importer is unavailable")
                            })?;
                        let plan = crate::tasks::import_plan::BoundImportPlan::bind(
                            task,
                            parameters,
                            self.environment_context.cloned(),
                        )
                        .map_err(|message| rejected("IMP001", statement.span, message))?;
                        (
                            BoundTask::ImportPlan(plan),
                            Arc::new(Typing::new(Shape::Meta(wes_core::MetaType::ImportPlan))),
                        )
                    }
                    Task::Meta(task) if task.spec.command == MetaCommand::Describe => {
                        if self.pipe_input.is_some() {
                            return Err(rejected(
                                "DSC001",
                                statement.span,
                                "describe takes an explicit documentation source, not pipeline input",
                            ));
                        }
                        let described = crate::describe::BoundDescribe::bind(
                            task,
                            self.describe_service.clone(),
                            statement.span,
                        )
                        .map_err(WorkspaceError::from)?;
                        (
                            BoundTask::Describe(described),
                            Arc::new(Typing {
                                shape: Shape::Unknown,
                                provenance: Default::default(),
                            }),
                        )
                    }
                    Task::Meta(task) if task.spec.command == MetaCommand::Type => {
                        let checked = BoundTypeCheck::bind(task, self.contracts, statement.span)
                            .map_err(|error| {
                                WorkspaceError::from(error).preceded_by(&diagnostics)
                            })?;
                        let typing =
                            Arc::new(checked.predicted_typing(|output| self.typing(output)));
                        (BoundTask::TypeCheck(checked), typing)
                    }
                    Task::Meta(task) if task.spec.command == MetaCommand::Accumulate => {
                        let accumulation = crate::accumulation::BoundAccumulation::bind(
                            task,
                            self.pipe_input,
                            statement.span,
                        )?;
                        let typing =
                            Arc::new(accumulation.predicted_typing(|output| self.typing(output)));
                        (BoundTask::Accumulation(accumulation), typing)
                    }
                    Task::Meta(task) if task.spec.command == MetaCommand::Help => {
                        let help = BoundHelp::new(
                            task.target.clone().expect("resolved help target"),
                            self.catalogue(),
                        )
                        .with_importers(self.importers.parameters())
                        .map_err(|message| rejected("RES005", statement.span, message))?;
                        let typing = Arc::new(help.predicted_typing());
                        (BoundTask::Help(help), typing)
                    }
                    Task::Meta(task) if BoundQuery::supports(&task) => {
                        let query = BoundQuery::new(task, self.environment_context.cloned());
                        let typing = Arc::new(query.predicted_typing());
                        (BoundTask::Query(query), typing)
                    }
                    Task::Meta(task) => {
                        return Ok(Preparation::Meta(PreparedMeta {
                            stamp: self.stamp(),
                            plan: Plan::NewNode(plan::NewNode {
                                task: Task::Meta(task),
                                ..node
                            }),
                            diagnostics,
                            span: statement.span,
                        }));
                    }
                };
                self.node(statement, task, typing, diagnostics)
            }
        }
    }
    fn calculation_invocation(
        &self,
        statement: &Statement,
        expanded: &wes_language::templates::Expanded,
        definition: Arc<wes_language::templates::CalculationDefinition>,
    ) -> Result<Preparation, WorkspaceError> {
        self.validate_names(statement)?;
        if !statement.annotations.is_empty() {
            return Err(rejected(
                "CAL002",
                statement.span,
                "calculation annotations are not supported",
            ));
        }
        let mut inputs = IndexMap::new();
        for argument in &expanded.call.arguments {
            let checks = &expanded.contracts[&argument.key.text];
            let expected =
                wes_core::contracts::boundary::shape(checks, &Shape::Unknown).map_err(|_| {
                    rejected(
                        "TYP009",
                        argument.span,
                        "incompatible calculation parameter contracts",
                    )
                })?;
            let mut input = plan::input(&argument.value, &expected, &|name| self.resolve(name))
                .map_err(|error| match error {
                    plan::PlanError::Rejected { diagnostic, issues } => WorkspaceError::Rejected {
                        diagnostics: vec![diagnostic],
                        issues,
                    },
                    plan::PlanError::Cancelled => WorkspaceError::Cancelled,
                })?;
            if let plan::Input::Literal(value) = &mut input {
                *value = wes_core::contracts::boundary::literal(
                    &argument.key.text,
                    checks,
                    &Shape::Unknown,
                    value,
                )
                .map_err(|_| {
                    rejected(
                        "TYP005",
                        argument.span,
                        "literal does not satisfy calculation parameter contracts",
                    )
                })?;
            }
            inputs.insert(argument.key.text.clone(), input);
        }
        crate::runtime::OutputSelection::new(
            inputs.values().flat_map(plan::Input::dependencies).cloned(),
        )
        .map_err(|e| rejected("CAL002", statement.span, e.to_string()))?;
        let bound = crate::calc::BoundCalculation::bind(
            definition.compiled.clone(),
            IndexMap::new(),
            self.providers,
            self.calc_services.clone(),
        )
        .map_err(|_| {
            rejected(
                "CAL002",
                statement.span,
                "captured calculation provider is unavailable",
            )
        })?
        .with_parameters(inputs, expanded.contracts.clone(), definition.clone())
        .with_pipe_input(self.pipe_input.cloned());
        self.node(
            statement,
            BoundTask::Calculation(bound),
            Arc::new(Typing::new(definition.output.shape())),
            vec![],
        )
    }
    fn calculation(
        &self,
        statement: &Statement,
        source: &wes_language::CalculationSource,
    ) -> Result<Preparation, WorkspaceError> {
        use wes_language::calc;
        self.validate_names(statement)?;
        if !statement.annotations.is_empty() {
            return Err(rejected(
                "CAL002",
                statement.span,
                "calculation annotations are not supported",
            ));
        }
        let mut program = calc::parse_context(
            &source.text,
            source.span.start(),
            self.calculation_package.clone(),
        )
        .map_err(WorkspaceError::from)?;
        program.origin = source.origin.clone();
        program.origin_offset = 0;
        if let Some(input) = self.pipe_input {
            super::pipeline::bind_calculation_input(&mut program, input, source.span);
        }
        let compiled = calc::analyze(
            program.into(),
            calc::Environment {
                catalogue: self.catalogue(),
                contracts: self.contracts,
                workspace: &|name| {
                    self.resolve(name)
                        .map(|output| self.typing(&output).map_or(Shape::Unknown, |t| t.shape))
                },
            },
        )
        .map_err(WorkspaceError::from)?;
        let inputs: IndexMap<_, _> = compiled
            .workspace
            .keys()
            .map(|name| {
                (
                    name.clone(),
                    self.resolve(name).expect("analyzed dependency"),
                )
            })
            .collect();
        crate::runtime::OutputSelection::new(inputs.values().cloned())
            .map_err(|e| rejected("CAL002", source.span, e.to_string()))?;
        let output = compiled.output_shape();
        let bound = crate::calc::BoundCalculation::bind(
            compiled,
            inputs,
            self.providers,
            self.calc_services.clone(),
        )
        .map_err(|e| rejected("CAL002", source.span, e.to_string()))?;
        self.node(
            statement,
            BoundTask::Calculation(bound),
            Arc::new(Typing::new(output)),
            vec![],
        )
    }
    fn node(
        &self,
        statement: &Statement,
        task: BoundTask,
        typing: Arc<Typing>,
        diagnostics: Vec<Diagnostic>,
    ) -> Result<Preparation, WorkspaceError> {
        let names = statement
            .binding
            .as_ref()
            .map(|b| b.name.text.clone())
            .into_iter()
            .map(|name| (name, OutputPort::Data))
            .chain(
                statement
                    .error_binding
                    .as_ref()
                    .map(|b| b.name.text.clone())
                    .into_iter()
                    .map(|name| (name, OutputPort::Error)),
            )
            .collect::<Vec<_>>();
        let id = match self.allocation {
            NodeAllocation::Recorded(Some(id)) => {
                if self.graph.node(id).is_some()
                    || self.bindings.names().contains_key(id.as_str())
                    || names.iter().any(|(name, _)| name == id.as_str())
                {
                    return Err(rejected(
                        "ENG008",
                        statement.span,
                        "recorded node identity conflicts with the restored namespace",
                    ));
                }
                id.clone()
            }
            NodeAllocation::Recorded(None) => {
                return Err(rejected(
                    "ENG008",
                    statement.span,
                    "replay creates more nodes than its recorded identities",
                ));
            }
            NodeAllocation::Automatic => self
                .graph
                .next_id_avoiding(
                    self.bindings
                        .names()
                        .keys()
                        .map(String::as_str)
                        .chain(names.iter().map(|(name, _)| name.as_str()))
                        .chain(self.reserved_names.iter().map(String::as_str)),
                )
                .map_err(|e| WorkspaceError::Runtime(e.into()))?,
        };
        Ok(Preparation::Change(self.change(
            Change::Node {
                node: id,
                task,
                activation: self.pipe_input.cloned(),
                stream_origin: None,
                pipeline: Default::default(),
                typing,
                names,
            },
            diagnostics,
        )))
    }
    fn occupied(&self, name: &str) -> bool {
        wes_language::vocabulary::commands::is_command_root(name)
            || self.catalogue().provider(name).is_some()
    }
    fn validate_names(&self, statement: &Statement) -> Result<(), WorkspaceError> {
        if statement
            .binding
            .iter()
            .chain(statement.error_binding.iter())
            .any(|b| self.reserved_names.contains(&b.name.text))
        {
            return Err(rejected(
                "NAM002",
                statement.span,
                "This name belongs to a workspace object and cannot be replaced by an output.",
            ));
        }
        Bindings::validate_names(
            statement
                .binding
                .iter()
                .chain(statement.error_binding.iter())
                .map(|b| b.name.text.as_str()),
            self.graph,
        )
        .map_err(|error| {
            let code = if matches!(error, BindingError::ShadowsId(_)) {
                "ENG003"
            } else {
                "ENG004"
            };
            rejected(code, statement.span, error.to_string())
        })
    }
    fn environment(&self, call: &wes_language::Call) -> Environment {
        let mut environment = Environment::default();
        for value in call
            .operands
            .iter()
            .chain(call.arguments.iter().map(|a| &a.value))
            .flat_map(wes_language::Value::references)
        {
            let name = value;
            if let Some(output) = self.resolve(name.text.split('.').next().expect("reference root"))
            {
                let fields: Vec<String> = name.text.split('.').skip(1).map(str::to_owned).collect();
                if let Some(mut typing) = self.typing(&output)
                    && let Some(shape) = crate::plan::field_shape(&typing.shape, &fields)
                {
                    typing.shape = shape;
                    environment.bindings.insert(name.text.clone(), typing);
                }
                if !fields.is_empty() {
                    continue;
                }
                let Some(call) = self
                    .graph
                    .node(&output.node)
                    .expect("resolved node")
                    .payload()
                    .call()
                else {
                    continue;
                };
                let call = call.invocation();
                let arguments = call
                    .inputs
                    .iter()
                    .map(|(key, input)| {
                        (
                            key.clone(),
                            crate::plan::given(input, &|output| self.typing(output)),
                        )
                    })
                    .collect();
                environment.calls.insert(
                    name.text.clone(),
                    ExistingCall {
                        capability: call.capability.clone(),
                        arguments,
                        excused: call
                            .cautions
                            .iter()
                            .filter_map(|c| c.strip_prefix("unchecked:").map(str::to_owned))
                            .collect(),
                        validated_inputs: plan::guarded_shapes(&call.guards, &call.capability)
                            .expect("bound guards were checked"),
                    },
                );
            }
        }
        environment
    }
}
