//! Public command paths and pure lowering to domain operation signatures.
//! Domain handlers consume operation IDs; source spelling is never an authority decision.
use super::{Arity, CommandSpec, MetaCommand};
use crate::{Argument, Call, Diagnostic, Name, Value};

#[derive(Clone, Copy, Debug)]
pub struct CommandPath {
    pub path: &'static [&'static str],
    pub operation: MetaCommand,
    pub short: Option<&'static str>,
    pub summary: &'static str,
}
use MetaCommand::*;
pub const COMMAND_PATHS: &[CommandPath] = &[
    CommandPath {
        path: &["view", "query"],
        operation: ViewQuery,
        short: None,
        summary: "Configure without running: :view query $detail template:Query from:$timeline output:selection [trigger:manual|commit]. Add mode:live adapter:Draw for an owned SAFE stream and typed bounded-window projection.",
    },
    CommandPath {
        path: &["view", "apply"],
        operation: ViewApply,
        short: None,
        summary: "Run a configured SAFE query: :view apply $detail. Commit mode follows selection changes until Stop; restore waits for Apply.",
    },
    CommandPath {
        path: &["view", "start"],
        operation: ViewStart,
        short: None,
        summary: "Start observing Current references: :view start $dashboard. Does not run source commands.",
    },
    CommandPath {
        path: &["view", "stop"],
        operation: ViewStop,
        short: None,
        summary: "Stop view observations: :view stop $dashboard. Shared external streams keep running.",
    },
    CommandPath {
        path: &["view", "link"],
        operation: ViewLink,
        short: None,
        summary: "Bind committed output to a field: :view link $chart output:selection to:$detail field:selection.",
    },
    CommandPath {
        path: &["view", "unlink"],
        operation: ViewUnlink,
        short: None,
        summary: "Remove an input field binding: :view unlink $detail field:selection. The base input is preserved.",
    },
    CommandPath {
        path: &["view", "pin"],
        operation: ViewPin,
        short: None,
        summary: "Keep the displayed input and pin this view after acknowledged retention: :view pin $chart > pinned. Optional instance, revision and inputRevision guards refuse a changed display.",
    },
    CommandPath {
        path: &["view", "capture"],
        operation: ViewCapture,
        short: None,
        summary: "Capture visible data and source metadata: :view capture $dashboard > evidence. Keep the result to retain it.",
    },
    CommandPath {
        path: &["view", "output"],
        operation: ViewOutput,
        short: None,
        summary: "Read a committed output: :view output $chart port:selection > selectedRange. Hover stays local.",
    },
    CommandPath {
        path: &["view", "create"],
        operation: ViewCreate,
        short: None,
        summary: "Create a view instance: :view create Timeline input:$data > chart. A finite pipeline may end with :view create ViewName > chart; maps belong in preceding adapter stages. Creation runs once; later updates feed the same Current instance. Names are not retention promises; use Pin/Keep for durable input.",
    },
    CommandPath {
        path: &["view", "bind"],
        operation: ViewBind,
        short: None,
        summary: "Bind a current work output: :view bind $chart input:$data. Visible views read committed inputs without starting the source.",
    },
    CommandPath {
        path: &["view", "connect"],
        operation: ViewConnect,
        short: None,
        summary: "Add a view to a container slot: :view connect $chart to:$group [slot:members].",
    },
    CommandPath {
        path: &["view", "disconnect"],
        operation: ViewDisconnect,
        short: None,
        summary: "Remove a view from a container slot: :view disconnect $chart to:$group [slot:members].",
    },
    CommandPath {
        path: &["workspace", "plan"],
        operation: WorkspacePlan,
        short: None,
        summary: "Plan deletion: :workspace plan delete [workspace:\"name\"] [> plan]. Defaults to the issuing workspace; returns WorkspaceDeletePlan.",
    },
    CommandPath {
        path: &["workspace", "delete"],
        operation: WorkspaceDelete,
        short: None,
        summary: "Apply a WorkspaceDeletePlan: :workspace delete $plan [stop:true] [protected:true].",
    },
    CommandPath {
        path: &["sandbox"],
        operation: Sandbox,
        short: None,
        summary: "Retain an isolated program; only definitions persist. Providers act on real targets, not simulated machines. Use :list sandboxes, :read/:inspect $name, :cancel/:refresh $name, or :remove $name scope:downstream (joined stop and definition removal). Example: :sandbox { :calc { return 1; } > value } > preview",
    },
    CommandPath {
        path: &["info"],
        operation: Info,
        short: None,
        summary: "Show retained provider constraints and evidence. Example: :info catalog",
    },
    CommandPath {
        path: &["describe"],
        operation: Describe,
        short: None,
        summary: "Read OpenAPI JSON/YAML into an editable API draft; no API call.",
    },
    CommandPath {
        path: &["stream"],
        operation: Stream,
        short: None,
        summary: "Apply a fixed native operator to pipeline events.",
    },
    CommandPath {
        path: &["fork"],
        operation: Fork,
        short: None,
        summary: "Branch one input into ordered pipelines with isolated failure.",
    },
    CommandPath {
        path: &["accumulate"],
        operation: Accumulate,
        short: None,
        summary: "Accumulate each ordered event with a bounded, atomic checkpoint.",
    },
    CommandPath {
        path: &["scan"],
        operation: Scan,
        short: None,
        summary: "Analyze captured finite records with declared pure transitions, coverage and cumulative limits.",
    },
    CommandPath {
        path: &["help"],
        operation: Help,
        short: None,
        summary: "Explain a command family, provider group or operation.",
    },
    CommandPath {
        path: &["inspect"],
        operation: Inspect,
        short: None,
        summary: "Describe one object's definition and current state.",
    },
    CommandPath {
        path: &["list"],
        operation: List,
        short: None,
        summary: "List the entries of a declared registry.",
    },
    CommandPath {
        path: &["read"],
        operation: Read,
        short: None,
        summary: "Read selected output or retained trace content.",
    },
    CommandPath {
        path: &["node", "refresh"],
        operation: Refresh,
        short: Some("refresh"),
        summary: "Start a new run of the same node definition.",
    },
    CommandPath {
        path: &["node", "change"],
        operation: Change,
        short: Some("change"),
        summary: "Change a provider node's arguments.",
    },
    CommandPath {
        path: &["node", "cancel"],
        operation: Cancel,
        short: Some("cancel"),
        summary: "Cancel the node's active run.",
    },
    CommandPath {
        path: &["node", "timeout"],
        operation: Timeout,
        short: Some("timeout"),
        summary: "Set the node's execution budget.",
    },
    CommandPath {
        path: &["node", "policy"],
        operation: Policy,
        short: Some("policy"),
        summary: "Set an explicit node execution policy.",
    },
    CommandPath {
        path: &["node", "remove"],
        operation: Drop,
        short: Some("remove"),
        summary: "Remove a node and its downstream dependents.",
    },
    CommandPath {
        path: &["name", "unbind"],
        operation: Drop,
        short: None,
        summary: "Remove only a workspace name binding.",
    },
    CommandPath {
        path: &["workspace", "policy"],
        operation: Policy,
        short: None,
        summary: "Set the policy inherited by nodes without an override.",
    },
    CommandPath {
        path: &["workspace", "save"],
        operation: Save,
        short: None,
        summary: "Save a workspace snapshot.",
    },
    CommandPath {
        path: &["workspace", "load"],
        operation: Load,
        short: None,
        summary: "Open a saved workspace without executing its work.",
    },
    CommandPath {
        path: &["package", "load"],
        operation: Type,
        short: None,
        summary: "Load type/iterator definitions or a compiled View package into this workspace.",
    },
    CommandPath {
        path: &["type", "check"],
        operation: Type,
        short: None,
        summary: "Validate an output against a type contract.",
    },
    CommandPath {
        path: &["env"],
        operation: Env,
        short: None,
        summary: "Plan, manage and select environment definitions.",
    },
    CommandPath {
        path: &["calc"],
        operation: Calc,
        short: None,
        summary: "Evaluate a bounded calculation; pure asserts no external operations.",
    },
    CommandPath {
        path: &["def"],
        operation: Def,
        short: None,
        summary: "Declare a parameterized command template.",
    },
    CommandPath {
        path: &["import", "plan"],
        operation: ImportPlan,
        short: None,
        summary: "Freeze importer arguments; contents have not been read.",
    },
    CommandPath {
        path: &["import", "apply"],
        operation: ImportApply,
        short: None,
        summary: "Read and install a live import plan once.",
    },
    CommandPath {
        path: &["import"],
        operation: Import,
        short: None,
        summary: "Import provider metadata into a catalogue.",
    },
    CommandPath {
        path: &["wait"],
        operation: Wait,
        short: None,
        summary: "Wait for selected outputs to become available.",
    },
];

