//! Bounded AST substitution. Templates never interpret source strings or capture a workspace.
use crate::{
    Diagnostic, Span,
    ast::{Argument, Call, Name, Template, TemplateBody, Value},
};
use indexmap::{IndexMap, IndexSet};
use std::sync::Arc;
use wes_core::contracts::{Contract, ContractRegistry};

#[derive(Clone, Debug)]
pub struct Definition {
    pub calculation: Option<Arc<CalculationDefinition>>,
    pub syntax: Template,
    pub parameters: IndexSet<String>,
    pub contracts: IndexMap<String, Arc<Contract>>,
}

impl Definition {
    /// Semantic template identity excludes source file names and byte offsets.
    pub fn revision(&self) -> String {
        use sha2::{Digest, Sha256};
        if let Some(calc) = &self.calculation {
            return calc.revision.clone();
        }
        let mut hash = Sha256::new();
        let mut field = |value: &str| {
            hash.update((value.len() as u64).to_be_bytes());
            hash.update(value.as_bytes());
        };
        field(&self.syntax.name.text);
        for parameter in &self.syntax.parameters {
            field(&parameter.name);
            field(&parameter.type_expression);
        }
        field(&self.syntax.body.to_string());
        format!("{:x}", hash.finalize())
    }
}

#[derive(Clone, Debug)]
pub struct CalculationDefinition {
    pub compiled: Arc<crate::calc::Compiled>,
    pub output: Arc<Contract>,
    pub revision: String,
}
impl CalculationDefinition {
    pub fn conversion_eligible(&self) -> bool {
        !self.compiled.effectful() && self.compiled.parameters.contains_key("input")
    }
}

#[derive(Clone, Debug)]
pub struct Expanded {
    pub calculation: Option<Arc<CalculationDefinition>>,
    pub call: Call,
    /// Guards belong to destination arguments, not to the producer of a referenced value.
    pub contracts: IndexMap<String, Vec<Arc<Contract>>>,
}

#[derive(Clone, Debug, Default)]
pub struct Templates {
    definitions: IndexMap<String, Definition>,
}

impl Templates {
    pub fn new() -> Self {
        Self::default()
    }
    pub fn snapshot(&self) -> &IndexMap<String, Definition> {
        &self.definitions
    }
    pub fn contains(&self, name: &str) -> bool {
        self.definitions.contains_key(name)
    }
    pub fn define(&mut self, syntax: Template, types: &ContractRegistry) -> Result<(), Diagnostic> {
        let name = &syntax.name.text;
        identifier(name, syntax.name.span)?;
        if self.contains(name) {
            return Err(problem(
                "TMP002",
                syntax.span,
                format!("template is already defined: {name}"),
            ));
        }
        let TemplateBody::Call(body) = &syntax.body else {
            return Err(problem(
                "TMP001",
                syntax.span,
                "calc definitions require captured analysis",
            ));
        };
        if body.marker.is_some() || !body.operands.is_empty() || body.path.is_empty() {
            return Err(problem(
                "TMP001",
                body.span,
                "a template body must be one object command with named arguments",
            ));
        }
        if body.path.iter().any(|part| part.text.starts_with('?')) {
            return Err(problem(
                "TMP001",
                body.span,
                "placeholders may fill argument values, not a command path",
            ));
        }
        let mut parameters = IndexSet::new();
        let mut keys = IndexSet::new();
        for argument in &body.arguments {
            if !keys.insert(&argument.key.text) {
                return Err(problem(
                    "TMP001",
                    argument.span,
                    format!("duplicate body argument: {}", argument.key.text),
                ));
            }
            if argument.key.text.starts_with('?') {
                return Err(problem(
                    "TMP001",
                    argument.span,
                    "argument names must be fixed",
                ));
            }
            if let Some(parameter) = placeholder(&argument.value)? {
                parameters.insert(parameter.to_string());
            }
        }
        let mut contracts = IndexMap::new();
        for parameter in &syntax.parameters {
            identifier(&parameter.name, parameter.span)?;
            if !parameters.contains(&parameter.name) {
                return Err(problem(
                    "TMP001",
                    parameter.span,
                    format!("declared parameter is unused: {}", parameter.name),
                ));
            }
            let contract = types
                .resolve(&parameter.type_expression)
                .map_err(|error| problem(error.code, parameter.span, error.message))?;
            if contracts.insert(parameter.name.clone(), contract).is_some() {
                return Err(problem(
                    "TMP001",
                    parameter.span,
                    format!("duplicate parameter declaration: {}", parameter.name),
                ));
            }
        }
        if self.definitions.len() >= 1000 || parameters.len() > 256 {
            return Err(problem(
                "TMP005",
                syntax.span,
                "template registry or parameter limit reached",
            ));
        }
        let mut seen = IndexSet::from([name.as_str()]);
        let mut target = body.path[0].text.as_str();
        for depth in 0.. {
            if !seen.insert(target) {
                return Err(problem(
                    "TMP005",
                    body.span,
                    format!("cyclic template expansion: {target}"),
                ));
            }
            let Some(nested) = self.definitions.get(target) else {
                break;
            };
            if depth >= 63 {
                return Err(problem("TMP005", body.span, "template nesting exceeds 64"));
            }
            let TemplateBody::Call(body) = &nested.syntax.body else {
                break;
            };
            target = &body.path[0].text;
        }
        self.definitions.insert(
            name.clone(),
            Definition {
                calculation: None,
                syntax,
                parameters,
                contracts,
            },
        );
        Ok(())
    }

