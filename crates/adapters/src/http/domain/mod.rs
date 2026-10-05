//! Pure HTTP definitions and analysis. Views are thin entry points, never network clients.
mod analysis;
mod catalogue;
#[cfg(test)]
mod tests;

use indexmap::IndexMap;
use wes_core::{Data, ErrorId, ErrorValue, Primitive, Provenance, Shape, Value};
use wes_engine::{
    calc::LocalError,
    driver::CancellationToken,
    trace::{integer, record, text},
};

#[derive(Clone, Copy)]
pub enum HttpFunction {
    Status,
    Error,
    Catalogue,
    Analysis,
}
impl HttpFunction {
    pub fn evaluate(
        &self,
        input: &Value,
        cancellation: &CancellationToken,
    ) -> Result<Value, LocalError> {
        let result = (|| {
            if cancellation.is_cancelled() {
                return Err(LocalError::Cancelled);
            }
            // Bound traversal and encoded content before interpreting caller-controlled structures.
            crate::codec::encode_json(
                input.data(),
                crate::codec::Limits {
                    bytes: 128 * 1024,
                    nodes: 8192,
                },
            )
            .map_err(|_| {
                failure(
                    "HTTPD002",
                    "HTTP domain input exceeds its size or work budget",
                )
            })?;
            if cancellation.is_cancelled() {
                return Err(LocalError::Cancelled);
            }
            match self {
                Self::Status => catalogue::status_input(input.data()),
                Self::Error => catalogue::error_input(input.data()),
                Self::Catalogue => catalogue::catalogue(input.data()),
                Self::Analysis => analysis::analyze(input.data(), cancellation),
            }
        })();
        match result {
            Ok(value) => {
                Ok(value.with_provenance(value.provenance().inheriting(input.provenance())))
            }
            Err(LocalError::Failed(error)) => Err(LocalError::Failed(
                error.with_policy(input.provenance().policy()),
            )),
            Err(error) => Err(error),
        }
    }
}
fn failure(code: &str, message: &str) -> LocalError {
    LocalError::Failed(
        ErrorValue::new(
            ErrorId::new(uuid::Uuid::new_v4().to_string()).expect("UUID"),
            code,
            message,
            vec![],
            None,
        )
        .expect("domain error"),
    )
}
fn invalid(message: &str) -> LocalError {
    failure("HTTPD001", message)
}
fn boolean(value: bool) -> Value {
    Value::new(
        Shape::Primitive(Primitive::Bool),
        Data::Bool(value),
        Provenance::default(),
    )
    .expect("bool")
}
fn list(shape: Shape, values: impl IntoIterator<Item = Value>) -> Value {
    let values: Vec<_> = values.into_iter().collect();
    let provenance = values
        .iter()
        .fold(Provenance::default(), |p, v| p.inheriting(v.provenance()));
    Value::new(
        Shape::List(Box::new(shape)),
        Data::List(values.into_iter().map(|v| v.data().clone()).collect()),
        provenance,
    )
    .expect("typed list")
}
