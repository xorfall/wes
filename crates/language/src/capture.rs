//! Literal capture selection. It supplies no execution or live control authority.
use crate::{Annotation, Value};
use std::time::Duration;

#[derive(Clone, Debug)]
pub struct CaptureAnnotation {
    pub name: Option<String>,
    pub tail: Option<Duration>,
}
pub fn milliseconds(text: &str, maximum: u64) -> Option<u64> {
    let (number, scale) = if let Some(n) = text.strip_suffix("ms") {
        (n, 1)
    } else if let Some(n) = text.strip_suffix('s') {
        (n, 1000)
    } else if let Some(n) = text.strip_suffix('m') {
        (n, 60000)
    } else {
        return None;
    };
    if number.is_empty()
        || number.len() > 10
        || !number.bytes().all(|b| b.is_ascii_digit())
        || number.len() > 1 && number.starts_with('0')
    {
        return None;
    }
    number
        .parse::<u64>()
        .ok()?
        .checked_mul(scale)
        .filter(|n| *n <= maximum)
}
impl CaptureAnnotation {
    pub fn parse(annotation: &Annotation) -> Result<Self, &'static str> {
        let invalid = "Use @trace(tcp) with an explicit default, or @trace(capture:\"name\", tail:\"5s\") on a finite provider call. Options must be bounded literals.";
        if annotation.targets.len() == 1
            && annotation.targets[0].text == "tcp"
            && annotation.arguments.is_empty()
        {
            return Ok(Self {
                name: None,
                tail: None,
            });
        }
        if !annotation.targets.is_empty() || !(1..=2).contains(&annotation.arguments.len()) {
            return Err(invalid);
        }
        let mut name = None;
        let mut tail = None;
        for argument in &annotation.arguments {
            let (Value::Text(value) | Value::Word(value)) = &argument.value else {
                return Err(invalid);
            };
            match argument.key.text.as_str() {
                "capture"
                    if name.is_none()
                        && !value.text.is_empty()
                        && value.text.len() <= 64
                        && value.text.trim() == value.text
                        && !value.text.chars().any(char::is_control) =>
                {
                    name = Some(value.text.clone())
                }
                "tail" if tail.is_none() => {
                    tail = Some(Duration::from_millis(
                        milliseconds(&value.text, 60000).ok_or(invalid)?,
                    ))
                }
                _ => return Err(invalid),
            }
        }
        Ok(Self {
            name: Some(name.ok_or(invalid)?),
            tail,
        })
    }
}
