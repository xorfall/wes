//! Typed management source forms, independent of runtime and transport.
use super::MetaCommand;
use crate::{Diagnostic, Expression, Statement, Value};
pub enum WorkspaceManagementCommand {
    Plan {
        binding: Option<String>,
        workspace: Option<String>,
    },
    Delete(String, bool, bool),
}
pub fn workspace_management(
    statement: &Statement,
) -> Result<WorkspaceManagementCommand, Diagnostic> {
    let error = |message: &str| {
        Diagnostic::error("MET012", statement.span, message).with_public_message(message)
    };
    let Expression::Call(call) = &statement.expression else {
        return Err(error(
            "Management values cannot be used in pipelines, forks or sandboxes.",
        ));
    };
    if !statement.annotations.is_empty()
        || statement.error_binding.is_some()
        || call.marker.is_none()
    {
        return Err(error(
            "Management commands do not accept execution annotations or error bindings.",
        ));
    }
    let invocation = super::commands::invocation(call).map_err(|d| error(&d.message))?;
    match invocation.spec.command {
        MetaCommand::WorkspacePlan => {
            if call.path.len() != 3 || call.path[2].text != "delete" || !call.operands.is_empty() {
                return Err(error(
                    "Expected :workspace plan delete > plan; delete is the operation argument.",
                ));
            }
            let name = statement.binding.as_ref();
            let workspace = match call.arguments.as_slice() {
                [] => None,
                [argument] if argument.key.text == "workspace" => {
                    let Value::Text(value) = &argument.value else {
                        return Err(error(
                            "workspace: requires a quoted workspace name, not a reference or bare word.",
                        ));
                    };
                    Some(value.text.clone())
                }
                _ => {
                    return Err(error(
                        "Only one optional workspace:\"name\" argument is accepted.",
                    ));
                }
            };
            Ok(WorkspaceManagementCommand::Plan {
                binding: name.map(|name| name.name.text.clone()),
                workspace,
            })
        }
        MetaCommand::WorkspaceDelete => {
            if statement.binding.is_some() {
                return Err(error(
                    "This management operation does not create another binding.",
                ));
            }
            let [Value::Reference(name)] = call.operands.as_slice() else {
                return Err(error("Expected one WorkspaceDeletePlan reference: $plan."));
            };
            if call.path.len() != 2 {
                return Err(error("Expected :workspace delete $plan."));
            }
            let mut stop = None;
            let mut protected = None;
            for a in &call.arguments {
                let target = match a.key.text.as_str() {
                    "stop" => &mut stop,
                    "protected" => &mut protected,
                    _ => {
                        return Err(error(
                            "Only stop:true and protected:true are accepted deletion options.",
                        ));
                    }
                };
                if target.is_some() {
                    return Err(error("Duplicate deletion option."));
                }
                *target = Some(match &a.value {
                    Value::Word(n) if n.text == "true" => true,
                    Value::Word(n) if n.text == "false" => false,
                    _ => return Err(error("Deletion consent must be a literal true or false.")),
                });
            }
            Ok(WorkspaceManagementCommand::Delete(
                name.text.clone(),
                stop.unwrap_or(false),
                protected.unwrap_or(false),
            ))
        }
        _ => Err(error(
            "WorkspaceDeletePlan is a management value; use :read, :inspect or :workspace delete.",
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn statement(text: &str) -> Statement {
        let parsed = crate::parse(&crate::SourceText::new("test", text));
        assert!(
            parsed.diagnostics.is_empty(),
            "{text}: {:?}",
            parsed.diagnostics
        );
        parsed.script.statements.into_iter().next().unwrap()
    }
    #[test]
    fn operation_is_a_closed_parameter_not_a_third_command_path() {
        let statement = statement(":workspace plan delete > plan");
        let Expression::Call(call) = &statement.expression else {
            panic!()
        };
        let invocation = super::super::commands::invocation(call).unwrap();
        assert_eq!(invocation.spec.canonical, "workspace plan");
        assert_eq!(invocation.spec.tail_words, ["delete"]);
        assert!(
            matches!(workspace_management(&statement), Ok(WorkspaceManagementCommand::Plan { binding, workspace: None }) if binding.as_deref() == Some("plan"))
        );
        assert!(
            !super::super::commands::COMMAND_PATHS
                .iter()
                .any(|c| c.path == ["workspace", "plan", "delete"])
        );
        let statement = self::statement(":workspace delete $plan stop:true protected:false");
        assert!(
            matches!(workspace_management(&statement), Ok(WorkspaceManagementCommand::Delete(n, true, false)) if n == "plan")
        );
    }
    #[test]
    fn explicit_target_is_an_optional_quoted_parameter_in_the_command_signature() {
        let statement = statement(":workspace plan delete workspace:\"demo\" > plan");
        assert!(
            matches!(workspace_management(&statement), Ok(WorkspaceManagementCommand::Plan { binding, workspace: Some(target) }) if binding.as_deref() == Some("plan") && target == "demo")
        );
        let spec = super::super::commands::signature(&["workspace".into(), "plan".into()]).unwrap();
        assert!(!spec.parameter("workspace").unwrap().required);
        assert_eq!(
            super::super::commands::help_usage(&spec),
            ":workspace plan delete [workspace:\"name\"] [> plan]"
        );
    }
    #[test]
    fn management_grammar_rejects_ambiguous_or_execution_forms() {
        for text in [
            ":workspace plan",
            ":workspace plan save > p",
            ":workspace plan delete extra > p",
            ":workspace plan \"delete\" > p",
            ":workspace plan delete unexpected:true > p",
            ":workspace plan delete workspace:bare > p",
            ":workspace plan delete workspace:$name > p",
            ":workspace plan delete workspace:\"a\" workspace:\"b\" > p",
            ":workspace delete $p workspace:\"other\"",
            ":workspace delete",
            ":workspace delete \"token\"",
            ":workspace delete $a $b",
            ":workspace delete $a > p",
            ":workspace delete $a stop:$yes",
            ":workspace delete $a stop:true stop:false",
            ":workspace delete $a protected:yes",
            ":workspace delete $a extra:true",
            "@timeout(2) :workspace delete $a",
            ":workspace delete $a *> error",
            ":read $p > copy",
            ":inspect $p extra:true",
        ] {
            assert!(workspace_management(&statement(text)).is_err(), "{text}");
        }
        assert!(matches!(
            workspace_management(&statement(":workspace plan delete")),
            Ok(WorkspaceManagementCommand::Plan {
                binding: None,
                workspace: None
            })
        ));
    }
}
