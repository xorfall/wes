//! Source coordinates are data, not a suffix in an error message. Byte ranges remain half-open.
use crate::{Data, Primitive, RecordShape, Shape};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SourceLocation {
    pub source: String,
    pub start: usize,
    pub end: usize,
    pub line: usize,
    /// One-based UTF-16 columns, matching the editor.
    pub column: usize,
    pub end_line: usize,
    pub end_column: usize,
}
impl SourceLocation {
    pub fn valid(&self) -> bool {
        !self.source.is_empty()
            && self.source.len() <= 512
            && self.start <= self.end
            && self.line > 0
            && self.column > 0
            && self.end_column > 0
            && (self.line, self.column) <= (self.end_line, self.end_column)
            && [
                self.start,
                self.end,
                self.line,
                self.column,
                self.end_line,
                self.end_column,
            ]
            .iter()
            .all(|n| i64::try_from(*n).is_ok())
    }
    pub fn shape() -> Shape {
        Shape::Record(
            RecordShape::new(
                "SourceLocation",
                std::iter::once(("source".into(), Shape::Primitive(Primitive::Text))).chain(
                    ["start", "end", "line", "column", "endLine", "endColumn"]
                        .map(|n| (n.into(), Shape::Primitive(Primitive::Int))),
                ),
            )
            .expect("constant source fields"),
        )
    }
    pub fn data(&self) -> Data {
        Data::Record(
            std::iter::once(("source".into(), Data::Text(self.source.as_str().into())))
                .chain(
                    [
                        ("start", self.start),
                        ("end", self.end),
                        ("line", self.line),
                        ("column", self.column),
                        ("endLine", self.end_line),
                        ("endColumn", self.end_column),
                    ]
                    .map(|(name, n)| {
                        (
                            name.into(),
                            Data::Int(i64::try_from(n).expect("validated source location")),
                        )
                    }),
                )
                .collect(),
        )
    }
}