    pub fn define_calculation(
        &mut self,
        syntax: Template,
        types: &ContractRegistry,
        catalogue: &wes_core::capability::Catalogue,
        package: Arc<crate::calc::Package>,
    ) -> Result<(), Diagnostic> {
        use crate::calc;
        use sha2::{Digest, Sha256};
        identifier(&syntax.name.text, syntax.name.span)?;
        if self.contains(&syntax.name.text) {
            return Err(problem(
                "TMP002",
                syntax.span,
                "definition is already installed",
            ));
        }
        if self.definitions.len() >= 1000 || syntax.parameters.len() > 256 {
            return Err(problem(
                "TMP005",
                syntax.span,
                "definition or parameter limit reached",
            ));
        }
        let TemplateBody::Calculation(source) = &syntax.body else {
            return self.define(syntax, types);
        };
        let output = syntax.output.as_ref().ok_or_else(|| {
            problem(
                "TMP001",
                syntax.span,
                "calc definitions require an explicit output contract (-> Type)",
            )
        })?;
        let output = types
            .resolve(&output.text)
            .map_err(|e| problem(e.code, output.span, e.message))?;
        let mut contracts = IndexMap::new();
        for parameter in &syntax.parameters {
            identifier(&parameter.name, parameter.span)?;
            if parameter.name.contains('-') {
                return Err(problem(
                    "TMP001",
                    parameter.span,
                    "calc parameter names cannot contain a hyphen",
                ));
            }
            let contract = types
                .resolve(&parameter.type_expression)
                .map_err(|e| problem(e.code, parameter.span, e.message))?;
            if contracts.insert(parameter.name.clone(), contract).is_some() {
                return Err(problem(
                    "TMP001",
                    parameter.span,
                    "duplicate parameter declaration",
                ));
            }
        }
        let mut program = calc::parse_context(&source.text, source.span.start(), package.clone())?;
        program.origin = source.origin.clone();
        program.origin_offset = 0;
        if let Some(free) = program
            .expressions
            .iter()
            .find(|e| matches!(e.kind, calc::ExprKind::Workspace(_)))
        {
            return Err(problem(
                "TMP001",
                free.span,
                "calc definitions require explicit parameters instead of workspace captures",
            ));
        }
        let shapes = contracts
            .iter()
            .map(|(name, contract)| (name.clone(), contract.shape()))
            .collect();
        let compiled = calc::analyze_with_parameters(
            program.into(),
            calc::Environment {
                catalogue,
                contracts: types,
                workspace: &|_| None,
            },
            &shapes,
        )?;
        // Identity includes source, package semantics and captured contract sources, with
        // length framing. No source is exposed through the digest or inspection reasons.
        let mut digest = Sha256::new();
        for text in std::iter::once(syntax.body.to_string())
            .chain(std::iter::once(
                syntax
                    .parameters
                    .iter()
                    .map(|p| format!("{}: {}", p.name, p.type_expression))
                    .collect::<Vec<_>>()
                    .join(", "),
            ))
            .chain(std::iter::once(
                syntax.output.as_ref().unwrap().text.clone(),
            ))
            .chain(std::iter::once(package.source().to_owned()))
            .chain(types.sources().iter().cloned())
        {
            digest.update((text.len() as u64).to_le_bytes());
            digest.update(text.as_bytes());
        }
        let revision = format!("sha256:{:x}", digest.finalize());
        self.definitions.insert(
            syntax.name.text.clone(),
            Definition {
                parameters: contracts.keys().cloned().collect(),
                contracts,
                syntax,
                calculation: Some(Arc::new(CalculationDefinition {
                    compiled: Arc::new(compiled),
                    output,
                    revision,
                })),
            },
        );
        Ok(())
    }

