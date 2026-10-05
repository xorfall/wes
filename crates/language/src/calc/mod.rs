//! Versioned calculation language definitions. Analysis owns no execution or I/O.
mod analysis;
mod ast;
pub mod diagnostics;
pub use analysis::{
    CallSelection, Compiled, Environment, FunctionSymbols, PurityAnalysis, Resolution, Symbol,
    analyze, analyze_with_parameters,
};
mod documentation;
mod lexer;
mod package;
pub use documentation::OperationHelp;
mod parser;
pub use ast::*;
pub use lexer::context_end;
pub use package::{Binary, BinarySpec, Keyword, Operation, OperationSpec, Package};
pub use parser::{parse_body, parse_context};

pub const DEFAULT_PACKAGE: &str = include_str!("default.yaml");
mod flow;
mod purity;

mod collections;
pub use collections::{merge_collection_shapes, merge_inferred_shapes};
