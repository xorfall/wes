//! HTTP vocabulary and previews live above the generic observation collector.
use super::auth::Redactor;
use reqwest::{Request, Url, header::HeaderMap};
use wes_core::{Data, Primitive, Provenance, Shape, Value};
use wes_engine::trace::{TraceSink, integer, record, text};

pub(super) fn header_shape() -> Shape {
    Shape::List(Box::new(
        record(
            "HttpHeader",
            [("name".into(), text("")), ("value".into(), text(""))],
            Provenance::default(),
        )
        .shape()
        .clone(),
    ))
}
pub(super) fn headers(headers: &HeaderMap, redactor: Option<&Redactor>) -> Value {
    let rows = headers
        .iter()
        .map(|(name, value)| {
            let safe = matches!(
                name.as_str(),
                "content-type"
                    | "content-length"
                    | "content-encoding"
                    | "transfer-encoding"
                    | "accept"
                    | "cache-control"
                    | "connection"
                    | "allow"
            );
            let value = match redactor {
                None => String::from_utf8_lossy(value.as_bytes()).into_owned(),
                Some(redactor) if safe => redactor.excerpt(value.as_bytes(), 512),
                Some(_) => "[REDACTED]".into(),
            };
            record(
                "HttpHeader",
                [
                    ("name".into(), text(name.as_str())),
                    ("value".into(), text(value)),
                ],
                Provenance::default(),
            )
            .data()
            .clone()
        })
        .take(if redactor.is_some() { 64 } else { usize::MAX })
        .collect();
    Value::new(header_shape(), Data::List(rows), Provenance::default()).expect("headers")
}
fn url(url: &Url, redactor: &Redactor) -> String {
    let mut shown = url.clone();
    shown.set_fragment(None);
    if url.query().is_some() {
        shown.set_query(None);
        let mut query = shown.query_pairs_mut();
        for (name, _) in url.query_pairs().take(32) {
            query.append_pair(&name, "[REDACTED]");
        }
    }
    redactor.excerpt(shown.as_str().as_bytes(), 2048)
}
pub(super) fn body(bytes: &[u8], redactor: &Redactor) -> Value {
    let preview = match std::str::from_utf8(bytes) {
        Ok(text)
            if !text
                .chars()
                .any(|c| c.is_control() && !matches!(c, '\n' | '\r' | '\t')) =>
        {
            redactor.excerpt(bytes, 4096)
        }
        _ => "[binary body]".into(),
    };
    record(
        "HttpBodyPreview",
        [
            ("bytes".into(), integer(bytes.len() as i64)),
            ("preview".into(), text(preview)),
            (
                "complete".into(),
                Value::new(
                    Shape::Primitive(Primitive::Bool),
                    Data::Bool(bytes.len() <= 4096),
                    Provenance::default(),
                )
                .expect("bool"),
            ),
        ],
        Provenance::default(),
    )
}
pub(super) fn request(trace: &TraceSink, request: &Request, redactor: &Redactor) {
    let mut fields = vec![
        ("method".into(), text(request.method().as_str())),
        ("url".into(), text(url(request.url(), redactor))),
        (
            "headerCount".into(),
            integer(request.headers().len() as i64),
        ),
        ("headers".into(), headers(request.headers(), Some(redactor))),
    ];
    if let Some(bytes) = request.body().and_then(|b| b.as_bytes()) {
        fields.push(("body".into(), body(bytes, redactor)));
    }
    trace.emit(
        "http.request",
        record("HttpRequest", fields, Provenance::default()),
    );
}
pub(super) fn response(trace: &TraceSink, response: &reqwest::Response, redactor: &Redactor) {
    trace.emit(
        "http.response",
        record(
            "HttpResponseHead",
            [
                ("status".into(), integer(response.status().as_u16().into())),
                (
                    "headerCount".into(),
                    integer(response.headers().len() as i64),
                ),
                ("version".into(), text(format!("{:?}", response.version()))),
                ("url".into(), text(url(response.url(), redactor))),
                (
                    "headers".into(),
                    headers(response.headers(), Some(redactor)),
                ),
            ],
            Provenance::default(),
        ),
    );
}
