//! Pure checking over metadata. The caller supplies snapshots, never an execution callback.
use crate::{
    Annotation, Argument, Call, Diagnostic, Severity, Span, Value,
    resolve::Resolution,
    vocabulary::{ANNOTATIONS, Arity},
};
use indexmap::IndexMap;
use std::{collections::BTreeSet, sync::Arc};
use wes_core::{
    Provenance, Shape,
    capability::{Capability, DeclaredRule, Parameter, Rule, RuleBasis, Sort, Typing},
    literals,
};

#[derive(Clone, Debug)]
pub enum GivenValue {
    Written(String),
    Known(Typing),
    Missing(String),
}
#[derive(Clone, Debug)]
pub struct Given {
    pub value: GivenValue,
    pub key: Span,
    pub span: Span,
}

/// A snapshot of a node's call, used to check the merged result of :change.
#[derive(Clone, Debug)]
pub struct ExistingCall {
    pub capability: Arc<Capability>,
    pub arguments: IndexMap<String, Given>,
    pub excused: BTreeSet<String>,
    pub validated_inputs: IndexMap<String, Shape>,
}

#[derive(Debug, Default)]
pub struct Environment {
    pub bindings: IndexMap<String, Typing>,
    pub calls: IndexMap<String, ExistingCall>,
}

