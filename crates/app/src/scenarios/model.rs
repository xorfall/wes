use serde::Deserialize;
use std::collections::BTreeSet;
use wes_core::contracts::{PackageNode as Node, PackageScalarKind as Scalar, read_package};
use wes_language::{Expression, Severity, SourceText, calc, parse, vocabulary::MetaCommand};

#[derive(Debug, thiserror::Error)]
#[error("TEST001: {0}")]
pub struct ScenarioError(pub(super) String);

#[derive(Debug)]
pub struct Scenario {
    pub(super) document: Document,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Document {
    pub(super) version: u8,
    pub(super) name: String,
    #[serde(default = "timeout")]
    pub(super) timeout_seconds: u64,
    pub(super) steps: Vec<Step>,
    #[serde(default)]
    pub(super) cleanup: Vec<Step>,
}
fn timeout() -> u64 {
    60
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Step {
    pub name: String,
    pub run: String,
    #[serde(default)]
    pub expect: Vec<Expectation>,
    pub expect_error: Option<ExpectedError>,
}
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Expectation {
    pub name: String,
    pub condition: String,
}
impl Expectation {
    pub fn source(&self) -> String {
        format!(":calc {{ return ({}); }}", self.condition)
    }
}
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct ExpectedError {
    pub code: String,
    pub issue: Option<String>,
}
fn fail(message: impl Into<String>) -> ScenarioError {
    ScenarioError(message.into())
}
fn name(value: &str) -> Result<(), ScenarioError> {
    if value.trim().is_empty() || value.len() > 256 || value.chars().any(char::is_control) {
        return Err(fail(
            "names/codes must be nonempty, single-line text of at most 256 bytes",
        ));
    }
    Ok(())
}
impl Scenario {
    /// Validate all source before a runner can dispatch any step. Does not inspect services/files.
    pub fn parse(source: &str) -> Result<Self, ScenarioError> {
        if source.len() > 1024 * 1024 {
            return Err(fail("scenario exceeds 1 MiB"));
        }
        let yaml = read_package(source).map_err(|_| {
            fail("invalid scenario YAML (one document, unique keys, bounded nesting required)")
        })?;
        let scenario: Document = serde_json::from_value(json(yaml)?)
            .map_err(|_| fail("invalid scenario fields or types; expected version, name, steps and optional cleanup/timeout_seconds"))?;
        if scenario.version != 1 {
            return Err(fail("unsupported scenario version; expected 1"));
        }
        name(&scenario.name)?;
        if !(1..=3600).contains(&scenario.timeout_seconds) {
            return Err(fail("timeout_seconds must be between 1 and 3600"));
        }
        if scenario.steps.is_empty() || scenario.steps.len() + scenario.cleanup.len() > 128 {
            return Err(fail(
                "scenario needs at least one step and at most 128 steps including cleanup",
            ));
        }
        let mut names = BTreeSet::new();
        let mut checks = 0;
        for step in scenario.steps.iter().chain(&scenario.cleanup) {
            name(&step.name)?;
            if !names.insert(&step.name) {
                return Err(fail("step names must be unique, including cleanup"));
            }
            validate_step(&step.run)?;
            if let Some(error) = &step.expect_error {
                name(&error.code)?;
                if let Some(issue) = &error.issue {
                    name(issue)?;
                }
            }
            let mut check_names = BTreeSet::new();
            for check in &step.expect {
                checks += 1;
                if checks > 512 {
                    return Err(fail("scenario exceeds 512 expectations"));
                }
                name(&check.name)?;
                if !check_names.insert(&check.name) {
                    return Err(fail("expectation names must be unique within a step"));
                }
                validate_condition(&check.source())?;
            }
        }
        Ok(Self { document: scenario })
    }
}
fn validate_step(source: &str) -> Result<(), ScenarioError> {
    if source.len() > 64 * 1024 {
        return Err(fail("a step exceeds 64 KiB"));
    }
    let parsed = parse(&SourceText::new("scenario step", source));
    if parsed
        .diagnostics
        .iter()
        .any(|d| d.severity == Severity::Error)
    {
        return Err(fail("a step contains invalid wes syntax"));
    }
    let script = parsed.script;
    if script.statements.len() != 1 {
        return Err(fail("each step must contain exactly one wes statement"));
    }
    let statement = &script.statements[0];
    if matches!(statement.expression, Expression::Pipeline(_)) {
        return Err(fail(
            "pipeline steps are not supported by the scenario runner yet; use separate steps",
        ));
    }
    if statement.annotations.iter().any(|a| {
        !matches!(
            a.name.text.as_str(),
            "trace" | "timeout" | "unchecked" | "env"
        )
    }) {
        return Err(fail(
            "scenario steps support finite work only; unsupported annotation",
        ));
    }
    if let Expression::Call(call) = &statement.expression {
        let head = call.path.first().map(|n| n.text.as_str()).unwrap_or("");
        if head.starts_with('/') {
            return Err(fail("client commands are not scenario steps"));
        }
        if let Ok(invocation) = wes_language::vocabulary::commands::invocation(call)
            && !matches!(
                invocation.spec.command,
                MetaCommand::Import
                    | MetaCommand::Type
                    | MetaCommand::Read
                    | MetaCommand::Trace
                    | MetaCommand::Inspect
                    | MetaCommand::List
                    | MetaCommand::Help
                    | MetaCommand::ViewCreate
                    | MetaCommand::ViewBind
                    | MetaCommand::ViewConnect
                    | MetaCommand::ViewDisconnect
                    | MetaCommand::ViewOutput
                    | MetaCommand::ViewCapture
                    | MetaCommand::ViewPin
                    | MetaCommand::ViewLink
                    | MetaCommand::ViewUnlink
            )
        {
            return Err(fail(
                "workspace/authority/lifecycle commands are not scenario steps; use startup options",
            ));
        }
    }
    Ok(())
}
fn validate_condition(source: &str) -> Result<(), ScenarioError> {
    if source.len() > 16 * 1024 {
        return Err(fail("an expectation exceeds 16 KiB"));
    }
    let program = calc::parse_context(source, 0, calc::Package::standard())
        .map_err(|_| fail("expectation must be one pure calc expression"))?;
    let calc::StmtKind::Block(statements) = &program.statements[program.root].kind else {
        return Err(fail("invalid expectation"));
    };
    if statements.len() != 1
        || !matches!(
            program.statements[statements[0]].kind,
            calc::StmtKind::Return(_)
        )
    {
        return Err(fail("expectation must be one pure calc expression"));
    }
    if program.expressions.iter().any(|e| {
        matches!(&e.kind, calc::ExprKind::Name(name)
        if program.package.operation(name).is_some_and(|s| s.operation.effectful()))
    }) {
        return Err(fail(
            "expectations cannot call providers; place effects in an explicit run step",
        ));
    }
    Ok(())
}
fn json(node: Node) -> Result<serde_json::Value, ScenarioError> {
    Ok(match node {
        Node::Scalar(Scalar::Text, s) => s.into(),
        Node::Scalar(Scalar::Int, s) => {
            serde_json::Value::Number(s.parse().map_err(|_| fail("invalid integer"))?)
        }
        Node::Scalar(Scalar::Bool, s) => serde_json::Value::Bool(s == "true"),
        Node::Scalar(Scalar::Decimal, _) => {
            return Err(fail(
                "decimal YAML configuration values are unsupported; use a quoted calc expression",
            ));
        }
        Node::Sequence(xs) => {
            serde_json::Value::Array(xs.into_iter().map(json).collect::<Result<_, _>>()?)
        }
        Node::Mapping(xs) => serde_json::Value::Object(
            xs.into_iter()
                .map(|(k, v)| Ok((k, json(v)?)))
                .collect::<Result<_, ScenarioError>>()?,
        ),
    })
}

/// Explicit inputs must have been produced by this run, including cleanup after partial failure.
pub(super) fn references(source: &str) -> Vec<String> {
    let parsed = parse(&SourceText::new("scenario", source));
    let mut refs = Vec::new();
    let mut statements = parsed.script.statements;
    while let Some(statement) = statements.pop() {
        match statement.expression {
            Expression::Sandbox(_) => {} // Local references are not workspace dependencies.
            Expression::Fork(branches) => {
                statements.extend(branches.into_iter().rev().map(|b| b.body))
            }
            Expression::Pipeline(stages) => statements.extend(stages.into_iter().rev()),
            Expression::Calculation(source) => {
                if let Ok(program) = calc::parse_context(&source.text, 0, calc::Package::standard())
                {
                    refs.extend(
                        program
                            .expressions
                            .into_iter()
                            .filter_map(|e| match e.kind {
                                calc::ExprKind::Workspace(name) => Some(name),
                                _ => None,
                            }),
                    );
                }
            }
            Expression::Reference(name) => refs.push(name.text),
            Expression::Definition(wes_language::Template {
                body: wes_language::TemplateBody::Calculation(_),
                ..
            }) => {}
            Expression::Call(call)
            | Expression::Definition(wes_language::Template {
                body: wes_language::TemplateBody::Call(call),
                ..
            }) => {
                refs.extend(
                    call.operands
                        .into_iter()
                        .chain(call.arguments.into_iter().map(|a| a.value))
                        .flat_map(|v| {
                            v.references()
                                .into_iter()
                                .map(|name| name.text.clone())
                                .collect::<Vec<_>>()
                        }),
                );
            }
        }
    }
    refs
}
