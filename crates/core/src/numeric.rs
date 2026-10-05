use regex::Regex;
use std::{borrow::Cow, sync::LazyLock};

static DIGIT: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\A\p{Nd}\z").expect("decimal digit category"));
pub(crate) static DECIMAL: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"\A[+-]?(?:[0-9]+(?:\.[0-9]*)?|\.[0-9]+)(?:[eE][+-]?[0-9]+)?\z")
        .expect("decimal literal grammar")
});

/// Numeric literals accept BMP decimal digits, not supplementary UTF-16 pairs.
/// Nd blocks in the BMP consist of ten ordered digits. ASCII needs no allocation.
pub(crate) fn ascii_digits(text: &str) -> Cow<'_, str> {
    if text.is_ascii() {
        return Cow::Borrowed(text);
    }
    Cow::Owned(
        text.chars()
            .map(|ch| {
                let mut buffer = [0; 4];
                if ch.is_ascii()
                    || ch.len_utf16() != 1
                    || !DIGIT.is_match(ch.encode_utf8(&mut buffer))
                {
                    return ch;
                }
                let mut value = 0u8;
                while value < 9 {
                    let previous = char::from_u32(ch as u32 - u32::from(value) - 1)
                        .expect("non-ASCII BMP digit");
                    if !DIGIT.is_match(previous.encode_utf8(&mut buffer)) {
                        break;
                    }
                    value += 1;
                }
                char::from(b'0' + value)
            })
            .collect(),
    )
}

pub(crate) fn integer(text: &str) -> Option<i64> {
    ascii_digits(text).parse().ok()
}