/// Runtime-guarded shapes are local to the supplied argument keys. The environment is immutable.
pub fn check(
    resolution: &Resolution,
    annotations: &[Annotation],
    validated: &IndexMap<String, Shape>,
    environment: &Environment,
) -> Vec<Diagnostic> {
    let mut diagnostics = Vec::new();
    for annotation in annotations {
        if !ANNOTATIONS
            .iter()
            .any(|(name, _)| *name == annotation.name.text)
        {
            diagnostics.push(
                Diagnostic::error(
                    "CHK014",
                    annotation.name.span,
                    format!("there is no annotation called '@{}'", annotation.name.text),
                )
                .with_public_message("Unknown annotation.")
                .with_hint(format!(
                    "there is {}",
                    ANNOTATIONS
                        .iter()
                        .map(|(name, _)| *name)
                        .collect::<Vec<_>>()
                        .join(", ")
                )),
            );
        }
    }
    let held: Vec<_> = annotations
        .iter()
        .filter(|a| a.name.text == "hold")
        .collect();
    if !held.is_empty()
        && (held.len() != 1
            || !held[0].targets.is_empty()
            || !matches!(resolution, Resolution::Capability { capability, .. } if capability.streaming)
            || annotations.iter().any(|a| a.name.text == "interactive"))
    {
        diagnostics.push(Diagnostic::error(
            "CHK014", held[0].name.span,
            "Use one @hold without arguments on a non-interactive streaming source. It declares work, not producer execution.",
        ));
    }
    let traces: Vec<_> = annotations
        .iter()
        .filter(|a| a.name.text == "trace")
        .collect();
    if !traces.is_empty()
        && (traces.len() != 1
            || traces[0].targets.len() != 1
            || traces[0].targets[0].text.is_empty()
            || traces[0].targets[0].text.len() > 64
            || !matches!(resolution, Resolution::Capability { capability, .. } if !capability.streaming)
            || annotations.iter().any(|a| a.name.text == "interactive"))
    {
        diagnostics.push(Diagnostic::error(
            "TRC001",
            traces[0].name.span,
            "Use one @trace(profile) on a finite provider call; the profile must be 1–64 bytes.",
        ));
    }
    let call = resolution.call();
    let arguments = arguments_by_key(call, &mut diagnostics);
    let mut givens = givens(&arguments, environment, &|key| match resolution {
        Resolution::Capability { capability, .. } => capability
            .parameter(key)
            .map_or(Shape::Unknown, |p| p.shape.clone()),
        Resolution::Meta { spec, call, .. }
            if spec.command == crate::vocabulary::MetaCommand::Change =>
        {
            call.operands
                .first()
                .and_then(|v| {
                    if let Value::Reference(name) = v {
                        environment.calls.get(&name.text)
                    } else {
                        None
                    }
                })
                .and_then(|existing| existing.capability.parameter(key))
                .map_or(Shape::Unknown, |p| p.shape.clone())
        }
        Resolution::Meta { spec, .. } => spec
            .parameter(key)
            .map_or(Shape::Unknown, |p| p.shape.clone()),
    });
    match resolution {
        Resolution::Capability { capability, .. } => {
            if let Some(operand) = call.operands.first() {
                diagnostics.push(
                    Diagnostic::error(
                        "CHK010",
                        operand.name().span,
                        format!("'{}' takes named arguments only", capability.path.join(" ")),
                    )
                    .with_public_message(
                        "This provider operation takes named arguments only. Expected key:value.",
                    )
                    .with_hint("write it as 'key:value'"),
                );
            }
            refine(&mut givens, validated);
            let excused = annotations
                .iter()
                .filter(|a| a.name.text == "unchecked")
                .flat_map(|a| a.targets.iter().map(|n| n.text.clone()))
                .collect();
            check_call(capability, &givens, &excused, call.span, &mut diagnostics);
        }
        Resolution::Meta { spec, tail, .. } => {
            for annotation in annotations.iter().filter(|a| a.name.text == "interactive") {
                diagnostics.push(
                    Diagnostic::error(
                        "CHK016",
                        annotation.name.span,
                        format!(
                            "'@interactive' is for a call to a service, and ':{}' is not one",
                            spec.canonical
                        ),
                    )
                    .with_public_message("This annotation requires a provider call."),
                );
            }
            count(
                tail.len(),
                spec.path_tail,
                "CHK012",
                &format!("words after ':{}'", spec.canonical),
                spec.path_tail
                    .max
                    .and_then(|max| call.path.get(max + 1))
                    .map_or(call.span, |n| n.span),
                &mut diagnostics,
            );
            count(
                call.operands.len(),
                spec.operands,
                "CHK011",
                &format!("values for ':{}'", spec.canonical),
                spec.operands
                    .max
                    .and_then(|max| call.operands.get(max))
                    .map_or(call.span, |v| v.name().span),
                &mut diagnostics,
            );
            if !spec.tail_words.is_empty() {
                for word in tail
                    .iter()
                    .filter(|word| !spec.tail_words.contains(&word.as_str()))
                {
                    diagnostics.push(
                        Diagnostic::error(
                            "CHK015",
                            call.path
                                .iter()
                                .skip(1)
                                .find(|name| &name.text == word)
                                .map_or(call.span, |name| name.span),
                            format!("':{}' has nothing called '{word}'", spec.canonical),
                        )
                        .with_public_message("Unknown subcommand or registry.")
                        .with_hint(suggestions(&spec.tail_words)),
                    );
                }
            }
            let inspecting_type = spec.command == crate::vocabulary::MetaCommand::Inspect
                && givens.contains_key("type");
            if spec.command == crate::vocabulary::MetaCommand::Inspect
                && usize::from(!tail.is_empty())
                    + call.operands.len()
                    + usize::from(inspecting_type)
                    > 1
            {
                diagnostics.push(Diagnostic::error("CHK018", call.span,
                    "inspect requires exactly one target: a reference, capability path, or literal type:"));
            }
            if spec.requires_subject
                && tail.is_empty()
                && call.operands.is_empty()
                && !inspecting_type
            {
                diagnostics.push(
                    Diagnostic::error(
                        "CHK013",
                        call.span,
                        format!("':{}' needs something to act on", spec.canonical),
                    ).with_public_message("Missing command target. Expected a result reference or supported target name.")
                    .with_hint(format!(
                        "name a result, as in ':{} $result'",
                        spec.canonical
                    )),
                );
            }
            parameters(
                &givens,
                &spec.parameters,
                spec.open_arguments,
                &format!(":{}", spec.canonical),
                call.span,
                &mut diagnostics,
            );
            if spec.rewrites_target
                && let [Value::Reference(target)] = call.operands.as_slice()
                && let Some(existing) = environment.calls.get(&target.text)
            {
                let mut merged = existing.arguments.clone();
                merged.extend(givens);
                refine(&mut merged, &existing.validated_inputs);
                check_call(
                    &existing.capability,
                    &merged,
                    &existing.excused,
                    call.span,
                    &mut diagnostics,
                );
            }
        }
    }
    diagnostics
}

pub fn check_call(
    capability: &Capability,
    arguments: &IndexMap<String, Given>,
    excused: &BTreeSet<String>,
    span: Span,
    diagnostics: &mut Vec<Diagnostic>,
) {
    parameters(
        arguments,
        &capability.parameters,
        false,
        &capability.path.join(" "),
        span,
        diagnostics,
    );
    for declared in &capability.rules {
        if let Some(mut broken) = rule(&declared.rule, arguments, span) {
            if overridden(&declared.rule, excused) {
                broken = broken
                    .with_severity(Severity::Warning)
                    .with_hint("run anyway because of '@unchecked'");
            } else {
                broken = as_declared(broken, declared);
            }
            diagnostics.push(broken);
        }
    }
}

