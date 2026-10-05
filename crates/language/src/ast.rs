use std::fmt;

use crate::{Span, lexer::whitespace};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Name {
    pub text: String,
    pub span: Span,
}

/// Calculation text retains its original document for runtime source locations.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CalculationSource {
    pub text: String,
    pub span: Span,
    pub origin: std::sync::Arc<crate::SourceText>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Value {
    Word(Name),
    Text(Name),
    Reference(Name),
    Structured(Name, Structure),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Structure {
    Record(Vec<(Name, Value)>),
    List(Vec<Value>),
}

impl Value {
    pub fn name(&self) -> &Name {
        match self {
            Self::Word(n) | Self::Text(n) | Self::Reference(n) | Self::Structured(n, _) => n,
        }
    }
    pub fn node_count(&self) -> usize {
        1 + match self {
            Self::Structured(_, Structure::Record(fields)) => {
                fields.iter().map(|(_, value)| value.node_count()).sum()
            }
            Self::Structured(_, Structure::List(items)) => items.iter().map(Self::node_count).sum(),
            _ => 0,
        }
    }
    pub fn references(&self) -> Vec<&Name> {
        match self {
            Self::Reference(name) => vec![name],
            Self::Structured(_, Structure::Record(fields)) => fields
                .iter()
                .flat_map(|(_, value)| value.references())
                .collect(),
            Self::Structured(_, Structure::List(items)) => {
                items.iter().flat_map(Self::references).collect()
            }
            _ => vec![],
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Argument {
    pub key: Name,
    pub value: Value,
    pub span: Span,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Annotation {
    pub name: Name,
    pub targets: Vec<Name>,
    pub span: Span,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Binding {
    pub name: Name,
    pub span: Span,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Call {
    pub marker: Option<Span>,
    pub path: Vec<Name>,
    pub operands: Vec<Value>,
    pub arguments: Vec<Argument>,
    pub span: Span,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Parameter {
    pub name: String,
    pub type_expression: String,
    pub span: Span,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Template {
    pub name: Name,
    pub parameters: Vec<Parameter>,
    pub output: Option<Name>,
    pub body: TemplateBody,
    pub span: Span,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TemplateBody {
    Call(Call),
    Calculation(CalculationSource),
}
impl fmt::Display for TemplateBody {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Call(call) => call.fmt(f),
            Self::Calculation(source) => f.write_str(&source.text),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum BranchSelector {
    Value,
    Success,
    Failed,
    Cancelled,
    When(Name),
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Branch {
    pub selector: BranchSelector,
    pub body: Statement,
    pub span: Span,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Expression {
    /// An authored program owned by a separate volatile execution context.
    Sandbox(Vec<Statement>),
    Fork(Vec<Branch>),
    /// One declaration unit containing ordered stages. No implicit workspace names.
    Pipeline(Vec<Statement>),
    Call(Call),
    Calculation(CalculationSource),
    Reference(Name),
    Definition(Template),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Statement {
    pub annotations: Vec<Annotation>,
    pub expression: Expression,
    pub binding: Option<Binding>,
    pub error_binding: Option<Binding>,
    pub span: Span,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Script {
    pub statements: Vec<Statement>,
    pub span: Span,
}

/// Encode a command-language quoted literal (not JSON). Newlines remain data, not statements.
pub fn quote_text(text: &str) -> String {
    let mut out = String::with_capacity(text.len() + 2);
    out.push('"');
    for ch in text.chars() {
        match ch {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            ch => out.push(ch),
        }
    }
    out.push('"');
    out
}
fn quote(text: &str, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    f.write_str(&quote_text(text))
}

impl fmt::Display for Value {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Structured(_, Structure::Record(fields)) => {
                f.write_str("{")?;
                for (i, (key, value)) in fields.iter().enumerate() {
                    if i > 0 {
                        f.write_str(", ")?;
                    }
                    write!(f, "{}: {value}", quote_text(&key.text))?;
                }
                f.write_str("}")
            }
            Self::Structured(_, Structure::List(items)) => {
                f.write_str("[")?;
                for (i, value) in items.iter().enumerate() {
                    if i > 0 {
                        f.write_str(", ")?;
                    }
                    value.fmt(f)?;
                }
                f.write_str("]")
            }
            Self::Reference(n) => write!(f, "${}", n.text),
            Self::Text(n) => quote(&n.text, f),
            Self::Word(n)
                if n.text.is_empty()
                    || n.text
                        .chars()
                        .any(|ch| whitespace(ch) || matches!(ch, '>' | '|' | '"' | '\\')) =>
            {
                quote(&n.text, f)
            }
            Self::Word(n) => f.write_str(&n.text),
        }
    }
}

impl fmt::Display for Call {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.marker.is_some() {
            f.write_str(":")?;
        }
        for (i, name) in self.path.iter().enumerate() {
            if i > 0 {
                f.write_str(" ")?;
            }
            f.write_str(&name.text)?;
        }
        for operand in &self.operands {
            write!(f, " {operand}")?;
        }
        for argument in &self.arguments {
            write!(f, " {}:{}", argument.key.text, argument.value)?;
        }
        Ok(())
    }
}

impl fmt::Display for Statement {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for annotation in &self.annotations {
            write!(f, "@{}", annotation.name.text)?;
            if !annotation.targets.is_empty() {
                f.write_str(if annotation.name.text == "trace" {
                    "("
                } else {
                    "{"
                })?;
                for (i, target) in annotation.targets.iter().enumerate() {
                    if i > 0 {
                        f.write_str(", ")?;
                    }
                    f.write_str(&target.text)?;
                }
                f.write_str(if annotation.name.text == "trace" {
                    ")"
                } else {
                    "}"
                })?;
            }
            f.write_str(" ")?;
        }
        match &self.expression {
            Expression::Sandbox(statements) => {
                f.write_str(":sandbox {\n")?;
                for statement in statements {
                    writeln!(f, "{statement}")?;
                }
                f.write_str("}")?;
            }
            Expression::Fork(branches) => {
                f.write_str(":fork {")?;
                for branch in branches {
                    match &branch.selector {
                        BranchSelector::Value => f.write_str(" ")?,
                        BranchSelector::Success => f.write_str(" on success ")?,
                        BranchSelector::Failed => f.write_str(" on failed ")?,
                        BranchSelector::Cancelled => f.write_str(" on cancelled ")?,
                        BranchSelector::When(name) => write!(f, " when {} ", name.text)?,
                    }
                    write!(f, "{{ {} }}", branch.body)?;
                }
                f.write_str(" }")?;
            }
            Expression::Pipeline(stages) => {
                for (i, stage) in stages.iter().enumerate() {
                    if i > 0 {
                        f.write_str(" | ")?;
                    }
                    stage.fmt(f)?;
                }
            }
            Expression::Call(call) => call.fmt(f)?,
            Expression::Calculation(source) => f.write_str(&source.text)?,
            Expression::Reference(n) => write!(f, "${}", n.text)?,
            Expression::Definition(template) => {
                write!(f, ":def {}", template.name.text)?;
                if !template.parameters.is_empty() {
                    f.write_str("(")?;
                    for (i, p) in template.parameters.iter().enumerate() {
                        if i > 0 {
                            f.write_str(", ")?;
                        }
                        write!(f, "{}: {}", p.name, p.type_expression)?;
                    }
                    f.write_str(")")?;
                }
                if let Some(output) = &template.output {
                    write!(f, " -> {}", output.text)?;
                }
                write!(f, " as {}", template.body)?;
            }
        }
        if let Some(binding) = &self.binding {
            write!(f, " > {}", binding.name.text)?;
        }
        if let Some(binding) = &self.error_binding {
            write!(f, " *> {}", binding.name.text)?;
        }
        Ok(())
    }
}

impl fmt::Display for Script {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for (i, statement) in self.statements.iter().enumerate() {
            if i > 0 {
                f.write_str("\n")?;
            }
            statement.fmt(f)?;
        }
        Ok(())
    }
}

impl Statement {
    /// Includes nested branch work so admission limits cannot be evaded by grouping.
    pub fn stage_count(&self) -> usize {
        match &self.expression {
            Expression::Sandbox(statements) => {
                1 + statements.iter().map(Self::stage_count).sum::<usize>()
            }
            Expression::Pipeline(stages) => stages.iter().map(Self::stage_count).sum(),
            Expression::Fork(branches) => {
                1 + branches
                    .iter()
                    .map(|b| {
                        b.body.stage_count()
                            + usize::from(matches!(b.selector, BranchSelector::When(_)))
                    })
                    .sum::<usize>()
            }
            _ => 1,
        }
    }
}
