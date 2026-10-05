//! Collection operations share structural compatibility; they never coerce element values.
use wes_core::{RecordShape, Shape};

/// Unknown permits deferred checking in analysis; at runtime it is supplied only for empty
/// collections or an absent Option element, after inspecting the actual finite values.
pub fn merge_collection_shapes(left: &Shape, right: &Shape) -> Option<Shape> {
    match (left, right) {
        (Shape::Unknown, other) | (other, Shape::Unknown) => Some(other.clone()),
        (Shape::List(a), Shape::List(b)) => {
            Some(Shape::List(Box::new(merge_collection_shapes(a, b)?)))
        }
        (Shape::Option(a), Shape::Option(b)) => {
            Some(Shape::Option(Box::new(merge_collection_shapes(a, b)?)))
        }
        (Shape::Record(a), Shape::Record(b)) if a.fields().len() == b.fields().len() => {
            let fields = a
                .fields()
                .map(|(name, shape)| {
                    Some((
                        name.to_owned(),
                        merge_collection_shapes(shape, b.field(name)?)?,
                    ))
                })
                .collect::<Option<Vec<_>>>()?;
            Some(Shape::Record(
                RecordShape::new(if a.name() == b.name() { a.name() } else { "" }, fields).ok()?,
            ))
        }
        (a, b) if a == b => Some(a.clone()),
        _ => None,
    }
}

/// Literal/result inference cannot treat a heterogeneous List<Unknown> as an empty
/// list. Only the absent Option element is a hole; concat verifies its finite values.
pub fn merge_inferred_shapes(left: &Shape, right: &Shape) -> Option<Shape> {
    match (left, right) {
        (Shape::Option(a), Shape::Option(b)) => {
            let inner = match (a.as_ref(), b.as_ref()) {
                (Shape::Unknown, other) | (other, Shape::Unknown) => Some(other.clone()),
                (a, b) => merge_inferred_shapes(a, b),
            }?;
            Some(Shape::Option(Box::new(inner)))
        }
        (Shape::List(a), Shape::List(b)) => {
            Some(Shape::List(Box::new(merge_inferred_shapes(a, b)?)))
        }
        (Shape::Record(a), Shape::Record(b)) if a.fields().len() == b.fields().len() => {
            let fields = a
                .fields()
                .map(|(name, shape)| {
                    Some((
                        name.to_owned(),
                        merge_inferred_shapes(shape, b.field(name)?)?,
                    ))
                })
                .collect::<Option<Vec<_>>>()?;
            Some(Shape::Record(
                RecordShape::new(if a.name() == b.name() { a.name() } else { "" }, fields).ok()?,
            ))
        }
        (a, b) if a == b => Some(a.clone()),
        _ => None,
    }
}
