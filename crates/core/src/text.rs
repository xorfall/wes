//! Explicit terminal-text normalization with original, half-open UTF-8 byte spans.
use crate::{Primitive, RecordShape, Shape};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TextSpan {
    pub input_start: usize,
    pub input_end: usize,
    pub output_start: usize,
    pub output_end: usize,
}
pub fn normalized_shape() -> Shape {
    let int = Shape::Primitive(Primitive::Int);
    let span = Shape::Record(
        RecordShape::new(
            "",
            [
                ("inputStart".into(), int.clone()),
                ("inputEnd".into(), int.clone()),
                ("outputStart".into(), int.clone()),
                ("outputEnd".into(), int),
            ],
        )
        .expect("span fields"),
    );
    Shape::Record(
        RecordShape::new(
            "",
            [
                ("text".into(), Shape::Primitive(Primitive::Text)),
                ("spans".into(), Shape::List(Box::new(span))),
            ],
        )
        .expect("normalized text fields"),
    )
}

/// Visits unchanged runs without allocating or copying payload. `emit` owns budget,
/// cancellation and output admission. Only 7-bit CSI and OSC are accepted; unsupported
/// or incomplete escapes fail, without echoing source data in diagnostics.
pub fn ansi_spans<E>(
    source: &str,
    mut emit: impl FnMut(TextSpan) -> Result<(), E>,
) -> Result<(), AnsiError<E>> {
    let bytes = source.as_bytes();
    let (mut at, mut start, mut out) = (0, 0, 0);
    while at < bytes.len() {
        if bytes[at] != 0x1b {
            at += 1;
            continue;
        }
        if start < at {
            let len = at - start;
            emit(TextSpan {
                input_start: start,
                input_end: at,
                output_start: out,
                output_end: out + len,
            })
            .map_err(AnsiError::Admission)?;
            out += len;
        }
        at += 1;
        match bytes.get(at) {
            Some(b'[') => {
                at += 1;
                while bytes.get(at).is_some_and(|b| (0x30..=0x3f).contains(b)) {
                    at += 1;
                }
                while bytes.get(at).is_some_and(|b| (0x20..=0x2f).contains(b)) {
                    at += 1;
                }
                if !bytes.get(at).is_some_and(|b| (0x40..=0x7e).contains(b)) {
                    return Err(AnsiError::Invalid);
                }
                at += 1;
            }
            Some(b']') => {
                at += 1;
                loop {
                    match bytes.get(at) {
                        Some(7) => {
                            at += 1;
                            break;
                        }
                        Some(0x1b) if bytes.get(at + 1) == Some(&b'\\') => {
                            at += 2;
                            break;
                        }
                        Some(0x1b) | None => return Err(AnsiError::Invalid),
                        Some(_) => at += 1,
                    }
                }
            }
            _ => return Err(AnsiError::Invalid),
        }
        start = at;
    }
    if start < at {
        emit(TextSpan {
            input_start: start,
            input_end: at,
            output_start: out,
            output_end: out + (at - start),
        })
        .map_err(AnsiError::Admission)?;
    }
    Ok(())
}

#[derive(Debug, PartialEq, Eq)]
pub enum AnsiError<E> {
    Invalid,
    Admission(E),
}
