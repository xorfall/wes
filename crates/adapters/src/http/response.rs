//! Finite response interpretation. Receiving a response and validating its body are separate facts.
use super::{inspection, transport};
use crate::codec::{Limits, decode_json_for_contract, decode_json_preserving};
use wes_core::contracts::Contract;
use wes_core::{Data, Primitive, Provenance, RecordShape, Shape, ValidationIssue, Value};
use wes_engine::trace::{integer, record, text};

pub(super) enum Expectation<'a> {
    Contract(&'a Contract),
    Empty,
}
impl Expectation<'_> {
    fn decode(&self, bytes: &[u8], limits: Limits) -> Option<Value> {
        match self {
            Self::Contract(c) => decode_json_for_contract(bytes, limits, c)
                .ok()
                .map(|d| value(Shape::Unknown, d)),
            Self::Empty => None,
        }
    }
    fn check(&self, body: &Value) -> Vec<ValidationIssue> {
        match self {
            Self::Contract(c) => c.issues(body.data()),
            _ => vec![issue(
                "HTTP_BODY_CONTRACT",
                "Body does not satisfy the documented response shape",
            )],
        }
    }
    fn shape(&self) -> Shape {
        match self {
            Self::Contract(c) => c.shape(),
            Self::Empty => Shape::Option(Box::new(Shape::Unknown)),
        }
    }
}

pub(crate) fn shape() -> Shape {
    Shape::Record(
        RecordShape::new(
            "HttpResponse",
            [
                ("status".into(), Shape::Primitive(Primitive::Int)),
                ("version".into(), Shape::Primitive(Primitive::Text)),
                ("headers".into(), inspection::header_shape()),
                ("body".into(), Shape::Unknown),
                ("bodyKind".into(), Shape::Primitive(Primitive::Text)),
                ("originalBody".into(), Shape::Primitive(Primitive::Bytes)),
                (
                    "validation".into(),
                    validation("undocumented", vec![]).shape().clone(),
                ),
            ],
        )
        .expect("HTTP response shape"),
    )
}
fn value(shape: Shape, data: Data) -> Value {
    Value::new(shape, data, Provenance::default()).expect("response reading shape")
}
fn issue(code: &str, message: &str) -> ValidationIssue {
    ValidationIssue {
        path: "/body".into(),
        code: code.into(),
        message: message.into(),
    }
}
fn validation(state: &str, issues: Vec<ValidationIssue>) -> Value {
    let issue_shape = record(
        "HttpResponseIssue",
        [
            ("path".into(), text("")),
            ("code".into(), text("")),
            ("message".into(), text("")),
        ],
        Provenance::default(),
    )
    .shape()
    .clone();
    let rows = issues
        .into_iter()
        .take(100)
        .map(|i| {
            record(
                "HttpResponseIssue",
                [
                    ("path".into(), text(i.path)),
                    ("code".into(), text(i.code)),
                    ("message".into(), text(i.message)),
                ],
                Provenance::default(),
            )
            .data()
            .clone()
        })
        .collect();
    record(
        "HttpResponseValidation",
        [
            ("state".into(), text(state)),
            (
                "issues".into(),
                value(Shape::List(Box::new(issue_shape)), Data::List(rows)),
            ),
        ],
        Provenance::default(),
    )
}

