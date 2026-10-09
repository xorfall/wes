use wes_core::{Data, Provenance, Shape, Value, contracts::Contract};
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
        self.json_input(text.as_bytes(), contract, token, span, false)
    }
    fn decode(
        &self,
        bytes: &[u8],
        contract: Option<&Contract>,
        token: &CancellationToken,
        span: Span,
    ) -> Result<Value, Failure> {
        self.json_input(bytes, contract, token, span, true)
    }
}
impl CalculationServices {
    fn json_input(
        &self,
        bytes: &[u8],
        contract: Option<&Contract>,
        token: &CancellationToken,
        span: Span,
        diagnostic: bool,
    ) -> Result<Value, Failure> {
        if token.is_cancelled() {
            return Err(Failure::cancelled(span));
        }
        let limits = super::Limits {
            bytes: 1024 * 1024,
            nodes: 100_000,
        };
        let decoded = match contract {
            Some(contract) => super::decode_json_for_contract(bytes, limits, contract),
            None => super::decode_json_preserving(bytes, limits),
        };
        if token.is_cancelled() {
            return Err(Failure::cancelled(span));
        }
        let data = match decoded {
            Ok(data) => data,
            Err(error) if diagnostic => {
                let detail = match error {
                    super::CodecError::Json(ref json) if !json.is_io() => Some((
                        "JSON001",
                        "Invalid JSON input",
                        Some((json.line(), json.column())),
                    )),
                    super::CodecError::Contract => Some((
                        "JSON002",
                        "JSON does not satisfy the selected contract",
                        None,
                    )),
                    super::CodecError::AmbiguousContract => {
                        Some(("JSON003", "JSON has ambiguous native interpretations", None))
                    }
                    _ => None,
                };
                if let Some(detail) = detail {
                    return decode_result(None, contract, Some(detail), span);
                }
                return Err(json_failure(error, span));
            }
            Err(error) => return Err(json_failure(error, span)),
        };
        let shape = if let Some(contract) = contract {
            let issues = contract
                .issues_with_cancel(&data, &|| token.is_cancelled())
                .map_err(|_| Failure::cancelled(span))?;
            if issues.iter().any(|issue| issue.code == "TYP006") {
                return Err(Failure::new(
                    "CAL006",
                    span,
                    "JSON contract validation limit reached",
                ));
            }
            if !issues.is_empty() {
                if diagnostic {
                    return decode_result(
                        None,
                        Some(contract),
                        Some((
                            "JSON002",
                            "JSON does not satisfy the selected contract",
                            None,
                        )),
                        span,
                    );
                }
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
        let value = Value::new(shape, data, Provenance::default())
            .map_err(|_| Failure::new("CAL004", span, "invalid JSON value"))?;
        if diagnostic {
            decode_result(Some(value), contract, None, span)
        } else {
            Ok(value)
        }
    }
}

fn json_failure(error: super::CodecError, span: Span) -> Failure {
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
        super::CodecError::Bytes | super::CodecError::Work | super::CodecError::Depth => "CAL006",
        _ => wes_language::calc::diagnostics::Category::Parse.code(),
    };
    Failure::new(code, span, format!("JSON input: {bounded}"))
}

type JsonDiagnostic = (&'static str, &'static str, Option<(usize, usize)>);
fn decode_result(
    value: Option<Value>,
    contract: Option<&Contract>,
    error: Option<JsonDiagnostic>,
    span: Span,
) -> Result<Value, Failure> {
    let shape =
        wes_language::calc::json_decode_shape(contract.map_or(Shape::Unknown, Contract::shape));
    let position =
        |value: usize| Data::Option(i64::try_from(value).ok().map(|v| Box::new(Data::Int(v))));
    let detail = error.map(|(code, message, at)| {
        Box::new(Data::Record(
            [
                ("code".into(), Data::Text(code.into())),
                ("message".into(), Data::Text(message.into())),
                (
                    "line".into(),
                    at.map_or(Data::Option(None), |p| position(p.0)),
                ),
                (
                    "column".into(),
                    at.map_or(Data::Option(None), |p| position(p.1)),
                ),
            ]
            .into_iter()
            .collect(),
        ))
    });
    Value::new(
        shape,
        Data::Record(
            [
                ("ok".into(), Data::Bool(value.is_some())),
                (
                    "value".into(),
                    Data::Option(value.map(|v| Box::new(v.data().clone()))),
                ),
                ("error".into(), Data::Option(detail)),
            ]
            .into_iter()
            .collect(),
        ),
        Provenance::default(),
    )
    .map_err(|_| Failure::new("CAL004", span, "invalid JSON decode result"))
}