fn arguments_by_key<'a>(
    call: &'a Call,
    diagnostics: &mut Vec<Diagnostic>,
) -> IndexMap<String, &'a Argument> {
    let mut arguments = IndexMap::new();
    for argument in &call.arguments {
        if arguments
            .insert(argument.key.text.clone(), argument)
            .is_some()
        {
            diagnostics.push(
                Diagnostic::error(
                    "CHK003",
                    argument.span,
                    format!("'{}' was given twice", argument.key.text),
                )
                .with_public_message("Duplicate argument key."),
            );
        }
    }
    arguments
}
fn givens(
    arguments: &IndexMap<String, &Argument>,
    environment: &Environment,
    expected: &impl Fn(&str) -> Shape,
) -> IndexMap<String, Given> {
    arguments
        .iter()
        .map(|(name, argument)| {
            let value = match &argument.value {
                value @ Value::Structured(_, _) => {
                    match crate::structured::typing(value, &expected(name), &|reference| {
                        environment.bindings.get(reference).cloned()
                    }) {
                        Ok(typing) => GivenValue::Known(typing),
                        Err(reference) => GivenValue::Missing(reference.text),
                    }
                }
                Value::Word(word) | Value::Text(word) => GivenValue::Written(word.text.clone()),
                Value::Reference(reference) => environment
                    .bindings
                    .get(&reference.text)
                    .cloned()
                    .map(GivenValue::Known)
                    .unwrap_or_else(|| GivenValue::Missing(reference.text.clone())),
            };
            (
                name.clone(),
                Given {
                    value,
                    key: argument.key.span,
                    span: argument.value.name().span,
                },
            )
        })
        .collect()
}
fn refine(arguments: &mut IndexMap<String, Given>, validated: &IndexMap<String, Shape>) {
    for (key, shape) in validated {
        if let Some(Given {
            value: GivenValue::Known(typing),
            ..
        }) = arguments.get_mut(key)
        {
            typing.shape = shape.clone();
        }
    }
}
fn parameters(
    arguments: &IndexMap<String, Given>,
    parameters: &[Parameter],
    open: bool,
    subject: &str,
    span: Span,
    diagnostics: &mut Vec<Diagnostic>,
) {
    for parameter in parameters {
        let Some(given) = arguments.get(&parameter.name) else {
            if parameter.required {
                diagnostics.push(
                    Diagnostic::error(
                        "CHK002",
                        span,
                        format!("'{subject}' needs '{}:'", parameter.name),
                    )
                    .with_public_message("A required argument is missing."),
                );
            }
            continue;
        };
        if matches!(parameter.sort, Sort::Selector(_) | Sort::Fresh(_))
            && !matches!(given.value, GivenValue::Written(_))
        {
            let (what, hint) = match &parameter.sort {
                Sort::Fresh(namespace) => (
                    format!("a name for a {namespace}, written out"),
                    "it says what to call the new one, so there is nothing to refer to yet",
                ),
                Sort::Selector(registry) => (
                    format!("the name of a {registry}, written out"),
                    "it is settled before anything runs, so it cannot come from a result",
                ),
                Sort::Plain | Sort::Resource(_) => unreachable!("literal-only sort"),
            };
            diagnostics.push(
                Diagnostic::error(
                    "CHK017",
                    given.span,
                    format!("'{}:' takes {what}, not a reference", parameter.name),
                )
                .with_public_message("This parameter requires a literal name, not a reference.")
                .with_hint(hint),
            );
            continue;
        }
        match &given.value {
            GivenValue::Missing(name) => diagnostics.push(Diagnostic::error(
                "CHK005",
                given.span,
                format!("nothing is named '${name}'"),
            ).with_public_message("Referenced name is not defined in this workspace.")),
            GivenValue::Known(typing) if !typing.shape.is_assignable_to(&parameter.shape) => {
                diagnostics.push(Diagnostic::error(
                    "CHK004",
                    given.span,
                    format!(
                        "'{}:' needs {} but this is {}",
                        parameter.name, parameter.shape, typing.shape
                    ),
                ).with_public_message(format!("Parameter '{}:' requires {}. Use :type check $value as:\"{}\" to validate/refine a loosely typed result before passing it; the value's inferred type is withheld.", parameter.name, parameter.shape, parameter.shape)))
            }
            GivenValue::Written(text) if literals::read(text, &parameter.shape).is_none() => {
                diagnostics.push(Diagnostic::error(
                    "CHK004",
                    given.span,
                    format!("'{text}' cannot be read as {}", parameter.shape),
                ).with_public_message("Literal cannot be read as the required parameter type."))
            }
            _ => {}
        }
    }
    if !open {
        for (key, given) in arguments
            .iter()
            .filter(|(key, _)| !parameters.iter().any(|p| &p.name == *key))
        {
            let mut diagnostic = Diagnostic::error(
                "CHK001",
                given.key,
                format!("'{subject}' does not take '{key}:'"),
            )
            .with_public_message("Unexpected argument key for this command.");
            if !parameters.is_empty() {
                diagnostic = diagnostic.with_hint(format!(
                    "it takes {}",
                    parameters
                        .iter()
                        .map(|p| p.name.as_str())
                        .collect::<Vec<_>>()
                        .join(", ")
                ));
            }
            diagnostics.push(diagnostic);
        }
    }
}
fn count(
    actual: usize,
    arity: Arity,
    code: &'static str,
    what: &str,
    span: Span,
    diagnostics: &mut Vec<Diagnostic>,
) {
    if actual < arity.min {
        diagnostics.push(
            Diagnostic::error(
                code,
                span,
                format!("expected at least {} {what}, found {actual}", arity.min),
            )
            .with_public_message(if code == "CHK012" {
                "Missing required command path word."
            } else {
                "Missing required command operand."
            }),
        );
    } else if let Some(max) = arity.max
        && actual > max
    {
        diagnostics.push(
            Diagnostic::error(
                code,
                span,
                format!("expected at most {max} {what}, found {actual}"),
            )
            .with_public_message(if code == "CHK012" {
                "Unexpected extra command path word."
            } else {
                "Unexpected extra command operand."
            }),
        );
    }
}
fn suggestions(words: &[&str]) -> String {
    if words.len() <= 8 {
        format!("try: {}", words.join(", "))
    } else {
        format!(
            "try: {} … and {} more; ':help' lists them",
            words[..8].join(", "),
            words.len() - 8
        )
    }
}
fn overridden(rule: &Rule, excused: &BTreeSet<String>) -> bool {
    match rule {
        Rule::MutuallyExclusive(keys) => keys.iter().any(|key| excused.contains(key)),
        Rule::Requires { key, needs } => excused.contains(key) || excused.contains(needs),
        Rule::OneOf { key, .. } | Rule::ProvenanceFact { key, .. } => excused.contains(key),
    }
}
fn as_declared(broken: Diagnostic, declared: &DeclaredRule) -> Diagnostic {
    match &declared.basis {
        RuleBasis::Documented { note: Some(note) } => {
            broken.with_hint(format!("the service says: {note}"))
        }
        RuleBasis::Documented { note: None } => broken,
        RuleBasis::Inferred { reason } => broken
            .with_severity(Severity::Warning)
            .with_hint(format!("inferred from: {reason}")),
    }
}
fn rule(rule: &Rule, arguments: &IndexMap<String, Given>, span: Span) -> Option<Diagnostic> {
    match rule {
        Rule::MutuallyExclusive(keys) => {
            let present = keys
                .iter()
                .filter(|key| arguments.contains_key(*key))
                .map(String::as_str)
                .collect::<Vec<_>>();
            (present.len() > 1).then(|| {
                Diagnostic::error(
                    "CHK006",
                    span,
                    format!("only one of [{}] may be given", present.join(", ")),
                )
                .with_public_message("Arguments are mutually exclusive.")
            })
        }
        Rule::Requires { key, needs } => {
            let argument = arguments.get(key)?;
            (!arguments.contains_key(needs)).then(|| {
                Diagnostic::error(
                    "CHK007",
                    argument.span,
                    format!("'{key}:' means nothing without '{needs}:'"),
                )
                .with_public_message("Argument requires another parameter.")
            })
        }
        Rule::OneOf { key, values } => {
            let argument = arguments.get(key)?;
            let GivenValue::Written(text) = &argument.value else {
                return None;
            };
            let values = values.iter().map(String::as_str).collect::<Vec<_>>();
            (!values.contains(&text.as_str())).then(|| {
                Diagnostic::error(
                    "CHK008",
                    argument.span,
                    format!("'{text}' is not one of [{}]", values.join(", ")),
                )
                .with_public_message(
                    "Argument is outside the declared choices; choice details are withheld.",
                )
                .with_hint(format!("allowed: {}", values.join(", ")))
            })
        }
        Rule::ProvenanceFact {
            key,
            fact,
            expected,
        } => {
            let argument = arguments.get(key)?;
            let empty = Provenance::default();
            let provenance = match &argument.value {
                GivenValue::Known(typing) => &typing.provenance,
                GivenValue::Written(_) => &empty,
                GivenValue::Missing(_) => return None,
            };
            let actual = provenance.facts().get(fact);
            if actual == Some(expected) {
                return None;
            }
            let actual = actual.map_or_else(
                || format!("it does not say what {fact} it used"),
                |value| format!("it says {fact}={value}"),
            );
            Some(
                Diagnostic::error(
                    "CHK009",
                    argument.span,
                    format!("'{key}:' must come from {fact}={expected}, but {actual}"),
                )
                .with_public_message(
                    "Required provenance fact is not satisfied; fact details are withheld.",
                )
                .with_hint(format!("re-fetch it with '{fact}:{expected}'")),
            )
        }
    }
}
