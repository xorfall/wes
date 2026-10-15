//! Payload-free identity and contract of an immutable stored result.
use super::ValueHandle;
use wes_core::{Provenance, Shape, Value, contracts::metadata::ValueMetadata};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SnapshotDescriptor {
    pub handle: ValueHandle,
    pub digest: String,
    pub selection: Vec<String>,
    pub shape: Shape,
    pub provenance: Provenance,
    pub metadata: Option<ValueMetadata>,
    /// Conservative full decoded charge, used before materialization.
    pub charge: u64,
}
/// Clones share the immutable descriptor allocation, including contract and selection metadata.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StoredSnapshot(std::sync::Arc<SnapshotDescriptor>);
impl std::ops::Deref for StoredSnapshot {
    type Target = SnapshotDescriptor;
    fn deref(&self) -> &Self::Target {
        &self.0
    }
}
impl StoredSnapshot {
    pub fn new(descriptor: SnapshotDescriptor) -> Self {
        Self(std::sync::Arc::new(descriptor))
    }
    pub fn with_selection(&self, selection: Vec<String>) -> Self {
        let mut descriptor = (*self.0).clone();
        descriptor.selection = selection;
        Self::new(descriptor)
    }
    pub(crate) fn allocation(&self) -> wes_core::Allocation {
        wes_core::Allocation::shared(&self.0, self.metadata_charge())
    }

    pub fn capture(handle: ValueHandle, digest: String, value: &Value) -> Option<Self> {
        let allocations = value.allocations(wes_budgets::get("value_size.nodes") as usize)?;
        let physical = allocations
            .iter()
            .fold(0u64, |n, a| n.saturating_add(a.bytes().saturating_add(96)));
        Some(Self::new(SnapshotDescriptor {
            handle,
            digest,
            selection: vec![],
            shape: value.shape().clone(),
            provenance: value.provenance().clone(),
            metadata: value.metadata().cloned(),
            charge: crate::value_size::value_charge(value, u64::MAX)?.max(physical),
        }))
    }
}
impl SnapshotDescriptor {
    pub fn metadata_charge(&self) -> u64 {
        crate::value_size::shape_charge(&self.shape, u64::MAX)
            .unwrap_or(u64::MAX)
            .saturating_add(self.metadata.as_ref().map_or(0, |m| m.charge()))
            .saturating_add(self.provenance.facts().iter().fold(256u64, |n, (k, v)| {
                n.saturating_add((k.len() + v.len()) as u64 * 8)
            }))
            .saturating_add(
                self.provenance
                    .cautions()
                    .iter()
                    .fold(0u64, |n, s| n.saturating_add(128 + (s.len() as u64) * 2)),
            )
            .saturating_add(
                self.provenance
                    .policy()
                    .origins()
                    .iter()
                    .fold(0u64, |n, s| n.saturating_add(128 + (s.len() as u64) * 2)),
            )
            .saturating_add(self.provenance.policy().dataset_reads().len() as u64 * 512)
            .saturating_add(self.provenance.policy().provider_reads().len() as u64 * 768)
            .saturating_add(
                self.selection
                    .iter()
                    .fold(256u64, |n, s| n.saturating_add(64 + (s.len() as u64) * 2)),
            )
    }
}
impl StoredSnapshot {
    pub fn matches(&self, value: &Value) -> bool {
        &self.shape == value.shape()
            && &self.provenance == value.provenance()
            && self.metadata.as_ref() == value.metadata()
            && value.management_authority().is_none()
    }
}