pub fn is_command_root(name: &str) -> bool {
    COMMAND_PATHS
        .iter()
        .any(|p| p.path[0] == name || p.short == Some(name))
}
pub fn roots() -> Vec<&'static str> {
    let mut roots = vec![];
    for p in COMMAND_PATHS {
        if !roots.contains(&p.path[0]) {
            roots.push(p.path[0]);
        }
    }
    roots
}

#[derive(Clone, Debug)]
pub struct CommandInvocation {
    pub call: Call,
    pub spec: CommandSpec,
    pub tail: Vec<String>,
}
fn invalid(call: &Call, message: impl Into<String>) -> Diagnostic {
    let message = message.into();
    Diagnostic::error("CMD001", call.span, &message).with_public_message(message)
}
fn named(call: &mut Call, key: &str, value: Value) {
    let span = value.name().span;
    call.arguments.push(Argument {
        key: Name {
            text: key.into(),
            span,
        },
        value,
        span,
    });
}

/// Idempotent source-to-source AST normalization. Synthetic keys/path words point to
/// their originating token span; operands, options and source text retain their identity.
pub fn normalize(source: &Call) -> Result<Call, Diagnostic> {
    let mut call = source.clone();
    let Some(head) = source.path.first() else {
        return Ok(call);
    };
    if let Some(path) = COMMAND_PATHS
        .iter()
        .find(|p| p.short == Some(head.text.as_str()))
    {
        if source.path.len() != 1 || !matches!(source.operands.as_slice(), [Value::Reference(_)]) {
            return Err(invalid(
                source,
                "This short form requires one output reference and no extra path words.",
            ));
        }
        call.path = path
            .path
            .iter()
            .map(|word| Name {
                text: (*word).into(),
                span: head.span,
            })
            .collect();
    }
    if matches!(head.text.as_str(), "inspect" | "read")
        && source.path.len() == 1
        && source.operands.len() == 1
    {
        if let Value::Reference(reference) = &source.operands[0] {
            let key = if head.text == "inspect" {
                "node"
            } else {
                "value"
            };
            named(&mut call, key, Value::Reference(reference.clone()));
            call.operands.clear();
        }
    }
    Ok(call)
}

