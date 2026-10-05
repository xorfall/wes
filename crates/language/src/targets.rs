//! Shared metadata target resolution for help and inspect. No provider invocation or I/O.
use crate::{Call, Diagnostic, Name, Value, vocabulary::commands};
use wes_core::capability::Catalogue;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum QueryTarget {
    Root,
    Command(Vec<String>),
    Provider { name: String, path: Vec<String> },
    Object { kind: ObjectKind, name: Name },
    Output { metadata: bool, reference: Name },
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ObjectKind {
    Environment,
    Workspace,
    Home,
    Run,
    Name,
    Cell,
    Type,
    View,
    Importer,
    Template,
}
pub const OBJECT_KINDS: &[(ObjectKind, &str)] = &[
    (ObjectKind::Environment, "env"),
    (ObjectKind::Workspace, "workspace"),
    (ObjectKind::Home, "home"),
    (ObjectKind::Run, "run"),
    (ObjectKind::Name, "name"),
    (ObjectKind::Cell, "cell"),
    (ObjectKind::Type, "type"),
    (ObjectKind::View, "view"),
    (ObjectKind::Importer, "importer"),
    (ObjectKind::Template, "template"),
];
impl ObjectKind {
    pub fn name(self) -> &'static str {
        OBJECT_KINDS
            .iter()
            .find(|(kind, _)| *kind == self)
            .expect("total object vocabulary")
            .1
    }
    pub fn lookup(name: &str) -> Option<Self> {
        OBJECT_KINDS
            .iter()
            .find(|(_, n)| *n == name)
            .map(|(kind, _)| *kind)
    }
}

fn fail(call: &Call, code: &'static str, message: &'static str) -> Diagnostic {
    let target = call
        .arguments
        .first()
        .map(|a| a.value.name())
        .or_else(|| call.path.get(1));
    let span = target.map(|n| n.span).unwrap_or(call.span);
    let detail = target
        .map(|n| {
            format!(
                "{message} Target: {:?}.",
                n.text.chars().take(128).collect::<String>()
            )
        })
        .unwrap_or_else(|| message.into());
    Diagnostic::error(code, span, detail).with_public_message(message)
}
pub fn resolve(call: &Call, help: bool, catalogue: &Catalogue) -> Result<QueryTarget, Diagnostic> {
    let tail: Vec<_> = call.path.iter().skip(1).map(|n| n.text.clone()).collect();
    let count = usize::from(!tail.is_empty()) + call.operands.len() + call.arguments.len();
    if count == 0 && help {
        return Ok(QueryTarget::Root);
    }
    if count != 1 {
        return Err(fail(call, "CMD002", "Expected exactly one query target."));
    }
    if let Some(value) = call.operands.first() {
        return match value {
            Value::Reference(reference) if !help => Ok(QueryTarget::Output {
                metadata: true,
                reference: reference.clone(),
            }),
            _ => Err(fail(
                call,
                "CMD002",
                "Expected a target path or an explicit typed selector.",
            )),
        };
    }
    let mut explicit = None;
    let path = if let Some(arg) = call.arguments.first() {
        let kind = arg.key.text.as_str();
        if matches!(kind, "node" | "value") && !help {
            return match &arg.value {
                Value::Reference(reference) => Ok(QueryTarget::Output {
                    metadata: kind == "node",
                    reference: reference.clone(),
                }),
                _ => Err(fail(
                    call,
                    "CMD002",
                    "This target requires an output reference.",
                )),
            };
        }
        if let Some(object) = ObjectKind::lookup(kind).filter(|_| !help) {
            return match &arg.value {
                Value::Reference(_) | Value::Structured(_, _) => Err(fail(
                    call,
                    "CMD002",
                    "This object selector requires a literal identifier.",
                )),
                value => Ok(QueryTarget::Object {
                    kind: object,
                    name: value.name().clone(),
                }),
            };
        }
        if !["command", "provider", "capability"].contains(&kind)
            || !matches!(arg.value, Value::Word(_) | Value::Text(_))
        {
            return Err(fail(
                call,
                "CMD002",
                "Unknown target selector or incorrect reference kind.",
            ));
        }
        explicit = Some(kind);
        let path: Vec<String> = arg
            .value
            .name()
            .text
            .split_whitespace()
            .map(str::to_owned)
            .collect();
        if path.is_empty()
            || (kind == "provider" && path.len() != 1)
            || (kind == "capability" && path.len() < 2)
        {
            return Err(fail(
                call,
                "CMD002",
                "Invalid target path for this selector.",
            ));
        }
        path
    } else {
        tail
    };
    let root = path
        .first()
        .ok_or_else(|| fail(call, "CMD002", "Missing target path."))?;
    let command = commands::is_command_root(root)
        || (help && matches!(root.as_str(), "errors" | "arguments"));
    let provider = catalogue.provider(root);
    let choose_command = match explicit {
        Some("command") => true,
        Some("provider" | "capability") => false,
        _ => match (command, provider.is_some()) {
            (true, true) => {
                return Err(fail(
                    call,
                    "RES001",
                    "Target name is ambiguous. Select command:, provider: or capability: explicitly.",
                ));
            }
            (true, false) => true,
            (false, true) => false,
            (false, false) => {
                let diagnostic = fail(
                    call,
                    "RES004",
                    "Target is absent from the command and provider catalogues.",
                );
                return Err(if !help && path.len() == 1 && crate::binding_name(root) {
                    diagnostic.with_hint(format!(
                        "Did you mean :inspect ${root}? Result references start with $."
                    ))
                } else {
                    diagnostic
                });
            }
        },
    };
    if choose_command {
        let mut path = path;
        if let Some(entry) = commands::COMMAND_PATHS
            .iter()
            .find(|p| p.short == Some(path[0].as_str()))
        {
            let mut expanded: Vec<String> = entry.path.iter().map(|w| (*w).into()).collect();
            expanded.extend_from_slice(&path[1..]);
            path = expanded;
        }
        let words: Vec<_> = path.iter().map(String::as_str).collect();
        let known = (help && words.len() == 2 && words[0] == "calc"
            && crate::calc::Package::standard().operation(words[1]).is_some()) || words == ["render"] || (help && matches!(words.as_slice(), ["errors"] | ["arguments"])) || commands::COMMAND_PATHS
            .iter()
            .any(|p| p.path.starts_with(&words))
            // Importer names belong to the captured runtime registry, not this static grammar.
            || (words.len() == 2 && words[0] == "import")
            || (words.len()==3 && words[0]=="import" && words[1]=="plan")
            || (words.len() == 2
                && words[0] == "env"
                && crate::vocabulary::EnvironmentCommand::lookup(words[1]).is_some())
            || (words.len() == 2
                && words[0] == "list"
                && crate::vocabulary::ListRegistry::lookup(words[1]).is_some());
        if !known {
            return Err(fail(
                call,
                "RES005",
                "Command family or operation path does not exist.",
            ));
        }
        Ok(QueryTarget::Command(path))
    } else {
        let provider = provider.ok_or_else(|| {
            fail(
                call,
                "RES004",
                "Provider is absent from the selected catalogue.",
            )
        })?;
        let rest = path[1..].to_vec();
        let known = rest.is_empty() || provider.capabilities().any(|c| c.path.starts_with(&rest));
        if !known {
            return Err(fail(
                call,
                "RES005",
                "Capability or group path does not exist in this provider.",
            )
            .with_hint(format!(
                "Use :help provider:{root} to discover its capability groups."
            )));
        }
        Ok(QueryTarget::Provider {
            name: root.clone(),
            path: rest,
        })
    }
}
