//! Read-only workspace queries. The owner captures metadata at worker entry; formatting is joined
//! local work. No executor gets a workspace lock, graph authority, provider handle or credential.
use crate::{
    driver::CancellationToken,
    graph::{NodeId, OutputPort, OutputRef},
    plan::{Input, MetaTask},
    runtime::{Outcome, RuntimeCode},
    workspace::{Workspace, WorkspaceName},
};
use indexmap::IndexMap;
use std::sync::Arc;
use wes_core::{
    Data, ErrorValue, Provenance, Shape, Value,
    capability::{Capability, ProviderDescription, Rule, Safety, Typing},
};
use wes_language::targets::ObjectKind;
use wes_language::vocabulary::{ListRegistry, MetaCommand};
mod nodes;
pub(crate) mod types;

fn max_rows() -> usize {
    wes_budgets::get("query.rows") as usize
}
fn max_bytes() -> usize {
    wes_budgets::get("query.bytes") as usize
}
fn max_work() -> usize {
    wes_budgets::get("query.work") as usize
}

#[derive(Clone, Debug)]
enum Failure {
    Session(&'static str),
    Selection(&'static str),
    Limit,
    Missing { kind: &'static str, target: String },
    OutputUnavailable(String),
    TraceMissing,
    Unsupported,
    Uncaptured,
    Cancelled,
}
impl Failure {
    fn missing(kind: &'static str, target: impl AsRef<str>) -> Self {
        // Only the selected identifier, never a result body or credential. Bounded even
        // for malformed calls; debug quoting keeps controls from becoming display markup.
        Self::Missing {
            kind,
            target: target.as_ref().chars().take(256).collect(),
        }
    }
}
fn output_unavailable(workspace: &Workspace, output: &OutputRef) -> Failure {
    let name = workspace
        .bindings()
        .names()
        .iter()
        .find(|(_, selected)| selected.node == output.node && selected.port == OutputPort::Data)
        .map_or(output.node.as_str(), |(name, _)| name.as_str());
    let node = workspace.runtime().graph().node(&output.node);
    let state = node.map(|n| n.state());
    let upstream = node
        .and_then(|node| node.dependencies().keys().next())
        .map(|id| {
            workspace
                .bindings()
                .names()
                .iter()
                .find(|(_, selected)| selected.node == *id && selected.port == OutputPort::Data)
                .map_or(id.as_str(), |(name, _)| name.as_str())
        });
    let refresh_hint = upstream.map_or_else(
        || format!(":refresh ${name}"),
        |name| format!(":refresh ${name} scope:downstream"),
    );
    let message = match state {
        Some(crate::graph::NodeState::Stale) => format!(
            "Result ${name} is stale: {}. Use {refresh_hint} to recompute the selected work; if an earlier upstream is also stale, refresh that root with scope:downstream. This read did not rerun anything.",
            workspace
                .runtime()
                .stale_reason(&output.node)
                .map_or("its input or definition changed", |reason| reason
                    .message()
                    .trim_end_matches('.'))
        ),
        Some(crate::graph::NodeState::Running | crate::graph::NodeState::Pending) => format!(
            "{} output from ${name} is pending. Wait for the current run; this read did not rerun anything.",
            match output.port {
                OutputPort::Data => "data",
                OutputPort::Error => "error",
                OutputPort::Cancel => "cancel",
            }
        ),
        Some(_) => format!(
            "Selected output from ${name} is closed or unavailable in this run. Inspect ${name} for its execution state; this read did not rerun anything."
        ),
        None => format!(
            "Node ${name} is missing from this workspace. Inspect the current names; this read did not rerun anything."
        ),
    };
    Failure::OutputUnavailable(message)
}

#[derive(Clone, Debug)]
enum Capture {
    Trace(Value),
    Information(Value),
    Help(super::BoundHelp),
    Data(Data),
    Types(Vec<Arc<wes_core::contracts::Contract>>),
    Type(types::Captured),
    Names(Vec<String>),
    Providers(Vec<(String, Option<String>)>),
    Capabilities(Vec<(Arc<ProviderDescription>, Arc<Capability>)>),
    Nodes(Vec<nodes::NodeDescription>),
    Bindings(Vec<nodes::BoundName>),
}
pub(crate) enum SessionQuery {
    Sandboxes,
    Cells,
    Cell(String),
}
#[derive(Clone, Debug)]
pub struct BoundQuery {
    task: MetaTask,
    environment_context: Option<wes_core::environments::EnvironmentContext>,
    captured: Option<Result<Capture, Failure>>,
    captured_at: Option<String>,
}
impl BoundQuery {
    pub(crate) fn session_request(&self) -> Option<SessionQuery> {
        if self.task.spec.command == MetaCommand::List
            && self.task.tail.first().is_some_and(|s| s == "sandboxes")
        {
            return Some(SessionQuery::Sandboxes);
        }
        if self.task.spec.command == MetaCommand::List
            && self.task.tail.first().is_some_and(|s| s == "cells")
        {
            return Some(SessionQuery::Cells);
        }
        if let Some(wes_language::targets::QueryTarget::Object { kind, name }) = &self.task.target {
            if *kind == ObjectKind::Cell {
                return Some(SessionQuery::Cell(name.text.clone()));
            }
        }
        None
    }
    pub(crate) fn capture_session(&mut self, data: Result<Data, &'static str>) {
        self.captured = Some(data.map_err(Failure::Session).and_then(|data| {
            crate::value_size::data_charge(&data, max_bytes() as u64).ok_or(Failure::Limit)?;
            Ok(Capture::Data(data))
        }));
    }
    pub(crate) fn supports(task: &MetaTask) -> bool {
        match task.spec.command {
            MetaCommand::List => task
                .tail
                .first()
                .and_then(|name| ListRegistry::lookup(name))
                .is_some(),
            MetaCommand::Info | MetaCommand::Inspect | MetaCommand::Read | MetaCommand::Trace => {
                true
            }
            _ => false,
        }
    }
    pub(crate) fn new(
        task: MetaTask,
        environment_context: Option<wes_core::environments::EnvironmentContext>,
    ) -> Self {
        Self {
            task,
            environment_context,
            captured: None,
            captured_at: None,
        }
    }
    pub(crate) fn dependencies(&self) -> impl Iterator<Item = OutputRef> + '_ {
        self.task
            .subjects
            .iter()
            .chain(self.task.inputs.values())
            .filter(move |_| {
                !matches!(
                    self.task.spec.command,
                    MetaCommand::Trace | MetaCommand::Inspect | MetaCommand::Read
                )
            })
            .flat_map(Input::dependencies)
            .cloned()
    }
    pub(crate) fn predicted_typing(&self) -> Typing {
        let shape = match self.task.spec.command {
            MetaCommand::Trace | MetaCommand::Read | MetaCommand::Inspect => Shape::Unknown,
            MetaCommand::List => {
                let element = match self
                    .task
                    .tail
                    .first()
                    .and_then(|name| ListRegistry::lookup(name))
                {
                    Some(
                        ListRegistry::Sandboxes
                        | ListRegistry::Homes
                        | ListRegistry::Environments
                        | ListRegistry::Templates
                        | ListRegistry::Adapters
                        | ListRegistry::Views
                        | ListRegistry::Importers
                        | ListRegistry::Workspaces
                        | ListRegistry::Commands,
                    ) => Shape::Primitive(wes_core::Primitive::Text),
                    _ => Shape::Unknown,
                };
                Shape::List(Box::new(element))
            }
            _ => match self
                .task
                .subjects
                .first()
                .and_then(Input::dependency)
                .map(|output| output.port)
            {
                Some(OutputPort::Error) => ErrorValue::shape(),
                Some(OutputPort::Cancel) => ErrorValue::cancellation_shape(),
                _ => Shape::Unknown,
            },
        };
        Typing::new(shape)
    }
    pub(crate) fn capture(&mut self, workspace: &Workspace) {
        self.captured_at = None;
        self.captured = Some(self.read(workspace).and_then(|capture| {
            match &capture {
                Capture::Data(data) => {
                    crate::value_size::data_charge(data, max_bytes() as u64)
                        .ok_or(Failure::Limit)?;
                }
                Capture::Information(value) => {
                    crate::value_size::data_charge(value.data(), max_bytes() as u64)
                        .ok_or(Failure::Limit)?;
                }
                _ => {}
            }
            Ok(capture)
        }));
        if matches!(self.captured, Some(Ok(Capture::Bindings(_)))) {
            self.captured_at = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .ok()
                .and_then(|time| {
                    wes_core::Timestamp::new(
                        i64::try_from(time.as_secs()).ok()?,
                        time.subsec_nanos(),
                    )
                    .ok()
                })
                .map(|time| time.to_string());
        }
    }
    fn read(&self, workspace: &Workspace) -> Result<Capture, Failure> {
        if self.task.spec.command == MetaCommand::Info {
            let name = self.task.tail.first().ok_or(Failure::Unsupported)?;
            let catalogue = self.catalogue(workspace)?;
            let provider = catalogue
                .provider(name)
                .ok_or_else(|| Failure::missing("provider", name))?;
            let information = provider.information_value().cloned().unwrap_or_else(|| {
                Value::new(
                    Shape::Unknown,
                    fields([
                        ("notes", Data::List(vec![])),
                        ("advisories", Data::List(vec![])),
                        ("evidence", Data::Record(IndexMap::new())),
                    ]),
                    Provenance::default(),
                )
                .expect("empty provider information")
            });
            let shape = Shape::Record(
                wes_core::RecordShape::new(
                    "ProviderInfo",
                    [
                        (
                            "provider".into(),
                            Shape::Primitive(wes_core::Primitive::Text),
                        ),
                        ("information".into(), information.shape().clone()),
                    ],
                )
                .expect("info fields"),
            );
            return Ok(Capture::Information(
                Value::new(
                    shape,
                    fields([
                        ("provider", Data::Text(provider.name().into())),
                        ("information", information.data().clone()),
                    ]),
                    information.provenance().clone(),
                )
                .expect("validated provider information"),
            ));
        }
        if self.task.spec.command == MetaCommand::Read {
            let output = self
                .task
                .subjects
                .first()
                .and_then(Input::dependency)
                .ok_or(Failure::Unsupported)?;
            let value = if self.task.tail.first().is_some_and(|v| v == "trace") {
                let run = self.literal_text("run")?;
                workspace
                    .traces
                    .get(&output.node, run)
                    .ok_or(Failure::TraceMissing)?
            } else {
                match workspace.runtime().output(output) {
                    crate::runtime::OutputState::Available(value) => value,
                    _ if output.port == OutputPort::Data => workspace
                        .runtime()
                        .evidence_value(&output.node)
                        .map(|v| {
                            v.value.clone().with_provenance(
                                v.value
                                    .provenance()
                                    .clone()
                                    .with_fact("wes.observation.state", "stopped")
                                    .with_fact("wes.observation.run", v.run.as_str())
                                    .with_fact("wes.observation.source", v.source.as_str()),
                            )
                        })
                        .ok_or_else(|| output_unavailable(workspace, output))?,
                    _ => return Err(output_unavailable(workspace, output)),
                }
            };
            return self.project_read(&value).map(Capture::Trace);
        }
        if self.task.spec.command == MetaCommand::Trace {
            let Some(Input::FromNode(output)) = self.task.subjects.first() else {
                return Err(Failure::missing("query target", "unspecified"));
            };
            let run = match self.task.inputs.get("run") {
                Some(Input::Literal(v)) => match v.data() {
                    Data::Text(s) => Some(s.as_ref()),
                    _ => return Err(Failure::missing("query target", "unspecified")),
                },
                None => None,
                _ => return Err(Failure::missing("query target", "unspecified")),
            };
            return workspace
                .traces
                .get(&output.node, run)
                .map(Capture::Trace)
                .ok_or(Failure::TraceMissing);
        }
        if self.task.spec.command == MetaCommand::Inspect {
            if let Some(target) = &self.task.target {
                use wes_language::targets::QueryTarget;
                match target {
                    QueryTarget::Command(_) | QueryTarget::Provider { .. } => {
                        return Ok(Capture::Help(
                            super::BoundHelp::new(
                                target.clone(),
                                self.catalogue(workspace)?.as_ref(),
                            )
                            .with_importers(workspace.importer_metadata())
                            .map_err(Failure::Session)?,
                        ));
                    }
                    QueryTarget::Object { kind, name } if *kind == ObjectKind::View => {
                        let definition = workspace
                            .view_catalogue()
                            .get(&name.text)
                            .ok_or_else(|| Failure::missing("view definition", &name.text))?;
                        return Ok(Capture::Data(wes_views::metadata(definition.description())));
                    }
                    QueryTarget::Object { kind, name } if *kind == ObjectKind::Type => {
                        return Ok(Capture::Type(types::capture(
                            workspace.contracts(),
                            &name.text,
                        )));
                    }
                    QueryTarget::Object { kind, name } if *kind == ObjectKind::Environment => {
                        let env = workspace
                            .environments()
                            .inspect(&name.text)
                            .ok_or_else(|| Failure::missing("environment", &name.text))?;
                        return Ok(Capture::Data(fields([
                            ("name", Data::Text(env.name().into())),
                            ("revision", Data::Text(env.revision().to_string().into())),
                            ("identity", Data::Text(env.identity().to_string().into())),
                            ("abstract", Data::Bool(env.is_abstract())),
                            ("retired", Data::Bool(env.is_retired())),
                            ("protected", Data::Bool(env.is_protected())),
                            ("drift", Data::Bool(env.has_drift())),
                            (
                                "providers",
                                Data::List(
                                    env.imports()
                                        .iter()
                                        .map(|(alias, _)| Data::Text(alias.to_owned().into()))
                                        .collect(),
                                ),
                            ),
                        ])));
                    }
                    QueryTarget::Object { kind, name } if *kind == ObjectKind::Home => {
                        if workspace.home_identity() != Some(name.text.as_str()) {
                            return Err(Failure::missing("attached data home", &name.text));
                        }
                        return Ok(Capture::Data(fields([
                            ("id", Data::Text(name.text.as_str().into())),
                            ("attached", Data::Bool(true)),
                        ])));
                    }
                    QueryTarget::Object { kind, name } if *kind == ObjectKind::Template => {
                        let template = workspace
                            .templates()
                            .snapshot()
                            .get(&name.text)
                            .ok_or_else(|| Failure::missing("template", &name.text))?;
                        let mut description = IndexMap::from([
                            ("name".into(), Data::Text(name.text.as_str().into())),
                            (
                                "parameters".into(),
                                Data::List(
                                    template
                                        .parameters
                                        .iter()
                                        .map(|p| Data::Text(p.as_str().into()))
                                        .collect(),
                                ),
                            ),
                        ]);
                        if let Some(definition) = &template.calculation {
                            description.extend(crate::calc::describe_purity(&definition.compiled));
                            description.insert("kind".into(), Data::Text("calculation".into()));
                            description.insert(
                                "revision".into(),
                                Data::Text(definition.revision.as_str().into()),
                            );
                            description.insert(
                                "outputType".into(),
                                Data::Text(definition.output.name().into()),
                            );
                            description.insert(
                                "inputTypes".into(),
                                Data::Record(
                                    template
                                        .contracts
                                        .iter()
                                        .map(|(name, c)| {
                                            (name.clone(), Data::Text(c.name().into()))
                                        })
                                        .collect(),
                                ),
                            );
                            description.insert(
                                "conversionEligible".into(),
                                Data::Bool(definition.conversion_eligible()),
                            );
                            description.insert("conversionReason".into(), Data::Text(if definition.compiled.effectful() { "Possible external operation." } else if !definition.compiled.parameters.contains_key("input") { "An explicit typed input parameter is required." } else { "Pure typed calculation with explicit parameters and an output contract." }.into()));
                        } else {
                            description.insert("kind".into(), Data::Text("command".into()));
                            description.insert("purity".into(), Data::Text("unverified".into()));
                            description.insert("conversionEligible".into(), Data::Bool(false));
                            description.insert(
                                "purityReason".into(),
                                Data::Text(
                                    "Object-command template; resolved at invocation.".into(),
                                ),
                            );
                        }
                        return Ok(Capture::Data(Data::Record(description)));
                    }
                    QueryTarget::Object { kind, name } if *kind == ObjectKind::Name => {
                        let output = workspace
                            .bindings()
                            .names()
                            .get(&name.text)
                            .ok_or_else(|| Failure::missing("name binding", &name.text))?;
                        return Ok(Capture::Data(fields([
                            ("name", Data::Text(name.text.as_str().into())),
                            ("node", Data::Text(output.node.as_str().into())),
                            (
                                "port",
                                Data::Text(format!("{:?}", output.port).to_lowercase().into()),
                            ),
                        ])));
                    }
                    QueryTarget::Object { kind, name } if *kind == ObjectKind::Run => {
                        return run_metadata(workspace)?.into_iter().find(|row|matches!(row,Data::Record(fields) if fields.get("id") == Some(&Data::Text(name.text.as_str().into())))).map(Capture::Data).ok_or_else(||Failure::missing("retained run metadata",&name.text));
                    }
                    QueryTarget::Object { kind, name } if *kind == ObjectKind::Workspace => {
                        let names = workspace.saved_workspace_names();
                        if !names.iter().any(|n| n.as_str() == name.text) {
                            return Err(Failure::missing("saved workspace", &name.text));
                        }
                        return Ok(Capture::Data(fields([
                            ("name", Data::Text(name.text.as_str().into())),
                            ("saved", Data::Bool(true)),
                        ])));
                    }
                    QueryTarget::Object { kind, name } if *kind == ObjectKind::Importer => {
                        let found = workspace.importer_names().any(|n| n == name.text);
                        if !found {
                            return Err(Failure::missing("registry entry", &name.text));
                        }
                        return Ok(Capture::Data(fields([
                            ("kind", Data::Text(kind.name().into())),
                            ("name", Data::Text(name.text.as_str().into())),
                        ])));
                    }
                    QueryTarget::Output { metadata: true, .. } => {
                        let output = self
                            .task
                            .subjects
                            .first()
                            .and_then(Input::dependency)
                            .ok_or(Failure::Unsupported)?;
                        return nodes::capture_nodes(workspace, Some(&output.node))
                            .map(Capture::Nodes);
                    }
                    QueryTarget::Output {
                        metadata: false, ..
                    } => {
                        let output = self
                            .task
                            .subjects
                            .first()
                            .and_then(Input::dependency)
                            .ok_or(Failure::Unsupported)?;
                        if let crate::runtime::OutputState::Available(value) =
                            workspace.runtime().output(output)
                            && value.shape() == &Shape::Meta(wes_core::MetaType::ViewInstance)
                        {
                            return workspace.views.inspect(&value).map(Capture::Data).map_err(
                                |_| Failure::missing("live view instance", output.node.as_str()),
                            );
                        }
                        let node = workspace
                            .runtime()
                            .graph()
                            .node(&output.node)
                            .ok_or_else(|| Failure::missing("node", output.node.as_str()))?;
                        let stopped = output.port == OutputPort::Data
                            && workspace.runtime().evidence_value(&output.node).is_some();
                        let available = matches!(
                            workspace.runtime().output(output),
                            crate::runtime::OutputState::Available(_)
                        );
                        let shape = match output.port {
                            OutputPort::Data => workspace
                                .data_typing(&output.node)
                                .map(|t| t.shape.clone())
                                .unwrap_or(Shape::Unknown),
                            OutputPort::Error => ErrorValue::shape(),
                            OutputPort::Cancel => ErrorValue::cancellation_shape(),
                        };
                        let token = CancellationToken::new();
                        let mut budget = Budget {
                            bytes: 0,
                            work: 0,
                            token: &token,
                        };
                        let shape = budget.shape(&shape)?;
                        return Ok(Capture::Data(fields([
                            ("type", shape),
                            ("available", Data::Bool(available)),
                            ("stopped", Data::Bool(stopped)),
                            ("node", Data::Text(output.node.as_str().into())),
                            (
                                "port",
                                Data::Text(format!("{:?}", output.port).to_lowercase().into()),
                            ),
                            (
                                "state",
                                Data::Text(format!("{:?}", node.state()).to_lowercase().into()),
                            ),
                        ])));
                    }
                    _ => return Err(Failure::Unsupported),
                }
            }
            return Err(Failure::Uncaptured);
        }

        match self
            .task
            .tail
            .first()
            .and_then(|name| ListRegistry::lookup(name))
            .ok_or(Failure::Unsupported)?
        {
            ListRegistry::Homes => names(workspace.home_identity().into_iter()),
            ListRegistry::Cells | ListRegistry::Sandboxes => Err(Failure::Uncaptured),
            ListRegistry::Runs => Ok(Capture::Data(Data::List(run_metadata(workspace)?))),
            ListRegistry::Environments => names(workspace.environments().names()),
            ListRegistry::Templates => {
                names(workspace.templates().snapshot().keys().map(String::as_str))
            }
            ListRegistry::Adapters => names(workspace.templates().snapshot().iter().filter_map(
                |(name, definition)| {
                    definition
                        .calculation
                        .as_ref()
                        .filter(|calculation| calculation.conversion_eligible())
                        .map(|_| name.as_str())
                },
            )),
            ListRegistry::Views => names(workspace.view_catalogue().keys().map(String::as_str)),
            ListRegistry::Types => {
                let registry = workspace.contracts().snapshot();
                if registry.len().saturating_add(types::CONSTRUCTORS.len()) > max_rows() {
                    return Err(Failure::Limit);
                }
                Ok(Capture::Types(registry.values().cloned().collect()))
            }
            ListRegistry::Nodes => nodes::capture_nodes(workspace, None).map(Capture::Nodes),
            ListRegistry::Names => nodes::capture_names(workspace).map(Capture::Bindings),
            ListRegistry::Providers => {
                let catalogue = self.catalogue(workspace)?;
                if catalogue.provider_names().len() > max_rows() {
                    return Err(Failure::Limit);
                }
                Ok(Capture::Providers(
                    catalogue
                        .provider_names()
                        .map(|name| {
                            (
                                name.into(),
                                self.environment_context
                                    .as_ref()
                                    .and_then(|c| c.selected.clone()),
                            )
                        })
                        .collect(),
                ))
            }
            ListRegistry::Importers => names(workspace.importer_names()),
            ListRegistry::Workspaces => names(
                workspace
                    .saved_workspace_names()
                    .iter()
                    .map(WorkspaceName::as_str),
            ),
            ListRegistry::Commands => names(wes_language::vocabulary::commands::roots()),
            ListRegistry::Capabilities => {
                let catalogue = self.catalogue(workspace)?;
                let filter = match self.task.inputs.get("provider") {
                    Some(Input::Literal(value)) => match value.data() {
                        Data::Text(name) => Some(name.as_ref()),
                        _ => return Err(Failure::Unsupported),
                    },
                    None => None,
                    _ => return Err(Failure::Unsupported),
                };
                if catalogue.provider_names().len() > max_rows() {
                    return Err(Failure::Limit);
                }
                let mut found = vec![];
                for name in catalogue
                    .provider_names()
                    .filter(|name| filter.is_none_or(|filter| *name == filter))
                {
                    let provider = catalogue.provider(name).expect("catalogue name");
                    if found.len().saturating_add(provider.capabilities().len()) > max_rows() {
                        return Err(Failure::Limit);
                    }
                    found.extend(
                        provider
                            .capabilities()
                            .map(|capability| (provider.clone(), capability.clone())),
                    );
                }
                Ok(Capture::Capabilities(found))
            }
        }
    }
    fn literal_text(&self, key: &str) -> Result<Option<&str>, Failure> {
        match self.task.inputs.get(key) {
            None => Ok(None),
            Some(Input::Literal(value)) => match value.data() {
                Data::Text(s) => Ok(Some(s)),
                _ => Err(Failure::Unsupported),
            },
            _ => Err(Failure::Unsupported),
        }
    }
    fn project_read(&self, value: &Value) -> Result<Value, Failure> {
        if !["select", "offset", "limit"]
            .iter()
            .any(|key| self.task.inputs.contains_key(*key))
        {
            crate::value_size::value_charge(value, 64 * 1024).ok_or(Failure::Limit)?;
            return Ok(value.clone());
        }
        let mut selected = value.data();
        if let Some(path) = self.literal_text("select")? {
            if path.len() > 4096 {
                return Err(Failure::Limit);
            }
            if path.is_empty() || path.split('.').any(str::is_empty) {
                return Err(Failure::Selection(
                    "select: requires nonempty dot-separated record fields.",
                ));
            }
            for field in path.split('.') {
                selected = match selected {
                    Data::Record(fields) => fields
                        .get(field)
                        .ok_or_else(|| Failure::missing("selected field", field))?,
                    _ => {
                        return Err(Failure::Selection(
                            "select: cannot traverse a non-record value.",
                        ));
                    }
                };
            }
        }
        let number = |key| -> Result<Option<usize>, Failure> {
            match self.task.inputs.get(key) {
                None => Ok(None),
                Some(Input::Literal(value)) => match value.data() {
                    Data::Int(n) => usize::try_from(*n).map(Some).map_err(|_| {
                        Failure::Selection("offset: and limit: must be nonnegative integers.")
                    }),
                    _ => Err(Failure::Unsupported),
                },
                _ => Err(Failure::Unsupported),
            }
        };
        let offset = number("offset")?.unwrap_or(0);
        let limit = number("limit")?;
        if limit.is_some_and(|n| n > max_rows()) {
            return Err(Failure::Limit);
        }
        let mut remaining = 64 * 1024u64;
        let data = match selected {
            Data::List(items) => {
                let from = offset.min(items.len());
                let end = from
                    .saturating_add(limit.unwrap_or(items.len()))
                    .min(items.len());
                let page = &items[from..end];
                if page.len() > max_rows() {
                    return Err(Failure::Limit);
                }
                for item in page {
                    remaining = remaining
                        .checked_sub(
                            crate::value_size::data_charge(item, remaining)
                                .ok_or(Failure::Limit)?,
                        )
                        .ok_or(Failure::Limit)?;
                }
                fields([
                    ("data", Data::List(page.to_vec())),
                    ("offset", Data::Int(from as i64)),
                    ("total", Data::Int(items.len() as i64)),
                    ("hasMore", Data::Bool(end < items.len())),
                ])
            }
            _ if self.task.inputs.contains_key("offset") || limit.is_some() => {
                return Err(Failure::Selection(
                    "offset: and limit: require a list value.",
                ));
            }
            _ => {
                crate::value_size::data_charge(selected, remaining).ok_or(Failure::Limit)?;
                fields([("data", selected.clone())])
            }
        };
        let projected = Value::new(Shape::Unknown, data, value.provenance().clone())
            .map_err(|_| Failure::Unsupported)?;
        crate::value_size::value_charge(&projected, 64 * 1024).ok_or(Failure::Limit)?;
        Ok(projected)
    }
    fn catalogue<'a>(
        &self,
        workspace: &'a Workspace,
    ) -> Result<std::borrow::Cow<'a, wes_core::capability::Catalogue>, Failure> {
        use std::borrow::Cow;
        match &self.environment_context {
            None => Ok(Cow::Borrowed(workspace.catalogue())),
            Some(context) => match &context.selected {
                None => Ok(Cow::Owned(wes_core::capability::Catalogue::default())),
                Some(name) => workspace
                    .environment_images
                    .get(
                        name,
                        *context
                            .revisions
                            .get(name)
                            .ok_or_else(|| Failure::missing("environment revision", name))?,
                    )
                    .map(|image| Cow::Borrowed(image.catalogue()))
                    .ok_or_else(|| Failure::missing("environment image", name)),
            },
        }
    }
    pub(crate) fn evaluate(
        &self,
        _inputs: &IndexMap<NodeId, Value>,
        token: &CancellationToken,
    ) -> Outcome {
        if let Some(Ok(Capture::Type(types::Captured::Invalid(error)))) = &self.captured {
            if token.is_cancelled() {
                return Outcome::Cancelled(
                    RuntimeCode::Cancelled.error("The inspection was cancelled.", None),
                );
            }
            return Outcome::Failed(
                ErrorValue::new(
                    wes_core::ErrorId::new(uuid::Uuid::new_v4().to_string()).expect("UUID"),
                    error.code,
                    error.message.clone(),
                    vec![],
                    None,
                )
                .expect("type diagnostic"),
            );
        }
        if let Some(Ok(Capture::Help(help))) = &self.captured {
            return help.evaluate(token);
        }
        let result = (|| {
            let captured = self
                .captured
                .as_ref()
                .ok_or(Failure::Uncaptured)?
                .as_ref()
                .map_err(Clone::clone)?;
            let mut budget = Budget {
                bytes: 0,
                work: 0,
                token,
            };
            budget.step()?;
            if let Capture::Trace(value) | Capture::Information(value) = captured {
                return Ok(value.clone());
            }
            let data = match captured {
                Capture::Types(contracts) => types::list(contracts, &mut budget)?,
                Capture::Type(contract) => types::describe(contract, &mut budget)?,
                Capture::Names(names) => Data::List(
                    names
                        .iter()
                        .map(|name| budget.text(name))
                        .collect::<Result<_, _>>()?,
                ),
                Capture::Providers(providers) => Data::List(
                    providers
                        .iter()
                        .map(|(name, environment)| {
                            Ok(Data::Record(IndexMap::from([
                                ("provider".into(), budget.text(name)?),
                                (
                                    "environment".into(),
                                    match environment {
                                        Some(name) => budget.text(name)?,
                                        None => Data::Option(None),
                                    },
                                ),
                            ])))
                        })
                        .collect::<Result<_, Failure>>()?,
                ),
                Capture::Capabilities(capabilities) => {
                    let mut descriptions = capabilities
                        .iter()
                        .map(|(provider, capability)| describe(provider, capability, &mut budget));
                    if self.task.spec.command == MetaCommand::Inspect {
                        descriptions
                            .next()
                            .ok_or(Failure::missing("query target", "unspecified"))??
                    } else {
                        Data::List(descriptions.collect::<Result<_, _>>()?)
                    }
                }
                Capture::Trace(_) | Capture::Information(_) | Capture::Help(_) => unreachable!(),
                Capture::Data(data) => data.clone(),
                Capture::Nodes(nodes) => {
                    let mut descriptions = nodes.iter().map(|node| node.describe(&mut budget));
                    if self.task.spec.command == MetaCommand::Inspect {
                        descriptions
                            .next()
                            .ok_or(Failure::missing("query target", "unspecified"))??
                    } else {
                        Data::List(descriptions.collect::<Result<_, _>>()?)
                    }
                }
                Capture::Bindings(names) => Data::List(
                    names
                        .iter()
                        .map(|name| name.describe(&mut budget))
                        .collect::<Result<_, _>>()?,
                ),
            };
            Ok(Value::new(
                self.predicted_typing().shape,
                data,
                self.captured_at
                    .as_ref()
                    .map_or_else(Provenance::default, |at| {
                        Provenance::default()
                            .with_fact("snapshot.kind", "names")
                            .with_fact("snapshot.capturedAt", at)
                    }),
            )
            .expect("query value shape"))
        })();
        match result {
            Ok(value) => Outcome::Produced(value),
            Err(Failure::Cancelled) => Outcome::Cancelled(
                RuntimeCode::Cancelled.error("The inspection was cancelled.", None),
            ),
            Err(error) => {
                let message = match error {
                    Failure::Session(message) | Failure::Selection(message) => message.into(),
                    Failure::Limit => "The inspection exceeds its supported size or work budget.".into(),
                    Failure::TraceMissing => "No trace retained for that node and run. Use @trace(profile) on a supported call; old attempts may have been evicted.".into(),
                    Failure::Missing { kind, target } => format!("Missing {kind}: {target:?}. Inspect the current workspace/catalogue; the query did not rerun the target."),
                    Failure::OutputUnavailable(message) => message,
                    Failure::Unsupported => "This workspace query is not implemented yet.".into(),
                    Failure::Uncaptured => "Workspace queries require a ticket captured by the workspace owner at entry.".into(),
                    Failure::Cancelled => unreachable!(),
                };
                Outcome::Failed(RuntimeCode::ExecutionFailed.error(&message, None))
            }
        }
    }
}
fn run_metadata(workspace: &Workspace) -> Result<Vec<Data>, Failure> {
    if workspace
        .runtime()
        .graph()
        .nodes()
        .take(max_rows() + 1)
        .count()
        > max_rows()
    {
        return Err(Failure::Limit);
    }
    Ok(workspace
        .runtime()
        .graph()
        .nodes()
        .filter_map(|node| {
            let run = workspace.runtime().run_of(node.id())?;
            Some(fields([
                ("id", Data::Text(run.as_str().into())),
                ("node", Data::Text(node.id().as_str().into())),
                (
                    "state",
                    Data::Text(format!("{:?}", node.state()).to_lowercase().into()),
                ),
            ]))
        })
        .collect())
}
#[cfg(test)]
pub(super) fn importer_help(
    name: &str,
    parameters: &[wes_core::capability::Parameter],
    token: &CancellationToken,
) -> Outcome {
    importer_help_command(
        name,
        &crate::imports::ImporterMetadata {
            parameters: parameters.to_vec(),
            ..Default::default()
        },
        MetaCommand::Import,
        token,
    )
}
pub(super) fn importer_help_command(
    name: &str,
    metadata: &crate::imports::ImporterMetadata,
    command: MetaCommand,
    token: &CancellationToken,
) -> Outcome {
    let result = (|| {
        let mut budget = Budget {
            bytes: 0,
            work: 0,
            token,
        };
        let path = if command == MetaCommand::ImportPlan {
            format!("import plan {name}")
        } else {
            format!("import {name}")
        };
        let mut usage = format!(":{path}");
        let mut rows = vec![];
        let common = command.spec(&[]);
        for parameter in metadata.parameters.iter().chain(common.parameters.iter()) {
            let shape = budget.shape(&parameter.shape)?;
            // Structured type data and the human usage spelling are separate projections.
            // shape() already bounded depth/work/bytes before formatting the spelling.
            let shape_text = parameter.shape.to_string();
            let argument = format!(" {}:<{}>", parameter.name, shape_text);
            if parameter.required {
                budget.append(&mut usage, &argument)?;
            } else {
                budget.append(&mut usage, &format!(" [{}]", argument.trim()))?;
            }
            rows.push(fields([
                ("name", budget.text(&parameter.name)?),
                ("type", shape),
                ("required", Data::Bool(parameter.required)),
            ]));
        }
        Ok::<_, Failure>(fields([
            ("path", budget.text(&path)?),
            (
                "summary",
                budget.text(metadata.summary.unwrap_or(common.summary))?,
            ),
            (
                "invocation",
                fields([
                    ("command", budget.text(&format!(":{path}"))?),
                    ("usage", budget.text(&usage)?),
                    ("parameters", Data::List(rows)),
                ]),
            ),
            (
                "requirements",
                Data::List(
                    metadata
                        .exactly_one
                        .iter()
                        .map(|keys| {
                            fields([
                                ("kind", Data::Text("exactly_one".into())),
                                (
                                    "arguments",
                                    Data::List(
                                        keys.iter()
                                            .map(|key| Data::Text(key.as_str().into()))
                                            .collect(),
                                    ),
                                ),
                            ])
                        })
                        .collect(),
                ),
            ),
            ("children", Data::List(vec![])),
        ]))
    })();
    match result {
        Ok(data) => Outcome::Produced(super::help::help_value(data)),
        Err(Failure::Cancelled) => {
            Outcome::Cancelled(RuntimeCode::Cancelled.error("Help cancelled.", None))
        }
        Err(_) => Outcome::Failed(
            RuntimeCode::ExecutionFailed.error("Help metadata exceeds its budget.", None),
        ),
    }
}

