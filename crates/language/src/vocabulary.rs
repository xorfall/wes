//! Closed language declarations, shared by analysis, help, completion and execution dispatch.
use wes_core::{Primitive, Shape, capability::Parameter};
pub mod commands;
mod environment;
pub mod scan;
mod workspace;
pub use workspace::{WorkspaceManagementCommand, workspace_management};
mod registry;
pub use environment::{ENVIRONMENT_COMMANDS, EnvironmentCommand};
pub use registry::{LIST_SEMANTICS, ListRegistry, RegistryScope, list_metadata, list_metadata_for};

/// Literal-only refresh selectors, shared by command binding and completion metadata.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RefreshScope {
    Downstream,
}
impl RefreshScope {
    pub const ALL: &[Self] = &[Self::Downstream];
    pub const fn name(self) -> &'static str {
        match self {
            Self::Downstream => "downstream",
        }
    }
    pub fn lookup(name: &str) -> Option<Self> {
        Self::ALL.iter().copied().find(|scope| scope.name() == name)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum MetaCommand {
    Env,
    Calc,
    Import,
    ImportPlan,
    ImportApply,
    Describe,
    Change,
    Accumulate,
    Scan,
    Stream,
    Fork,
    Sandbox,
    Inspect,
    Info,
    List,
    Wait,
    Refresh,
    Cancel,
    Timeout,
    Policy,
    Drop,
    ViewQuery,
    ViewApply,
    ViewCreate,
    ViewBind,
    ViewConnect,
    ViewDisconnect,
    ViewOutput,
    ViewCapture,
    ViewPin,
    ViewLink,
    ViewUnlink,
    ViewStart,
    ViewStop,
    WorkspacePlan,
    WorkspaceDelete,
    Save,
    Help,
    Load,
    Type,
    For,
    If,
    Use,
    Trace,
    Read,
    Alias,
    Def,
    Let,
    Set,
}

pub const COMMANDS: &[MetaCommand] = &[
    MetaCommand::Env,
    MetaCommand::Calc,
    MetaCommand::Import,
    MetaCommand::ImportPlan,
    MetaCommand::ImportApply,
    MetaCommand::Describe,
    MetaCommand::Change,
    MetaCommand::Accumulate,
    MetaCommand::Scan,
    MetaCommand::Stream,
    MetaCommand::Fork,
    MetaCommand::Sandbox,
    MetaCommand::Inspect,
    MetaCommand::Info,
    MetaCommand::List,
    MetaCommand::Wait,
    MetaCommand::Refresh,
    MetaCommand::Cancel,
    MetaCommand::Timeout,
    MetaCommand::Policy,
    MetaCommand::Drop,
    MetaCommand::ViewQuery,
    MetaCommand::ViewApply,
    MetaCommand::ViewCreate,
    MetaCommand::ViewBind,
    MetaCommand::ViewConnect,
    MetaCommand::ViewDisconnect,
    MetaCommand::ViewOutput,
    MetaCommand::ViewCapture,
    MetaCommand::ViewPin,
    MetaCommand::ViewLink,
    MetaCommand::ViewUnlink,
    MetaCommand::ViewStart,
    MetaCommand::ViewStop,
    MetaCommand::WorkspacePlan,
    MetaCommand::WorkspaceDelete,
    MetaCommand::Save,
    MetaCommand::Help,
    MetaCommand::Load,
    MetaCommand::Type,
    MetaCommand::For,
    MetaCommand::If,
    MetaCommand::Use,
    MetaCommand::Trace,
    MetaCommand::Read,
    MetaCommand::Alias,
    MetaCommand::Def,
    MetaCommand::Let,
    MetaCommand::Set,
];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Arity {
    pub min: usize,
    pub max: Option<usize>,
}
impl Arity {
    pub const NONE: Self = Self {
        min: 0,
        max: Some(0),
    };
    pub const fn bounded(min: usize, max: usize) -> Self {
        assert!(min <= max);
        Self {
            min,
            max: Some(max),
        }
    }
    pub const fn unbounded(min: usize) -> Self {
        Self { min, max: None }
    }
    pub fn accepts(self, count: usize) -> bool {
        count >= self.min && self.max.is_none_or(|max| count <= max)
    }
}

#[derive(Clone, Debug)]
pub struct CommandSpec {
    pub command: MetaCommand,
    pub canonical: String,
    pub reserved: bool,
    pub path_tail: Arity,
    pub operands: Arity,
    pub parameters: Vec<Parameter>,
    pub open_arguments: bool,
    pub requires_subject: bool,
    pub produces_value: bool,
    pub recorded: bool,
    pub derived: bool,
    pub summary: &'static str,
    pub tail_words: Vec<&'static str>,
    pub rewrites_target: bool,
    pub tail_registry: Option<&'static str>,
}

impl CommandSpec {
    pub fn parameter(&self, name: &str) -> Option<&Parameter> {
        self.parameters.iter().find(|p| p.name == name)
    }
}

impl MetaCommand {
    pub fn name(self) -> &'static str {
        match self {
            Self::Env => "env",
            Self::Calc => "calc",
            Self::Import => "import",
            Self::ImportPlan => "import-plan",
            Self::ImportApply => "import-apply",
            Self::Describe => "describe",
            Self::Change => "change",
            Self::Accumulate => "accumulate",
            Self::Scan => "scan",
            Self::Stream => "stream",
            Self::Fork => "fork",
            Self::Sandbox => "sandbox",
            Self::Inspect => "inspect",
            Self::Info => "info",
            Self::List => "list",
            Self::Wait => "wait",
            Self::Refresh => "refresh",
            Self::Cancel => "cancel",
            Self::Timeout => "timeout",
            Self::Policy => "policy",
            Self::Drop => "drop",
            Self::ViewQuery => "view-query",
            Self::ViewApply => "view-apply",
            Self::ViewCreate => "view-create",
            Self::ViewBind => "view-bind",
            Self::ViewConnect => "view-connect",
            Self::ViewDisconnect => "view-disconnect",
            Self::ViewOutput => "view-output",
            Self::ViewCapture => "view-capture",
            Self::ViewPin => "view-pin",
            Self::ViewLink => "view-link",
            Self::ViewUnlink => "view-unlink",
            Self::ViewStart => "view-start",
            Self::ViewStop => "view-stop",
            Self::WorkspacePlan => "workspace-plan",
            Self::WorkspaceDelete => "workspace-delete",
            Self::Save => "save",
            Self::Help => "help",
            Self::Load => "load",
            Self::Type => "type",
            Self::For => "for",
            Self::If => "if",
            Self::Use => "use",
            Self::Trace => "trace",
            Self::Read => "read",
            Self::Alias => "alias",
            Self::Def => "def",
            Self::Let => "let",
            Self::Set => "set",
        }
    }
    pub fn lookup(name: &str) -> Option<Self> {
        COMMANDS.iter().copied().find(|c| c.name() == name)
    }
    pub fn spec(self, tail: &[String]) -> CommandSpec {
        let mut spec = CommandSpec {
            command: self,
            canonical: self.name().into(),
            reserved: false,
            path_tail: Arity::NONE,
            operands: Arity::NONE,
            parameters: Vec::new(),
            open_arguments: false,
            requires_subject: false,
            produces_value: false,
            recorded: true,
            derived: false,
            summary: "",
            tail_words: Vec::new(),
            rewrites_target: false,
            tail_registry: None,
        };
        let text =
            |name, required| Parameter::new(name, Shape::Primitive(Primitive::Text), required);
        match self {
            Self::ViewQuery => {
                spec.summary =
                    "Bind a typed SAFE query template to a view; binding does not execute it";
                spec.operands = Arity::bounded(1, 1);
                spec.parameters = vec![
                    text("template", true),
                    text("mode", false),
                    text("adapter", false),
                    text("output", true),
                    text("trigger", false),
                    Parameter::new("from", Shape::Meta(wes_core::MetaType::ViewInstance), true),
                    Parameter::new("revision", Shape::Primitive(Primitive::Int), false),
                ];
                spec.produces_value = true;
            }
            Self::ViewApply => {
                spec.summary = "Run the bound SAFE view query; commit mode follows changed selections until Stop or last close";
                spec.operands = Arity::bounded(1, 1);
                spec.produces_value = true;
            }
            Self::ViewStart | Self::ViewStop => {
                spec.summary = "Start or stop observing already-running sources; no provider is started or cancelled";
                spec.operands = Arity::bounded(1, 1);
                spec.produces_value = true;
            }
            Self::ViewLink | Self::ViewUnlink => {
                spec.summary = "Connect a shared state output to an input field, or remove that connection; no provider runs";
                spec.operands = Arity::bounded(1, 1);
                spec.parameters = vec![
                    text("field", true),
                    Parameter::new("revision", Shape::Primitive(Primitive::Int), false),
                ];
                if self == Self::ViewLink {
                    spec.parameters.extend([
                        text("output", true),
                        Parameter::new("to", Shape::Meta(wes_core::MetaType::ViewInstance), true),
                    ]);
                }
                spec.produces_value = true;
            }
            Self::ViewPin => {
                spec.summary = "Keep the displayed typed input as a protected result, then bind the unchanged view to its acknowledged retained reference; never runs the source";
                spec.operands = Arity::bounded(1, 1);
                spec.parameters = vec![
                    text("instance", false),
                    Parameter::new("revision", Shape::Primitive(Primitive::Int), false),
                    Parameter::new("inputRevision", Shape::Primitive(Primitive::Int), false),
                ];
                spec.produces_value = true;
            }
            Self::ViewCapture => {
                spec.summary = "Capture current typed view inputs and committed outputs as finite evidence; normal retention applies";
                spec.operands = Arity::bounded(1, 1);
                spec.produces_value = true;
            }
            Self::ViewOutput => {
                spec.summary = "Read a committed view state output as a normal typed snapshot; no provider runs";
                spec.operands = Arity::bounded(1, 1);
                spec.parameters = vec![text("port", true)];
                spec.produces_value = true;
            }
            Self::ViewCreate => {
                spec.summary = "Create a workspace view instance with an optional finite input; no provider runs";
                spec.path_tail = Arity::bounded(1, 1);
                spec.tail_registry = Some("view");
                spec.parameters = vec![Parameter::new("input", Shape::Unknown, false)];
                spec.produces_value = true;
            }
            Self::ViewBind | Self::ViewConnect | Self::ViewDisconnect => {
                spec.summary = "Edit an existing view instance; its identity is preserved and no provider runs";
                spec.operands = Arity::bounded(1, 1);
                spec.produces_value = true;
                spec.parameters = if self == Self::ViewBind {
                    vec![Parameter::new("input", Shape::Unknown, true)]
                } else {
                    vec![
                        Parameter::new("to", Shape::Meta(wes_core::MetaType::ViewInstance), true),
                        text("slot", false),
                    ]
                };
                spec.parameters.push(Parameter::new(
                    "revision",
                    Shape::Primitive(Primitive::Int),
                    false,
                ));
            }
            Self::Describe => {
                spec.summary = "Convert OpenAPI JSON/YAML into an editable API draft without calling the API; reviewed operation safety: safe/unsafe may override the HTTP method default (SAFE permits automatic repetition)";
                spec.produces_value = true;
                spec.parameters = vec![
                    text("url", false),
                    text("file", false),
                    text("provider", true),
                    text("out", false),
                ];
            }
            Self::Env => {
                spec.summary = "plans environment definitions or changes this client's selection";
                spec.path_tail = Arity::bounded(1, 1);
                spec.tail_words = ENVIRONMENT_COMMANDS.iter().map(|spec| spec.name).collect();
                if let Some(action) = tail
                    .first()
                    .and_then(|name| EnvironmentCommand::lookup(name))
                {
                    spec.summary = action.summary;
                    spec.parameters = action
                        .parameters
                        .iter()
                        .map(|(name, required)| text(*name, *required))
                        .collect();
                    spec.operands = if action.plan_operand || action.name_operand {
                        Arity::bounded(1, 1)
                    } else {
                        Arity::NONE
                    };
                    spec.produces_value = action.plan_binding;
                }
                spec.recorded = false;
            }
            Self::Import => {
                spec.summary = "imports a service description (spec file:<path> or url:<http(s) URL>; process bin:<path>); replacing an existing provider requires explicit replace:true; omitted or false keeps the existing provider";
                spec.path_tail = Arity::bounded(1, 1);
                spec.tail_registry = Some("importer");
                spec.parameters.push(text("as", false).naming("provider"));
                spec.parameters.push(Parameter::new(
                    "replace",
                    Shape::Primitive(Primitive::Bool),
                    false,
                ));
                spec.open_arguments = true;
            }
            Self::ImportPlan => {
                spec.summary = "Freeze typed importer arguments without reading contents; apply explicitly with :import apply $plan. Live authority expires and cannot be restored.";
                spec.path_tail = Arity::bounded(1, 1);
                spec.tail_registry = Some("importer");
                spec.parameters.push(text("as", false).naming("provider"));
                spec.open_arguments = true;
                spec.produces_value = true;
            }
            Self::ImportApply => {
                spec.summary = "Read and install a live import plan once; changed inputs or authority refuse. Replacement requires literal replace:true.";
                spec.operands = Arity::bounded(1, 1);
                spec.parameters.push(Parameter::new(
                    "replace",
                    Shape::Primitive(Primitive::Bool),
                    false,
                ));
            }
            Self::Change => {
                spec.summary = "changes arguments of an existing captured call; does not change its provider connection";
                spec.operands = Arity::bounded(1, 1);
                spec.open_arguments = true;
                spec.rewrites_target = true;
            }
            Self::Stream => {
                spec.summary = "Native ordered stream operators; use filter field:/equals:, map field:, limit:, skip:, or accumulate limit:.";
                spec.path_tail = Arity::bounded(0, 1);
                spec.tail_words = vec!["filter", "map", "accumulate"];
                spec.parameters = vec![
                    text("field", false),
                    Parameter::new("equals", Shape::Unknown, false),
                    Parameter::new("condition", Shape::Primitive(Primitive::Bool), false),
                    Parameter::new("limit", Shape::Primitive(Primitive::Int), false),
                    Parameter::new("skip", Shape::Primitive(Primitive::Int), false),
                    text("overflow", false),
                ];
                spec.produces_value = true;
                spec.derived = true;
            }
            Self::Sandbox => {
                spec.summary = "Retain a named program with memory-only execution. Example: :sandbox { :calc { return 1; } > value } > preview";
                spec.recorded = false;
            }
            Self::Fork => {
                spec.summary = "Branch a preceding value or event with ordered, isolated pipelines; blocks may select on success/failed/cancelled or when a pure Bool definition.";
            }
            Self::Accumulate => {
                spec.summary = "Accumulate ordered events into an atomic items/checkpoint record.";
                spec.operands = Arity::bounded(1, 1);
                spec.parameters = vec![
                    Parameter::new("limit", Shape::Primitive(Primitive::Int), true),
                    text("overflow", false),
                ];
                spec.produces_value = true;
                spec.derived = true;
            }
            Self::Scan => {
                spec.summary = "Analyze an immutable finite source with captured pure step/finish definitions and one cumulative budget. One analysis is one run; memory output uses normal retention.";
                spec.parameters = scan::parameters();
                spec.produces_value = true;
            }
            Self::Info => {
                spec.summary = "shows retained provider constraints, advisories and source evidence; no API call";
                spec.path_tail = Arity::bounded(1, 1);
                spec.tail_registry = Some("provider");
                spec.produces_value = true;
            }
            Self::Inspect => {
                spec.summary = "describes a result, node, capability, named type contract or view package (:inspect view:Timeline)";
                spec.parameters = ["command", "provider", "capability"]
                    .into_iter()
                    .chain(crate::targets::OBJECT_KINDS.iter().map(|(_, name)| *name))
                    .map(|key| text(key, false).selecting(key))
                    .collect();
                spec.parameters.extend([
                    Parameter::new("node", Shape::Unknown, false),
                    Parameter::new("value", Shape::Unknown, false),
                ]);
                spec.operands = Arity::bounded(0, 1);
                spec.path_tail = Arity::unbounded(0);
                spec.requires_subject = true;
                spec.produces_value = true;
            }
            Self::List => {
                spec.summary = "lists a workspace registry";
                spec.path_tail = Arity::bounded(1, 1);
                spec.tail_words = ListRegistry::ALL.iter().map(|r| r.name()).collect();
                if let Some(registry) = tail.first().and_then(|name| ListRegistry::lookup(name)) {
                    spec.summary = registry.summary();
                    spec.parameters = registry.parameters();
                }
                spec.produces_value = true;
            }
            Self::Wait => {
                spec.summary = "waits for selected outputs";
                spec.operands = Arity::unbounded(1);
                spec.recorded = false;
            }
            Self::Refresh => {
                spec.summary = "runs a captured node again; changed provider bindings require a new command or New branch; scope:downstream also explicitly reruns its finite dependents";
                spec.operands = Arity::bounded(1, 1);
                spec.parameters
                    .push(text("scope", false).selecting("refresh-scope"));
                spec.recorded = false;
            }
            Self::Cancel => {
                spec.summary = "requests cancellation of an active run";
                spec.operands = Arity::bounded(1, 1);
                spec.recorded = false;
            }
            Self::Timeout => {
                spec.summary = "sets a local execution budget that cancels on expiry";
                spec.operands = Arity::bounded(1, 1);
                spec.parameters.push(Parameter::new(
                    "after",
                    Shape::Primitive(Primitive::Duration),
                    true,
                ));
            }
            Self::Policy => {
                spec.summary = "sets automatic (bounded pure work only), manual or explicitly reactive execution";
                spec.operands = Arity::bounded(0, 1);
                spec.parameters.push(text("mode", true).selecting("policy"));
            }
            Self::Drop => {
                spec.summary = "removes a name or a node and its dependents";
                spec.operands = Arity::bounded(0, 1);
                spec.path_tail = Arity::bounded(0, 1);
                spec.requires_subject = true;
            }
            Self::WorkspacePlan => {
                spec.summary = "Produce a WorkspaceDeletePlan for workspace:\"name\" (default: the issuing workspace); no commands are run";
                spec.path_tail = Arity::bounded(1, 1);
                spec.tail_words = vec!["delete"];
                spec.parameters.push(text("workspace", false));
                spec.produces_value = true;
            }
            Self::WorkspaceDelete => {
                spec.summary = "Apply a WorkspaceDeletePlan; stop:true and protected:true explicitly approve additional effects";
                spec.operands = Arity::bounded(1, 1);
                spec.parameters = ["stop", "protected"]
                    .into_iter()
                    .map(|name| Parameter::new(name, Shape::Primitive(Primitive::Bool), false))
                    .collect();
                spec.recorded = false;
            }
            Self::Save => {
                spec.summary = "saves the workspace under a name";
                spec.path_tail = Arity::bounded(1, 1);
                spec.recorded = false;
            }
            Self::Calc => {
                spec.summary =
                    "evaluates a bounded calculation block; :calc { return expression; }";
                spec.produces_value = true;
            }
            Self::Help => {
                spec.parameters = ["command", "provider", "capability"]
                    .into_iter()
                    .map(|key| text(key, false).selecting(key))
                    .collect();
                spec.summary = "describes meta commands and providers in the selected environment";
                spec.path_tail = Arity::bounded(0, 1);
                spec.produces_value = true;
                spec.tail_words = commands::roots();
            }
            Self::Load => {
                spec.summary = "opens a saved workspace";
                spec.path_tail = Arity::bounded(1, 1);
                spec.recorded = false;
            }
            Self::Type => {
                spec.summary = "loads contracts or validates a value";
                spec.path_tail = Arity::bounded(1, 2);
                spec.tail_words = vec!["load", "check"];
                spec.open_arguments = true;
                match tail.first().map(String::as_str) {
                    Some("load") => {
                        spec.summary = "loads an immutable YAML type package";
                        spec.path_tail = Arity::bounded(1, 1);
                        spec.parameters.push(text("path", false));
                        spec.parameters.push(text("source", false));
                        spec.parameters.push(text("origin", false));
                        spec.open_arguments = false;
                        spec.tail_words.clear();
                    }
                    Some("check") => {
                        spec.summary = "validates without converting an existing result";
                        spec.parameters.push(text("as", false).selecting("type"));
                        spec.parameters
                            .push(Parameter::new("value", Shape::Unknown, true));
                        spec.open_arguments = false;
                        spec.tail_words.clear();
                        spec.produces_value = true;
                        spec.derived = true;
                    }
                    _ => {}
                }
            }
            Self::Def => {
                spec.summary = "defines a reusable single-command template";
                spec.path_tail = Arity::unbounded(1);
                spec.open_arguments = true;
            }
            Self::Read => {
                spec.summary = "reads selected output or retained trace content";
                spec.operands = Arity::bounded(0, 1);
                spec.produces_value = true;
                spec.parameters = vec![
                    Parameter::new("value", Shape::Unknown, false),
                    Parameter::new("trace", Shape::Unknown, false),
                    text("run", false),
                    text("select", false),
                    Parameter::new("offset", Shape::Primitive(Primitive::Int), false),
                    Parameter::new("limit", Shape::Primitive(Primitive::Int), false),
                ];
            }
            Self::Trace => {
                spec.summary = "reads retained observations without repeating a call";
                spec.operands = Arity::bounded(1, 1);
                spec.parameters.push(text("run", false));
                spec.produces_value = true;
            }
            Self::For | Self::If | Self::Use | Self::Alias | Self::Let | Self::Set => {
                spec.reserved = true;
            }
        }
        spec
    }
}

pub const ANNOTATIONS: &[(&str, &str)] = &[
    (
        "trace",
        "collects bounded observations using an explicit provider-supported profile: @trace(profile)",
    ),
    (
        "env",
        "selects an acknowledged environment for a new invocation or calculation",
    ),
    (
        "unchecked",
        "downgrades eligible rule failures and records a caution",
    ),
    (
        "interactive",
        "connects an interactive command to the client",
    ),
];
pub fn caution(annotation: &str, target: &str) -> String {
    format!("{annotation}:{target}")
}
pub fn excused(
    cautions: impl IntoIterator<Item = impl AsRef<str>>,
) -> std::collections::BTreeSet<String> {
    cautions
        .into_iter()
        .filter_map(|value| value.as_ref().strip_prefix("unchecked:").map(str::to_owned))
        .collect()
}
