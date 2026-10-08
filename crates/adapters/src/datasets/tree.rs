//! Persistent ordinal index. Every node is independently bounded; appends copy only a path.
use super::{
    FormatError, SourceRange, Stream,
    catalog::{ObjectRef, valid_digest, valid_uuid},
    format::bounded_json,
};
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug)]
pub struct IndexLimits {
    pub bytes: usize,
    pub fanout: usize,
    pub depth: u8,
    pub traversal_nodes: usize,
}
impl Default for IndexLimits {
    fn default() -> Self {
        Self {
            bytes: 256 * 1024,
            fanout: 64,
            depth: 12,
            traversal_nodes: 65536,
        }
    }
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct IndexSummary {
    pub first: u64,
    pub end: u64,
    /// Physical segment envelopes, once per leaf entry; excludes index/manifest overhead.
    pub segment_bytes: u64,
    pub coverage: Option<wes_engine::storage::datasets::CoverageSpan>,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct IndexEntry {
    pub summary: IndexSummary,
    pub segment: ObjectRef,
    pub source: SourceRange,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Branch {
    pub summary: IndexSummary,
    pub node: ObjectRef,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum Entries {
    Leaf { entries: Vec<IndexEntry> },
    Branch { children: Vec<Branch> },
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct IndexNode {
    pub version: u16,
    pub store: String,
    pub dataset: String,
    pub stream: Stream,
    /// Leaves are height zero. Parent children must have exactly height - 1.
    pub height: u8,
    pub summary: IndexSummary,
    pub entries: Entries,
}
impl IndexNode {
    pub fn encode(&self, limits: IndexLimits) -> Result<Vec<u8>, FormatError> {
        self.validate(limits)?;
        bounded_json(self, limits.bytes)
    }
    pub fn decode(bytes: &[u8], limits: IndexLimits) -> Result<Self, FormatError> {
        if bytes.len() > limits.bytes {
            return Err(FormatError::Limit("index bytes"));
        }
        let node: Self = serde_json::from_slice(bytes).map_err(|_| FormatError::Corrupt)?;
        if node.encode(limits)? != bytes {
            return Err(FormatError::Corrupt);
        }
        Ok(node)
    }
    pub(crate) fn validate(&self, limits: IndexLimits) -> Result<(), FormatError> {
        if limits.fanout < 2 || limits.depth == 0 || limits.traversal_nodes == 0 {
            return Err(FormatError::Limit("index configuration"));
        }
        if self.version != 3 {
            return Err(FormatError::Version);
        }
        if self.height > limits.depth {
            return Err(FormatError::Limit("index depth"));
        }
        if !valid_uuid(&self.store) || !valid_uuid(&self.dataset) {
            return Err(FormatError::Corrupt);
        }
        let mut first = None;
        let mut end = 0;
        let mut segment_bytes = 0u64;
        let mut count = 0usize;
        let mut coverage: Option<wes_engine::storage::datasets::CoverageSpan> = None;
        let mut accept =
            |summary: &IndexSummary, reference: &ObjectRef| -> Result<(), FormatError> {
                validate_reference(reference)?;
                if summary.first >= summary.end
                    || (count != 0 && summary.first != end)
                    || summary.segment_bytes == 0
                {
                    return Err(FormatError::Corrupt);
                }
                first.get_or_insert(summary.first);
                end = summary.end;
                segment_bytes = segment_bytes
                    .checked_add(summary.segment_bytes)
                    .ok_or(FormatError::Corrupt)?;
                match (&self.stream, &summary.coverage) {
                    (Stream::Outputs, None) => {}
                    (Stream::Coverage, Some(next)) if next.valid(summary.end - summary.first) => {
                        match &mut coverage {
                            Some(span) => span.merge(next).map_err(|_| FormatError::Corrupt)?,
                            None => coverage = Some(next.clone()),
                        }
                    }
                    _ => return Err(FormatError::Corrupt),
                }
                count += 1;
                if count > limits.fanout {
                    return Err(FormatError::Limit("index fanout"));
                }
                Ok(())
            };
        match &self.entries {
            Entries::Leaf { entries } if self.height == 0 => {
                for entry in entries {
                    validate_source(&entry.source)?;
                    if entry.summary.segment_bytes != entry.segment.bytes {
                        return Err(FormatError::Corrupt);
                    }
                    accept(&entry.summary, &entry.segment)?;
                }
            }
            Entries::Branch { children } if self.height > 0 => {
                for child in children {
                    accept(&child.summary, &child.node)?;
                }
            }
            _ => return Err(FormatError::Corrupt),
        }
        if count == 0
            || self.summary
                != (IndexSummary {
                    first: first.unwrap(),
                    end,
                    segment_bytes,
                    coverage,
                })
        {
            return Err(FormatError::Corrupt);
        }
        Ok(())
    }
    pub(crate) fn refresh_summary(&mut self) -> Result<(), FormatError> {
        let summaries: Vec<&IndexSummary> = match &self.entries {
            Entries::Leaf { entries } => entries.iter().map(|e| &e.summary).collect(),
            Entries::Branch { children } => children.iter().map(|e| &e.summary).collect(),
        };
        let first = summaries.first().ok_or(FormatError::Corrupt)?.first;
        let end = summaries.last().unwrap().end;
        let segment_bytes = summaries
            .iter()
            .try_fold(0u64, |n, e| n.checked_add(e.segment_bytes))
            .ok_or(FormatError::Corrupt)?;
        let mut coverage: Option<wes_engine::storage::datasets::CoverageSpan> = None;
        for summary in &summaries {
            let records = summary
                .end
                .checked_sub(summary.first)
                .filter(|n| *n > 0)
                .ok_or(FormatError::Corrupt)?;
            match (&self.stream, &summary.coverage) {
                (Stream::Outputs, None) => {}
                (Stream::Coverage, Some(next)) if next.valid(records) => match &mut coverage {
                    Some(span) => span.merge(next).map_err(|_| FormatError::Corrupt)?,
                    None => coverage = Some(next.clone()),
                },
                _ => return Err(FormatError::Corrupt),
            }
        }
        self.summary = IndexSummary {
            first,
            end,
            segment_bytes,
            coverage,
        };
        Ok(())
    }
}
pub(crate) fn validate_reference(reference: &ObjectRef) -> Result<(), FormatError> {
    if !valid_uuid(&reference.id) || !valid_digest(&reference.digest) || reference.bytes == 0 {
        return Err(FormatError::Corrupt);
    }
    Ok(())
}
pub(crate) fn validate_source(source: &SourceRange) -> Result<(), FormatError> {
    if source.identity.is_empty()
        || source.identity.len() > 4096
        || source.identity.chars().any(char::is_control)
        || source.start > source.end
    {
        return Err(FormatError::Corrupt);
    }
    Ok(())
}
