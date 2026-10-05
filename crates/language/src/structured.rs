//! Bounded declarative data constructors. No operation or effect expression is accepted here.
use crate::{Diagnostic, Name, SourceText, Span, Structure, TokenKind, Value, lex};
use std::collections::BTreeSet;
use wes_core::{Primitive, Provenance, RecordShape, Shape, capability::Typing};

pub const MAX_DEPTH: usize = 32;
pub const MAX_NODES: usize = 1000;
pub const MAX_BYTES: usize = 64 * 1024;

pub fn parse(text: &str, offset: usize) -> Result<Value, Diagnostic> {
    let mut parser = Parser {
        text,
        offset,
        position: 0,
        left: MAX_NODES,
    };
    if text.len() > MAX_BYTES {
        return Err(parser.error("Structured argument exceeds its byte budget."));
    }
    let value = parser.value(0)?;
    parser.space();
    if parser.position != text.len() {
        return Err(parser.error("Unexpected structured argument content."));
    }
    Ok(value)
}
struct Parser<'a> {
    text: &'a str,
    offset: usize,
    position: usize,
    left: usize,
}
impl Parser<'_> {
    fn peek(&self) -> Option<char> {
        self.text[self.position..].chars().next()
    }
    fn advance(&mut self) {
        self.position += self.peek().map_or(0, char::len_utf8);
    }
    fn space(&mut self) {
        loop {
            while self.peek().is_some_and(crate::lexer::whitespace) {
                self.advance();
            }
            if !self.text[self.position..].starts_with("//") {
                break;
            }
            while self.peek().is_some_and(|c| c != '\n') {
                self.advance();
            }
        }
    }
    fn error(&self, message: &str) -> Diagnostic {
        Diagnostic::error("ARG001", Span::at(self.offset + self.position), message)
    }
    fn name(&self, start: usize, text: String) -> Name {
        Name {
            text,
            span: Span::new(self.offset + start, self.offset + self.position).unwrap(),
        }
    }
    fn atom(&mut self, key: bool) -> Result<Value, Diagnostic> {
        let start = self.position;
        if self.peek() == Some('"') {
            self.advance();
            let mut escaped = false;
            loop {
                let ch = self
                    .peek()
                    .ok_or_else(|| self.error("Unclosed quoted value."))?;
                self.advance();
                if !escaped && ch == '"' {
                    break;
                }
                if !escaped && ch == '\\' {
                    escaped = true;
                } else {
                    escaped = false;
                }
            }
        } else {
            while self.peek().is_some_and(|c| {
                !crate::lexer::whitespace(c)
                    && !matches!(c, ',' | '}' | ']' | '{' | '[' | '"')
                    && (c != ':' || !key)
            }) {
                self.advance();
            }
        }
        if self.position == start {
            return Err(self.error("Expected a scalar or reference."));
        }
        let text = &self.text[start..self.position];
        if text.starts_with('?') {
            return Err(self.error("Nested template placeholders are unsupported; compose the body in a typed :calc definition."));
        }
        let lexed = lex(&SourceText::new("argument", text));
        if !lexed.diagnostics.is_empty() || lexed.tokens.len() != 2 {
            return Err(
                self.error("Quote literal text containing structured argument punctuation.")
            );
        }
        let token = &lexed.tokens[0];
        let name = self.name(start, token.text.clone());
        match token.kind {
            TokenKind::String => Ok(Value::Text(name)),
            TokenKind::Ref if !key => Ok(Value::Reference(name)),
            TokenKind::Word => Ok(Value::Word(name)),
            _ => Err(self.error("Expected a scalar or reference.")),
        }
    }
    fn value(&mut self, depth: usize) -> Result<Value, Diagnostic> {
        self.space();
        if depth >= MAX_DEPTH || self.left == 0 {
            return Err(self.error("Structured argument exceeds its nesting or node budget."));
        }
        self.left -= 1;
        let start = self.position;
        let (record, close) = match self.peek() {
            Some('{') => (true, '}'),
            Some('[') => (false, ']'),
            _ => return self.atom(false),
        };
        self.advance();
        self.space();
        let mut fields = vec![];
        let mut items = vec![];
        let mut keys = BTreeSet::new();
        while self.peek() != Some(close) {
            if self.peek().is_none() {
                return Err(self.error("Unclosed structured argument."));
            }
            if record {
                let key = self.atom(true)?.name().clone();
                if key.text.is_empty()
                    || key.text.len() > 256
                    || key.text.chars().any(char::is_control)
                {
                    return Err(self.error(
                        "Record keys must be nonempty, bounded text without control characters.",
                    ));
                }
                if !keys.insert(key.text.clone()) {
                    return Err(self.error("Duplicate record field."));
                }
                self.space();
                if self.peek() != Some(':') {
                    return Err(self.error("Expected ':' after record field."));
                }
                self.advance();
                fields.push((key, self.value(depth + 1)?));
            } else {
                items.push(self.value(depth + 1)?);
            }
            self.space();
            if self.peek() == Some(close) {
                break;
            }
            if self.peek() != Some(',') {
                return Err(self.error("Expected a comma between structured values."));
            }
            self.advance();
            self.space();
        }
        self.advance();
        let name = self.name(start, String::new());
        Ok(Value::Structured(
            name,
            if record {
                Structure::Record(fields)
            } else {
                Structure::List(items)
            },
        ))
    }
}