/// Select a declared source form and lower its arguments without consulting a registry.
/// The caller resolves unmarked command/provider collisions before calling this function.
pub fn invocation(source: &Call) -> Result<CommandInvocation, Diagnostic> {
    let normalized = normalize(source)?;
    let source = &normalized;
    let words: Vec<_> = source.path.iter().map(|n| n.text.as_str()).collect();
    let Some(root) = words.first() else {
        return Err(invalid(source, "Missing command name."));
    };
    let path = COMMAND_PATHS
        .iter()
        .find(|p| words.starts_with(p.path))
        .or_else(|| COMMAND_PATHS.iter().find(|p| p.short == Some(*root)))
        .ok_or_else(|| {
            invalid(
                source,
                "Unknown command path. Use :help for command families.",
            )
        })?;
    let short = path.short == Some(*root);
    let consumed = if short { 1 } else { path.path.len() };
    if short && !matches!(source.operands.as_slice(), [Value::Reference(_)]) {
        return Err(invalid(
            source,
            "This short form requires one output reference. Use :help for its explicit command path.",
        ));
    }
    let mut call = source.clone();
    let mut tail: Vec<String> = words[consumed..].iter().map(|s| (*s).into()).collect();
    let canonical = path.path.join(" ");
    match path.path {
        ["import", "apply"]
        | ["node", _]
        | ["workspace", "policy"]
        | ["name", "unbind"]
        | ["workspace", "save" | "load"]
        | ["package", "load"]
        | ["type", "check"]
            if !tail.is_empty() =>
        {
            return Err(invalid(
                source,
                "Unexpected command path word. Positional text must be quoted.",
            ));
        }
        _ => {}
    }
    match path.path {
        ["workspace", "save" | "load"] | ["name", "unbind"] => {
            let [Value::Text(name)] = call.operands.as_slice() else {
                return Err(invalid(source, "Expected one quoted text target."));
            };
            tail.push(name.text.clone());
            call.operands.clear();
        }
        ["package", "load"] => tail.push("load".into()),
        ["type", "check"] => {
            let [value] = call.operands.as_slice() else {
                return Err(invalid(
                    source,
                    "Expected one output reference to validate.",
                ));
            };
            if !matches!(value, Value::Reference(_) | Value::Text(_)) {
                return Err(invalid(
                    source,
                    "Type checking requires an output reference or quoted literal.",
                ));
            }
            if call.arguments.iter().any(|a| a.key.text == "value") {
                return Err(invalid(
                    source,
                    "Use the primary output reference, not value:.",
                ));
            }
            let value = value.clone();
            call.operands.clear();
            named(&mut call, "value", value);
            tail.push("check".into());
        }
        ["node", "remove"] => {
            let scopes: Vec<_> = call
                .arguments
                .iter()
                .filter(|a| a.key.text == "scope")
                .collect();
            if !matches!(scopes.as_slice(), [a] if !matches!(a.value, Value::Reference(_)) && a.value.name().text == "downstream")
            {
                return Err(invalid(
                    source,
                    "Removal requires scope:downstream to approve stopping affected work and removing its definitions or names. Nothing was removed.",
                ));
            }
            call.arguments.retain(|a| a.key.text != "scope");
        }
        _ => {}
    }
    if path.operation == ImportApply && !matches!(call.operands.as_slice(), [Value::Reference(_)]) {
        return Err(invalid(
            source,
            "Import apply requires one live plan reference.",
        ));
    }
    if path.operation == Read {
        if !tail.is_empty() {
            return Err(invalid(
                source,
                "Read requires a reference or a typed content selector, not a bare path.",
            ));
        }
        let selectors: Vec<_> = call
            .arguments
            .iter()
            .filter(|a| matches!(a.key.text.as_str(), "value" | "trace"))
            .collect();
        if call.operands.len() + selectors.len() != 1 {
            return Err(invalid(
                source,
                "Read requires exactly one output or trace reference.",
            ));
        }
        if let Some(selector) = selectors.first() {
            if !matches!(selector.value, Value::Reference(_)) {
                return Err(invalid(source, "Read requires an output reference."));
            }
            let kind = selector.key.text.clone();
            let value = selector.value.clone();
            call.arguments
                .retain(|a| !matches!(a.key.text.as_str(), "value" | "trace"));
            call.operands.push(value);
            if kind == "trace" {
                tail.push("trace".into());
            }
        }
        if !matches!(call.operands.as_slice(), [Value::Reference(_)]) {
            return Err(invalid(source, "Read requires an output reference."));
        }
        if call
            .arguments
            .iter()
            .any(|a| !matches!(a.value, Value::Word(_) | Value::Text(_)))
        {
            return Err(invalid(source, "Read selection options must be literals."));
        }
    }
    if path.operation == Read
        && tail.is_empty()
        && call.arguments.iter().any(|a| a.key.text == "run")
    {
        return Err(invalid(
            source,
            "run: selects a retained trace; use :read trace:$output run:<id>.",
        ));
    }
    let mut spec = path.operation.spec(&tail);
    spec.canonical = canonical;
    if path.operation == Read {
        spec.path_tail = Arity::bounded(0, 1);
        spec.operands = Arity::bounded(1, 1);
    }
    if path.path[0] == "node" {
        spec.operands = Arity::bounded(1, 1);
        if !matches!(call.operands.as_slice(), [Value::Reference(_)]) {
            return Err(invalid(
                source,
                "Expected one output reference identifying the producer node.",
            ));
        }
    }
    if path.path == ["workspace", "policy"] {
        spec.operands = Arity::NONE;
    }
    let span = source.path[0].span;
    call.path = std::iter::once(Name {
        text: path.operation.name().into(),
        span,
    })
    .chain(tail.iter().enumerate().map(|(index, text)| {
        Name {
            text: text.clone(),
            span: source
                .path
                .get(consumed + index)
                .map(|n| n.span)
                .unwrap_or(span),
        }
    }))
    .collect();
    Ok(CommandInvocation { call, spec, tail })
}

