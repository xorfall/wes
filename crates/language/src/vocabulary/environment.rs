//! Environment command signatures shared by help and live admission.
use crate::{Diagnostic, Expression, Span, Statement, Value};

pub struct EnvironmentCommand {
    pub name: &'static str,
    pub summary: &'static str,
    pub parameters: &'static [(&'static str, bool)],
    pub plan_operand: bool,
    pub name_operand: bool,
    pub plan_binding: bool,
}

pub const ENVIRONMENT_COMMANDS: &[EnvironmentCommand] = &[
    EnvironmentCommand {
        name: "plan",
        summary: "Validate and capture a reviewable environment plan without calling providers. Relative SSH client/key/known-hosts paths use the package directory, or explicit absolute base for source: text; never invocation CWD.",
        parameters: &[
            ("file", false),
            ("source", false),
            ("origin", false),
            ("base", false),
            ("reconcile", false),
        ],
        name_operand: false,
        plan_operand: false,
        plan_binding: true,
    },
    EnvironmentCommand {
        name: "apply",
        summary: "Apply this client’s reviewed plan; consumes the plan and updates environment definitions.",
        parameters: &[],
        name_operand: false,
        plan_operand: true,
        plan_binding: false,
    },
    EnvironmentCommand {
        name: "discard",
        summary: "Release this client’s unused environment plan.",
        parameters: &[],
        name_operand: false,
        plan_operand: true,
        plan_binding: false,
    },
    EnvironmentCommand {
        name: "use",
        summary: "Select a named environment and optional revision for this client only.",
        parameters: &[("revision", false)],
        name_operand: true,
        plan_operand: false,
        plan_binding: false,
    },
    EnvironmentCommand {
        name: "clear",
        summary: "Clear this client’s environment selection; pure calculations remain available.",
        parameters: &[],
        name_operand: false,
        plan_operand: false,
        plan_binding: false,
    },
    EnvironmentCommand {
        name: "disable",
        summary: "Disable dispatch and revoke grants for this live session only; saved definitions are unchanged.",
        parameters: &[],
        name_operand: true,
        plan_operand: false,
        plan_binding: false,
    },
    EnvironmentCommand {
        name: "enable",
        summary: "Enable dispatch for this live session; reopening saved definitions still requires explicit activation. Credentials and grants are separate.",
        parameters: &[],
        name_operand: true,
        plan_operand: false,
        plan_binding: false,
    },
    EnvironmentCommand {
        name: "rename",
        summary: "Rename an environment while preserving its identity and existing bindings; marks drift.",
        parameters: &[("to", true)],
        name_operand: true,
        plan_operand: false,
        plan_binding: false,
    },
    EnvironmentCommand {
        name: "retire",
        summary: "Prevent new selection, imports and bindings; preserve existing-node refresh.",
        parameters: &[],
        name_operand: true,
        plan_operand: false,
        plan_binding: false,
    },
    EnvironmentCommand {
        name: "delete",
        summary: "Delete a retired, unreferenced environment; never cascades or erases history.",
        parameters: &[],
        name_operand: true,
        plan_operand: false,
        plan_binding: false,
    },
    EnvironmentCommand {
        name: "export",
        summary: "Write the environment lock to a file without secret values.",
        parameters: &[("file", true)],
        name_operand: false,
        plan_operand: false,
        plan_binding: false,
    },
];

