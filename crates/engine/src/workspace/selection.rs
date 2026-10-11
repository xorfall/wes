//! Read-only authoring analysis shares normal provider binding and type checking.
use super::*;
impl Workspace {
    pub(crate) fn selection_template(
        &self,
        text: &str,
        context: Option<wes_core::environments::EnvironmentContext>,
    ) -> Result<Option<NodeId>, String> {
        let parsed = wes_language::parse(&wes_language::SourceText::new("draft", text));
        if parsed
            .diagnostics
            .iter()
            .any(|d| d.severity == wes_language::Severity::Error)
        {
            return Ok(None);
        }
        fn references(statement: &wes_language::Statement) -> Vec<&wes_language::Name> {
            match &statement.expression {
                wes_language::Expression::Call(call) => call
                    .operands
                    .iter()
                    .chain(call.arguments.iter().map(|a| &a.value))
                    .flat_map(wes_language::Value::references)
                    .collect(),
                wes_language::Expression::Pipeline(stages)
                | wes_language::Expression::Sandbox(stages)
                | wes_language::Expression::Transient(stages) => {
                    stages.iter().flat_map(references).collect()
                }
                wes_language::Expression::Fork(branches) => branches
                    .iter()
                    .flat_map(|branch| references(&branch.body))
                    .collect(),
                _ => vec![],
            }
        }
        let selects = parsed
            .script
            .statements
            .iter()
            .flat_map(references)
            .any(|name| {
                let mut fields = name.text.split('.');
                let root = fields.next().unwrap_or_default();
                if fields.next() != Some("selected") {
                    return false;
                }
                // A record's ordinary field named selected remains ordinary data.
                self.resolve(root)
                    .and_then(|output| self.data_typing_handle(&output.node))
                    .is_none_or(|typing| {
                        crate::selection::rendered_package(&typing.shape, self.view_catalogue())
                            .is_some()
                            || matches!(
                                typing.shape,
                                wes_core::Shape::List(_)
                                    | wes_core::Shape::Unknown
                                    | wes_core::Shape::Meta(wes_core::MetaType::ViewInstance)
                            )
                    })
            });
        if !selects {
            return Ok(None);
        }
        if parsed.script.statements.len() != 1
            || !matches!(
                parsed.script.statements[0].expression,
                wes_language::Expression::Call(_)
            )
        {
            return Err("Selection activation requires one standalone provider command.".into());
        }
        let prepared = self
            .draft()
            .map_err(|error| error.to_string())?
            .with_environment_context(context, false)
            .prepare(&parsed.script.statements[0])
            .map_err(|error| error.to_string())?;
        let Preparation::Change(PreparedChange {
            operation:
                Change::Node {
                    task: BoundTask::Call(call),
                    ..
                },
            ..
        }) = prepared
        else {
            return Err("Selection activation requires a finite provider command.".into());
        };
        let sources = call.selection_sources();
        let source = sources
            .first()
            .ok_or("Selection could not be bound to a source.")?;
        if sources.iter().any(|other| other.node != source.node) {
            return Err("A selection command requires one source.".into());
        }
        Ok(Some(source.node.clone()))
    }
}