pub(super) fn provider_help_path(
    provider: &ProviderDescription,
    path: &[String],
    token: &CancellationToken,
) -> Outcome {
    let result = (|| {
        let mut budget = Budget {
            bytes: 0,
            work: 0,
            token,
        };
        let invocation = match provider.capability(path) {
            Some(capability) => describe(provider, capability, &mut budget)?,
            None => Data::Option(None),
        };
        let mut children = std::collections::BTreeMap::new();
        for capability in provider.capabilities() {
            budget.step()?;
            if capability.path.starts_with(path) && capability.path.len() > path.len() {
                let name = &capability.path[path.len()];
                if !children.contains_key(name) {
                    children.insert(
                        name,
                        fields([
                            ("name", budget.text(name)?),
                            (
                                "summary",
                                if capability.path.len() == path.len() + 1 {
                                    budget.text(&capability.summary)?
                                } else {
                                    budget.text("Capability group.")?
                                },
                            ),
                        ]),
                    );
                }
            }
        }
        Ok::<_, Failure>(fields([
            ("provider", budget.text(provider.name())?),
            ("path", budget.text(&path.join(" "))?),
            ("invocation", invocation),
            ("children", Data::List(children.into_values().collect())),
        ]))
    })();
    match result {
        Ok(data) => Outcome::Produced(super::help::help_value(data)),
        Err(Failure::Cancelled) => {
            Outcome::Cancelled(RuntimeCode::Cancelled.error("Help cancelled.", None))
        }
        Err(_) => Outcome::Failed(
            RuntimeCode::ExecutionFailed.error("Help metadata exceeds its budget.", None),
        ),
    }
}

