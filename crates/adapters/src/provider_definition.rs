//! Declarative executable providers. Compilation, target execution and output decoding are
//! separate boundaries; definitions contain no shell programs or ambient provider references.
mod finite_profile;
mod fixed_arguments;
mod importer;
mod invocation;
mod record_bytes;
mod schema;
mod session;
mod source_profile;
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
    read_domain: Option<wes_engine::provider_reads::ReadDomain>,
}
#[derive(Clone)]
struct Operation {
    path: Vec<String>,
    parameters: Vec<(String, Arc<Contract>, bool)>,
    argv: Vec<schema::Argument>,
    result: Option<Arc<Contract>>,
    format: schema::OutputFormat,
    byte_field: Option<Vec<String>>,
    success_codes: Vec<i64>,
    streaming: bool,
    control: Option<Control>,
    recording: Option<Recording>,
    byte_input: Option<wes_engine::byte_input::ByteInputSpec>,
    exclusive: Vec<Exclusive>,
    fixed_arguments: indexmap::IndexMap<String, wes_core::Value>,
    source_profile: Option<Arc<wes_engine::providers::profiles::SourceProfile>>,
    finite_profile: Option<Arc<wes_engine::providers::finite_profiles::FiniteProfile>>,
}
#[derive(Clone)]
struct Exclusive {
    namespace: String,
    fields: Vec<Vec<String>>,
}
#[derive(Clone)]
struct Control {
    input: Arc<Contract>,
    output: Arc<Contract>,
    policy: wes_core::flow::OutputPolicy,
}

#[derive(Clone, Debug, thiserror::Error)]
#[error("invalid provider definition: {message}")]
pub struct DefinitionError {
    message: std::borrow::Cow<'static, str>,
}
impl DefinitionError {
    fn new(message: impl Into<std::borrow::Cow<'static, str>>) -> Self {
        Self {
            message: message.into(),
        }
    }
}
impl From<DefinitionError> for wes_engine::imports::ImportError {
    fn from(error: DefinitionError) -> Self {
        Self::Declaration(error.message.into_owned())
    }
}

#[derive(Clone)]
struct Recording {
    contract: Arc<Contract>,
    fields: Vec<String>,
    maximum: u64,
    duration: std::time::Duration,
}
