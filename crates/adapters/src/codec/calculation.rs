use wes_core::{Provenance, Shape, Value, contracts::Contract};
use wes_engine::{
    calc::{Failure, LocalServices},
    driver::CancellationToken,
};
use wes_language::Span;
#[derive(Debug)]
pub struct CalculationServices;
impl LocalServices for CalculationServices {
    fn http(
        &self,
        operation: wes_engine::calc::HttpOperation,
        input: &Value,
        token: &CancellationToken,
        span: Span,
    ) -> Result<Value, Failure> {
        use wes_engine::{calc::HttpOperation, calc::LocalError};
        let operation = match operation {
            HttpOperation::Status => crate::http::HttpFunction::Status,
            HttpOperation::Error => crate::http::HttpFunction::Error,
            HttpOperation::Catalogue => crate::http::HttpFunction::Catalogue,
            HttpOperation::Analysis => crate::http::HttpFunction::Analysis,
        };
        operation
            .evaluate(input, token)
            .map_err(|error| match error {
                LocalError::Cancelled => Failure::cancelled(span),
                LocalError::Failed(error) => Failure {
                    cause: Some(Box::new(error.clone())),
                    ..Failure::new("CAL004", span, error.message())
                },
            })
    }

    fn read(
        &self,
        text: &str,
        contract: Option<&Contract>,
        token: &CancellationToken,
        span: Span,
    ) -> Result<Value, Failure> {
        if token.is_cancelled() {
            return Err(Failure::cancelled(span));
        }
        let limits = super::Limits {
            bytes: 1024 * 1024,
            nodes: 100_000,
        };
        let data = match contract {
            Some(contract) => super::decode_json_for_contract(text.as_bytes(), limits, contract),
            None => super::decode_json_preserving(text.as_bytes(), limits),
        }
        .map_err(|error| {
            let message = error.to_string();
            let mut chars = message.chars();
            let mut bounded = chars.by_ref().take(1024).collect::<String>();
            if chars.next().is_some() {
                bounded.push('…');
                if let super::CodecError::Json(json) = &error {
                    bounded.push_str(&format!(
                        " at JSON line {}, column {}",
                        json.line(),
                        json.column()
                    ));
                }
            }
            let code = match error {
                super::CodecError::Contract | super::CodecError::AmbiguousContract => {
                    wes_language::calc::diagnostics::Category::Contract.code()
                }
                super::CodecError::Bytes | super::CodecError::Work | super::CodecError::Depth => {
                    "CAL006"
                }
                _ => wes_language::calc::diagnostics::Category::Parse.code(),
            };
            Failure::new(code, span, format!("JSON input: {bounded}"))
        })?;
        let shape = if let Some(contract) = contract {
            let issues = contract
                .issues_with_cancel(&data, &|| token.is_cancelled())
                .map_err(|_| Failure::cancelled(span))?;
            if !issues.is_empty() {
                return Err(Failure {
                    issues,
                    ..Failure::new(
                        wes_language::calc::diagnostics::Category::Contract.code(),
                        span,
                        "JSON does not satisfy the selected contract",
                    )
                });
            }
            contract.shape()
        } else {
            Shape::Unknown
        };
        if token.is_cancelled() {
            return Err(Failure::cancelled(span));
        }
        Value::new(shape, data, Provenance::default())
            .map_err(|_| Failure::new("CAL004", span, "invalid JSON value"))
    }
}
