//! Bounded incremental byte framing. Read-block boundaries carry no record meaning.
use crate::text::TextSpan;

#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum Delimiter {
    /// LF and CRLF; a lone CR remains content.
    Lines,
    Literal(Vec<u8>),
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum Decoding {
    StrictUtf8,
    LossyUtf8,
}
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Profile {
    pub delimiter: Delimiter,
    pub decoding: Decoding,
    pub raw_bytes: usize,
    pub decoded_bytes: usize,
    pub spans: usize,
}
impl Profile {
    pub fn valid(&self) -> bool {
        (1..=1024 * 1024).contains(&self.raw_bytes)
            && (1..=4 * 1024 * 1024).contains(&self.decoded_bytes)
            && (1..=65_536).contains(&self.spans)
            && match &self.delimiter {
                Delimiter::Lines => true,
                Delimiter::Literal(bytes) => (1..=4096).contains(&bytes.len()),
            }
    }
}
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ByteSpan {
    pub start: u64,
    pub end: u64,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Record {
    pub ordinal: u64,
    pub source: ByteSpan,
    pub delimiter: ByteSpan,
    pub raw: Vec<u8>,
    pub text: String,
    /// Mapping of decoded UTF-8 bytes to record-relative original bytes.
    pub spans: Vec<TextSpan>,
    pub lossy: bool,
    pub unterminated: bool,
}
#[derive(Debug, PartialEq, Eq)]
pub enum Error<E> {
    InvalidProfile,
    Closed,
    PositionExhausted,
    RawLimit(ByteSpan),
    DecodedLimit(ByteSpan),
    SpanLimit(ByteSpan),
    InvalidUtf8(ByteSpan),
    Admission(E),
}

pub struct Framer {
    profile: Profile,
    delimiter: Vec<u8>,
    prefixes: Vec<usize>,
    matched: usize,
    pending: Vec<u8>,
    start: u64,
    position: u64,
    ordinal: u64,
    closed: bool,
}
impl Framer {
    pub fn profile(&self) -> &Profile {
        &self.profile
    }
    pub fn new(profile: Profile) -> Result<Self, Error<()>> {
        if !profile.valid() {
            return Err(Error::InvalidProfile);
        }
        let delimiter = match &profile.delimiter {
            Delimiter::Lines => vec![b'\n'],
            Delimiter::Literal(bytes) => bytes.clone(),
        };
        // KMP makes overlapping literal delimiters linear in input plus delimiter size.
        let mut prefixes = vec![0; delimiter.len()];
        for i in 1..delimiter.len() {
            let mut j = prefixes[i - 1];
            while j > 0 && delimiter[i] != delimiter[j] {
                j = prefixes[j - 1];
            }
            if delimiter[i] == delimiter[j] {
                j += 1;
            }
            prefixes[i] = j;
        }
        Ok(Self {
            profile,
            delimiter,
            prefixes,
            matched: 0,
            pending: vec![],
            start: 0,
            position: 0,
            ordinal: 0,
            closed: false,
        })
    }
    pub fn position(&self) -> u64 {
        self.position
    }
    /// Restore an already validated committed record boundary; no pending bytes are replayed.
    pub fn at_boundary(profile: Profile, position: u64, ordinal: u64) -> Result<Self, Error<()>> {
        let mut framer = Self::new(profile)?;
        framer.start = position;
        framer.position = position;
        framer.ordinal = ordinal;
        Ok(framer)
    }
    pub fn buffered_bytes(&self) -> usize {
        self.pending.len()
    }
    /// Caller admits the bounded block's work/memory before calling. A failed emitter
    /// closes this cursor; it cannot retry a partly consumed block or duplicate output.
    pub fn push<E>(
        &mut self,
        bytes: &[u8],
        mut emit: impl FnMut(Record) -> Result<(), E>,
    ) -> Result<(), Error<E>> {
        if self.closed {
            return Err(Error::Closed);
        }
        let result = self
            .push_inner(bytes, &mut emit, false, &mut |_| Ok(()))
            .map(|_| ());
        if result.is_err() {
            self.closed = true;
        }
        result
    }
    /// Pull at most one record. The returned count is the exact consumed block
    /// prefix; the caller retains its suffix and can yield without buffering rows.
    pub fn pull(&mut self, bytes: &[u8]) -> Result<(usize, Option<Record>), Error<()>> {
        self.pull_admitted(bytes, |_| Ok(()))
    }
    /// Debit before each byte is examined. Unconsumed suffixes earn no debit;
    /// block size therefore cannot multiply work when a block contains many rows.
    /// The caller's per-byte debit also prepays bounded decoding of that byte.
    pub fn pull_admitted<E>(
        &mut self,
        bytes: &[u8],
        mut admit: impl FnMut(usize) -> Result<(), E>,
    ) -> Result<(usize, Option<Record>), Error<E>> {
        if self.closed {
            return Err(Error::Closed);
        }
        let mut record = None;
        let result = self.push_inner(
            bytes,
            &mut |row| {
                record = Some(row);
                Ok(())
            },
            true,
            &mut admit,
        );
        match result {
            Ok(consumed) => Ok((consumed, record)),
            Err(error) => {
                self.closed = true;
                Err(error)
            }
        }
    }
    fn push_inner<E>(
        &mut self,
        bytes: &[u8],
        emit: &mut impl FnMut(Record) -> Result<(), E>,
        stop_after_record: bool,
        admit: &mut impl FnMut(usize) -> Result<(), E>,
    ) -> Result<usize, Error<E>> {
        for (i, &byte) in bytes.iter().enumerate() {
            admit(1).map_err(Error::Admission)?;
            self.position = self
                .position
                .checked_add(1)
                .ok_or(Error::PositionExhausted)?;
            self.pending.push(byte);
            while self.matched > 0 && byte != self.delimiter[self.matched] {
                self.matched = self.prefixes[self.matched - 1];
            }
            if byte == self.delimiter[self.matched] {
                self.matched += 1;
            }
            if self.matched == self.delimiter.len() {
                let mut delimiter_bytes = self.delimiter.len();
                if self.profile.delimiter == Delimiter::Lines
                    && self.pending.len() > 1
                    && self.pending[self.pending.len() - 2] == b'\r'
                {
                    delimiter_bytes += 1;
                }
                self.emit(delimiter_bytes, false, emit)?;
                self.matched = 0;
                if stop_after_record {
                    return Ok(i + 1);
                }
            } else {
                // A possible delimiter prefix is bounded carry, not yet confirmed content.
                let possible_cr =
                    usize::from(self.profile.delimiter == Delimiter::Lines && byte == b'\r');
                if self.pending.len() - self.matched - possible_cr > self.profile.raw_bytes {
                    return Err(Error::RawLimit(self.pending_span()));
                }
            }
        }
        Ok(bytes.len())
    }
    /// Only call for an explicit clean extent end. Empty input and a trailing
    /// delimiter add no synthetic record. The final unterminated record is explicit.
    pub fn finish<E>(
        &mut self,
        mut emit: impl FnMut(Record) -> Result<(), E>,
    ) -> Result<(), Error<E>> {
        if self.closed {
            return Err(Error::Closed);
        }
        self.closed = true;
        if self.pending.is_empty() {
            Ok(())
        } else {
            self.emit(0, true, &mut emit)
        }
    }
    fn pending_span(&self) -> ByteSpan {
        ByteSpan {
            start: self.start,
            end: self.position,
        }
    }
    fn emit<E>(
        &mut self,
        delimiter_bytes: usize,
        unterminated: bool,
        emit: &mut impl FnMut(Record) -> Result<(), E>,
    ) -> Result<(), Error<E>> {
        let content = self.pending.len() - delimiter_bytes;
        let end = self.position - delimiter_bytes as u64;
        let span = ByteSpan {
            start: self.start,
            end,
        };
        if content > self.profile.raw_bytes {
            return Err(Error::RawLimit(span));
        }
        let mut raw = std::mem::take(&mut self.pending);
        raw.truncate(content);
        let (text, spans, lossy) = decode(&raw, &self.profile, span)?;
        emit(Record {
            ordinal: self.ordinal,
            source: span,
            delimiter: ByteSpan {
                start: end,
                end: self.position,
            },
            raw,
            text,
            spans,
            lossy,
            unterminated,
        })
        .map_err(Error::Admission)?;
        self.ordinal = self
            .ordinal
            .checked_add(1)
            .ok_or(Error::PositionExhausted)?;
        self.start = self.position;
        Ok(())
    }
}

fn decode<E>(
    raw: &[u8],
    profile: &Profile,
    source: ByteSpan,
) -> Result<(String, Vec<TextSpan>, bool), Error<E>> {
    let mut text = String::new();
    let mut spans = vec![];
    let mut position = 0;
    let mut lossy = false;
    while position < raw.len() {
        let rest = &raw[position..];
        let (valid, invalid) = match std::str::from_utf8(rest) {
            Ok(value) => (value.len(), 0),
            Err(error) => {
                if profile.decoding == Decoding::StrictUtf8 {
                    let start = source.start + position as u64 + error.valid_up_to() as u64;
                    return Err(Error::InvalidUtf8(ByteSpan {
                        start,
                        end: start
                            + error
                                .error_len()
                                .unwrap_or(rest.len() - error.valid_up_to())
                                as u64,
                    }));
                }
                (
                    error.valid_up_to(),
                    error
                        .error_len()
                        .unwrap_or(rest.len() - error.valid_up_to()),
                )
            }
        };
        if valid > 0 {
            append(
                raw,
                &mut text,
                &mut spans,
                position,
                position + valid,
                false,
                profile,
                source,
            )?;
            position += valid;
        }
        if invalid > 0 {
            append(
                raw,
                &mut text,
                &mut spans,
                position,
                position + invalid,
                true,
                profile,
                source,
            )?;
            lossy = true;
            position += invalid;
        }
    }
    Ok((text, spans, lossy))
}
fn append<E>(
    raw: &[u8],
    text: &mut String,
    spans: &mut Vec<TextSpan>,
    start: usize,
    end: usize,
    replacement: bool,
    profile: &Profile,
    source: ByteSpan,
) -> Result<(), Error<E>> {
    let output = if replacement {
        "\u{fffd}"
    } else {
        std::str::from_utf8(&raw[start..end]).expect("validated UTF-8 run")
    };
    if output.len() > profile.decoded_bytes.saturating_sub(text.len()) {
        return Err(Error::DecodedLimit(source));
    }
    if spans.len() >= profile.spans {
        return Err(Error::SpanLimit(source));
    }
    let output_start = text.len();
    text.push_str(output);
    spans.push(TextSpan {
        input_start: start,
        input_end: end,
        output_start,
        output_end: text.len(),
    });
    Ok(())
}
