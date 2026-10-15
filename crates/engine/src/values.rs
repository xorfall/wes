//! Materialized typed values shared by native contracts and adapters.
use indexmap::IndexMap;
use wes_core::{Data, Primitive, Provenance, RecordShape, Shape, Value};

/// Construct a materialized typed record without registering protocol-specific core primitives.
pub fn record(
    name: &str,
    fields: impl IntoIterator<Item = (String, Value)>,
    provenance: Provenance,
) -> Value {
    let fields: IndexMap<_, _> = fields.into_iter().collect();
    let shape = Shape::Record(
        RecordShape::new(
            name,
            fields.iter().map(|(k, v)| (k.clone(), v.shape().clone())),
        )
        .expect("unique record fields"),
    );
    let provenance = fields
        .values()
        .fold(provenance, |p, v| p.inheriting(v.provenance()));
    Value::new(
        shape,
        Data::Record(
            fields
                .into_iter()
                .map(|(k, v)| (k, v.data().clone()))
                .collect(),
        ),
        provenance,
    )
    .expect("record shape")
}
pub fn text(value: impl Into<String>) -> Value {
    Value::new(
        Shape::Primitive(Primitive::Text),
        Data::Text(value.into().into()),
        Provenance::default(),
    )
    .expect("text")
}
pub fn integer(value: i64) -> Value {
    Value::new(
        Shape::Primitive(Primitive::Int),
        Data::Int(value),
        Provenance::default(),
    )
    .expect("integer")
}
