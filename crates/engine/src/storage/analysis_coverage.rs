//! Bounded rejected-frame evidence. Ordinals and offsets use exact unsigned text.
use super::StoreError;
use serde::{Deserialize, Serialize};
use wes_core::{
    Data, Provenance, Value,
    contracts::{ContractRegistry, ResolvedContractBundle, SnapshotLimits},
    flow::FlowPolicy,
    framing::{ByteSpan, Rejection, RejectionReason},
};

/// Shared conservative logical reservation for native evidence and its unpublished copies.
/// Ordinary output row counts remain independent of this retained charge.
pub fn rejection_charge(row: &Rejection) -> Option<u64> {
    rejection_reservation(row.excerpt.capacity() as u64)
}
pub fn rejection_reservation(excerpt_capacity: u64) -> Option<u64> {
    excerpt_capacity.checked_mul(32)?.checked_add(8192)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CoveragePolicy {
    pub excerpt_bytes: u32,
}
impl CoveragePolicy {
    pub fn valid(self) -> bool {
        (1..=4096).contains(&self.excerpt_bytes)
    }
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CoverageProgress {
    pub policy: CoveragePolicy,
    pub records: u64,
    /// Original rejected content and its delimiter, counted once at acknowledgement.
    pub input_bytes: u64,
    pub last_ordinal: Option<u64>,
    pub through: Option<u64>,
}
impl CoverageProgress {
    pub fn empty(policy: CoveragePolicy) -> Self {
        Self {
            policy,
            records: 0,
            input_bytes: 0,
            last_ordinal: None,
            through: None,
        }
    }
    pub fn valid(&self, inputs: u64, position: u64, input_bytes: u64) -> bool {
        self.policy.valid()
            && self.records <= inputs
            && self.input_bytes <= input_bytes
            && if self.records == 0 {
                self.input_bytes == 0 && self.last_ordinal.is_none() && self.through.is_none()
            } else {
                self.input_bytes > 0
                    && self
                        .last_ordinal
                        .is_some_and(|n| n >= self.records - 1 && n < inputs)
                    && self.through.is_some_and(|n| n > 0 && n <= position)
            }
    }
    pub fn extends(&self, old: &Self) -> bool {
        self.policy == old.policy
            && self.records >= old.records
            && self.input_bytes >= old.input_bytes
            && self.last_ordinal >= old.last_ordinal
            && self.through >= old.through
            && (self.records != old.records || self == old)
    }
    pub fn observe(
        &mut self,
        row: &Rejection,
        inputs: u64,
        position: u64,
    ) -> Result<(), StoreError> {
        if !self.policy.valid()
            || row.ordinal >= inputs
            || row.source.start >= row.source.end
            || row.delimiter.start != row.source.end
            || row.delimiter.end < row.delimiter.start
            || row.delimiter.end > position
            || row.unterminated != (row.delimiter.start == row.delimiter.end)
            || row.reason_span.start < row.source.start
            || row.reason_span.end > row.source.end
            || row.reason_span.start >= row.reason_span.end
            || row.excerpt.len() as u64
                != (self.policy.excerpt_bytes as u64).min(row.source.end - row.source.start)
            || row.excerpt.len() as u64 > row.source.end - row.source.start
            || row.excerpt_truncated
                != ((row.excerpt.len() as u64) < row.source.end - row.source.start)
            || self.last_ordinal.is_some_and(|n| row.ordinal <= n)
            || self.through.is_some_and(|n| row.source.start < n)
        {
            return Err(StoreError::Conflict);
        }
        let records = self
            .records
            .checked_add(1)
            .ok_or(StoreError::Limit("coverage records"))?;
        let input_bytes = self
            .input_bytes
            .checked_add(row.delimiter.end - row.source.start)
            .ok_or(StoreError::Limit("coverage input bytes"))?;
        self.records = records;
        self.input_bytes = input_bytes;
        self.last_ordinal = Some(row.ordinal);
        self.through = Some(row.delimiter.end);
        Ok(())
    }
}
#[derive(Clone, Debug)]
pub struct CoverageInfo {
    pub progress: CoverageProgress,
    pub schema: ResolvedContractBundle,
    pub segment_bytes: u64,
}

/// Immutable aggregate for a contiguous slice of the exceptional-observation tree.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CoverageSpan {
    pub first_ordinal: u64,
    pub last_ordinal: u64,
    pub from: u64,
    pub through: u64,
    pub input_bytes: u64,
}
impl CoverageSpan {
    pub fn single(row: &Rejection) -> Result<Self, StoreError> {
        let input_bytes = row
            .delimiter
            .end
            .checked_sub(row.source.start)
            .filter(|n| *n > 0)
            .ok_or(StoreError::Conflict)?;
        Ok(Self {
            first_ordinal: row.ordinal,
            last_ordinal: row.ordinal,
            from: row.source.start,
            through: row.delimiter.end,
            input_bytes,
        })
    }
    pub fn valid(&self, records: u64) -> bool {
        records > 0
            && self.first_ordinal <= self.last_ordinal
            && records - 1 <= self.last_ordinal - self.first_ordinal
            && self.from < self.through
            && self.input_bytes >= records
            && self.input_bytes <= self.through - self.from
    }
    pub fn merge(&mut self, next: &Self) -> Result<(), StoreError> {
        if !self.valid(1)
            || !next.valid(1)
            || self.last_ordinal >= next.first_ordinal
            || self.through > next.from
        {
            return Err(StoreError::Conflict);
        }
        let bytes = self
            .input_bytes
            .checked_add(next.input_bytes)
            .ok_or(StoreError::Limit("coverage input bytes"))?;
        self.last_ordinal = next.last_ordinal;
        self.through = next.through;
        self.input_bytes = bytes;
        Ok(())
    }
    pub fn matches(&self, progress: &CoverageProgress) -> bool {
        progress.input_bytes == self.input_bytes
            && progress.last_ordinal == Some(self.last_ordinal)
            && progress.through == Some(self.through)
    }
}

pub fn rejection_schema() -> ResolvedContractBundle {
    static SCHEMA: std::sync::OnceLock<ResolvedContractBundle> = std::sync::OnceLock::new();
    SCHEMA.get_or_init(|| {
    let mut registry = ContractRegistry::new();
    registry.load("version: 2\ntypes:\n  ScanRejectionKind: {base: Text, enum: [rejected]}\n  ScanRejectionReason: {base: Text, enum: [raw_limit, invalid_utf8, decoded_limit, span_limit]}\n  ScanRejection:\n    base: Record\n    fields:\n      kind: ScanRejectionKind\n      reason: ScanRejectionReason\n      recordOrdinal: Text\n      sourceStart: Text\n      sourceEnd: Text\n      delimiterStart: Text\n      delimiterEnd: Text\n      unterminated: Bool\n      reasonStart: Text\n      reasonEnd: Text\n      excerpt: Bytes\n      excerptStart: Text\n      excerptTruncated: Bool\n").expect("native rejection contract");
    ResolvedContractBundle::capture(registry.resolve("ScanRejection").expect("native record"), SnapshotLimits::default())
        .expect("bounded native rejection schema")
    }).clone()
}
pub fn rejection_value(row: &Rejection, policy: &FlowPolicy) -> Result<Value, StoreError> {
    let text = |n: u64| Data::Text(n.to_string().into());
    Value::new(
        rejection_schema().root().shape(),
        Data::Record(
            [
                ("kind".into(), Data::Text("rejected".into())),
                ("reason".into(), Data::Text(row.reason.name().into())),
                ("recordOrdinal".into(), text(row.ordinal)),
                ("sourceStart".into(), text(row.source.start)),
                ("sourceEnd".into(), text(row.source.end)),
                ("delimiterStart".into(), text(row.delimiter.start)),
                ("delimiterEnd".into(), text(row.delimiter.end)),
                ("unterminated".into(), Data::Bool(row.unterminated)),
                ("reasonStart".into(), text(row.reason_span.start)),
                ("reasonEnd".into(), text(row.reason_span.end)),
                ("excerpt".into(), Data::Bytes(row.excerpt.clone().into())),
                ("excerptStart".into(), text(row.source.start)),
                ("excerptTruncated".into(), Data::Bool(row.excerpt_truncated)),
            ]
            .into(),
        ),
        Provenance::default().with_policy(policy),
    )
    .map_err(|_| StoreError::Conflict)
}

/// Decode evidence through its native contract, preserving unsigned positions exactly.
pub fn read_rejection(value: &Value, policy: CoveragePolicy) -> Result<Rejection, StoreError> {
    if !policy.valid() || value.shape() != &rejection_schema().root().shape() {
        return Err(StoreError::DatasetCorrupt);
    }
    let Data::Record(fields) = value.data() else {
        return Err(StoreError::DatasetCorrupt);
    };
    let text = |name: &str| match fields.get(name) {
        Some(Data::Text(text)) => Ok(text.as_ref()),
        _ => Err(StoreError::DatasetCorrupt),
    };
    let uint = |name: &str| {
        let text = text(name)?;
        let value: u64 = text.parse().map_err(|_| StoreError::DatasetCorrupt)?;
        if text != value.to_string() {
            return Err(StoreError::DatasetCorrupt);
        }
        Ok(value)
    };
    let boolean = |name: &str| match fields.get(name) {
        Some(Data::Bool(value)) => Ok(*value),
        _ => Err(StoreError::DatasetCorrupt),
    };
    if fields.len() != 13 || text("kind")? != "rejected" {
        return Err(StoreError::DatasetCorrupt);
    }
    let reason = match text("reason")? {
        "raw_limit" => RejectionReason::RawLimit,
        "invalid_utf8" => RejectionReason::InvalidUtf8,
        "decoded_limit" => RejectionReason::DecodedLimit,
        "span_limit" => RejectionReason::SpanLimit,
        _ => return Err(StoreError::DatasetCorrupt),
    };
    let excerpt = match fields.get("excerpt") {
        Some(Data::Bytes(bytes)) if bytes.len() <= policy.excerpt_bytes as usize => bytes.to_vec(),
        _ => return Err(StoreError::DatasetCorrupt),
    };
    let row = Rejection {
        ordinal: uint("recordOrdinal")?,
        source: ByteSpan {
            start: uint("sourceStart")?,
            end: uint("sourceEnd")?,
        },
        delimiter: ByteSpan {
            start: uint("delimiterStart")?,
            end: uint("delimiterEnd")?,
        },
        unterminated: boolean("unterminated")?,
        reason,
        reason_span: ByteSpan {
            start: uint("reasonStart")?,
            end: uint("reasonEnd")?,
        },
        excerpt,
        excerpt_truncated: boolean("excerptTruncated")?,
    };
    if uint("excerptStart")? != row.source.start {
        return Err(StoreError::DatasetCorrupt);
    }
    CoverageProgress::empty(policy)
        .observe(
            &row,
            row.ordinal
                .checked_add(1)
                .ok_or(StoreError::DatasetCorrupt)?,
            row.delimiter.end,
        )
        .map_err(|_| StoreError::DatasetCorrupt)?;
    Ok(row)
}

#[cfg(test)]
mod tests {
    use super::*;
    fn row() -> Rejection {
        Rejection {
            ordinal: 7,
            source: ByteSpan {
                start: u64::MAX - 4,
                end: u64::MAX - 1,
            },
            delimiter: ByteSpan {
                start: u64::MAX - 1,
                end: u64::MAX,
            },
            unterminated: false,
            reason: RejectionReason::InvalidUtf8,
            reason_span: ByteSpan {
                start: u64::MAX - 3,
                end: u64::MAX - 2,
            },
            excerpt: b"a\xffb".to_vec(),
            excerpt_truncated: false,
        }
    }
    #[test]
    fn native_evidence_roundtrips_unsigned_offsets_and_keeps_policy_out_of_data() {
        let row = row();
        let policy = CoveragePolicy { excerpt_bytes: 3 };
        let provenance = FlowPolicy::default().from_origin("synthetic-source");
        let value = rejection_value(&row, &provenance).unwrap();
        assert_eq!(read_rejection(&value, policy).unwrap(), row);
        assert!(
            value
                .provenance()
                .policy()
                .origins()
                .contains("synthetic-source")
        );
        let mut progress = CoverageProgress::empty(policy);
        progress.observe(&row, 8, u64::MAX).unwrap();
        assert!(progress.valid(8, u64::MAX, 4));
        let span = CoverageSpan::single(&row).unwrap();
        assert!(span.valid(1));
        assert!(span.matches(&progress));
        let saved = progress.clone();
        assert!(progress.observe(&row, 8, u64::MAX).is_err());
        assert_eq!(progress, saved, "a refused observation is inert");
    }
    #[test]
    fn native_evidence_refuses_noncanonical_counts_wrong_heads_and_changed_extent() {
        let original = rejection_value(&row(), &FlowPolicy::default()).unwrap();
        let policy = CoveragePolicy { excerpt_bytes: 3 };
        for (key, data) in [
            ("recordOrdinal", Data::Text("07".into())),
            ("recordOrdinal", Data::Text("18446744073709551616".into())),
            ("excerptStart", Data::Text("0".into())),
            ("reasonStart", Data::Text("0".into())),
            ("excerptTruncated", Data::Bool(true)),
            ("excerpt", Data::Bytes(b"a".as_slice().into())),
            ("kind", Data::Text("unknown".into())),
        ] {
            let Data::Record(mut fields) = original.data().clone() else {
                panic!("record");
            };
            fields.insert(key.into(), data);
            let value = Value::new(
                original.shape().clone(),
                Data::Record(fields),
                Provenance::default(),
            )
            .unwrap();
            assert!(read_rejection(&value, policy).is_err(), "{key}");
        }
        for cap in [0, 2, 4097] {
            assert!(read_rejection(&original, CoveragePolicy { excerpt_bytes: cap }).is_err());
        }
        let mut invalid = row();
        invalid.delimiter.end = 0;
        assert!(CoverageSpan::single(&invalid).is_err());
        let saved = CoverageSpan::single(&row()).unwrap();
        let mut merged = saved.clone();
        assert!(merged.merge(&saved).is_err());
        assert_eq!(merged, saved);
    }
    #[test]
    fn progress_cannot_add_unreported_bytes_or_change_the_frozen_excerpt_bound() {
        let policy = CoveragePolicy { excerpt_bytes: 3 };
        let mut progress = CoverageProgress::empty(policy);
        progress.observe(&row(), 8, u64::MAX).unwrap();
        assert!(!progress.valid(7, u64::MAX, 4));
        assert!(!progress.valid(8, u64::MAX - 1, 4));
        assert!(!progress.valid(8, u64::MAX, 3));
        let mut changed = progress.clone();
        changed.input_bytes += 1;
        assert!(!changed.extends(&progress));
        let mut changed = progress.clone();
        changed.policy.excerpt_bytes = 4;
        assert!(!changed.extends(&progress));
    }
}