/// Public signature projection used by help, completion and API discovery.
/// Lowered handler arguments (for example the internal value input of type checking)
/// are not exposed as additional source forms.
pub fn signature(path: &[String]) -> Option<CommandSpec> {
    let words: Vec<_> = path.iter().map(String::as_str).collect();
    let entry = COMMAND_PATHS.iter().find(|p| p.path == words);
    let mut spec = if let Some(entry) = entry {
        match entry.path {
            ["package", "load"] => Type.spec(&["load".into()]),
            ["type", "check"] => Type.spec(&["check".into()]),
            _ => entry.operation.spec(&[]),
        }
    } else if words.len() == 3 && words[0] == "import" && words[1] == "plan" {
        ImportPlan.spec(&[words[2].into()])
    } else if words.len() == 2 && matches!(words[0], "env" | "list" | "import") {
        let command = match words[0] {
            "env" => Env,
            "list" => List,
            _ => Import,
        };
        if words[0] == "env" && super::EnvironmentCommand::lookup(words[1]).is_none() {
            return None;
        }
        if words[0] == "list" && super::ListRegistry::lookup(words[1]).is_none() {
            return None;
        }
        command.spec(&[words[1].into()])
    } else {
        return None;
    };
    spec.canonical = path.join(" ");
    if let Some(entry) = entry {
        spec.summary = entry.summary;
    }
    if words.len() == 2 {
        spec.tail_words.clear();
    }
    match words.as_slice() {
        ["node", "remove"] => {
            spec.parameters.push(wes_core::capability::Parameter::new(
                "scope",
                wes_core::Shape::Primitive(wes_core::Primitive::Text),
                true,
            ));
            spec.operands = Arity::bounded(1, 1);
            spec.path_tail = Arity::NONE;
        }
        ["node", _] => {
            spec.operands = Arity::bounded(1, 1);
            spec.path_tail = Arity::NONE;
        }
        ["workspace", "policy"] => {
            spec.operands = Arity::NONE;
            spec.path_tail = Arity::NONE;
        }
        ["workspace", "save" | "load"] | ["name", "unbind"] => {
            spec.operands = Arity::bounded(1, 1);
            spec.path_tail = Arity::NONE;
        }
        ["type", "check"] => {
            spec.parameters.retain(|p| p.name != "value");
            spec.operands = Arity::bounded(1, 1);
            spec.path_tail = Arity::NONE;
        }
        ["package", "load"] => {
            spec.path_tail = Arity::NONE;
        }
        _ => {}
    }
    Some(spec)
}

