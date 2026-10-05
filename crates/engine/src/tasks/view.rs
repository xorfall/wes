//! View edits are applied at the serialized workspace run-entry boundary. The worker only
//! publishes the already determined receipt; it never owns the view store or starts a provider.
use crate::{
    plan::{Input, MetaTask},
    runtime::{Outcome, OutputState, RuntimeCode},
    workspace::Workspace,
};
use wes_core::Value;
use wes_language::{Diagnostic, Span, vocabulary::MetaCommand};

enum EditError {
    Message(String),
    View(crate::views::Error),
    Input(crate::plan::InputResolutionError),
}
impl From<String> for EditError {
    fn from(message: String) -> Self {
        Self::Message(message)
    }
}
impl From<&str> for EditError {
    fn from(message: &str) -> Self {
        Self::Message(message.into())
    }
}
impl EditError {
    fn failure(self) -> wes_core::ErrorValue {
        match self {
            Self::Input(error) => error.error(RuntimeCode::InputFailed, "input"),
            Self::View(crate::views::Error::InputContract { contract, issues }) => {
                wes_core::ErrorValue::new(
                    wes_core::ErrorId::new(uuid::Uuid::new_v4().to_string())
                        .expect("UUID is nonblank"),
                    "TYP005",
                    format!("View input does not satisfy {contract}"),
                    issues,
                    None,
                )
                .expect("canonical contract issues are valid")
            }
            Self::View(error) => RuntimeCode::ExecutionFailed.error(error.to_string(), None),
            Self::Message(message) => RuntimeCode::ExecutionFailed.error(message, None),
        }
    }
}

