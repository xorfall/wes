//! Framed commit log. Complete corrupt frames are errors, never a successful older root.
use super::format::{FormatError, Input, bounded_json};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::BTreeSet;
use uuid::Uuid;
use wes_core::DatasetRef;

const MAGIC: &[u8; 8] = b"WESCAT04";
const END: &[u8; 8] = b"CATEND04";
const PREFIX: usize = 8 + 2 + 4;
const TRAILER: usize = 32 + 8;
pub const GENESIS: &str = "sha256:0000000000000000000000000000000000000000000000000000000000000000";

#[derive(Clone, Copy, Debug)]
pub struct CatalogLimits {
    pub frame_bytes: usize,
    pub recovery_bytes: usize,
    pub frames: usize,
    pub roots_per_frame: usize,
}
impl Default for CatalogLimits {
    fn default() -> Self {
        Self {
            frame_bytes: 256 * 1024,
            recovery_bytes: 16 * 1024 * 1024,
            frames: 4096,
            roots_per_frame: 128,
        }
    }
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ObjectRef {
    pub id: String,
    pub digest: String,
    pub bytes: u64,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RootChange {
    pub dataset: String,
    pub expected_generation: u64,
    pub generation: u64,
    pub manifest: ObjectRef,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RootKind {
    Value,
    Workspace,
    Keep,
    Pin,
    Checkpoint,
    Recording,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RootRetention {
    Temporary,
    Automatic,
    Protected,
    Unknown,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReferenceRootChange {
    pub root: String,
    pub kind: RootKind,
    pub expected_generation: u64,
    pub generation: u64,
    /// Empty means released; its revision still prevents stale root mutations.
    pub prefixes: Vec<DatasetRef>,
    /// Checkpoint output ownership is distinct from prefixes retained as inputs.
    pub owner_dataset: Option<String>,
    /// Immutable physical saved-generation ownership; only Workspace roots have it.
    pub owner_workspace: Option<String>,
    pub captures: Vec<wes_engine::storage::datasets::CapturedValue>,
    pub retention: RootRetention,
    pub transaction: String,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CatalogCommit {
    pub store: String,
    pub sequence: u64,
    pub transaction: String,
    pub previous_digest: String,
    pub roots: Vec<RootChange>,
    pub references: Vec<ReferenceRootChange>,
    /// Opaque read withdrawal facts. No payload, schema, provenance or error text.
    pub gates: Vec<ReadGate>,
    pub writes: Vec<WriteWitness>,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WriteState {
    Requested,
    Committed,
    Absent,
    Retired,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WriteOperation {
    Create,
    Append,
    Resume,
    Continue,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WriteWitness {
    pub dataset: String,
    pub owner: wes_engine::storage::datasets::DatasetWriteOwner,
    pub transaction: String,
    pub operation: WriteOperation,
    pub admission: String,
    pub predecessor: Option<DatasetRef>,
    pub committed: Option<DatasetRef>,
    pub state: WriteState,
}
impl WriteWitness {
    pub(super) fn validate(&self, store: &str) -> Result<(), FormatError> {
        if !valid_uuid(&self.dataset)
            || !valid_uuid(&self.owner.run)
            || !valid_uuid(&self.owner.lineage)
            || (self.owner.role == wes_engine::storage::datasets::DatasetWriteRole::Recording
                && self.owner.run != self.owner.lineage)
            || (self.operation == WriteOperation::Create) != self.predecessor.is_none()
            || (matches!(
                self.operation,
                WriteOperation::Resume | WriteOperation::Continue
            ) && self.owner.role != wes_engine::storage::datasets::DatasetWriteRole::Analysis)
            || !valid_uuid(&self.transaction)
            || !valid_uuid(&self.admission)
            || (self.state == WriteState::Committed) != self.committed.is_some()
            || self
                .predecessor
                .iter()
                .chain(self.committed.iter())
                .any(|p| p.store() != store || p.dataset() != self.dataset)
        {
            return Err(FormatError::Corrupt);
        }
        if let Some(next) = &self.committed {
            if next.generation()
                != self
                    .predecessor
                    .as_ref()
                    .map_or(Some(1), |p| p.generation().checked_add(1))
                    .ok_or(FormatError::Corrupt)?
                || self.predecessor.as_ref().is_some_and(|p| {
                    p.schema_digest() != next.schema_digest()
                        || p.authorization_generation() != next.authorization_generation()
                        || p.records() > next.records()
                })
            {
                return Err(FormatError::Corrupt);
            }
        }
        Ok(())
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReadGateStatus {
    Restricted,
    Deleted,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReadGate {
    pub dataset: String,
    pub expected_generation: u64,
    pub generation: u64,
    pub status: ReadGateStatus,
}
#[derive(Clone, Debug)]
pub struct RecoveredCommit {
    pub commit: CatalogCommit,
    pub digest: String,
    pub offset: usize,
    pub end: usize,
}
#[derive(Clone, Debug)]
pub struct CatalogRecovery {
    pub commits: Vec<RecoveredCommit>,
    /// Only incomplete bytes, never a complete checksum-invalid frame.
    pub torn_tail: Option<usize>,
}
pub fn encode_commit(
    commit: &CatalogCommit,
    limits: CatalogLimits,
) -> Result<Vec<u8>, FormatError> {
    validate(commit, limits)?;
    let payload_limit = limits
        .frame_bytes
        .checked_sub(PREFIX + TRAILER)
        .ok_or(FormatError::Limit("catalog frame"))?;
    let payload = bounded_json(commit, payload_limit)?;
    let length = u32::try_from(payload.len()).map_err(|_| FormatError::Limit("catalog frame"))?;
    let mut bytes = Vec::with_capacity(PREFIX + payload.len() + TRAILER);
    bytes.extend_from_slice(MAGIC);
    bytes.extend_from_slice(&4u16.to_le_bytes());
    bytes.extend_from_slice(&length.to_le_bytes());
    bytes.extend_from_slice(&payload);
    let checksum = Sha256::digest(&bytes);
    bytes.extend_from_slice(&checksum);
    bytes.extend_from_slice(END);
    Ok(bytes)
}
/// Verify a bounded catalog prefix with an explicit predecessor after catalog rotation.
pub fn recover_catalog(
    bytes: &[u8],
    store: &str,
    first_sequence: u64,
    previous_digest: &str,
    limits: CatalogLimits,
) -> Result<CatalogRecovery, FormatError> {
    if bytes.len() > limits.recovery_bytes {
        return Err(FormatError::Limit("catalog recovery"));
    }
    if !valid_uuid(store)
        || !valid_digest(previous_digest)
        || first_sequence == 0
        || limits.frames == 0
    {
        return Err(FormatError::Corrupt);
    }
    let mut input = Input::new(bytes);
    let mut commits = Vec::new();
    let mut sequence = first_sequence;
    let mut predecessor = previous_digest.to_owned();
    let mut transactions = BTreeSet::new();
    while input.position < bytes.len() {
        if commits.len() >= limits.frames {
            return Err(FormatError::Limit("catalog frames"));
        }
        let offset = input.position;
        let remainder = &bytes[offset..];
        if remainder.len() < PREFIX {
            let prefix_len = remainder.len().min(MAGIC.len());
            if remainder[..prefix_len] != MAGIC[..prefix_len] {
                return Err(FormatError::Corrupt);
            }
            if remainder.len() >= 10
                && u16::from_le_bytes(remainder[8..10].try_into().unwrap()) != 4
            {
                return Err(FormatError::Version);
            }
            return Ok(CatalogRecovery {
                commits,
                torn_tail: Some(offset),
            });
        }
        if input.take(8)? != MAGIC {
            return Err(FormatError::Corrupt);
        }
        if input.u16()? != 4 {
            return Err(FormatError::Version);
        }
        let length = input.u32()? as usize;
        let frame_size = length
            .checked_add(PREFIX + TRAILER)
            .filter(|n| *n <= limits.frame_bytes)
            .ok_or(FormatError::Limit("catalog frame"))?;
        if remainder.len() < frame_size {
            return Ok(CatalogRecovery {
                commits,
                torn_tail: Some(offset),
            });
        }
        let raw = input.take(length)?;
        let checksum = input.take(32)?;
        if input.take(8)? != END
            || Sha256::digest(&bytes[offset..offset + PREFIX + length]).as_slice() != checksum
        {
            return Err(FormatError::Corrupt);
        }
        let commit: CatalogCommit =
            serde_json::from_slice(raw).map_err(|_| FormatError::Corrupt)?;
        validate(&commit, limits)?;
        if bounded_json(&commit, limits.frame_bytes)? != raw
            || commit.store != store
            || commit.sequence != sequence
            || commit.previous_digest != predecessor
            || !transactions.insert(commit.transaction.clone())
        {
            return Err(FormatError::Corrupt);
        }
        let digest = format!(
            "sha256:{:x}",
            Sha256::digest(&bytes[offset..input.position])
        );
        predecessor = digest.clone();
        sequence = sequence.checked_add(1).ok_or(FormatError::Corrupt)?;
        commits.push(RecoveredCommit {
            commit,
            digest,
            offset,
            end: input.position,
        });
    }
    Ok(CatalogRecovery {
        commits,
        torn_tail: None,
    })
}
fn validate(commit: &CatalogCommit, limits: CatalogLimits) -> Result<(), FormatError> {
    if !valid_uuid(&commit.store)
        || !valid_uuid(&commit.transaction)
        || !valid_digest(&commit.previous_digest)
        || commit.sequence == 0
        || (commit.roots.is_empty()
            && commit.references.is_empty()
            && commit.gates.is_empty()
            && commit.writes.is_empty())
    {
        return Err(FormatError::Corrupt);
    }
    if commit
        .roots
        .len()
        .saturating_add(commit.references.len())
        .saturating_add(commit.gates.len())
        .saturating_add(commit.writes.len())
        > limits.roots_per_frame
    {
        return Err(FormatError::Limit("catalog roots"));
    }
    let mut writes = BTreeSet::new();
    for write in &commit.writes {
        write.validate(&commit.store)?;
        if !writes.insert(&write.dataset) {
            return Err(FormatError::Corrupt);
        }
    }
    let mut ids = BTreeSet::new();
    for gate in &commit.gates {
        if !valid_uuid(&gate.dataset)
            || !ids.insert(&gate.dataset)
            || gate.expected_generation.checked_add(1) != Some(gate.generation)
        {
            return Err(FormatError::Corrupt);
        }
    }
    for root in &commit.roots {
        if !valid_uuid(&root.dataset)
            || !ids.insert(&root.dataset)
            || root.expected_generation.checked_add(1) != Some(root.generation)
            || !valid_uuid(&root.manifest.id)
            || !valid_digest(&root.manifest.digest)
            || root.manifest.bytes == 0
        {
            return Err(FormatError::Corrupt);
        }
    }
    let mut roots = BTreeSet::new();
    let mut total = 0usize;
    for root in &commit.references {
        if !valid_uuid(&root.root)
            || root.expected_generation.checked_add(1) != Some(root.generation)
            || !roots.insert(&root.root)
            || root.transaction != commit.transaction
        {
            return Err(FormatError::Corrupt);
        }
        if (root.kind == RootKind::Workspace) != root.owner_workspace.is_some()
            || root
                .owner_workspace
                .as_ref()
                .is_some_and(|id| !valid_uuid(id))
            || (root.kind == RootKind::Workspace
                && (!root.captures.is_empty()
                    || (!root.prefixes.is_empty() && root.retention != RootRetention::Protected)))
            || (root.kind == RootKind::Checkpoint) != root.owner_dataset.is_some()
            || root.owner_dataset.as_ref().is_some_and(|owner| {
                !valid_uuid(owner)
                    || (!root.prefixes.is_empty()
                        && root
                            .prefixes
                            .iter()
                            .filter(|p| p.dataset() == owner)
                            .count()
                            != 1)
            })
        {
            return Err(FormatError::Corrupt);
        }
        let mut prefixes = BTreeSet::new();
        let mut captures = BTreeSet::new();
        for capture in &root.captures {
            total += 1;
            if total > limits.roots_per_frame {
                return Err(FormatError::Limit("captured references"));
            }
            if !valid_uuid(&capture.handle)
                || !valid_digest(&capture.digest)
                || capture.bytes == 0
                || !captures.insert(&capture.handle)
            {
                return Err(FormatError::Corrupt);
            }
        }
        for prefix in &root.prefixes {
            total += 1;
            if total > limits.roots_per_frame {
                return Err(FormatError::Limit("referenced prefixes"));
            }
            if prefix.store() != commit.store
                || !prefixes.insert((prefix.dataset(), prefix.generation(), prefix.manifest()))
            {
                return Err(FormatError::Corrupt);
            }
        }
    }
    Ok(())
}
pub(super) fn valid_uuid(text: &str) -> bool {
    Uuid::parse_str(text).is_ok_and(|id| id.hyphenated().to_string() == text)
}
pub(super) fn valid_digest(text: &str) -> bool {
    text.len() == 71
        && text.starts_with("sha256:")
        && text[7..]
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

#[cfg(test)]
mod tests {
    use super::*;
    fn commit(sequence: u64, previous: &str) -> CatalogCommit {
        CatalogCommit {
            writes: vec![],
            store: "bda44444-4444-4444-8444-444444444444".into(),
            sequence,
            transaction: Uuid::new_v4().to_string(),
            previous_digest: previous.into(),
            references: vec![],
            gates: vec![],
            roots: vec![RootChange {
                dataset: "bda55555-5555-4555-8555-555555555555".into(),
                expected_generation: sequence - 1,
                generation: sequence,
                manifest: ObjectRef {
                    id: Uuid::new_v4().to_string(),
                    digest: format!("sha256:{}", "a".repeat(64)),
                    bytes: 100,
                },
            }],
        }
    }
    #[test]
    fn complete_prefix_and_every_torn_second_frame_recover_without_guessing() {
        let c1 = commit(1, GENESIS);
        let first = encode_commit(&c1, CatalogLimits::default()).unwrap();
        let digest = format!("sha256:{:x}", Sha256::digest(&first));
        let c2 = commit(2, &digest);
        let second = encode_commit(&c2, CatalogLimits::default()).unwrap();
        for cut in 1..second.len() {
            let mut bytes = first.clone();
            bytes.extend_from_slice(&second[..cut]);
            let recovery =
                recover_catalog(&bytes, &c1.store, 1, GENESIS, CatalogLimits::default()).unwrap();
            assert_eq!(recovery.commits.len(), 1);
            assert_eq!(recovery.torn_tail, Some(first.len()));
        }
        let mut bytes = first.clone();
        bytes.extend_from_slice(&second);
        let recovery =
            recover_catalog(&bytes, &c1.store, 1, GENESIS, CatalogLimits::default()).unwrap();
        assert_eq!(recovery.commits.len(), 2);
        assert!(recovery.torn_tail.is_none());
        assert_eq!(recovery.commits[1].commit, c2);
        // A rotated catalog retains its predecessor; it cannot reset the chain.
        assert_eq!(
            recover_catalog(&second, &c1.store, 2, &digest, CatalogLimits::default())
                .unwrap()
                .commits
                .len(),
            1
        );
        assert!(recover_catalog(&second, &c1.store, 2, GENESIS, CatalogLimits::default()).is_err());
    }
    #[test]
    fn complete_corruption_wrong_store_duplicate_transaction_and_oversized_header_fail() {
        let c1 = commit(1, GENESIS);
        let first = encode_commit(&c1, CatalogLimits::default()).unwrap();
        let mut bad = first.clone();
        bad[100] ^= 1;
        assert!(recover_catalog(&bad, &c1.store, 1, GENESIS, CatalogLimits::default()).is_err());
        let mut bad = first.clone();
        bad[10..14].copy_from_slice(&u32::MAX.to_le_bytes());
        assert!(matches!(
            recover_catalog(&bad, &c1.store, 1, GENESIS, CatalogLimits::default()),
            Err(FormatError::Limit("catalog frame"))
        ));
        assert!(
            recover_catalog(
                &first,
                &c1.roots[0].dataset,
                1,
                GENESIS,
                CatalogLimits::default()
            )
            .is_err()
        );
        let digest = format!("sha256:{:x}", Sha256::digest(&first));
        let mut c2 = commit(2, &digest);
        c2.transaction = c1.transaction.clone();
        let mut bytes = first.clone();
        bytes.extend_from_slice(&encode_commit(&c2, CatalogLimits::default()).unwrap());
        assert!(recover_catalog(&bytes, &c1.store, 1, GENESIS, CatalogLimits::default()).is_err());
        assert!(
            recover_catalog(
                &bytes,
                &c1.store,
                1,
                GENESIS,
                CatalogLimits {
                    frames: 1,
                    ..Default::default()
                }
            )
            .is_err()
        );
        let mut padded = first;
        padded.push(0);
        assert!(recover_catalog(&padded, &c1.store, 1, GENESIS, CatalogLimits::default()).is_err());
    }
}
