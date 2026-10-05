//! Structural pipe lowering. Original source remains the journal/replay authority.
use super::{WorkspaceError, rejected};
use crate::graph::{OutputPort, OutputRef};
use std::borrow::Cow;
use wes_language::{Argument, Call, Expression, Name, Span, Statement, Value, calc};

/// A typed calc's declared input slot receives the preceding output when omitted.
/// Explicit mappings retain their meaning. Ordinary calls/templates are untouched;
/// normal expansion still owns name resolution, argument validation and contracts.
pub(super) fn bind_definition_input<'a>(
    call: &'a Call,
    input: Option<&OutputRef>,
    templates: &wes_language::templates::Templates,
) -> Cow<'a, Call> {
    let Some(input) = input else {
        return Cow::Borrowed(call);
    };
    if call.marker.is_some() || call.path.len() != 1 {
        return Cow::Borrowed(call);
    }
    let Some(definition) = templates.snapshot().get(&call.path[0].text) else {
        return Cow::Borrowed(call);
    };
    if definition.calculation.is_none()
        || !definition.parameters.contains("input")
        || call
            .arguments
            .iter()
            .any(|argument| argument.key.text == "input")
    {
        return Cow::Borrowed(call);
    }
    let mut bound = call.clone();
    bound.arguments.push(Argument {
        key: Name {
            text: "input".into(),
            span: call.span,
        },
        value: Value::Reference(Name {
            text: reference(input),
            span: call.span,
        }),
        span: call.span,
    });
    Cow::Owned(bound)
}

pub(super) fn reference(input: &OutputRef) -> String {
    let suffix = match input.port {
        OutputPort::Data => "",
        OutputPort::Error => "::error",
        OutputPort::Cancel => "::cancel",
    };
    format!("{}{suffix}", input.node)
}

/// Introduce an ordinary lexical const in the parsed AST, retaining user spans.
/// This forces a dependency even for a constant-returning stage. No global binding
/// or string substitution is involved; normal lexical scope/TDZ rules still apply.
pub(super) fn bind_calculation_input(program: &mut calc::Program, input: &OutputRef, span: Span) {
    let value = program.expressions.len();
    program.expressions.push(calc::Expr {
        kind: calc::ExprKind::Workspace(reference(input)),
        span,
    });
    let declaration = program.statements.len();
    program.statements.push(calc::Stmt {
        kind: calc::StmtKind::Binding {
            name: Name {
                text: "input".into(),
                span,
            },
            mutable: false,
            value,
        },
        span,
    });
    let calc::StmtKind::Block(body) = &mut program.statements[program.root].kind else {
        unreachable!("calc context is a block")
    };
    body.insert(0, declaration);
}

pub(super) fn lower(
    stage: &Statement,
    input: Option<&OutputRef>,
) -> Result<Statement, WorkspaceError> {
    let mut stage = stage.clone();
    match &mut stage.expression {
        Expression::Calculation(_) => (),
        Expression::Call(call)
            if call.marker.is_some() && call.path.first().is_some_and(|n| n.text == "stream") =>
        {
            if call.path.get(1).is_some_and(|n| n.text == "accumulate") {
                call.path.remove(0);
                return lower(&stage, input);
            }
        }
        Expression::Call(call)
            if call.marker.is_some()
                && call.path.first().is_some_and(|n| n.text == "accumulate") =>
        {
            let input = input.ok_or_else(|| {
                rejected(
                    "ACC001",
                    stage.span,
                    "accumulate requires an ordered event pipeline",
                )
            })?;
            if !call.operands.is_empty() {
                return Err(rejected(
                    "ACC001",
                    stage.span,
                    "accumulate takes the preceding event, not an explicit subject",
                ));
            }
            call.operands.push(Value::Reference(Name {
                text: reference(input),
                span: stage.span,
            }));
        }
        Expression::Call(call)
            if call.marker.is_some()
                && call
                    .path
                    .iter()
                    .take(2)
                    .map(|name| name.text.as_str())
                    .eq(["view", "create"]) =>
        {
            let input = input.ok_or_else(|| {
                rejected(
                    "PIP001",
                    stage.span,
                    "A presentation stage requires a preceding value",
                )
            })?;
            if call
                .arguments
                .iter()
                .any(|argument| argument.key.text == "input")
            {
                return Err(rejected(
                    "VIE003",
                    stage.span,
                    "The final view receives the preceding value; project or map it in an adapter stage instead of supplying input:",
                ));
            }
            call.arguments.push(Argument {
                key: Name {
                    text: "input".into(),
                    span: stage.span,
                },
                value: Value::Reference(Name {
                    text: reference(input),
                    span: stage.span,
                }),
                span: stage.span,
            });
        }
        Expression::Call(call) if call.marker.is_none() => {
            for argument in &mut call.arguments {
                lower_value(&mut argument.value, input)?;
            }
        }
        Expression::Reference(_) if input.is_none() => (),
        _ => {
            return Err(rejected(
                "PIP001",
                stage.span,
                "pipeline stages must be finite calls or :calc, with an optional final :view create; only the first stage may be an output reference",
            ));
        }
    }
    Ok(stage)
}

fn lower_value(value: &mut Value, input: Option<&OutputRef>) -> Result<(), WorkspaceError> {
    match value {
        Value::Structured(_, wes_language::Structure::Record(fields)) => {
            for (_, value) in fields {
                lower_value(value, input)?;
            }
        }
        Value::Structured(_, wes_language::Structure::List(items)) => {
            for value in items {
                lower_value(value, input)?;
            }
        }
        Value::Word(name) if name.text == "input" || name.text.starts_with("input.") => {
            let input = input.ok_or_else(|| {
                rejected(
                    "PIP002",
                    name.span,
                    "input requires a preceding pipeline stage; quote it for literal text",
                )
            })?;
            let fields = &name.text[5..];
            if !fields.is_empty()
                && fields[1..].split('.').any(|field| {
                    field.is_empty()
                        || field
                            .chars()
                            .any(|c| !(c.is_alphanumeric() || c == '_' || c == '-'))
                })
            {
                return Err(rejected(
                    "PIP002",
                    name.span,
                    "use input or input.field with nonempty field names",
                ));
            }
            *value = Value::Reference(Name {
                text: format!("{}{fields}", reference(input)),
                span: name.span,
            });
        }
        _ => (),
    }
    Ok(())
}
