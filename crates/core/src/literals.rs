use crate::{Data, Primitive, Shape, numeric};

/// Contextual reading only. Existing values never pass through this textual conversion.
pub fn read(text: &str, expected: &Shape) -> Option<Data> {
    match expected {
        Shape::Unknown | Shape::Primitive(Primitive::Text) => Some(Data::Text(text.into())),
        Shape::Primitive(Primitive::Bytes) => Some(Data::Bytes(text.as_bytes().into())),
        Shape::Primitive(Primitive::Int) => numeric::integer(text).map(Data::Int),
        Shape::Primitive(Primitive::Decimal) => text.parse().ok().map(Data::Decimal),
        Shape::Primitive(Primitive::Bool) => match text {
            "true" => Some(Data::Bool(true)),
            "false" => Some(Data::Bool(false)),
            _ => None,
        },
        Shape::Primitive(Primitive::Instant) => text.parse().ok().map(Data::Instant),
        Shape::Primitive(Primitive::Duration) => text.parse().ok().map(Data::Duration),
        Shape::Primitive(Primitive::Interval) => text.parse().ok().map(Data::Interval),
        Shape::Meta(_)
        | Shape::Record(_)
        | Shape::List(_)
        | Shape::Option(_)
        | Shape::Iter(_)
        | Shape::Dataset(_) => None,
    }
}