#[derive(Clone, Debug)]
pub struct BoundView {
    task: MetaTask,
    captured: Option<Outcome>,
    pub(crate) pipe_input: Option<crate::graph::OutputRef>,
    input_contract: Option<std::sync::Arc<wes_core::contracts::Contract>>,
    pub(crate) pin: Option<crate::views::PinIntent>,
    pub(crate) origin: Option<crate::runtime::Run>,
    pub(crate) caller: Option<crate::source::SourceInput>,
    pub(crate) environment: Option<wes_core::environments::EnvironmentContext>,
    pub(crate) scope: crate::access::WriteScope,
}
impl BoundView {
    pub(crate) fn supports(command: MetaCommand) -> bool {
        matches!(
            command,
            MetaCommand::ViewQuery
                | MetaCommand::ViewApply
                | MetaCommand::ViewCreate
                | MetaCommand::ViewBind
                | MetaCommand::ViewConnect
                | MetaCommand::ViewDisconnect
                | MetaCommand::ViewOutput
                | MetaCommand::ViewCapture
                | MetaCommand::ViewPin
                | MetaCommand::ViewLink
                | MetaCommand::ViewUnlink
                | MetaCommand::ViewStart
                | MetaCommand::ViewStop
        )
    }
    pub(crate) fn input_shape(&self) -> Option<wes_core::Shape> {
        self.input_contract
            .as_ref()
            .map(|contract| contract.shape())
    }
    pub(crate) fn command(&self) -> MetaCommand {
        self.task.spec.command
    }
    pub(crate) fn bind(
        task: MetaTask,
        span: Span,
        views: &wes_views::Catalogue,
    ) -> Result<Self, Diagnostic> {
        if task.spec.command == MetaCommand::ViewCreate
            && task.tail.first().and_then(|n| views.get(n)).is_none()
        {
            return Err(Diagnostic::error(
                "VIE001",
                span,
                "Unknown view definition; use :list views",
            ));
        }
        if task
            .subjects
            .iter()
            .any(|i| !matches!(i, Input::FromNode(_)))
        {
            return Err(Diagnostic::error(
                "VIE002",
                span,
                "Use a view instance reference, such as $chart",
            ));
        }
        Ok(Self {
            input_contract: if task.spec.command == MetaCommand::ViewCreate {
                task.tail
                    .first()
                    .and_then(|name| views.get(name))
                    .map(|package| package.input())
            } else {
                None
            },
            task,
            captured: None,
            pipe_input: None,
            pin: None,
            caller: None,
            origin: None,
            environment: None,
            scope: Default::default(),
        })
    }
    pub(crate) fn capture(&mut self, workspace: &mut Workspace, node: &crate::graph::NodeId) {
        self.captured = Some(match self.apply(workspace, node) {
            Ok(value) => {
                workspace.views.update_query_intents();
                Outcome::Produced(value)
            }
            Err(error) => Outcome::Failed(error.failure()),
        });
    }
    pub(crate) fn reject_pin(&mut self, message: &str) {
        self.pin = None;
        self.captured = Some(Outcome::Failed(
            RuntimeCode::RecordingFailed.error(message, None),
        ));
    }
    pub(crate) fn outcome(self) -> Outcome {
        if self.pin.is_some() {
            return Outcome::Failed(RuntimeCode::RecordingFailed.error(
                "Pin was not admitted by a session storage owner; view binding was unchanged",
                None,
            ));
        }
        self.captured.unwrap_or_else(|| {
            Outcome::Failed(
                RuntimeCode::ExecutionFailed
                    .error("View edit was not captured by its workspace owner", None),
            )
        })
    }
    fn apply(
        &mut self,
        workspace: &mut Workspace,
        node: &crate::graph::NodeId,
    ) -> Result<Value, EditError> {
        let resolve = |input: &Input| -> Result<Value, EditError> {
            let mut values = indexmap::IndexMap::new();
            if let Some(output) = input.dependency() {
                let value=match workspace.runtime().output(output){
                    OutputState::Available(value)=>value,
                    _ if output.port==crate::graph::OutputPort::Data=>workspace.runtime().value_of(&output.node).cloned().ok_or("View binding needs a completed value; binding never starts the source")?,
                    _=>return Err("View binding needs a ready value. Wait for the source, then repeat this edit; binding never starts the source.".into()),
                };
                values.insert(output.node.clone(), value);
            }
            input
                .resolve(&values)
                .map(|v| v.into_owned())
                .map_err(EditError::Input)
        };
        let input = self
            .task
            .inputs
            .get("input")
            .map(|input| {
                let value = resolve(input)?;
                Ok::<_, EditError>(if let Some(output) = input.dependency() {
                    let definition = workspace
                        .runtime()
                        .graph()
                        .node(&output.node)
                        .ok_or("View source no longer exists")?
                        .definition();
                    crate::views::Input::from_source(
                        crate::views::Source::new(output.clone(), definition)
                            .with_digest(&value)
                            .with_run(if output.port == crate::graph::OutputPort::Data {
                                workspace.runtime().value_run(&output.node).cloned()
                            } else {
                                workspace.runtime().run_of(&output.node).cloned()
                            })
                            .with_fields(match input {
                                Input::FieldPath { fields, .. } => fields.clone(),
                                _ => vec![],
                            }),
                        value,
                    )
                } else {
                    crate::views::Input::constant(value)
                })
            })
            .transpose()?;
        let view_error = EditError::View;
        if self.task.spec.command == MetaCommand::ViewCreate {
            let definition = workspace
                .views
                .catalogue()
                .get(&self.task.tail[0])
                .cloned()
                .ok_or("View definition is no longer installed")?;
            let handle = workspace
                .views
                .create(
                    node.clone(),
                    &definition.manifest.name,
                    &definition.digest,
                    input,
                )
                .map_err(view_error)?;
            return workspace.views.value(&handle).map_err(view_error);
        }
        let subject = resolve(
            self.task
                .subjects
                .first()
                .ok_or("Expected a view reference")?,
        )?;
        let child = workspace.views.resolve(&subject).map_err(view_error)?;
        if self.task.spec.command == MetaCommand::ViewCapture {
            return workspace.capture_view(child.id()).map_err(Into::into);
        }
        if self.task.spec.command == MetaCommand::ViewOutput {
            let port = resolve(self.task.inputs.get("port").ok_or("Expected port name")?)?;
            let wes_core::Data::Text(port) = port.data() else {
                return Err("View port must be Text".into());
            };
            let snapshot = workspace.views.read(&child).map_err(view_error)?;
            workspace.view_interaction(child.id(), &snapshot.identity)?;
            return workspace.views.output(&child, port).map_err(view_error);
        }
        let parent = self
            .task
            .inputs
            .get("to")
            .map(|i| resolve(i).and_then(|v| workspace.views.resolve(&v).map_err(view_error)))
            .transpose()?;
        let target = parent.as_ref().unwrap_or(&child);
        workspace
            .check_view_access(&child, &self.scope)
            .map_err(|e| e.to_string())?;
        if let Some(parent) = &parent {
            workspace
                .check_view_access(parent, &self.scope)
                .map_err(|e| e.to_string())?;
        }
        let revision = self
            .task
            .inputs
            .get("revision")
            .map(|i| match resolve(i)?.data() {
                wes_core::Data::Int(n) if *n >= 0 => Ok(*n as u64),
                _ => Err(EditError::from("View revision must be a nonnegative Int")),
            })
            .transpose()?
            .unwrap_or(workspace.views.read(target).map_err(view_error)?.revision);
        if self.task.spec.command == MetaCommand::ViewPin {
            let snapshot = workspace.views.read(&child).map_err(view_error)?;
            if let Some(expected) = self.task.inputs.get("instance") {
                if !matches!(resolve(expected)?.data(), wes_core::Data::Text(id) if id.as_ref() == snapshot.identity.as_ref())
                {
                    return Err(crate::views::Error::Revision).map_err(view_error);
                }
            }
            if let Some(expected) = self.task.inputs.get("inputRevision") {
                if !matches!(resolve(expected)?.data(), wes_core::Data::Int(n) if *n >= 0 && *n as u64 == snapshot.input_revision)
                {
                    return Err(crate::views::Error::Revision).map_err(view_error);
                }
            }
            let intent = workspace
                .capture_pin(&child, revision, &self.scope)
                .map_err(EditError::from)?;
            let value = intent.value.clone();
            self.pin = Some(intent);
            return Ok(value);
        }
        let slot = self
            .task
            .inputs
            .get("slot")
            .map(|i| match resolve(i)?.data() {
                wes_core::Data::Text(s) => Ok(s.to_string()),
                _ => Err(EditError::from("View slot must be Text")),
            })
            .transpose()?;
        match self.task.spec.command {
            MetaCommand::ViewQuery => {
                let text = |name: &str| -> Result<String, EditError> {
                    match resolve(
                        self.task
                            .inputs
                            .get(name)
                            .ok_or("Missing query binding parameter")?,
                    )?
                    .data()
                    {
                        wes_core::Data::Text(value) => Ok(value.to_string()),
                        _ => Err("Query binding parameter must be Text".into()),
                    }
                };
                let template = text("template")?;
                let port = text("output")?;
                let source = workspace
                    .views
                    .resolve(&resolve(
                        self.task.inputs.get("from").ok_or("Expected from:$view")?,
                    )?)
                    .map_err(view_error)?;
                let source_snapshot = workspace.views.read(&source).map_err(view_error)?;
                workspace.view_interaction(source.id(), &source_snapshot.identity)?;
                let mode = self
                    .task
                    .inputs
                    .get("mode")
                    .map(|_| text("mode"))
                    .transpose()?
                    .unwrap_or_else(|| "finite".into());
                let adapter = match (mode.as_str(), self.task.inputs.get("adapter")) {
                    ("finite", None) => None,
                    ("live", Some(_)) => {
                        let template = text("adapter")?;
                        let revision = workspace.query_template(&template)?.revision.clone();
                        Some(crate::views::QueryAdapter { template, revision })
                    }
                    _ => {
                        return Err(
                            "Use mode:finite without adapter, or mode:live adapter:TypedCalc"
                                .into(),
                        );
                    }
                };
                let template_revision = workspace.query_revision(&template, adapter.is_some())?;
                let trigger = match self
                    .task
                    .inputs
                    .get("trigger")
                    .map(|_| text("trigger"))
                    .transpose()?
                    .as_deref()
                    .unwrap_or("manual")
                {
                    "manual" => crate::views::QueryTrigger::Manual,
                    "commit" => crate::views::QueryTrigger::Commit,
                    _ => return Err("Query trigger must be manual or commit".into()),
                };
                let binding = crate::views::QueryBinding {
                    environment: self
                        .environment
                        .clone()
                        .or_else(|| self.caller.as_ref().and_then(|c| c.environments().cloned())),
                    source,
                    port,
                    template,
                    template_revision,
                    adapter,
                    trigger,
                };
                workspace.validate_query_contracts(
                    &binding,
                    &source_snapshot.definition,
                    &workspace.views.read(&child).map_err(view_error)?.definition,
                )?;
                workspace
                    .views
                    .configure_query(&child, binding, revision)
                    .map_err(view_error)?;
            }
            MetaCommand::ViewApply => {
                let snapshot = workspace.views.read(&child).map_err(view_error)?;
                let query = snapshot.query.ok_or("This view has no query binding")?;
                let source = workspace.views.read(&query.source).map_err(view_error)?;
                workspace.view_interaction(query.source.id(), &source.identity)?;
                workspace.query_access(&query)?;
                let mut caller = self
                    .caller
                    .clone()
                    .ok_or("A view query requires a live caller")?
                    .without_environments();
                if let Some(environment) = &query.environment {
                    caller = caller
                        .with_environments(environment.clone())
                        .map_err(|e| e.to_string())?;
                }
                workspace
                    .views
                    .apply_query(
                        &child,
                        caller,
                        self.origin
                            .clone()
                            .ok_or("Query execution origin is unavailable")?,
                    )
                    .map_err(view_error)?;
            }
            MetaCommand::ViewStart | MetaCommand::ViewStop => {
                for member in workspace.views.included(&child).map_err(view_error)? {
                    workspace
                        .check_view_access(&member, &self.scope)
                        .map_err(|e| e.to_string())?;
                }
                workspace
                    .views
                    .set_observing(&child, self.task.spec.command == MetaCommand::ViewStart)
                    .map_err(view_error)?;
            }
            MetaCommand::ViewLink | MetaCommand::ViewUnlink => {
                let text = |name: &str| -> Result<String, EditError> {
                    let value = resolve(
                        self.task
                            .inputs
                            .get(name)
                            .ok_or("Missing view binding field")?,
                    )?;
                    match value.data() {
                        wes_core::Data::Text(s) => Ok(s.to_string()),
                        _ => Err("View binding field must be Text".into()),
                    }
                };
                let field = text("field")?;
                if self.task.spec.command == MetaCommand::ViewLink {
                    workspace
                        .views
                        .link(&child, &text("output")?, target, &field, revision)
                        .map_err(view_error)?;
                } else {
                    workspace
                        .views
                        .unlink(target, &field, revision)
                        .map_err(view_error)?;
                }
            }
            MetaCommand::ViewBind => {
                workspace
                    .views
                    .bind(&child, revision, input)
                    .map_err(view_error)?;
            }
            MetaCommand::ViewConnect => {
                workspace
                    .views
                    .connect(
                        &child,
                        parent.as_ref().ok_or("Expected to:$container")?,
                        slot.as_deref(),
                        revision,
                    )
                    .map_err(view_error)?;
            }
            MetaCommand::ViewDisconnect => {
                workspace
                    .views
                    .disconnect(
                        &child,
                        parent.as_ref().ok_or("Expected to:$container")?,
                        slot.as_deref(),
                        revision,
                    )
                    .map_err(view_error)?;
            }
            _ => return Err("Unsupported view edit".into()),
        }
        workspace.views.value(target).map_err(view_error)
    }
}
