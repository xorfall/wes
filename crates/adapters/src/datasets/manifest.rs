//! An immutable exact generation, never an unbounded list of segments.
use super::{
    FormatError, SourceRange, Stream,
    catalog::{ObjectRef, valid_digest, valid_uuid},
    format::bounded_json,
    tree::{IndexSummary, validate_reference, validate_source},
};
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug)]
pub struct ManifestLimits {
    pub bytes: usize,
}
impl Default for ManifestLimits {
    fn default() -> Self {
        Self { bytes: 256 * 1024 }
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DatasetKind {
    Analysis,
    EventLog,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Lifecycle {
    Open,
    Sealed,
    Incomplete,
    Interrupted,
    Cancelled,
    Restricted,
    Deleted,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Persistence {
    FileSynced,
    FileAndDirectorySynced,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Manifest {
    pub version: u16,
    pub store: String,
    pub dataset: String,
    pub kind: DatasetKind,
    pub generation: u64,
    pub previous: Option<ObjectRef>,
    /// Exact committed ancestors at distances 1, 2, 4, ...; at most 64 references.
    /// The shared publisher derives these links from the admitted predecessor.
    pub ancestors: Vec<ObjectRef>,
    pub transaction: String,
    pub schema: ObjectRef,
    pub schema_digest: String,
    pub index: Option<ObjectRef>,
    pub summary: IndexSummary,
    pub coverage: Option<CoverageRoot>,
    pub source: SourceRange,
    pub lifecycle: Lifecycle,
    /// Complete captured runner state and bindings; committed atomically with the index.
    pub checkpoint: Option<ObjectRef>,
    pub recording: Option<wes_engine::storage::datasets::EventLogCoverage>,
    pub requested: Persistence,
    pub established: Persistence,
    /// Revocation is checked against the current root even when reading an older generation.
    pub authorization_generation: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub protection: Option<wes_core::flow::Residence>,
    pub origins: Vec<String>,
    pub dataset_reads: Vec<wes_core::flow::DatasetReadOrigin>,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CoverageRoot {
    pub schema: ObjectRef,
    pub schema_digest: String,
    pub index: Option<ObjectRef>,
    pub summary: IndexSummary,
    pub progress: wes_engine::storage::datasets::CoverageProgress,
}
pub(super) struct ManifestStream<'a> {
    pub stream: Stream,
    pub schema: &'a ObjectRef,
    pub schema_digest: &'a str,
    pub index: Option<&'a ObjectRef>,
    pub summary: &'a IndexSummary,
}
impl Manifest {
    pub(crate) fn policy(&self) -> wes_core::flow::FlowPolicy {
        let mut p = self
            .origins
            .iter()
            .fold(wes_core::flow::FlowPolicy::default(), |p, o| {
                p.from_origin(o)
            });
        if let Some(residence) = self.protection {
            p = p.confidential(residence);
        }
        self.dataset_reads
            .iter()
            .cloned()
            .fold(p, |p, o| p.with_dataset_read(o))
    }

    /// Every physical-tree consumer enumerates roots here; there is no independent side dataset.
    pub(super) fn streams(&self) -> impl Iterator<Item = ManifestStream<'_>> {
        std::iter::once(ManifestStream {
            stream: Stream::Outputs,
            schema: &self.schema,
            schema_digest: &self.schema_digest,
            index: self.index.as_ref(),
            summary: &self.summary,
        })
        .chain(self.coverage.iter().map(|root| ManifestStream {
            stream: Stream::Coverage,
            schema: &root.schema,
            schema_digest: &root.schema_digest,
            index: root.index.as_ref(),
            summary: &root.summary,
        }))
    }
    pub fn encode(&self, limits: ManifestLimits) -> Result<Vec<u8>, FormatError> {
        self.validate()?;
        bounded_json(self, limits.bytes)
    }
    pub fn decode(bytes: &[u8], limits: ManifestLimits) -> Result<Self, FormatError> {
        if bytes.len() > limits.bytes {
            return Err(FormatError::Limit("manifest bytes"));
        }
        let manifest: Self = serde_json::from_slice(bytes).map_err(|_| FormatError::Corrupt)?;
        if manifest.encode(limits)? != bytes {
            return Err(FormatError::Corrupt);
        }
        Ok(manifest)
    }
    fn validate(&self) -> Result<(), FormatError> {
        if self.version != 3 {
            return Err(FormatError::Version);
        }
        if !valid_uuid(&self.store)
            || !valid_uuid(&self.dataset)
            || !valid_uuid(&self.transaction)
            || !valid_digest(&self.schema_digest)
            || self.generation == 0
            || self.summary.first != 0
            || self.summary.coverage.is_some()
            || self.authorization_generation == 0
        {
            return Err(FormatError::Corrupt);
        }
        validate_reference(&self.schema)?;
        validate_source(&self.source)?;
        if let Some(previous) = &self.previous {
            validate_reference(previous)?;
        }
        if (self.generation == 1) != self.previous.is_none() {
            return Err(FormatError::Corrupt);
        }
        let required = (u64::BITS - (self.generation - 1).leading_zeros()) as usize;
        if self.ancestors.len() != required || self.ancestors.first() != self.previous.as_ref() {
            return Err(FormatError::Corrupt);
        }
        let mut seen = std::collections::BTreeSet::new();
        for ancestor in &self.ancestors {
            validate_reference(ancestor)?;
            if !seen.insert(&ancestor.id) {
                return Err(FormatError::Corrupt);
            }
        }
        match &self.index {
            Some(index) if self.summary.end > 0 && self.summary.segment_bytes > 0 => {
                validate_reference(index)?
            }
            None if self.summary.end == 0 && self.summary.segment_bytes == 0 => {}
            _ => return Err(FormatError::Corrupt),
        }
        if let Some(checkpoint) = &self.checkpoint {
            validate_reference(checkpoint)?;
        }
        if let Some(root) = &self.coverage {
            if self.kind != DatasetKind::Analysis
                || self.checkpoint.is_none()
                || self.source.unit != super::PositionUnit::Bytes
                || root.summary.first != 0
                || root.summary.end != root.progress.records
                || !root.progress.policy.valid()
                || !valid_digest(&root.schema_digest)
            {
                return Err(FormatError::Corrupt);
            }
            match &root.summary.coverage {
                Some(span)
                    if span.valid(root.summary.end)
                        && span.matches(&root.progress)
                        && span.from >= self.source.start
                        && span.through <= self.source.end => {}
                None if root.summary.end == 0
                    && root.progress.records == 0
                    && root.progress.valid(0, 0, 0) => {}
                _ => return Err(FormatError::Corrupt),
            }
            validate_reference(&root.schema)?;
            match &root.index {
                Some(index) if root.summary.end > 0 && root.summary.segment_bytes > 0 => {
                    validate_reference(index)?
                }
                None if root.summary.end == 0 && root.summary.segment_bytes == 0 => {}
                _ => return Err(FormatError::Corrupt),
            }
        }
        match (&self.kind, &self.recording) {
            (DatasetKind::EventLog, Some(c)) => {
                if self.checkpoint.is_some()
                    || !valid_uuid(&c.run)
                    || !valid_uuid(&c.epoch)
                    || c.first == 0
                    || c.committed_through < c.first - 1
                    || c.accepted_through < c.committed_through
                    || c.pending != Some(c.accepted_through - c.committed_through)
                    || self.source.start != c.first - 1
                    || self.source.end != c.committed_through
                    || self.summary.end != c.committed_through - (c.first - 1)
                    || (self.lifecycle == Lifecycle::Open) != c.termination.is_none()
                {
                    return Err(FormatError::Corrupt);
                }
            }
            (DatasetKind::Analysis, None) => {}
            _ => return Err(FormatError::Corrupt),
        }
        if self.established != self.requested {
            return Err(FormatError::Corrupt);
        }
        if self.protection == Some(wes_core::flow::Residence::Memory)
            || self.dataset_reads.len() > 128
            || self.dataset_reads.windows(2).any(|s| s[0] >= s[1])
            || self.origins.len() > 128
            || self
                .origins
                .iter()
                .any(|s| s.is_empty() || s.len() > 256 || s.chars().any(char::is_control))
            || self.origins.windows(2).any(|s| s[0] >= s[1])
        {
            return Err(FormatError::Corrupt);
        }
        Ok(())
    }
}
