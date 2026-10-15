//! Declarative executable providers. Compilation, target execution and output decoding are
//! separate boundaries; definitions contain no shell programs or ambient provider references.
mod importer;
mod invocation;
mod schema;
mod session;
#[cfg(test)]
mod tests;
pub use importer::ProviderImporter;
pub(crate) use importer::{FORMAT, capture_file, captured_executable};
pub(crate) use invocation::build;

use std::sync::Arc;
use wes_core::{capability::ProviderDescription, contracts::Contract};

#[derive(Clone)]
pub(crate) struct Definition {
    description: ProviderDescription,
    executable: String,
    operations: Vec<Operation>,
    session: Option<Vec<String>>,
}
#[derive(Clone)]
struct Operation {
    path: Vec<String>,
    parameters: Vec<(String, Arc<Contract>, bool)>,
    argv: Vec<schema::Argument>,
    result: Option<Arc<Contract>>,
    format: schema::OutputFormat,
    success_codes: Vec<i64>,
    streaming: bool,
    control: Option<Control>,
}
#[derive(Clone)]
struct Control {
    input: Arc<Contract>,
    output: Arc<Contract>,
    policy: wes_core::flow::OutputPolicy,
}

#[derive(Clone, Copy, Debug, thiserror::Error)]
#[error("invalid provider definition: {0}")]
pub struct DefinitionError(pub &'static str);
