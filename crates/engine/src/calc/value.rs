use super::{Budget, Failure};
use indexmap::IndexMap;
use std::sync::Arc;
use wes_core::{Data, RecordShape, Shape};
use wes_language::{
    Span,
    calc::{FunctionId, OperationSpec},
};

#[derive(Clone, Debug)]
pub(super) enum Item {
    Scalar(Arc<Data>),
    Iter(Arc<super::iteration::Pipeline>),
    IterNamespace,
    Typed(
        Arc<Item>,
        Arc<Shape>,
        Option<Arc<wes_core::contracts::metadata::ValueMetadata>>,
    ),
    List(Arc<Vec<Item>>),
    Record(Arc<IndexMap<String, Item>>),
    Option(Option<Arc<Item>>),
    Function {
        function: FunctionId,
        environment: usize,
    },
    Builtin(OperationSpec),
    Method(OperationSpec, Arc<Item>),
}
impl Item {
    /// Constant-size context: never walk a collection or print its contents.
    pub fn kind(&self) -> &'static str {
        match self.untyped() {
            Self::Scalar(data) => match data.as_ref() {
                Data::Text(_) => "Text",
                Data::Int(_) => "Int",
                Data::Decimal(_) => "Decimal",
                Data::Bool(_) => "Bool",
                Data::Bytes(_) => "Bytes",
                Data::Instant(_) => "Instant",
                Data::Duration(_) => "Duration",
                Data::Interval(_) => "Interval",
                Data::List(_) => "List",
                Data::Record(_) => "Record",
                Data::Option(_) => "Option",
                Data::Iter(_) => "Iter",
                Data::Dataset(_) => "Dataset",
            },
            Self::List(_) => "List",
            Self::Record(_) => "Record",
            Self::Option(_) => "Option",
            Self::Iter(_) => "Iter",
            Self::IterNamespace => "Iter namespace",
            Self::Function { .. } | Self::Builtin(_) | Self::Method(..) => "Function",
            Self::Typed(..) => unreachable!("unwrapped"),
        }
    }
    pub fn expected(&self, expected: &str, span: Span) -> Failure {
        Failure::new(
            "CAL004",
            span,
            format!("expected {expected}; received {}", self.kind()),
        )
    }
    pub fn untyped(&self) -> &Self {
        match self {
            Self::Typed(item, _, _) => item.untyped(),
            _ => self,
        }
    }
    pub fn typed(self, shape: Shape) -> Self {
        Self::Typed(Arc::new(self.untyped().clone()), Arc::new(shape), None)
    }
    pub fn metadata(&self) -> Option<&wes_core::contracts::metadata::ValueMetadata> {
        if let Self::Typed(_, _, m) = self {
            m.as_deref()
        } else {
            None
        }
    }
    pub fn annotated(self, meta: Option<wes_core::contracts::metadata::ValueMetadata>) -> Self {
        if let Self::Typed(item, shape, _) = self {
            Self::Typed(item, shape, meta.map(Arc::new))
        } else if meta.is_some() {
            Self::Typed(Arc::new(self), Arc::new(Shape::Unknown), meta.map(Arc::new))
        } else {
            self
        }
    }
    pub fn project_annotation(&self, value: Self, path: &str) -> Self {
        value.annotated(self.metadata().and_then(|m| m.project(path)))
    }
    pub fn project_field_shape(&self, value: Self, name: &str) -> Self {
        let value = if let Self::Typed(_, shape, _) = self
            && let Shape::Record(record) = shape.as_ref()
            && let Some(field) = record.field(name)
        {
            value.typed(field.clone())
        } else {
            value
        };
        self.project_annotation(value, &wes_core::contracts::metadata::field_segment(name))
    }
    pub fn output_shape(&self, data: &Data) -> Shape {
        match self {
            Self::Typed(_, declared, _) if **declared == Shape::Unknown => shape(data, 0),
            Self::Typed(_, shape, _) => shape.as_ref().clone(),
            Self::List(items) => {
                let Data::List(values) = data else {
                    unreachable!()
                };
                let first = items
                    .first()
                    .zip(values.first())
                    .map(|(item, data)| item.output_shape(data))
                    .unwrap_or(Shape::Unknown);
                let common = items
                    .iter()
                    .zip(values)
                    .skip(1)
                    .try_fold(first, |left, (item, data)| {
                        wes_language::calc::merge_inferred_shapes(&left, &item.output_shape(data))
                    })
                    .unwrap_or(Shape::Unknown);
                Shape::List(Box::new(common))
            }
            Self::Record(items) => {
                let Data::Record(values) = data else {
                    unreachable!()
                };
                Shape::Record(
                    RecordShape::new(
                        "",
                        items
                            .iter()
                            .map(|(key, item)| (key.clone(), item.output_shape(&values[key]))),
                    )
                    .expect("unique fields"),
                )
            }
            Self::Option(Some(item)) => {
                let Data::Option(Some(data)) = data else {
                    unreachable!()
                };
                Shape::Option(Box::new(item.output_shape(data)))
            }
            _ => shape(data, 0),
        }
    }

    pub fn from_data(
        data: &Data,
        budget: &mut Budget,
        span: Span,
        depth: usize,
    ) -> Result<Self, Failure> {
        if depth > 128 {
            return Err(Failure::new(
                "CAL006",
                span,
                "calculation value nesting exceeds 128",
            ));
        }
        budget.work(1, span)?;
        budget.allocate(96, span)?;
        Ok(match data {
            Data::Iter(iter) => Self::Iter(Arc::new(super::iteration::Pipeline::from_value(
                iter.clone(),
            ))),
            Data::List(items) => Self::List(Arc::new(
                items
                    .iter()
                    .map(|item| Self::from_data(item, budget, span, depth + 1))
                    .collect::<Result<_, _>>()?,
            )),
            Data::Record(fields) => {
                let mut result = IndexMap::new();
                for (key, value) in fields {
                    budget.allocate(key.len() as u64, span)?;
                    result.insert(
                        key.clone(),
                        Self::from_data(value, budget, span, depth + 1)?,
                    );
                }
                Self::Record(Arc::new(result))
            }
            Data::Option(value) => Self::Option(
                value
                    .as_deref()
                    .map(|v| Self::from_data(v, budget, span, depth + 1).map(Arc::new))
                    .transpose()?,
            ),
            data => {
                match data {
                    Data::Text(s) => budget.allocate(s.len() as u64, span)?,
                    Data::Bytes(b) => budget.allocate(b.len() as u64, span)?,
                    Data::Decimal(n) => budget.allocate(n.compact_text_size_bound(), span)?,
                    _ => {}
                }
                Self::Scalar(Arc::new(data.clone()))
            }
        })
    }
    pub fn data(&self, budget: &mut Budget, span: Span, depth: usize) -> Result<Data, Failure> {
        if depth > 128 {
            return Err(Failure::new(
                "CAL006",
                span,
                "calculation result nesting exceeds 128",
            ));
        }
        budget.work(1, span)?;
        budget.allocate(96, span)?;
        Ok(match self.untyped() {
            Self::Iter(iter) => Data::Iter(iter.stored(span)?),
            Self::Scalar(data) => {
                match data.as_ref() {
                    Data::Text(s) => budget.allocate(s.len() as u64, span)?,
                    Data::Bytes(b) => budget.allocate(b.len() as u64, span)?,
                    Data::Decimal(n) => budget.allocate(n.compact_text_size_bound(), span)?,
                    _ => {}
                }
                data.as_ref().clone()
            }
            Self::List(items) => Data::List(
                items
                    .iter()
                    .map(|v| v.data(budget, span, depth + 1))
                    .collect::<Result<_, _>>()?,
            ),
            Self::Record(fields) => {
                let mut result = IndexMap::new();
                for (key, value) in fields.iter() {
                    budget.allocate(key.len() as u64, span)?;
                    result.insert(key.clone(), value.data(budget, span, depth + 1)?);
                }
                Data::Record(result)
            }
            Self::Option(value) => Data::Option(
                value
                    .as_deref()
                    .map(|v| v.data(budget, span, depth + 1).map(Box::new))
                    .transpose()?,
            ),
            Self::Typed(..) => unreachable!("unwrapped"),
            Self::IterNamespace | Self::Function { .. } | Self::Builtin(_) | Self::Method(..) => {
                return Err(Failure::new(
                    "CAL004",
                    span,
                    "functions cannot cross a value boundary; call the function to return data (for size use length(value) or value.length())",
                ));
            }
        })
    }
    pub fn iter(&self, span: Span) -> Result<Arc<super::iteration::Pipeline>, Failure> {
        if let Self::Iter(iter) = self.untyped() {
            Ok(iter.clone())
        } else {
            Err(self.expected("Iter", span))
        }
    }
    pub fn bool(&self, span: Span) -> Result<bool, Failure> {
        if let Self::Scalar(value) = self.untyped()
            && let Data::Bool(b) = value.as_ref()
        {
            Ok(*b)
        } else {
            Err(self.expected("Bool", span))
        }
    }
    pub fn int(&self, span: Span) -> Result<i64, Failure> {
        if let Self::Scalar(value) = self.untyped()
            && let Data::Int(n) = value.as_ref()
        {
            Ok(*n)
        } else {
            Err(self.expected("Int", span))
        }
    }
    pub fn text(&self, span: Span) -> Result<&str, Failure> {
        if let Self::Scalar(value) = self.untyped()
            && let Data::Text(s) = value.as_ref()
        {
            Ok(s)
        } else {
            Err(self.expected("Text", span))
        }
    }
    pub fn list(&self, span: Span) -> Result<Arc<Vec<Self>>, Failure> {
        if let Self::List(items) = self.untyped() {
            Ok(items.clone())
        } else {
            Err(self.expected("List", span))
        }
    }
    pub fn scalar(data: Data) -> Self {
        Self::Scalar(Arc::new(data))
    }
}
pub(super) fn shape(data: &Data, depth: usize) -> Shape {
    if depth > 128 {
        return Shape::Unknown;
    }
    match data {
        Data::Iter(iter) => Shape::Iter(Box::new(iter.item_shape().clone())),
        Data::Dataset(_) => Shape::Dataset(Box::new(Shape::Unknown)),
        Data::List(items) => {
            let first = items
                .first()
                .map(|d| shape(d, depth + 1))
                .unwrap_or(Shape::Unknown);
            let common = items
                .iter()
                .skip(1)
                .try_fold(first, |left, value| {
                    wes_language::calc::merge_inferred_shapes(&left, &shape(value, depth + 1))
                })
                .unwrap_or(Shape::Unknown);
            Shape::List(Box::new(common))
        }
        Data::Record(fields) => Shape::Record(
            RecordShape::new(
                "",
                fields.iter().map(|(k, v)| (k.clone(), shape(v, depth + 1))),
            )
            .expect("distinct keys"),
        ),
        Data::Option(value) => Shape::Option(Box::new(
            value
                .as_deref()
                .map(|v| shape(v, depth + 1))
                .unwrap_or(Shape::Unknown),
        )),
        Data::Text(_) => Shape::Primitive(wes_core::Primitive::Text),
        Data::Int(_) => Shape::Primitive(wes_core::Primitive::Int),
        Data::Decimal(_) => Shape::Primitive(wes_core::Primitive::Decimal),
        Data::Bool(_) => Shape::Primitive(wes_core::Primitive::Bool),
        Data::Bytes(_) => Shape::Primitive(wes_core::Primitive::Bytes),
        Data::Instant(_) => Shape::Primitive(wes_core::Primitive::Instant),
        Data::Duration(_) => Shape::Primitive(wes_core::Primitive::Duration),
        Data::Interval(_) => Shape::Primitive(wes_core::Primitive::Interval),
    }
}
