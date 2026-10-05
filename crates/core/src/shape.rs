use std::fmt;

use indexmap::IndexMap;

use crate::ModelError;

/// Closed scalar shapes. Unknown is represented separately, not as a coercion.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Primitive {
    Text,
    Int,
    Decimal,
    Bool,
    Instant,
    Duration,
    Interval,
    Bytes,
}

impl fmt::Display for Primitive {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{self:?}")
    }
}

/// A record retains declaration order for display, but compares structurally.
#[derive(Clone, Debug)]
pub struct RecordShape {
    name: String,
    fields: IndexMap<String, Shape>,
}

impl RecordShape {
    pub fn new(
        name: impl Into<String>,
        fields: impl IntoIterator<Item = (String, Shape)>,
    ) -> Result<Self, ModelError> {
        let mut ordered = IndexMap::new();
        for (key, shape) in fields {
            if ordered.insert(key.clone(), shape).is_some() {
                return Err(ModelError::DuplicateField(key));
            }
        }
        Ok(Self {
            name: name.into(),
            fields: ordered,
        })
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    pub fn fields(&self) -> impl ExactSizeIterator<Item = (&str, &Shape)> {
        self.fields.iter().map(|(key, value)| (key.as_str(), value))
    }

    pub fn field(&self, name: &str) -> Option<&Shape> {
        self.fields.get(name)
    }
}

impl PartialEq for RecordShape {
    fn eq(&self, other: &Self) -> bool {
        self.fields == other.fields
    }
}

impl Eq for RecordShape {}

/// Nominal management types cannot be supplied where ordinary data is expected.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MetaType {
    WorkspaceDeletePlan,
    ImportPlan,
    ViewInstance,
}
impl fmt::Display for MetaType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::WorkspaceDeletePlan => "WorkspaceDeletePlan",
            Self::ImportPlan => "ImportPlan",
            Self::ViewInstance => "ViewInstance",
        })
    }
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Shape {
    Primitive(Primitive),
    List(Box<Shape>),
    Option(Box<Shape>),
    Iter(Box<Shape>),
    Record(RecordShape),
    Unknown,
    Meta(MetaType),
}

impl Shape {
    pub fn field(&self, name: &str) -> Option<&Shape> {
        match self {
            Self::Record(record) => record.field(name),
            Self::Primitive(Primitive::Interval) if matches!(name, "start" | "end") => {
                Some(&Shape::Primitive(Primitive::Instant))
            }
            Self::Unknown => Some(&Shape::Unknown),
            _ => None,
        }
    }

    pub fn contains_meta(&self) -> bool {
        match self {
            Self::Meta(_) => true,
            Self::List(s) | Self::Option(s) | Self::Iter(s) => s.contains_meta(),
            Self::Record(r) => r.fields().any(|(_, s)| s.contains_meta()),
            _ => false,
        }
    }
    /// Structural assignability, without implicit conversions.
    pub fn is_assignable_to(&self, expected: &Self) -> bool {
        match (self, expected) {
            (Self::Meta(a), Self::Meta(b)) => a == b,
            (_, Self::Unknown) => !self.contains_meta(),
            (Self::Primitive(a), Self::Primitive(b)) => a == b,
            (Self::List(a), Self::List(b)) | (Self::Iter(a), Self::Iter(b)) => {
                a.is_assignable_to(b)
            }
            (Self::Option(a), Self::Option(b)) => a.is_assignable_to(b),
            (Self::Record(a), Self::Record(b)) => b.fields().all(|(key, shape)| {
                a.field(key)
                    .is_some_and(|actual| actual.is_assignable_to(shape))
            }),
            _ => false,
        }
    }
}

impl fmt::Display for Shape {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Primitive(p) => p.fmt(f),
            Self::Unknown => f.write_str("Unknown"),
            Self::Meta(t) => t.fmt(f),
            Self::List(element) => write!(f, "List<{element}>"),
            Self::Option(element) => write!(f, "Option<{element}>"),
            Self::Iter(element) => write!(f, "Iter<{element}>"),
            Self::Record(record) if !record.name.is_empty() => f.write_str(&record.name),
            Self::Record(record) => {
                f.write_str("{ ")?;
                for (i, (name, shape)) in record.fields().enumerate() {
                    if i > 0 {
                        f.write_str(", ")?;
                    }
                    write!(f, "{name}: {shape}")?;
                }
                f.write_str(" }")
            }
        }
    }
}

#[cfg(test)]
mod management_tests {
    use super::*;
    use crate::{Data, Provenance, Value};
    #[test]
    fn management_is_nominal_and_disjoint_from_data_even_unknown_and_containers() {
        let shape = Shape::Meta(MetaType::WorkspaceDeletePlan);
        assert!(shape.is_assignable_to(&shape));
        assert!(!shape.is_assignable_to(&Shape::Unknown));
        assert!(!Shape::List(Box::new(shape.clone())).is_assignable_to(&Shape::Unknown));
        assert!(!Shape::Unknown.is_assignable_to(&shape));
        let data = Data::Record(Default::default());
        let plan = Value::management(
            MetaType::WorkspaceDeletePlan,
            data.clone(),
            "synthetic-authority".into(),
        );
        assert!(plan.with_shape(Shape::Unknown).is_err());
        assert!(
            Value::new(Shape::Unknown, data, Provenance::default())
                .unwrap()
                .with_shape(shape)
                .is_err()
        );
    }
}
