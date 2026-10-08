use crate::codec::{self, Limits as ValueLimits};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::io::Write;
use thiserror::Error;
use uuid::Uuid;
use wes_core::{
    Data, Shape, Value,
    contracts::{Contract, ContractKind, ResolvedContractBundle, metadata::ValueMetadata},
};

const MAGIC: &[u8; 8] = b"WESSEG04";
const END: &[u8; 8] = b"WESEND04";
const FOOTER: usize = 8 + 8 + 8 + 32;
const PREFIX: usize = 8 + 2 + 2 + 4;
const FRAME_PREFIX: usize = 8 + 8 + 8 + 4;

#[derive(Clone, Copy, Debug)]
pub struct FormatLimits {
    pub segment_bytes: usize,
    pub header_bytes: usize,
    pub records: usize,
    pub record_bytes: usize,
    pub validation_work: usize,
    pub value: ValueLimits,
}
impl Default for FormatLimits {
    fn default() -> Self {
        Self {
            segment_bytes: 8 * 1024 * 1024,
            header_bytes: 64 * 1024,
            records: 4096,
            record_bytes: 1024 * 1024,
            validation_work: 1_000_000,
            value: ValueLimits {
                bytes: 1024 * 1024,
                nodes: 100_000,
            },
        }
    }
}
#[derive(Debug, Error)]
pub enum FormatError {
    #[error("dataset format exceeds its {0} limit")]
    Limit(&'static str),
    #[error("dataset contains private or unknown-policy data")]
    Restricted,
    #[error("invalid or corrupt dataset object")]
    Corrupt,
    #[error("unsupported dataset format or payload codec version")]
    Version,
    #[error("dataset row does not satisfy its captured contract")]
    Contract,
    #[error("dataset row is not inline data")]
    NonInline,
    #[error("retained row codec failed")]
    Codec(#[from] codec::CodecError),
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PositionUnit {
    Bytes,
    Records,
}
pub use wes_core::DatasetStream as Stream;
/// Positions refer to the original captured source, never display-text offsets.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SourceRange {
    pub identity: String,
    pub unit: PositionUnit,
    pub start: u64,
    pub end: u64,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SegmentHeader {
    pub store: String,
    pub dataset: String,
    pub stream: Stream,
    pub coverage: Option<SegmentCoverage>,
    pub schema: String,
    pub first: u64,
    pub count: u64,
    pub source: SourceRange,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SegmentCoverage {
    pub policy: wes_engine::storage::datasets::CoveragePolicy,
    pub span: wes_engine::storage::datasets::CoverageSpan,
}
#[derive(Clone, Debug)]
pub struct Record {
    pub source_start: u64,
    pub source_end: u64,
    pub value: Value,
}
/// Bound an entire segment before constructing it; never encode private payloads for disk.
pub fn encode_segment(
    header: &SegmentHeader,
    records: &[Record],
    schema: &ResolvedContractBundle,
    limits: FormatLimits,
) -> Result<Vec<u8>, FormatError> {
    validate_limits(limits)?;
    validate_header(header, schema, limits)?;
    if records.len() != usize::try_from(header.count).map_err(|_| FormatError::Corrupt)? {
        return Err(FormatError::Corrupt);
    }
    let mut remaining = limits.validation_work;
    inline_contract(schema.root(), &mut remaining, 0)?;
    let expected_shape = schema.root().shape();
    let mut coverage = None;
    let mut previous = header.source.start;
    for record in records {
        validate_range(
            record.source_start,
            record.source_end,
            previous,
            &header.source,
        )?;
        previous = record.source_start;
        validate_value(
            &record.value,
            schema.root(),
            &expected_shape,
            &mut remaining,
        )?;
        fold_coverage(
            &mut coverage,
            header,
            &record.value,
            record.source_start,
            record.source_end,
            &mut remaining,
        )?;
    }
    if coverage.as_ref() != header.coverage.as_ref().map(|c| &c.span) {
        return Err(FormatError::Corrupt);
    }
    let header_bytes = bounded_json(header, limits.header_bytes)?;
    let mut bytes = Vec::new();
    append(&mut bytes, MAGIC, limits.segment_bytes)?;
    append(&mut bytes, &4u16.to_le_bytes(), limits.segment_bytes)?;
    append(&mut bytes, &2u16.to_le_bytes(), limits.segment_bytes)?;
    append(
        &mut bytes,
        &u32::try_from(header_bytes.len())
            .map_err(|_| FormatError::Limit("header"))?
            .to_le_bytes(),
        limits.segment_bytes,
    )?;
    append(&mut bytes, &header_bytes, limits.segment_bytes)?;
    let mut value_limits = limits.value;
    value_limits.bytes = value_limits.bytes.min(limits.record_bytes);
    for (index, record) in records.iter().enumerate() {
        // Every row has this segment's pinned element declaration. Its full
        // metadata belongs to that captured schema, rather than a redundant
        // independently parsed copy in every row. Data and policy remain in
        // the exact retained-value codec. Reader restores metadata natively.
        let value = record.value.clone().with_metadata(None);
        let payload = codec::encode_value(&value, value_limits)?;
        let ordinal = header
            .first
            .checked_add(index as u64)
            .ok_or(FormatError::Corrupt)?;
        let mut prefix = Vec::with_capacity(FRAME_PREFIX);
        prefix.extend_from_slice(&ordinal.to_le_bytes());
        prefix.extend_from_slice(&record.source_start.to_le_bytes());
        prefix.extend_from_slice(&record.source_end.to_le_bytes());
        prefix.extend_from_slice(
            &u32::try_from(payload.len())
                .map_err(|_| FormatError::Limit("row"))?
                .to_le_bytes(),
        );
        let mut hash = Sha256::new();
        hash.update(&prefix);
        hash.update(&payload);
        append(&mut bytes, &prefix, limits.segment_bytes)?;
        append(&mut bytes, &payload, limits.segment_bytes)?;
        append(&mut bytes, &hash.finalize(), limits.segment_bytes)?;
    }
    let total = bytes
        .len()
        .checked_add(FOOTER)
        .ok_or(FormatError::Limit("segment"))?;
    append(&mut bytes, END, limits.segment_bytes)?;
    append(
        &mut bytes,
        &header.count.to_le_bytes(),
        limits.segment_bytes,
    )?;
    append(
        &mut bytes,
        &(total as u64).to_le_bytes(),
        limits.segment_bytes,
    )?;
    let checksum = Sha256::digest(&bytes);
    append(&mut bytes, &checksum, limits.segment_bytes)?;
    Ok(bytes)
}
#[derive(Clone, Debug)]
struct Location {
    start: usize,
    end: usize,
    source_start: u64,
    source_end: u64,
}
/// Verified immutable segment. Opening has bounded work; pages never scan unrelated segments.
pub struct SegmentReader<'a> {
    bytes: &'a [u8],
    header: SegmentHeader,
    locations: Vec<Location>,
    limits: FormatLimits,
    metadata: ValueMetadata,
}
impl<'a> SegmentReader<'a> {
    pub fn open(
        bytes: &'a [u8],
        store: &str,
        dataset: &str,
        stream: Stream,
        schema: &ResolvedContractBundle,
        limits: FormatLimits,
    ) -> Result<Self, FormatError> {
        validate_limits(limits)?;
        if bytes.len() > limits.segment_bytes {
            return Err(FormatError::Limit("segment"));
        }
        if bytes.len() < PREFIX + FOOTER || &bytes[..8] != MAGIC {
            return Err(FormatError::Corrupt);
        }
        let mut input = Input::new(bytes);
        input.take(8)?;
        if input.u16()? != 4 || input.u16()? != 2 {
            return Err(FormatError::Version);
        }
        let header_len = input.u32()? as usize;
        if header_len > limits.header_bytes {
            return Err(FormatError::Limit("header"));
        }
        let raw_header = input.take(header_len)?;
        let header: SegmentHeader =
            serde_json::from_slice(raw_header).map_err(|_| FormatError::Corrupt)?;
        if bounded_json(&header, limits.header_bytes)? != raw_header {
            return Err(FormatError::Corrupt);
        }
        validate_header(&header, schema, limits)?;
        if header.store != store || header.dataset != dataset || header.stream != stream {
            return Err(FormatError::Corrupt);
        }
        let body_end = bytes.len() - FOOTER;
        if input.position > body_end {
            return Err(FormatError::Corrupt);
        }
        // Integrity is checked before decoding any record payload.
        let digest = Sha256::digest(&bytes[..bytes.len() - 32]);
        if digest.as_slice() != &bytes[bytes.len() - 32..] {
            return Err(FormatError::Corrupt);
        }
        let mut footer = Input::new(&bytes[body_end..]);
        if footer.take(8)? != END
            || footer.u64()? != header.count
            || footer.u64()? != bytes.len() as u64
        {
            return Err(FormatError::Corrupt);
        }
        let mut locations = Vec::new();
        let mut remaining = limits.validation_work;
        inline_contract(schema.root(), &mut remaining, 0)?;
        let expected_shape = schema.root().shape();
        let mut coverage = None;
        let metadata = ValueMetadata::capture(schema.root());
        let mut previous = header.source.start;
        for ordinal in 0..header.count {
            let frame_start = input.position;
            if input.u64()?
                != header
                    .first
                    .checked_add(ordinal)
                    .ok_or(FormatError::Corrupt)?
            {
                return Err(FormatError::Corrupt);
            }
            let source_start = input.u64()?;
            let source_end = input.u64()?;
            validate_range(source_start, source_end, previous, &header.source)?;
            previous = source_start;
            let length = input.u32()? as usize;
            if length > limits.record_bytes || length > limits.value.bytes {
                return Err(FormatError::Limit("row"));
            }
            let start = input.position;
            input.take(length)?;
            let end = input.position;
            let checksum = input.take(32)?;
            if input.position > body_end
                || Sha256::digest(&bytes[frame_start..end]).as_slice() != checksum
            {
                return Err(FormatError::Corrupt);
            }
            let value = codec::decode_value(&bytes[start..end], limits.value)?.value;
            validate_value(&value, schema.root(), &expected_shape, &mut remaining)?;
            fold_coverage(
                &mut coverage,
                &header,
                &value,
                source_start,
                source_end,
                &mut remaining,
            )?;
            // A row may not supply a competing declaration. The digest-bound
            // schema is the only source of its captured metadata.
            if value.metadata().is_some() {
                return Err(FormatError::Contract);
            }
            locations.push(Location {
                start,
                end,
                source_start,
                source_end,
            });
        }
        if input.position != body_end {
            return Err(FormatError::Corrupt);
        }
        if coverage.as_ref() != header.coverage.as_ref().map(|c| &c.span) {
            return Err(FormatError::Corrupt);
        }
        Ok(Self {
            bytes,
            header,
            locations,
            limits,
            metadata,
        })
    }
    pub fn header(&self) -> &SegmentHeader {
        &self.header
    }
    fn location(&self, ordinal: u64) -> Option<&Location> {
        let index = ordinal
            .checked_sub(self.header.first)
            .and_then(|n| usize::try_from(n).ok())?;
        self.locations.get(index)
    }
    /// Length of the validated stored row payload, excluding its frame and schema.
    /// Opening checked its checksum, codec and contract; this does not decode it again.
    pub fn encoded_row_bytes(&self, ordinal: u64) -> Option<usize> {
        self.location(ordinal)
            .map(|location| location.end - location.start)
    }
    pub fn row(&self, ordinal: u64) -> Result<Option<Record>, FormatError> {
        let Some(location) = self.location(ordinal) else {
            return Ok(None);
        };
        let value =
            codec::decode_value(&self.bytes[location.start..location.end], self.limits.value)?
                .value
                .with_metadata(Some(self.metadata.clone()));
        // Bytes are immutable and were fully validated at open; no current registry lookup.
        Ok(Some(Record {
            value,
            source_start: location.source_start,
            source_end: location.source_end,
        }))
    }
}
fn validate_limits(limits: FormatLimits) -> Result<(), FormatError> {
    if limits.segment_bytes < PREFIX + FOOTER
        || limits.header_bytes == 0
        || limits.records == 0
        || limits.record_bytes == 0
        || limits.validation_work == 0
        || limits.value.nodes == 0
        || limits.value.bytes == 0
    {
        return Err(FormatError::Corrupt);
    }
    Ok(())
}
fn validate_header(
    header: &SegmentHeader,
    schema: &ResolvedContractBundle,
    limits: FormatLimits,
) -> Result<(), FormatError> {
    match (&header.stream, &header.coverage) {
        (Stream::Outputs, None) => {}
        (Stream::Coverage, Some(c))
            if c.policy.valid()
                && header.source.unit == PositionUnit::Bytes
                && c.span.valid(header.count)
                && c.span.from == header.source.start
                && c.span.through == header.source.end
                && schema.digest()
                    == wes_engine::storage::datasets::rejection_schema().digest() => {}
        _ => return Err(FormatError::Corrupt),
    }
    for id in [&header.store, &header.dataset] {
        if Uuid::parse_str(id).is_err()
            || Uuid::parse_str(id).unwrap().hyphenated().to_string() != *id
        {
            return Err(FormatError::Corrupt);
        }
    }
    if header.schema != schema.digest()
        || header.source.identity.is_empty()
        || header.source.identity.len() > 4096
        || header.source.identity.chars().any(char::is_control)
        || header.source.start > header.source.end
        || header.first.checked_add(header.count).is_none()
    {
        return Err(FormatError::Corrupt);
    }
    if header.count > limits.records as u64 {
        return Err(FormatError::Limit("records"));
    }
    Ok(())
}
fn fold_coverage(
    span: &mut Option<wes_engine::storage::datasets::CoverageSpan>,
    header: &SegmentHeader,
    value: &Value,
    source_start: u64,
    source_end: u64,
    remaining: &mut usize,
) -> Result<(), FormatError> {
    if let Some(coverage) = &header.coverage {
        let excerpt_bytes = match value.data() {
            Data::Record(fields) => match fields.get("excerpt") {
                Some(Data::Bytes(bytes)) => bytes.len(),
                _ => return Err(FormatError::Corrupt),
            },
            _ => return Err(FormatError::Corrupt),
        };
        *remaining = remaining
            .checked_sub(excerpt_bytes.saturating_add(128))
            .ok_or(FormatError::Limit("validation"))?;
        let row = wes_engine::storage::datasets::read_rejection(value, coverage.policy)
            .map_err(|_| FormatError::Corrupt)?;
        if row.source.start != source_start || row.delimiter.end != source_end {
            return Err(FormatError::Corrupt);
        }
        let next = wes_engine::storage::datasets::CoverageSpan::single(&row)
            .map_err(|_| FormatError::Corrupt)?;
        match span {
            Some(span) => span.merge(&next).map_err(|_| FormatError::Corrupt)?,
            None => *span = Some(next),
        }
    }
    Ok(())
}
fn validate_range(
    start: u64,
    end: u64,
    previous: u64,
    source: &SourceRange,
) -> Result<(), FormatError> {
    if start < previous || start > end || end > source.end {
        return Err(FormatError::Corrupt);
    }
    Ok(())
}
pub(crate) fn validate_value(
    value: &Value,
    contract: &Contract,
    expected_shape: &Shape,
    remaining: &mut usize,
) -> Result<(), FormatError> {
    let policy = value.provenance().policy();
    if policy.is_private() || policy.is_unknown() {
        return Err(FormatError::Restricted);
    }
    inline_shape(value.shape(), remaining, 0)?;
    inline_value(value.data(), remaining, 0)?;
    if !value.shape().is_assignable_to(expected_shape) {
        return Err(FormatError::Contract);
    }
    let issues = contract
        .issues_with_budget(value.data(), &|| false, remaining)
        .map_err(|_| FormatError::Limit("validation"))?;
    if issues.iter().any(|issue| issue.code == "TYP006") {
        return Err(FormatError::Limit("validation"));
    }
    if !issues.is_empty() {
        return Err(FormatError::Contract);
    }
    Ok(())
}
fn inline_shape(shape: &Shape, remaining: &mut usize, depth: usize) -> Result<(), FormatError> {
    charge(remaining, depth)?;
    match shape {
        Shape::Iter(_) | Shape::Dataset(_) | Shape::Meta(_) => Err(FormatError::NonInline),
        Shape::List(element) | Shape::Option(element) => {
            inline_shape(element, remaining, depth + 1)
        }
        Shape::Record(fields) => {
            for (_, shape) in fields.fields() {
                inline_shape(shape, remaining, depth + 1)?;
            }
            Ok(())
        }
        Shape::Unknown | Shape::Primitive(_) => Ok(()),
    }
}
fn charge(remaining: &mut usize, depth: usize) -> Result<(), FormatError> {
    if depth > 64 {
        return Err(FormatError::Limit("depth"));
    }
    *remaining = remaining
        .checked_sub(1)
        .ok_or(FormatError::Limit("validation"))?;
    Ok(())
}
fn inline_contract(
    contract: &Contract,
    remaining: &mut usize,
    depth: usize,
) -> Result<(), FormatError> {
    charge(remaining, depth)?;
    match contract.kind() {
        ContractKind::Iter(_) | ContractKind::Dataset(_) => Err(FormatError::NonInline),
        ContractKind::Record(fields) => {
            for field in fields.values() {
                inline_contract(&field.contract, remaining, depth + 1)?;
            }
            Ok(())
        }
        ContractKind::List(c) | ContractKind::Option(c) => inline_contract(c, remaining, depth + 1),
        ContractKind::Map(a, b) | ContractKind::Union(a, b) => {
            inline_contract(a, remaining, depth + 1)?;
            inline_contract(b, remaining, depth + 1)
        }
        ContractKind::Scalar(_) | ContractKind::Unknown => Ok(()),
    }
}
fn inline_value(data: &Data, remaining: &mut usize, depth: usize) -> Result<(), FormatError> {
    charge(remaining, depth)?;
    match data {
        Data::Iter(_) | Data::Dataset(_) => Err(FormatError::NonInline),
        Data::List(items) => {
            for item in items {
                inline_value(item, remaining, depth + 1)?;
            }
            Ok(())
        }
        Data::Record(fields) => {
            for item in fields.values() {
                inline_value(item, remaining, depth + 1)?;
            }
            Ok(())
        }
        Data::Option(Some(item)) => inline_value(item, remaining, depth + 1),
        Data::Option(None)
        | Data::Text(_)
        | Data::Int(_)
        | Data::Decimal(_)
        | Data::Bool(_)
        | Data::Instant(_)
        | Data::Duration(_)
        | Data::Interval(_)
        | Data::Bytes(_) => Ok(()),
    }
}
pub(super) fn bounded_json(value: &impl Serialize, limit: usize) -> Result<Vec<u8>, FormatError> {
    struct Buffer {
        bytes: Vec<u8>,
        limit: usize,
    }
    impl Write for Buffer {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            if self
                .bytes
                .len()
                .checked_add(bytes.len())
                .is_none_or(|n| n > self.limit)
            {
                return Err(std::io::Error::other("dataset encoding limit"));
            }
            self.bytes.extend_from_slice(bytes);
            Ok(bytes.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    let mut writer = Buffer {
        bytes: vec![],
        limit,
    };
    serde_json::to_writer(&mut writer, value).map_err(|_| FormatError::Limit("encoding"))?;
    Ok(writer.bytes)
}
fn append(output: &mut Vec<u8>, bytes: &[u8], limit: usize) -> Result<(), FormatError> {
    if output
        .len()
        .checked_add(bytes.len())
        .is_none_or(|n| n > limit)
    {
        return Err(FormatError::Limit("segment"));
    }
    output.extend_from_slice(bytes);
    Ok(())
}
pub(super) struct Input<'a> {
    pub(super) bytes: &'a [u8],
    pub(super) position: usize,
}
impl<'a> Input<'a> {
    pub(super) fn new(bytes: &'a [u8]) -> Self {
        Self { bytes, position: 0 }
    }
    pub(super) fn take(&mut self, count: usize) -> Result<&'a [u8], FormatError> {
        let end = self
            .position
            .checked_add(count)
            .ok_or(FormatError::Corrupt)?;
        let slice = self
            .bytes
            .get(self.position..end)
            .ok_or(FormatError::Corrupt)?;
        self.position = end;
        Ok(slice)
    }
    pub(super) fn u16(&mut self) -> Result<u16, FormatError> {
        Ok(u16::from_le_bytes(self.take(2)?.try_into().unwrap()))
    }
    pub(super) fn u32(&mut self) -> Result<u32, FormatError> {
        Ok(u32::from_le_bytes(self.take(4)?.try_into().unwrap()))
    }
    pub(super) fn u64(&mut self) -> Result<u64, FormatError> {
        Ok(u64::from_le_bytes(self.take(8)?.try_into().unwrap()))
    }
}