fn names<'a>(names: impl IntoIterator<Item = &'a str>) -> Result<Capture, Failure> {
    let mut found = vec![];
    let mut bytes = 0usize;
    for name in names {
        bytes = bytes.checked_add(name.len()).ok_or(Failure::Limit)?;
        if found.len() == max_rows() || bytes > max_bytes() {
            return Err(Failure::Limit);
        }
        found.push(name.to_string());
    }
    Ok(Capture::Names(found))
}
struct Budget<'a> {
    bytes: usize,
    work: usize,
    token: &'a CancellationToken,
}
impl Budget<'_> {
    fn step(&mut self) -> Result<(), Failure> {
        if self.token.is_cancelled() {
            return Err(Failure::Cancelled);
        }
        self.work += 1;
        if self.work > max_work() {
            return Err(Failure::Limit);
        }
        Ok(())
    }
    fn append(&mut self, target: &mut String, text: &str) -> Result<(), Failure> {
        self.step()?;
        self.bytes = self.bytes.checked_add(text.len()).ok_or(Failure::Limit)?;
        if self.bytes > max_bytes() {
            return Err(Failure::Limit);
        }
        target.push_str(text);
        Ok(())
    }
    fn text(&mut self, text: &str) -> Result<Data, Failure> {
        let mut output = String::new();
        self.append(&mut output, text)?;
        Ok(Data::Text(output.into()))
    }
    /// A shape as structured data, the way the wire types a value (`kind`, `name`, `element`,
    /// `fields`), so a client presents it with the same printer it uses for every type and an
    /// agent reads it without parsing an expression. Every node costs a step; names count bytes.
    fn shape(&mut self, shape: &Shape) -> Result<Data, Failure> {
        self.shape_data(shape, 0)
    }
    fn shape_data(&mut self, shape: &Shape, depth: usize) -> Result<Data, Failure> {
        self.step()?;
        if depth > 64 {
            return Err(Failure::Limit);
        }
        Ok(match shape {
            Shape::Primitive(primitive) => fields([
                ("kind", self.text("primitive")?),
                ("name", self.text(&primitive.to_string().to_uppercase())?),
            ]),
            Shape::Meta(t) => fields([
                ("kind", self.text("meta")?),
                ("name", self.text(&t.to_string())?),
            ]),
            Shape::Unknown => fields([("kind", self.text("unknown")?)]),
            Shape::List(element) | Shape::Option(element) | Shape::Iter(element) => {
                let kind = if matches!(shape, Shape::Iter(_)) {
                    "iter"
                } else if matches!(shape, Shape::Option(_)) {
                    "option"
                } else {
                    "list"
                };
                let mut description = IndexMap::from([
                    ("kind".into(), self.text(kind)?),
                    ("element".into(), self.shape_data(element, depth + 1)?),
                ]);
                if **element == Shape::Unknown {
                    description.insert("elementTypeNote".into(), self.text("No common element type is known: values may be heterogeneous, empty or unresolved. Values are not coerced. Validate an explicit contract with :type check before relying on a homogeneous element type.")?);
                }
                Data::Record(description)
            }
            Shape::Record(record) => {
                let mut described = Vec::new();
                for (name, field) in record.fields() {
                    described.push(fields([
                        ("name", self.text(name)?),
                        ("type", self.shape_data(field, depth + 1)?),
                    ]));
                }
                fields([
                    ("kind", self.text("record")?),
                    ("name", self.text(record.name())?),
                    ("fields", Data::List(described)),
                ])
            }
        })
    }
    fn joined<'a>(
        &mut self,
        parts: impl IntoIterator<Item = &'a str>,
        separator: &str,
        output: &mut String,
    ) -> Result<(), Failure> {
        for (index, part) in parts.into_iter().enumerate() {
            if index > 0 {
                self.append(output, separator)?;
            }
            self.append(output, part)?;
        }
        Ok(())
    }
}
fn describe(
    provider: &ProviderDescription,
    capability: &Capability,
    budget: &mut Budget<'_>,
) -> Result<Data, Failure> {
    budget.step()?;
    let mut path = String::new();
    budget.joined(capability.path.iter().map(String::as_str), " ", &mut path)?;
    let command = format!("{} {}", provider.name(), path);
    let mut usage = command.clone();
    let mut parameters = vec![];
    for parameter in &capability.parameters {
        let placeholder = format!("{}:<{}>", parameter.name, parameter.shape);
        budget.append(
            &mut usage,
            &if parameter.required {
                format!(" {placeholder}")
            } else {
                format!(" [{placeholder}]")
            },
        )?;
        budget.step()?;
        parameters.push(fields([
            ("name", budget.text(&parameter.name)?),
            ("type", budget.shape(&parameter.shape)?),
            ("required", Data::Bool(parameter.required)),
            (
                "constraints",
                Data::List(
                    parameter
                        .constraints
                        .iter()
                        .map(|hint| budget.text(hint))
                        .collect::<Result<_, _>>()?,
                ),
            ),
        ]));
    }
    let mut rules = vec![];
    for declared in &capability.rules {
        rules.push(rule(&declared.rule, budget)?);
    }
    Ok(fields([
        ("provider", budget.text(provider.name())?),
        ("command", budget.text(&command)?),
        ("usage", budget.text(&usage)?),
        ("usageNote", budget.text("Angle brackets are placeholders, not literal argument values; brackets mark optional arguments. Supply real values or existing $references. Showing help never calls the operation.")?),
        ("capability", Data::Text(path.into())),
        ("summary", budget.text(&capability.summary)?),
        (
            "safety",
            budget.text(match capability.safety {
                Safety::Safe => "SAFE",
                Safety::Unsafe => "UNSAFE",
            })?,
        ),
        ("result", budget.shape(&capability.result)?),
        ("streaming", Data::Bool(capability.streaming)),
        ("parameters", Data::List(parameters)),
        ("rules", Data::List(rules)),
    ]))
}
fn rule(rule: &Rule, budget: &mut Budget<'_>) -> Result<Data, Failure> {
    let mut output = String::new();
    match rule {
        Rule::MutuallyExclusive(keys) => {
            budget.append(&mut output, "at most one of [")?;
            budget.joined(keys.iter().map(String::as_str), ", ", &mut output)?;
            budget.append(&mut output, "]")?;
        }
        Rule::Requires { key, needs } => {
            budget.joined(["'", key, "' needs '", needs, "'"], "", &mut output)?
        }
        Rule::OneOf { key, values } => {
            budget.joined(["'", key, "' must be one of ["], "", &mut output)?;
            budget.joined(values.iter().map(String::as_str), ", ", &mut output)?;
            budget.append(&mut output, "]")?;
        }
        Rule::ProvenanceFact {
            key,
            fact,
            expected,
        } => budget.joined(
            ["'", key, "' must come from ", fact, "=", expected],
            "",
            &mut output,
        )?,
    }
    Ok(Data::Text(output.into()))
}
fn fields<const N: usize>(pairs: [(&str, Data); N]) -> Data {
    Data::Record(
        pairs
            .into_iter()
            .map(|(name, data)| (name.to_string(), data))
            .collect(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_node_query_preserves_the_selected_identity() {
        let workspace = Workspace::local(crate::providers::LocalScope::new("fixture").unwrap());
        let id = NodeId::new("retired-node").unwrap();
        let error = nodes::capture_nodes(&workspace, Some(&id)).unwrap_err();
        assert!(
            matches!(error, Failure::Missing { kind: "node in this workspace", target } if target == "retired-node")
        );
    }

    #[test]
    fn capture_bounds_check_rows_and_text_before_copying_the_rejected_entry() {
        assert!(matches!(
            names(std::iter::repeat_n("item", max_rows() + 1)),
            Err(Failure::Limit)
        ));
        assert!(names(std::iter::repeat_n("item", max_rows())).is_ok());
        let maximum = "x".repeat(max_bytes());
        assert!(names([maximum.as_str()]).is_ok());
        assert!(matches!(
            names([maximum.as_str(), "overflow"]),
            Err(Failure::Limit)
        ));
    }

    #[test]
    fn description_work_and_cancellation_limits_fail_before_returning_partial_results() {
        let token = CancellationToken::new();
        let mut budget = Budget {
            bytes: 0,
            work: max_work(),
            token: &token,
        };
        assert!(matches!(budget.text("unused"), Err(Failure::Limit)));
        assert_eq!(budget.bytes, 0);
        token.cancel();
        let mut budget = Budget {
            bytes: 0,
            work: 0,
            token: &token,
        };
        assert!(matches!(budget.text("unused"), Err(Failure::Cancelled)));
        assert_eq!(budget.bytes, 0);
    }
}

#[cfg(test)]
mod provider_help_tests {
    use super::*;

    #[test]
    fn provider_help_bounds_arbitrary_metadata_and_honors_cancellation() {
        let mut cap = Capability::new(["request"], Shape::Unknown, Safety::Safe);
        cap.summary = "x".repeat(max_bytes() + 1);
        let provider = ProviderDescription::new("synthetic", [cap], vec![]).unwrap();
        let token = CancellationToken::new();
        assert!(matches!(
            provider_help_path(&provider, &[], &token),
            Outcome::Failed(_)
        ));
        assert!(matches!(
            provider_help_path(&provider, &["request".into()], &token),
            Outcome::Failed(_)
        ));
        token.cancel();
        assert!(matches!(
            provider_help_path(&provider, &[], &token),
            Outcome::Cancelled(_)
        ));
    }
}

#[cfg(test)]
mod registry_laws {
    use super::*;
    use wes_language::vocabulary::RegistryScope;

    #[test]
    fn every_registry_dispatches_and_non_catalogue_queries_ignore_catalogue_context() {
        let workspace = Workspace::local(crate::providers::LocalScope::new("fixture").unwrap());
        for registry in ListRegistry::ALL {
            let tail = vec![registry.name().into()];
            let task = MetaTask {
                target: None,
                spec: MetaCommand::List.spec(&tail),
                tail,
                subjects: vec![],
                inputs: IndexMap::new(),
            };
            assert!(BoundQuery::supports(&task));
            let mut ordinary = BoundQuery::new(task.clone(), None);
            ordinary.capture(&workspace);
            if ordinary.session_request().is_some() {
                ordinary.capture_session(Ok(Data::List(vec![])));
            }
            let Outcome::Produced(expected) =
                ordinary.evaluate(&IndexMap::new(), &CancellationToken::new())
            else {
                panic!("{}", registry.name());
            };
            let mut changed = BoundQuery::new(
                task,
                Some(wes_core::environments::EnvironmentContext {
                    selected: Some("unavailable-environment".into()),
                    revisions: Default::default(),
                }),
            );
            changed.capture(&workspace);
            if changed.session_request().is_some() {
                changed.capture_session(Ok(Data::List(vec![])));
            }
            let result = changed.evaluate(&IndexMap::new(), &CancellationToken::new());
            if registry.scope() == RegistryScope::Catalogue {
                assert!(matches!(result, Outcome::Failed(_)));
            } else {
                let Outcome::Produced(actual) = result else {
                    panic!("{} depended on an unrelated catalogue", registry.name());
                };
                assert_eq!(actual.data(), expected.data(), "{}", registry.name());
                // The same captured query is deterministic, without recording new query nodes.
                let Outcome::Produced(again) =
                    changed.evaluate(&IndexMap::new(), &CancellationToken::new())
                else {
                    panic!();
                };
                assert_eq!(actual.data(), again.data());
            }
        }
    }
}

#[cfg(test)]
mod import_help_tests {
    use super::*;
    #[test]
    fn importer_help_includes_common_alias_and_explicit_replace_semantics() {
        let Outcome::Produced(value) = importer_help(
            "spec",
            &[wes_core::capability::Parameter::new(
                "file",
                Shape::Primitive(wes_core::Primitive::Text),
                false,
            )],
            &CancellationToken::new(),
        ) else {
            panic!("help")
        };
        let Data::Record(root) = value.data() else {
            panic!("record")
        };
        let Data::Record(invocation) = &root["invocation"] else {
            panic!("invocation")
        };
        let Data::Text(usage) = &invocation["usage"] else {
            panic!("usage")
        };
        assert!(usage.contains("replace:<Bool>"), "{usage}");
        assert!(usage.contains("as:<Text>"));
        let Data::Text(summary) = &root["summary"] else {
            panic!("summary")
        };
        assert!(summary.contains("replace:true"));
        let Data::List(parameters) = &invocation["parameters"] else {
            panic!("parameters")
        };
        assert_eq!(parameters.len(), 3);
    }
}
