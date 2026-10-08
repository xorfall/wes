//! Read a captured immutable value only. This port cannot acquire or restart a provider.
use super::ledger::{Dimension, Refusal};
use crate::{calc::Failure, driver::CancellationToken};
use wes_core::{
    Data, Primitive, Provenance, RecordShape, Shape, Value,
    framing::{self, Framer, Profile},
    text::normalized_shape,
};
use wes_language::Span;

pub enum SourcePoll {
    /// Original half-open position: bytes for framed sources, ordinals for records.
    Record {
        value: Value,
        start: u64,
        end: u64,
        input_charge: u64,
    },
    Pending,
    ReadPage,
    ReadHead,
    Incomplete,
    End,
}
#[derive(Debug)]
pub struct SourceFailure {
    pub failure: Failure,
    pub source_span: Option<framing::ByteSpan>,
    pub dimension: Option<Dimension>,
}
impl SourceFailure {
    fn record_limit(message: &str, span: Span, source_span: Option<framing::ByteSpan>) -> Self {
        Self {
            failure: Failure::new("CAL006", span, message),
            source_span,
            dimension: Some(Dimension::RecordMemory),
        }
    }
}
impl From<Failure> for SourceFailure {
    fn from(failure: Failure) -> Self {
        Self {
            failure,
            source_span: None,
            dimension: None,
        }
    }
}
enum Admission {
    Cancelled,
    Budget(Refusal),
}
fn budget_failure(error: Refusal, span: Span) -> SourceFailure {
    SourceFailure {
        failure: Failure::new(
            "CAL006",
            span,
            format!("scan {:?} limit reached ({})", error.dimension, error.limit),
        ),
        source_span: None,
        dimension: Some(error.dimension),
    }
}
/// Reserve native carry, decode buffers/maps and delimiter preprocessing before
/// constructing a cursor. This is conservative ownership charge, not RSS.
pub fn framing_charge(profile: &Profile) -> Option<u64> {
    if !profile.valid() {
        return None;
    }
    let delimiter = match &profile.delimiter {
        framing::Delimiter::Lines => 1,
        framing::Delimiter::Literal(bytes) => bytes.len(),
    };
    (profile.raw_bytes as u64)
        .checked_mul(4)?
        .checked_add((profile.decoded_bytes as u64).checked_mul(2)?)?
        .checked_add((profile.spans as u64).checked_mul(128)?)?
        .checked_add((delimiter as u64).checked_mul(32)?)?
        .checked_add(4096)
}
pub struct CapturedSource {
    /// Admission evidence stays frozen while the read cursor advances committed heads.
    /// Only Dataset descriptors need a second copy; finite inline sources retain one tree.
    admitted: Option<Value>,
    value: Value,
    framer: Option<Framer>,
    position: usize,
    ordinal: usize,
    ended: bool,
    record_charge: u64,
    total: u64,
    page: std::collections::VecDeque<Value>,
    lease: Option<crate::storage::datasets::DatasetReadLease>,
    follow: Option<(crate::storage::datasets::FollowedSource, FollowEnd)>,
}
#[derive(Clone, Copy)]
enum FollowEnd {
    Waiting,
    Natural,
    Incomplete,
}
impl CapturedSource {
    pub(super) fn value(&self) -> &Value {
        self.admitted.as_ref().unwrap_or(&self.value)
    }
    pub(super) fn profile(&self) -> Option<&Profile> {
        self.framer.as_ref().map(Framer::profile)
    }
    /// `value_charge` admits the whole source tree before traversal/cloning; framing
    /// profile limits separately bound raw carry, decoded text and mapping storage.
    pub fn new(
        value: Value,
        profile: Option<Profile>,
        source_charge: u64,
        record_charge: u64,
        span: Span,
    ) -> Result<Self, Failure> {
        let value = match value.data() {
            Data::Dataset(reference) => value.with_provenance(
                value.provenance().clone().with_policy(
                    &value
                        .provenance()
                        .policy()
                        .clone()
                        .read_from_dataset(reference),
                ),
            ),
            _ => value,
        };
        if crate::value_size::value_charge(&value, source_charge).is_none() {
            return Err(Failure::new(
                "CAL006",
                span,
                "captured scan source exceeds its declared charge limit",
            ));
        }
        let dataset = matches!(
            (value.data(), value.shape()),
            (Data::Dataset(_), Shape::Dataset(_))
        );
        if (!dataset && (!value.data().is_inline() || !value.shape().is_inline()))
            || value.shape().contains_meta()
            || value.management_authority().is_some()
        {
            return Err(Failure::new(
                "CAL004",
                span,
                "scan source must be captured Text, Bytes, List or an owned Dataset prefix",
            ));
        }
        let (total, framer) = match (value.data(), profile) {
            (Data::Text(text), Some(profile)) => (text.len() as u64, Some(profile)),
            (Data::Bytes(bytes), Some(profile)) => (bytes.len() as u64, Some(profile)),
            (Data::List(items), None) => (items.len() as u64, None),
            (Data::Dataset(reference), None) => (reference.records(), None),
            _ => {
                return Err(Failure::new(
                    "CAL004",
                    span,
                    "text/byte sources require an explicit framing profile; typed List sources require TypedRecords",
                ));
            }
        };
        if total > i64::MAX as u64 {
            return Err(Failure::new(
                "CAL006",
                span,
                "scan source extent exceeds the position representation",
            ));
        }
        let framer = framer
            .map(Framer::new)
            .transpose()
            .map_err(|error| framing_failure(error, span).failure)?;
        Ok(Self {
            admitted: None,
            value,
            framer,
            position: 0,
            ordinal: 0,
            ended: false,
            record_charge,
            total,
            page: Default::default(),
            lease: None,
            follow: None,
        })
    }
    pub fn total(&self) -> u64 {
        self.total
    }
    pub(super) fn stop_following(&mut self) {
        if let Some((_, ending)) = &mut self.follow {
            *ending = FollowEnd::Incomplete;
        }
    }
    pub(super) fn followed_source(&self) -> Option<&crate::storage::datasets::FollowedSource> {
        self.follow.as_ref().map(|(source, _)| source)
    }
    pub(super) fn follow(
        &mut self,
        source: crate::storage::datasets::FollowedSource,
        span: Span,
    ) -> Result<(), Failure> {
        let reference = self.dataset().ok_or_else(|| {
            Failure::new(
                "CAL004",
                span,
                "live scan requires an owned EventLog Dataset",
            )
        })?;
        if &source.prefix != reference || self.framer.is_some() || self.ordinal != 0 {
            return Err(Failure::new(
                "CAL004",
                span,
                "live source does not match its admitted prefix",
            ));
        }
        self.admitted = Some(self.value.clone());
        self.follow = Some((source, FollowEnd::Waiting));
        Ok(())
    }
    pub(super) fn acknowledge_head(
        &mut self,
        info: crate::storage::datasets::DatasetInfo,
        span: Span,
    ) -> Result<bool, Failure> {
        use crate::storage::datasets::{DatasetLifecycle as L, FollowedSource, RecordingEnd as E};
        let invalid = || {
            Failure::new(
                "CAL004",
                span,
                "EventLog head changed its committed identity, epoch or schema",
            )
        };
        let (prior, _) = self.follow.as_ref().ok_or_else(invalid)?;
        let coverage = info.recording.as_ref().ok_or_else(invalid)?;
        let next = FollowedSource {
            prefix: info.reference.clone(),
            run: coverage.run.clone(),
            epoch: coverage.epoch.clone(),
            first: coverage.first,
        };
        if !next.extends(prior)
            || info.schema.digest() != info.reference.schema_digest()
            || info.schema.root().shape() != self.item_shape()
            || info.reference.records() > i64::MAX as u64
            || !self.page.is_empty()
        {
            return Err(invalid());
        }
        let ending = match (info.lifecycle, coverage.termination) {
            (L::Open | L::Prefix, None) => FollowEnd::Waiting,
            (L::Sealed, Some(E::Natural))
                if coverage.pending == Some(0) && coverage.rejected == 0 =>
            {
                FollowEnd::Natural
            }
            _ => FollowEnd::Incomplete,
        };
        let provenance = self.value.provenance().clone().with_policy(
            &self
                .value
                .provenance()
                .policy()
                .join(&info.policy)
                .read_from_dataset(&info.reference),
        );
        self.value = Value::new(
            self.value.shape().clone(),
            Data::Dataset(info.reference.clone().into()),
            provenance,
        )
        .map_err(|_| invalid())?
        .with_metadata(self.value.metadata().cloned());
        self.total = info.reference.records();
        self.follow = Some((next, ending));
        self.ended = false;
        self.lease = None;
        Ok(self.ordinal() < self.total || !matches!(ending, FollowEnd::Waiting))
    }
    pub(super) fn producer_complete(&self) -> bool {
        self.follow
            .as_ref()
            .is_some_and(|(_, end)| matches!(end, FollowEnd::Natural))
    }
    pub(super) fn producer_status(&self) -> Option<bool> {
        self.follow.as_ref().and_then(|(_, end)| match end {
            FollowEnd::Waiting => None,
            FollowEnd::Natural => Some(true),
            FollowEnd::Incomplete => Some(false),
        })
    }
    pub(super) fn restore_boundary(
        &mut self,
        position: u64,
        ordinal: u64,
        span: Span,
    ) -> Result<(), Failure> {
        let invalid = || {
            Failure::new(
                "CAL004",
                span,
                "captured scan cursor is not a committed source boundary",
            )
        };
        if position > self.total {
            return Err(invalid());
        }
        if let Some(framer) = &self.framer {
            // A byte boundary is attested by the protected checkpoint/source digest, never a
            // user-provided offset. Fresh framing retains its original profile and ordinals.
            self.framer = Some(
                Framer::at_boundary(framer.profile().clone(), position, ordinal)
                    .map_err(|_| invalid())?,
            );
            self.position = usize::try_from(position).map_err(|_| invalid())?;
        } else {
            if position != ordinal {
                return Err(invalid());
            }
            self.ordinal = usize::try_from(ordinal).map_err(|_| invalid())?;
        }
        self.ended = false;
        Ok(())
    }
    pub fn position(&self) -> u64 {
        self.framer
            .as_ref()
            .map_or(self.ordinal as u64, Framer::position)
    }
    pub fn item_shape(&self) -> Shape {
        if self.framer.is_some() {
            frame_shape()
        } else {
            match self.value.shape() {
                Shape::List(item) | Shape::Dataset(item) => item.as_ref().clone(),
                _ => Shape::Unknown,
            }
        }
    }
    pub fn framed(&self) -> bool {
        self.framer.is_some()
    }
    pub fn provenance(&self) -> &Provenance {
        self.value.provenance()
    }
    pub(super) fn dataset(&self) -> Option<&wes_core::DatasetRef> {
        match self.value.data() {
            Data::Dataset(r) => Some(r),
            _ => None,
        }
    }
    pub(super) fn ordinal(&self) -> u64 {
        self.ordinal as u64
    }
    pub(super) fn acknowledge_page(
        &mut self,
        page: crate::storage::datasets::DatasetPage,
        span: Span,
    ) -> Result<(), Failure> {
        let invalid = || {
            Failure::new(
                "CAL004",
                span,
                "dataset source page does not match its captured prefix and ordinal",
            )
        };
        let reference = self.dataset().ok_or_else(invalid)?;
        if &page.reference != reference
            || page.first != self.ordinal()
            || !self.page.is_empty()
            || page.schema.root().shape() != self.item_shape()
            || page.next
                != page
                    .first
                    .checked_add(page.rows.len() as u64)
                    .ok_or_else(invalid)?
            || page.next > reference.records()
            || page.rows.is_empty()
        {
            return Err(invalid());
        }
        let mut charge = 0u64;
        for (i, row) in page.rows.iter().enumerate() {
            if row.ordinal != page.first + i as u64
                || row.value.shape() != &self.item_shape()
                || !row.value.data().is_inline()
            {
                return Err(invalid());
            }
            let part = crate::value_size::value_charge(&row.value, self.record_charge)
                .ok_or_else(invalid)?;
            charge = charge
                .checked_add(part)
                .filter(|n| *n <= self.record_charge)
                .ok_or_else(invalid)?;
        }
        self.page = page.rows.into_iter().map(|row| row.value).collect();
        self.lease = page.lease;
        Ok(())
    }
    /// Maximum bytes examined in the next read; caller prepays native work.
    pub fn next_bytes(&self, block: usize) -> usize {
        if self.framer.is_some() {
            block.min(self.total as usize - self.position)
        } else {
            0
        }
    }
    pub fn poll(
        &mut self,
        block: usize,
        token: &CancellationToken,
        span: Span,
    ) -> Result<SourcePoll, SourceFailure> {
        self.poll_admitted(block, token, span, |_| Ok(()))
    }
    pub fn poll_admitted(
        &mut self,
        block: usize,
        token: &CancellationToken,
        span: Span,
        mut admit: impl FnMut(u64) -> Result<(), Refusal>,
    ) -> Result<SourcePoll, SourceFailure> {
        if token.is_cancelled() {
            return Err(Failure::cancelled(span).into());
        }
        if self.ended {
            return Ok(SourcePoll::End);
        }
        if block == 0 || block > 65_536 {
            return Err(Failure::new(
                "CAL006",
                span,
                "scan read block must be from 1 to 65536 bytes",
            )
            .into());
        }
        if self.dataset().is_some() {
            if self.ordinal() >= self.total {
                self.lease = None;
                if let Some((_, ending)) = &self.follow {
                    match ending {
                        FollowEnd::Waiting => return Ok(SourcePoll::ReadHead),
                        FollowEnd::Incomplete => return Ok(SourcePoll::Incomplete),
                        FollowEnd::Natural => {}
                    }
                }
                self.ended = true;
                return Ok(SourcePoll::End);
            }
            let Some(value) = self.page.front() else {
                return Ok(SourcePoll::ReadPage);
            };
            let input_charge = crate::value_size::value_charge(value, self.record_charge)
                .ok_or_else(|| {
                    SourceFailure::record_limit(
                        "dataset source row exceeds its record charge",
                        span,
                        None,
                    )
                })?;
            admit(input_charge).map_err(|e| budget_failure(e, span))?;
            let value = self.page.pop_front().expect("admitted source row");
            let value = value.with_provenance(self.value.provenance().merge(value.provenance()));
            let start = self.ordinal as u64;
            self.ordinal += 1;
            return Ok(SourcePoll::Record {
                value,
                start,
                end: self.ordinal as u64,
                input_charge,
            });
        }
        let Some(framer) = &mut self.framer else {
            let Data::List(items) = self.value.data() else {
                unreachable!("captured typed-record source");
            };
            let Some(data) = items.get(self.ordinal) else {
                self.ended = true;
                return Ok(SourcePoll::End);
            };
            // Reserve before copying this record's containers. Shared payload bytes
            // still carry their full logical charge through the source/VM handoff.
            let input_charge = crate::value_size::data_charge(data, self.record_charge)
                .ok_or_else(|| {
                    SourceFailure::record_limit(
                        "scan input record exceeds its charge limit",
                        span,
                        None,
                    )
                })?;
            let shell = crate::value_size::value_shell_charge(&self.value, self.record_charge)
                .ok_or_else(|| {
                    SourceFailure::record_limit(
                        "scan source attribution and item declaration exceed the record charge",
                        span,
                        None,
                    )
                })?;
            let conversion = input_charge
                .checked_add(shell)
                .filter(|n| *n <= self.record_charge)
                .ok_or_else(|| {
                    SourceFailure::record_limit(
                        "scan typed record conversion exceeds its charge limit",
                        span,
                        None,
                    )
                })?;
            admit(conversion).map_err(|error| budget_failure(error, span))?;
            let shape = match self.value.shape() {
                Shape::List(item) => item.as_ref().clone(),
                _ => Shape::Unknown,
            };
            let value = Value::new(shape, data.clone(), self.value.provenance().clone())
                .map_err(|_| {
                    Failure::new(
                        "CAL004",
                        span,
                        "captured scan record does not match its item shape",
                    )
                })?
                .with_metadata(
                    self.value
                        .metadata()
                        .and_then(|metadata| metadata.project("/e")),
                );
            let start = self.ordinal as u64;
            self.ordinal += 1;
            return Ok(SourcePoll::Record {
                value,
                start,
                end: self.ordinal as u64,
                input_charge,
            });
        };
        let bytes = match self.value.data() {
            Data::Text(text) => text.as_bytes(),
            Data::Bytes(bytes) => bytes.as_ref(),
            _ => unreachable!("captured byte source"),
        };
        let row = if self.position < bytes.len() {
            let end = self.position + block.min(bytes.len() - self.position);
            let (consumed, row) = framer
                .pull_admitted(&bytes[self.position..end], |amount| {
                    if token.is_cancelled() {
                        return Err(Admission::Cancelled);
                    }
                    admit((amount as u64) * 16).map_err(Admission::Budget)
                })
                .map_err(|error| match error {
                    framing::Error::Admission(Admission::Cancelled) => {
                        SourceFailure::from(Failure::cancelled(span))
                    }
                    framing::Error::Admission(Admission::Budget(error)) => {
                        budget_failure(error, span)
                    }
                    error => framing_failure(without_admission(error), span),
                })?;
            self.position += consumed;
            row
        } else {
            self.ended = true;
            let mut row = None;
            framer
                .finish(|record| {
                    row = Some(record);
                    Ok::<_, ()>(())
                })
                .map_err(|error| framing_failure(error, span))?;
            row
        };
        let Some(row) = row else {
            return Ok(if self.ended {
                SourcePoll::End
            } else {
                SourcePoll::Pending
            });
        };
        let start = row.source.start;
        let end = row.delimiter.end;
        let original = Some(framing::ByteSpan { start, end });
        // Admit conversion containers before constructing the public framed value.
        // The native framer's bounded buffers have a separate held reservation.
        let shell = crate::value_size::value_shell_charge(&self.value, self.record_charge)
            .ok_or_else(|| {
                SourceFailure::record_limit(
                    "scan source attribution exceeds the frame charge",
                    span,
                    original,
                )
            })?;
        let conversion = (row.spans.len() as u64)
            .checked_mul(2048)
            .and_then(|n| n.checked_add((row.raw.len() as u64).checked_mul(2)?))
            .and_then(|n| n.checked_add((row.text.len() as u64).checked_mul(6)?))
            .and_then(|n| n.checked_add(4096))
            .and_then(|n| n.checked_add(shell))
            .filter(|n| *n <= self.record_charge);
        let conversion = conversion.ok_or_else(|| {
            SourceFailure::record_limit(
                "decoded scan frame exceeds its conversion charge limit",
                span,
                original,
            )
        })?;
        admit(conversion).map_err(|error| {
            let mut failure = budget_failure(error, span);
            failure.source_span = original;
            failure
        })?;
        let value = frame_value(row, self.value.provenance().clone());
        if crate::value_size::value_charge(&value, self.record_charge).is_none() {
            return Err(SourceFailure::record_limit(
                "decoded scan frame exceeds its input-record charge limit",
                span,
                original,
            ));
        }
        Ok(SourcePoll::Record {
            value,
            start,
            end,
            input_charge: end - start,
        })
    }
}
fn without_admission<E>(error: framing::Error<E>) -> framing::Error<()> {
    use framing::Error::*;
    match error {
        InvalidProfile => InvalidProfile,
        Closed => Closed,
        PositionExhausted => PositionExhausted,
        RawLimit(at) => RawLimit(at),
        DecodedLimit(at) => DecodedLimit(at),
        SpanLimit(at) => SpanLimit(at),
        InvalidUtf8(at) => InvalidUtf8(at),
        Admission(_) => unreachable!("admission handled before framing error conversion"),
    }
}
pub fn frame_shape() -> Shape {
    let int = Shape::Primitive(Primitive::Int);
    let bool_ = Shape::Primitive(Primitive::Bool);
    let Shape::Record(normalized) = normalized_shape() else {
        unreachable!()
    };
    Shape::Record(
        RecordShape::new(
            "",
            [
                ("ordinal".into(), int.clone()),
                ("start".into(), int.clone()),
                ("end".into(), int.clone()),
                ("delimiterStart".into(), int.clone()),
                ("delimiterEnd".into(), int),
                ("raw".into(), Shape::Primitive(Primitive::Bytes)),
                ("text".into(), Shape::Primitive(Primitive::Text)),
                (
                    "decodeSpans".into(),
                    normalized.field("spans").unwrap().clone(),
                ),
                ("lossy".into(), bool_.clone()),
                ("unterminated".into(), bool_),
            ],
        )
        .expect("frame fields"),
    )
}
fn frame_value(row: framing::Record, provenance: Provenance) -> Value {
    let spans = row
        .spans
        .into_iter()
        .map(|s| {
            Data::Record(
                [
                    ("inputStart".into(), Data::Int(s.input_start as i64)),
                    ("inputEnd".into(), Data::Int(s.input_end as i64)),
                    ("outputStart".into(), Data::Int(s.output_start as i64)),
                    ("outputEnd".into(), Data::Int(s.output_end as i64)),
                ]
                .into(),
            )
        })
        .collect();
    Value::new(
        frame_shape(),
        Data::Record(
            [
                ("ordinal".into(), Data::Int(row.ordinal as i64)),
                ("start".into(), Data::Int(row.source.start as i64)),
                ("end".into(), Data::Int(row.source.end as i64)),
                (
                    "delimiterStart".into(),
                    Data::Int(row.delimiter.start as i64),
                ),
                ("delimiterEnd".into(), Data::Int(row.delimiter.end as i64)),
                ("raw".into(), Data::Bytes(row.raw.into())),
                ("text".into(), Data::Text(row.text.into())),
                ("decodeSpans".into(), Data::List(spans)),
                ("lossy".into(), Data::Bool(row.lossy)),
                ("unterminated".into(), Data::Bool(row.unterminated)),
            ]
            .into(),
        ),
        provenance,
    )
    .expect("framer emits its declared record shape")
}
fn framing_failure(error: framing::Error<()>, span: Span) -> SourceFailure {
    use framing::Error::*;
    let source_span = match &error {
        InvalidUtf8(at) | RawLimit(at) | DecodedLimit(at) | SpanLimit(at) => Some(*at),
        _ => None,
    };
    let (code, message) = match error {
        InvalidProfile => ("CAL004", "invalid scan framing profile"),
        InvalidUtf8(_) => (
            "CAL016",
            "scan source contains invalid UTF-8 under its strict decoding profile",
        ),
        RawLimit(_) => ("CAL006", "scan raw record exceeds its declared byte limit"),
        DecodedLimit(_) => (
            "CAL006",
            "scan decoded record exceeds its declared byte limit",
        ),
        SpanLimit(_) => (
            "CAL006",
            "scan decoding map exceeds its declared span limit",
        ),
        PositionExhausted => ("CAL006", "scan source position exhausted"),
        Closed | Admission(()) => ("CAL003", "scan source cursor is not readable"),
    };
    SourceFailure {
        failure: Failure::new(code, span, message),
        source_span,
        dimension: None,
    }
}