impl EnvironmentCommand {
    pub fn lookup(name: &str) -> Option<&'static Self> {
        ENVIRONMENT_COMMANDS.iter().find(|spec| spec.name == name)
    }
    pub fn validate(statement: &Statement) -> Result<&'static Self, Diagnostic> {
        let Expression::Call(call) = &statement.expression else {
            unreachable!("environment command is a call")
        };
        let fail = |span: Span, reason: &'static str| {
            Diagnostic::error("ENV001", span, reason).with_public_message(reason)
        };
        let Some(action) = call.path.get(1) else {
            return Err(fail(
                Span::at(call.span.end()),
                "Missing environment subcommand. Expected :env <subcommand>.",
            ));
        };
        let Some(spec) = Self::lookup(&action.text) else {
            return Err(fail(action.span, "Unknown environment subcommand."));
        };
        if let Some(word) = call.path.get(2) {
            let reason = if spec.name_operand {
                "Unexpected command path word. The environment name must be a quoted text operand."
            } else {
                "Unexpected positional word after the environment subcommand."
            };
            return Err(fail(word.span, reason));
        }
        if let Some(annotation) = statement.annotations.first() {
            return Err(fail(
                annotation.span,
                "Environment controls do not accept annotations.",
            ));
        }
        if let Some(binding) = &statement.error_binding {
            return Err(fail(
                binding.span,
                "Environment controls do not accept error bindings.",
            ));
        }
        if !spec.plan_binding
            && let Some(binding) = &statement.binding
        {
            return Err(fail(
                binding.span,
                "This environment control has no output to bind.",
            ));
        }
        let mut keys = std::collections::BTreeSet::new();
        for argument in &call.arguments {
            if !spec
                .parameters
                .iter()
                .any(|(key, _)| *key == argument.key.text)
            {
                return Err(fail(
                    argument.key.span,
                    "Unexpected environment argument key.",
                ));
            }
            if !keys.insert(argument.key.text.as_str()) {
                return Err(fail(
                    argument.key.span,
                    "Duplicate environment argument key.",
                ));
            }
            if !matches!(argument.value, Value::Word(_) | Value::Text(_)) {
                return Err(fail(
                    argument.value.name().span,
                    "Environment argument requires literal text, not a reference.",
                ));
            }
        }
        for (key, required) in spec.parameters {
            if *required && !keys.contains(key) {
                // Both interpolated values come from this static signature, never source/data.
                let reason = format!(
                    "Missing required argument '{key}:'. Expected :env {} {key}:<text>.",
                    spec.name
                );
                return Err(
                    Diagnostic::error("ENV001", Span::at(call.span.end()), &reason)
                        .with_public_message(reason),
                );
            }
        }
        if spec.name_operand {
            match call.operands.as_slice() {
                [Value::Text(_)] => {}
                [Value::Reference(value)] => {
                    return Err(fail(
                        value.span,
                        "Expected an environment name, received a reference.",
                    ));
                }
                [] => {
                    return Err(fail(
                        Span::at(call.span.end()),
                        "Expected one quoted environment name.",
                    ));
                }
                values => {
                    return Err(fail(
                        values[1.min(values.len() - 1)].name().span,
                        "Expected one quoted environment name.",
                    ));
                }
            }
        } else if spec.plan_operand {
            match call.operands.as_slice() {
                [] => {
                    return Err(fail(
                        Span::at(call.span.end()),
                        "Missing environment plan reference. Expected $proposed.",
                    ));
                }
                [Value::Reference(_)] => {}
                [value] => {
                    return Err(fail(
                        value.name().span,
                        "Environment plan operand must be a reference. Expected $proposed.",
                    ));
                }
                values => {
                    return Err(fail(
                        values[1].name().span,
                        "Unexpected extra environment plan operand.",
                    ));
                }
            }
        } else if let Some(value) = call.operands.first() {
            return Err(fail(
                value.name().span,
                "This environment control does not accept positional operands.",
            ));
        }
        if spec.plan_binding && statement.binding.is_none() {
            return Err(fail(
                Span::at(statement.span.end()),
                "Environment plan requires an output binding. Expected > proposed.",
            ));
        }
        Ok(spec)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{SourceText, vocabulary::MetaCommand};
    #[test]
    fn signatures_and_precise_rejection_causes_agree() {
        for (source, reason, subject) in [
            (":env", "Missing environment subcommand", ""),
            (":env wat", "Unknown environment subcommand", "wat"),
            (":env use çığ", "Unexpected command path word", "çığ"),
            (":env use", "Expected one quoted environment name", ""),
            (
                ":env use typo:a",
                "Unexpected environment argument key",
                "typo",
            ),
            (
                ":env rename \"a\" to:b to:c",
                "Duplicate environment argument key",
                "to",
            ),
            (
                ":env use $secret",
                "Expected an environment name, received a reference",
                "secret",
            ),
            (
                ":env clear > output",
                "This environment control has no output",
                "> output",
            ),
            (
                ":env clear *> problem",
                "Environment controls do not accept error bindings",
                "*> problem",
            ),
            (
                "@timeout(2) :env clear",
                "Environment controls do not accept annotations",
                "@timeout(2)",
            ),
            (":env apply", "Missing environment plan reference", ""),
            (
                ":env apply \"literal\"",
                "Environment plan operand must be a reference",
                "literal",
            ),
            (
                ":env apply $first $second",
                "Unexpected extra environment plan operand",
                "second",
            ),
            (
                ":env plan source:text",
                "Environment plan requires an output binding",
                "",
            ),
        ] {
            let parsed = crate::parse(&SourceText::new("test", source));
            assert!(
                parsed.diagnostics.is_empty(),
                "{source}: {:?}",
                parsed.diagnostics
            );
            let diagnostic = EnvironmentCommand::validate(&parsed.script.statements[0])
                .err()
                .unwrap();
            assert!(
                diagnostic.message.starts_with(reason),
                "{source}: {diagnostic:?}"
            );
            assert_eq!(diagnostic.public_summary(), diagnostic.message);
            let actual = &source[diagnostic.span.start()..diagnostic.span.end()];
            assert!(
                actual.contains(subject),
                "{source}: {actual:?} != {subject:?}"
            );
            if subject.is_empty() {
                assert!(actual.is_empty(), "{source}: {actual}");
            }
        }
        for command in ENVIRONMENT_COMMANDS {
            let spec = MetaCommand::Env.spec(&[command.name.into()]);
            assert!(!spec.open_arguments);
            assert_eq!(
                spec.parameters
                    .iter()
                    .map(|p| (p.name.as_str(), p.required))
                    .collect::<Vec<_>>(),
                command.parameters
            );
            assert_eq!(
                spec.operands.min,
                usize::from(command.plan_operand || command.name_operand)
            );
        }
    }
}