    pub fn expand(
        &self,
        invocation: &Call,
        occupied: impl Fn(&str) -> bool,
    ) -> Result<Expanded, Diagnostic> {
        let mut call = invocation.clone();
        let mut constraints: IndexMap<String, Vec<Arc<Contract>>> = IndexMap::new();
        let mut seen = IndexSet::new();
        let mut argument_budget = 10_000usize;
        loop {
            if call
                .arguments
                .iter()
                .map(|a| a.value.node_count())
                .sum::<usize>()
                > crate::structured::MAX_NODES
            {
                return Err(problem(
                    "TMP005",
                    invocation.span,
                    "expanded command exceeds its argument node budget",
                ));
            }
            let Some(head) = call.path.first() else {
                return Err(problem("TMP003", call.span, "a command needs a path"));
            };
            if call.marker.is_some() {
                break;
            }
            let Some(definition) = self.definitions.get(&head.text) else {
                break;
            };
            if occupied(&head.text) {
                return Err(problem(
                    "TMP002",
                    invocation.span,
                    format!(
                        "template name also names a provider or meta command: {}",
                        head.text
                    ),
                ));
            }
            if !seen.insert(head.text.clone()) || seen.len() > 64 {
                return Err(problem(
                    "TMP005",
                    invocation.span,
                    "cyclic or overly deep template expansion",
                ));
            }
            if call.path.len() != 1 || !call.operands.is_empty() {
                return Err(problem(
                    "TMP003",
                    invocation.span,
                    "template calls accept named arguments only",
                ));
            }
            let mut actual = IndexMap::new();
            for argument in &call.arguments {
                let key = &argument.key.text;
                if !definition.parameters.contains(key) {
                    return Err(problem(
                        "TMP003",
                        argument.span,
                        format!("unknown template parameter: {key}"),
                    ));
                }
                if actual.insert(key.as_str(), &argument.value).is_some() {
                    return Err(problem(
                        "TMP003",
                        argument.span,
                        format!("duplicate template argument: {key}"),
                    ));
                }
            }
            for parameter in &definition.parameters {
                if !actual.contains_key(parameter.as_str()) {
                    return Err(problem(
                        "TMP003",
                        invocation.span,
                        format!("missing template argument: {parameter}"),
                    ));
                }
            }
            if let Some(calculation) = &definition.calculation {
                for (parameter, contract) in &definition.contracts {
                    constraints
                        .entry(parameter.clone())
                        .or_default()
                        .push(contract.clone());
                }
                return Ok(Expanded {
                    call,
                    contracts: constraints,
                    calculation: Some(calculation.clone()),
                });
            }
            let TemplateBody::Call(body) = &definition.syntax.body else {
                unreachable!("compiled definition");
            };
            let mut arguments = Vec::new();
            let mut next = IndexMap::new();
            for argument in &body.arguments {
                let charge = if let Some(parameter) = placeholder(&argument.value)? {
                    actual[parameter].node_count()
                } else {
                    argument.value.node_count()
                };
                argument_budget = argument_budget.checked_sub(charge).ok_or_else(|| {
                    problem(
                        "TMP005",
                        invocation.span,
                        "template expansion budget exceeded",
                    )
                })?;
                let value = if let Some(parameter) = placeholder(&argument.value)? {
                    let mut checks = constraints.get(parameter).cloned().unwrap_or_default();
                    if let Some(own) = definition.contracts.get(parameter) {
                        checks.push(own.clone());
                    }
                    if !checks.is_empty() {
                        next.insert(argument.key.text.clone(), checks);
                    }
                    (*actual[parameter]).clone()
                } else {
                    relocate(&argument.value, invocation.span)
                };
                arguments.push(Argument {
                    key: Name {
                        text: argument.key.text.clone(),
                        span: invocation.span,
                    },
                    value,
                    span: invocation.span,
                });
            }
            call = Call {
                marker: None,
                path: body
                    .path
                    .iter()
                    .map(|part| Name {
                        text: part.text.clone(),
                        span: invocation.span,
                    })
                    .collect(),
                operands: Vec::new(),
                arguments,
                span: invocation.span,
            };
            constraints = next;
        }
        Ok(Expanded {
            calculation: None,
            call,
            contracts: constraints,
        })
    }
}

fn relocate(value: &Value, span: Span) -> Value {
    let name = Name {
        text: value.name().text.clone(),
        span,
    };
    match value {
        Value::Word(_) => Value::Word(name),
        Value::Text(_) => Value::Text(name),
        Value::Reference(_) => Value::Reference(name),
        Value::Structured(_, structure) => Value::Structured(
            name,
            match structure {
                crate::Structure::Record(fields) => crate::Structure::Record(
                    fields
                        .iter()
                        .map(|(key, value)| {
                            (
                                Name {
                                    text: key.text.clone(),
                                    span,
                                },
                                relocate(value, span),
                            )
                        })
                        .collect(),
                ),
                crate::Structure::List(items) => crate::Structure::List(
                    items.iter().map(|value| relocate(value, span)).collect(),
                ),
            },
        ),
    }
}
fn placeholder(value: &Value) -> Result<Option<&str>, Diagnostic> {
    if let Value::Word(word) = value
        && let Some(name) = word.text.strip_prefix('?')
    {
        identifier(name, word.span)?;
        return Ok(Some(name));
    }
    Ok(None)
}
fn identifier(name: &str, span: Span) -> Result<(), Diagnostic> {
    let mut chars = name.chars();
    if !chars
        .next()
        .is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
        || !chars.all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-'))
    {
        return Err(problem(
            "TMP001",
            span,
            format!("invalid template or parameter name: {name}"),
        ));
    }
    Ok(())
}
fn problem(code: &'static str, span: Span, message: impl Into<String>) -> Diagnostic {
    Diagnostic::error(code, span, message)
}