/// Family prose is shared by root discovery and family help, never inferred from its first child.
pub fn help_summary(path: &[&str]) -> &'static str {
    match path {
        ["node"] => "Refresh, change, cancel or configure existing work.",
        ["workspace"] => "Save, open, configure or plan deletion of a workspace.",
        ["name"] => "Manage result-name bindings.",
        ["package"] => "Load type, view and iterator definitions.",
        ["type"] => "Validate values against type contracts.",
        ["env", name] => super::EnvironmentCommand::lookup(name).map_or("", |s| s.summary),
        ["list", name] => super::ListRegistry::lookup(name).map_or("", |s| s.summary()),
        _ => COMMAND_PATHS
            .iter()
            .find(|p| p.path == path)
            .map_or("", |p| p.summary),
    }
}

/// Source-facing operand names. Brackets denote optional syntax, angle brackets placeholders.
/// These are documentation; command admission remains owned by the operation signatures.
pub fn help_usage(spec: &CommandSpec) -> String {
    let path: Vec<_> = spec.canonical.split_whitespace().collect();
    let operand = match path.as_slice() {
        ["help"] => "[command path | provider path]",
        ["inspect"] => "[$result | provider path]",
        ["list"] => "<registry>",
        ["read"] => "[$result]",
        ["node", _] | ["type", "check"] => "$result",
        ["workspace", "plan"] => "delete",
        ["workspace", "delete"] => "$plan",
        ["workspace", "save" | "load"] => "\"workspace name\"",
        ["name", "unbind"] => "\"result name\"",
        ["env", name] => match super::EnvironmentCommand::lookup(name) {
            Some(s) if s.name_operand => "\"environment name\"",
            Some(s) if s.plan_operand => "$proposed",
            _ => "",
        },
        ["calc"] => "[pure] { return <expression>; }",
        ["def"] => "<name>(<parameter>: <Type>) [-> <Type>] as <command-or-calc>",
        ["import"] | ["import", "plan"] => "<importer>",
        ["wait"] => "$result [$other ...]",
        _ => "",
    };
    let mut usage = format!(":{}", spec.canonical);
    if !operand.is_empty() {
        usage.push(' ');
        usage.push_str(operand);
    }
    for parameter in &spec.parameters {
        let choices = help_choices(&path, &parameter.name);
        let value = if path == ["workspace", "plan"] && parameter.name == "workspace" {
            "\"name\"".into()
        } else if choices.is_empty() {
            format!("<{}>", parameter.shape)
        } else {
            choices.join("|")
        };
        let argument = format!("{}:{value}", parameter.name);
        usage.push(' ');
        if parameter.required {
            usage.push_str(&argument);
        } else {
            usage.push_str(&format!("[{argument}]"));
        }
    }
    if spec.open_arguments && spec.command != Def {
        usage.push_str(" [<argument>:<value> ...]");
    }
    if path == ["workspace", "plan"] {
        usage.push_str(" [> plan]");
    }
    if path == ["env", "plan"] {
        usage.push_str(" > proposed");
    }
    usage
}

pub fn help_choices(path: &[&str], parameter: &str) -> &'static [&'static str] {
    match (path, parameter) {
        (["node", "policy"] | ["workspace", "policy"], "mode") => {
            &["automatic", "manual", "reactive"]
        }
        (["node", "refresh" | "remove"], "scope") => &["downstream"],
        (["env", "plan"], "reconcile") => &["file", "source"],
        _ => &[],
    }
}
