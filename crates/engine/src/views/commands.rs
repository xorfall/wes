//! Prepare-only commands from owned public frames. The source guard is checked again at admission.
use super::*;
use crate::workspace::{Preparation, Workspace, WorkspaceError};
use serde::Serialize;
use sha2::{Digest, Sha256};
use wes_language::{Annotation, Argument, Call, Expression, Name, Span, Statement, Structure};

#[derive(Clone, Debug)]
pub struct CommandRequest {
    pub root: String,
    pub instance: String,
    pub member: String,
    pub revision: u64,
    pub input_revision: u64,
    pub environments: Option<wes_core::environments::EnvironmentContext>,
    pub template: String,
    pub arguments: BTreeMap<String, serde_json::Value>,
}
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CommandDraft {
    pub source: String,
}
/// Structural proofs only: no input payload, worker, lease or publication authority.
#[derive(Clone, Debug, Default)]
pub(crate) struct Context {
    roots: BTreeMap<NodeId, (String, BTreeSet<NodeId>)>,
    basis: String,
}
#[derive(Clone, Debug)]
pub(crate) struct Guard {
    root: NodeId,
    instance: String,
    member: NodeId,
    basis: String,
    template: String,
    template_revision: String,
}
fn invalid() -> WorkspaceError {
    wes_language::Diagnostic::error(
        "VIEWCMD001",
        Span::at(0),
        "View command is unavailable, invalid or changed; review it again",
    )
    .into()
}
impl Context {
    pub(crate) fn capture(workspace: &Workspace) -> Self {
        let mut context = Self::default();
        let mut hash = Sha256::new();
        hash.update(workspace.views.command_epoch.as_bytes());
        for (id, instance) in &workspace.views.instances {
            let s = &instance.snapshot;
            let source_run = s.input.as_ref().and_then(Input::source).map(|source| {
                (
                    workspace
                        .runtime()
                        .run_of(&source.output.node)
                        .map(ToString::to_string),
                    workspace
                        .runtime()
                        .value_run(&source.output.node)
                        .map(ToString::to_string),
                )
            });
            // Length-delimited serialization avoids ambiguous concatenation. No payload digest.
            hash.update(
                serde_json::to_vec(&(
                    id.as_str(),
                    s.identity.as_ref(),
                    s.revision,
                    s.input_revision,
                    instance.interaction.revision,
                    source_run,
                ))
                .expect("structural metadata"),
            );
            if let Ok(frame) = workspace.view_frame(id) {
                if frame.instances.iter().all(|entry| {
                    entry.input.as_ref().and_then(Input::value).is_none_or(|v| {
                        !v.provenance().policy().is_private()
                            && !v.provenance().policy().is_unknown()
                    })
                }) {
                    context.roots.insert(
                        id.clone(),
                        (
                            s.identity.to_string(),
                            frame.instances.iter().map(|i| i.id.clone()).collect(),
                        ),
                    );
                }
            }
        }
        context.basis = format!("{:x}", hash.finalize());
        context
    }
    pub(crate) fn check(
        &self,
        guard: &Guard,
        templates: &wes_language::templates::Templates,
    ) -> Result<(), WorkspaceError> {
        if self.basis != guard.basis
            || self.roots.get(&guard.root).is_none_or(|(id, members)| {
                id != &guard.instance || !members.contains(&guard.member)
            })
            || templates
                .snapshot()
                .get(&guard.template)
                .is_none_or(|t| t.revision() != guard.template_revision)
        {
            return Err(invalid());
        }
        Ok(())
    }
    pub(crate) fn take_guard(
        &self,
        statement: &mut Statement,
        templates: &wes_language::templates::Templates,
        replay: bool,
    ) -> Result<Option<Guard>, WorkspaceError> {
        let annotations: Vec<_> = statement
            .annotations
            .iter()
            .filter(|a| a.name.text == "view")
            .collect();
        if annotations.is_empty() {
            return Ok(None);
        }
        if annotations.len() != 1 || annotations[0].targets.len() != 5 {
            return Err(invalid());
        }
        let Expression::Call(call) = &statement.expression else {
            return Err(invalid());
        };
        if call.marker.is_some() || call.path.len() != 1 || !call.operands.is_empty() {
            return Err(invalid());
        }
        let names = &annotations[0].targets;
        let guard = Guard {
            root: NodeId::new(&names[0].text).map_err(|_| invalid())?,
            instance: names[1].text.clone(),
            member: NodeId::new(&names[2].text).map_err(|_| invalid())?,
            basis: names[3].text.clone(),
            template: call.path[0].text.clone(),
            template_revision: names[4].text.clone(),
        };
        if !replay {
            self.check(&guard, templates)?;
        }
        statement.annotations.retain(|a| a.name.text != "view");
        // Held reconstruction preserves recorded declarations, never live View authority.
        Ok((!replay).then_some(guard))
    }
}
impl Workspace {
    pub fn prepare_view_command(
        &self,
        request: CommandRequest,
    ) -> Result<CommandDraft, WorkspaceError> {
        if request.arguments.len() > 32
            || serde_json::to_vec(&request.arguments)
                .map_err(|_| invalid())?
                .len()
                > 16 * 1024
            || request.root.len() > 128
            || request.member.len() > 128
            || request.instance.len() > 128
            || request.template.len() > 128
        {
            return Err(invalid());
        }
        let root_id = NodeId::new(&request.root).map_err(|_| invalid())?;
        let member_id = NodeId::new(&request.member).map_err(|_| invalid())?;
        let frame = self.view_frame(&root_id).map_err(|_| invalid())?;
        let root = frame.instances.first().ok_or_else(invalid)?;
        let member = frame
            .instances
            .iter()
            .find(|m| m.id == member_id)
            .ok_or_else(invalid)?;
        if root.identity.as_ref() != request.instance
            || member.revision != request.revision
            || member.input_revision != request.input_revision
        {
            return Err(invalid());
        }
        let template = self
            .templates()
            .snapshot()
            .get(&request.template)
            .ok_or_else(invalid)?;
        if template.parameters.len() != template.contracts.len()
            || request
                .arguments
                .keys()
                .any(|k| !template.contracts.contains_key(k))
        {
            return Err(invalid());
        }
        let context = Context::capture(self);
        let guard = Guard {
            root: root_id,
            instance: request.instance,
            member: member_id,
            basis: context.basis.clone(),
            template: request.template.clone(),
            template_revision: template.revision(),
        };
        context.check(&guard, self.templates())?;
        let name = |text: String| Name {
            text,
            span: Span::at(0),
        };
        let mut work = 0;
        let arguments = request
            .arguments
            .into_iter()
            .map(|(key, value)| {
                Ok(Argument {
                    key: name(key),
                    value: literal(value, 0, &mut work)?,
                    span: Span::at(0),
                })
            })
            .collect::<Result<Vec<_>, WorkspaceError>>()?;
        let mut statement = Statement {
            annotations: vec![],
            expression: Expression::Call(Call {
                marker: None,
                path: vec![name(request.template)],
                operands: vec![],
                arguments,
                span: Span::at(0),
            }),
            binding: None,
            error_binding: None,
            span: Span::at(0),
        };
        let targets = [
            guard.root.as_str().to_string(),
            guard.instance,
            guard.member.as_str().to_string(),
            guard.basis,
            guard.template_revision,
        ];
        statement.annotations.push(Annotation {
            name: name("view".into()),
            targets: targets.into_iter().map(name).collect(),
            span: Span::at(0),
        });
        let source = statement.to_string();
        if source.len() > 16 * 1024 {
            return Err(invalid());
        }
        // Validate the exact serialized source through the same parser/planner as submit.
        let parsed = wes_language::parse(&wes_language::SourceText::new("view-command", &source));
        if !parsed.diagnostics.is_empty() || parsed.script.statements.len() != 1 {
            return Err(invalid());
        }
        let draft = self
            .draft()?
            .with_environment_context(request.environments, false);
        if !matches!(
            draft.prepare(&parsed.script.statements[0]),
            Ok(Preparation::Change(_))
        ) {
            return Err(invalid());
        }
        Ok(CommandDraft { source })
    }
}
fn literal(
    value: serde_json::Value,
    depth: usize,
    work: &mut usize,
) -> Result<wes_language::Value, WorkspaceError> {
    *work += 1;
    if depth > 16 || *work > 1024 {
        return Err(invalid());
    }
    let name = |text: String| Name {
        text,
        span: Span::at(0),
    };
    use wes_language::Value as V;
    Ok(match value {
        serde_json::Value::String(s) => V::Text(name(s)),
        serde_json::Value::Bool(b) => V::Word(name(b.to_string())),
        serde_json::Value::Number(n) => V::Word(name(n.to_string())),
        // Option/nominal native values need explicit typed Wes expressions; this JSON boundary
        // deliberately refuses null rather than inventing a representation or coercion.
        serde_json::Value::Null => return Err(invalid()),
        serde_json::Value::Array(items) => V::Structured(
            name(String::new()),
            Structure::List(
                items
                    .into_iter()
                    .map(|v| literal(v, depth + 1, work))
                    .collect::<Result<_, _>>()?,
            ),
        ),
        serde_json::Value::Object(fields) => V::Structured(
            name(String::new()),
            Structure::Record(
                fields
                    .into_iter()
                    .map(|(k, v)| Ok((name(k), literal(v, depth + 1, work)?)))
                    .collect::<Result<_, WorkspaceError>>()?,
            ),
        ),
    })
}