pub(super) fn envelope(
    response: transport::Response,
    contract: Option<Expectation<'_>>,
    limits: Limits,
) -> Value {
    // Bytes are shared between the original and an undecoded body; never re-serialize as evidence.
    let original = value(
        Shape::Primitive(Primitive::Bytes),
        Data::Bytes(response.body.into()),
    );
    let Data::Bytes(bytes) = original.data() else {
        unreachable!()
    };
    let media = response
        .headers
        .get_all(reqwest::header::CONTENT_TYPE)
        .iter()
        .map(|h| h.to_str().unwrap_or("").trim().to_ascii_lowercase())
        .collect::<Vec<_>>();
    let ambiguous = media.windows(2).any(|w| w[0] != w[1]);
    let media = media.first().map(String::as_str).unwrap_or("");
    let essence = media.split(';').next().unwrap_or("").trim();
    let encoded = response
        .headers
        .get_all(reqwest::header::CONTENT_ENCODING)
        .iter()
        .any(|h| {
            h.to_str()
                .unwrap_or("unsupported")
                .split(',')
                .any(|e| !matches!(e.trim().to_ascii_lowercase().as_str(), "" | "identity"))
        });
    let charset_supported = media.split(';').skip(1).all(|p| {
        let Some((name, charset)) = p.trim().split_once('=') else {
            return true;
        };
        name.trim() != "charset"
            || matches!(charset.trim().trim_matches('"'), "utf-8" | "utf8")
            || (charset.trim().trim_matches('"') == "us-ascii" && bytes.is_ascii())
    });
    let json = essence == "application/json"
        || (essence.starts_with("application/") && essence.ends_with("+json"));
    let mut problems = Vec::new();
    let mut contract_reading = false;
    let (mut body, kind) = if bytes.is_empty() {
        (
            value(Shape::Option(Box::new(Shape::Unknown)), Data::Option(None)),
            "empty",
        )
    } else if ambiguous || encoded || !charset_supported {
        problems.push(issue("HTTP_BODY_ENCODING", "Body retained as bytes: conflicting media types or unsupported content encoding/charset"));
        (original.clone(), "bytes")
    } else if json {
        // Contract-directed interpretation preserves Decimal and optional/union semantics.
        let typed = contract.as_ref().and_then(|c| c.decode(bytes, limits));
        contract_reading = typed.is_some();
        let decoded = typed.or_else(|| {
            decode_json_preserving(bytes, limits)
                .ok()
                .map(|d| value(Shape::Unknown, d))
        });
        match decoded {
            Some(data) => (data, "json"),
            None => {
                problems.push(issue("HTTP_BODY_JSON", "JSON body is malformed, ambiguous, or exceeds the parsing budget; original bytes retained"));
                (original.clone(), "bytes")
            }
        }
    } else if essence.starts_with("text/") {
        match std::str::from_utf8(bytes) {
            Ok(s) => (text(s), "text"),
            Err(_) => {
                problems.push(issue(
                    "HTTP_BODY_TEXT",
                    "Body is not valid UTF-8; original bytes retained",
                ));
                (original.clone(), "bytes")
            }
        }
    } else {
        (original.clone(), "bytes")
    };
    let mut captured_meta = None;
    let state = if !problems.is_empty() {
        "unreadable"
    } else if let Some(expected) = contract {
        if matches!(expected, Expectation::Empty) {
            if bytes.is_empty() {
                "validated"
            } else {
                problems.push(issue(
                    "HTTP_BODY_UNEXPECTED",
                    "The documented response has no body, but a body was received",
                ));
                "mismatch"
            }
        } else {
            problems = expected.check(&body);
            if bytes.is_empty() || (json && !contract_reading && problems.is_empty()) {
                problems.push(issue(
                    "HTTP_BODY_CONTRACT",
                    "Body has no unique interpretation under the documented response contract",
                ));
            }
            if problems.is_empty() {
                body = body
                    .with_shape(expected.shape())
                    .expect("validated contract");
                if let Expectation::Contract(c) = &expected {
                    captured_meta = Some((*c).clone());
                }
                "validated"
            } else {
                "mismatch"
            }
        }
    } else {
        "undocumented"
    };
    let envelope = record(
        "HttpResponse",
        [
            ("status".into(), integer(response.status.into())),
            ("version".into(), text(response.version)),
            (
                "headers".into(),
                inspection::headers(&response.headers, None),
            ),
            ("body".into(), body),
            ("bodyKind".into(), text(kind)),
            ("originalBody".into(), original),
            ("validation".into(), validation(state, problems)),
        ],
        Provenance::default(),
    );
    envelope.with_metadata(captured_meta.map(|c| {
        wes_core::contracts::metadata::ValueMetadata::record_wrapper("HttpResponse", "body", &c)
    }))
}

#[cfg(test)]
mod tests;