/// The same contextual scalar policy as ordinary arguments; references never undergo text reading.
pub fn scalar_shape(expected: &Shape) -> Shape {
    if expected == &Shape::Unknown {
        Shape::Primitive(Primitive::Text)
    } else {
        expected.clone()
    }
}
pub fn list_shape(shapes: &[Shape], expected_item: &Shape) -> Shape {
    let item = if shapes.is_empty()
        || (expected_item != &Shape::Unknown
            && shapes.iter().all(|s| s.is_assignable_to(expected_item)))
    {
        expected_item.clone()
    } else if shapes.iter().all(|s| Some(s) == shapes.first()) {
        shapes[0].clone()
    } else {
        Shape::Unknown
    };
    Shape::List(Box::new(item))
}
pub fn typing(
    value: &Value,
    expected: &Shape,
    reference: &impl Fn(&str) -> Option<Typing>,
) -> Result<Typing, Name> {
    match value {
        Value::Reference(name) => reference(&name.text).ok_or_else(|| name.clone()),
        Value::Word(name) | Value::Text(name) => Ok(Typing::new(
            if wes_core::literals::read(&name.text, expected).is_some() {
                scalar_shape(expected)
            } else {
                Shape::Unknown
            },
        )),
        Value::Structured(_, structure) => {
            let children: Vec<(String, Typing)> = match structure {
                Structure::Record(fields) => fields
                    .iter()
                    .map(|(key, value)| {
                        Ok((
                            key.text.clone(),
                            typing(
                                value,
                                expected.field(&key.text).unwrap_or(&Shape::Unknown),
                                reference,
                            )?,
                        ))
                    })
                    .collect::<Result<_, Name>>()?,
                Structure::List(items) => items
                    .iter()
                    .map(|value| {
                        Ok((
                            String::new(),
                            typing(
                                value,
                                if let Shape::List(item) = expected {
                                    item
                                } else {
                                    &Shape::Unknown
                                },
                                reference,
                            )?,
                        ))
                    })
                    .collect::<Result<_, Name>>()?,
            };
            let shape = match structure {
                Structure::Record(_) => Shape::Record(
                    RecordShape::new(
                        "",
                        children
                            .iter()
                            .map(|(key, t)| (key.clone(), t.shape.clone())),
                    )
                    .unwrap(),
                ),
                Structure::List(_) => list_shape(
                    &children
                        .iter()
                        .map(|(_, t)| t.shape.clone())
                        .collect::<Vec<_>>(),
                    if let Shape::List(item) = expected {
                        item
                    } else {
                        &Shape::Unknown
                    },
                ),
            };
            Ok(Typing {
                shape,
                provenance: Provenance::agreed_by(children.iter().map(|(_, t)| &t.provenance)),
            })
        }
    }
}
