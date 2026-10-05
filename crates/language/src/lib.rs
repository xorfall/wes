//! Command syntax and analysis. Parsing never executes a provider.

mod ast;
mod diagnostic;
mod lexer;
mod parser;
mod source;
pub mod structured;

pub use ast::{
    Annotation, Argument, Binding, Branch, BranchSelector, CalculationSource, Call, Expression,
    Name, Parameter, Script, Statement, Structure, Template, TemplateBody, Value, quote_text,
};
pub use diagnostic::{Diagnostic, Severity};
pub use lexer::{Lexed, Token, TokenKind, bare_named_text, binding_name, lex};
pub use parser::{Parsed, parse, parse_with_calculation};
pub use source::{Position, SourceError, SourceText, Span, portable_source_name};
pub mod calc;
pub mod check;
pub mod resolve;
pub mod templates;
pub mod vocabulary;

pub mod targets;
