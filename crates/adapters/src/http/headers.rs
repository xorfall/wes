//! Ordinary typed HTTP response headers.
use reqwest::header::HeaderMap;
use wes_core::{Data, Provenance, Shape, Value};
use wes_engine::values::{record, text};
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
pub(super) fn headers(headers: &HeaderMap) -> Value {
    let rows = headers
        .iter()
        .map(|(name, value)| {
            let value = String::from_utf8_lossy(value.as_bytes()).into_owned();
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
        .collect();
    Value::new(header_shape(), Data::List(rows), Provenance::default()).expect("headers")
}
