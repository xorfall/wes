use thiserror::Error;

/// A validated half-open span of UTF-8 bytes. Wire offsets require explicit conversion.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Span {
    start: usize,
    end: usize,
}

impl Span {
    pub fn new(start: usize, end: usize) -> Result<Self, SourceError> {
        if end < start {
            return Err(SourceError::ReversedSpan);
        }
        Ok(Self { start, end })
    }
    pub fn at(offset: usize) -> Self {
        Self {
            start: offset,
            end: offset,
        }
    }
    pub fn start(self) -> usize {
        self.start
    }
    pub fn end(self) -> usize {
        self.end
    }
    pub fn len(self) -> usize {
        self.end - self.start
    }
    pub fn is_empty(self) -> bool {
        self.start == self.end
    }
    pub fn contains(self, offset: usize) -> bool {
        self.start <= offset && offset < self.end
    }
    pub fn union(self, other: Self) -> Self {
        Self {
            start: self.start.min(other.start),
            end: self.end.max(other.end),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Position {
    pub line: usize,
    /// One-based UTF-16 column, matching browser client offsets.
    pub column: usize,
}

#[derive(Clone, Debug, Error, PartialEq, Eq)]
pub enum SourceError {
    #[error("span end precedes start")]
    ReversedSpan,
    #[error("offset is outside the source or splits an encoded character")]
    InvalidOffset,
    #[error("line number is outside the source")]
    InvalidLine,
}

/// A display label is portable metadata, never a filesystem authority or source identity.
pub fn portable_source_name(name: &str) -> String {
    let bytes = name.as_bytes();
    let absolute = name.starts_with('/')
        || name.starts_with("\\\\")
        || (bytes.len() > 2
            && bytes[0].is_ascii_alphabetic()
            && bytes[1] == b':'
            && matches!(bytes[2], b'/' | b'\\'));
    if absolute {
        name.rsplit(['/', '\\'])
            .find(|part| !part.is_empty())
            .unwrap_or("source")
            .into()
    } else {
        name.into()
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SourceText {
    name: String,
    text: String,
    lines: Vec<usize>,
    start: Position,
}

impl SourceText {
    pub fn new(name: impl Into<String>, text: impl Into<String>) -> Self {
        let text = text.into();
        let lines = std::iter::once(0)
            .chain(text.match_indices('\n').map(|(i, _)| i + 1))
            .collect();
        Self {
            name: portable_source_name(&name.into()),
            text,
            lines,
            start: Position { line: 1, column: 1 },
        }
    }
    /// Position of the first character in its containing source; byte spans remain local.
    pub fn with_start(mut self, start: Position) -> Result<Self, SourceError> {
        if start.line == 0 || start.column == 0 {
            return Err(SourceError::InvalidOffset);
        }
        self.start = start;
        Ok(self)
    }
    pub fn name(&self) -> &str {
        &self.name
    }
    pub fn text(&self) -> &str {
        &self.text
    }
    pub fn line_count(&self) -> usize {
        self.lines.len()
    }
    pub fn byte_len(&self) -> usize {
        self.text.len()
    }
    pub fn slice(&self, span: Span) -> Result<&str, SourceError> {
        self.text
            .get(span.start..span.end)
            .ok_or(SourceError::InvalidOffset)
    }
    pub fn utf16_offset(&self, byte: usize) -> Result<usize, SourceError> {
        Ok(self
            .text
            .get(..byte)
            .ok_or(SourceError::InvalidOffset)?
            .encode_utf16()
            .count())
    }
    pub fn byte_offset(&self, utf16: usize) -> Result<usize, SourceError> {
        let mut units = 0;
        for (byte, ch) in self.text.char_indices() {
            if units == utf16 {
                return Ok(byte);
            }
            units += ch.len_utf16();
            if units > utf16 {
                return Err(SourceError::InvalidOffset);
            }
        }
        if units == utf16 {
            Ok(self.text.len())
        } else {
            Err(SourceError::InvalidOffset)
        }
    }
    pub fn position(&self, byte: usize) -> Result<Position, SourceError> {
        if byte > self.text.len() || !self.text.is_char_boundary(byte) {
            return Err(SourceError::InvalidOffset);
        }
        let index = self.lines.partition_point(|start| *start <= byte) - 1;
        let column = self.text[self.lines[index]..byte].encode_utf16().count() + 1;
        Ok(Position {
            line: self
                .start
                .line
                .checked_add(index)
                .ok_or(SourceError::InvalidOffset)?,
            column: column
                .checked_add(if index == 0 { self.start.column - 1 } else { 0 })
                .ok_or(SourceError::InvalidOffset)?,
        })
    }
    pub fn line(&self, number: usize) -> Result<&str, SourceError> {
        let index = number.checked_sub(1).ok_or(SourceError::InvalidLine)?;
        let start = *self.lines.get(index).ok_or(SourceError::InvalidLine)?;
        let end = self
            .lines
            .get(index + 1)
            .map_or(self.text.len(), |next| next - 1);
        let text = &self.text[start..end];
        Ok(text.strip_suffix('\r').unwrap_or(text))
    }
}
