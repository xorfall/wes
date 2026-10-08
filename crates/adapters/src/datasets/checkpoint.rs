//! Captured runner state and cumulative work. Loading never grants execution permission.
use super::{
    FormatError, Lifecycle, SourceRange,
    catalog::{ObjectRef, valid_digest, valid_uuid},
    format::{bounded_json, validate_value},
    tree::{validate_reference, validate_source},
};
use crate::codec;
use base64::{Engine, engine::general_purpose::STANDARD};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use wes_core::{Value, contracts::ResolvedContractBundle};

#[derive(Clone, Copy, Debug)]
pub struct CheckpointLimits {
    pub bytes: usize,
    pub inline: codec::Limits,
    pub code_bytes: usize,
    pub carry_bytes: usize,
    pub validation_work: usize,
}
impl Default for CheckpointLimits {
    fn default() -> Self {
        Self {
            bytes: 8 * 1024 * 1024,
            inline: codec::Limits {
                bytes: 1024 * 1024,
                nodes: 100000,
            },
            code_bytes: 4 * 1024 * 1024,
            carry_bytes: 65536,
            validation_work: 1000000,
        }
    }
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct InlineSnapshot {
    pub schema: ObjectRef,
    pub schema_digest: String,
    pub value_digest: String,
    /// Canonical base64 of retained codec bytes, not display JSON.
    pub encoded: String,
}
impl InlineSnapshot {
    pub fn capture(
        value: &Value,
        schema_reference: ObjectRef,
        schema: &ResolvedContractBundle,
        limits: CheckpointLimits,
    ) -> Result<Self, FormatError> {
        let mut work = limits.validation_work;
        validate_value(value, schema.root(), &schema.root().shape(), &mut work)?;
        validate_reference(&schema_reference)?;
        let bytes = codec::encode_value(value, limits.inline)?;
        Ok(Self {
            schema: schema_reference,
            schema_digest: schema.digest().into(),
            value_digest: digest(&bytes),
            encoded: STANDARD.encode(bytes),
        })
    }
    pub fn value(
        &self,
        schema: &ResolvedContractBundle,
        limits: CheckpointLimits,
    ) -> Result<Value, FormatError> {
        if self.schema_digest != schema.digest() {
            return Err(FormatError::Contract);
        }
        self.check(limits)?;
        let bytes = STANDARD
            .decode(&self.encoded)
            .map_err(|_| FormatError::Corrupt)?;
        if digest(&bytes) != self.value_digest {
            return Err(FormatError::Corrupt);
        }
        let value = codec::decode_value(&bytes, limits.inline)?.value;
        let mut work = limits.validation_work;
        validate_value(&value, schema.root(), &schema.root().shape(), &mut work)?;
        Ok(value)
    }
    fn check(&self, limits: CheckpointLimits) -> Result<(), FormatError> {
        validate_reference(&self.schema)?;
        if !valid_digest(&self.schema_digest) || !valid_digest(&self.value_digest) {
            return Err(FormatError::Corrupt);
        }
        check_base64(&self.encoded, limits.inline.bytes)
    }
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CheckpointBindings {
    pub task_revision: String,
    pub source_handle: String,
    pub source_digest: String,
    pub source_bytes: u64,
    pub source: SourceRange,
    pub item_schema: ObjectRef,
    pub item_schema_digest: String,
    pub output_schema: ObjectRef,
    pub output_schema_digest: String,
    /// Pure calculation revisions use the language's canonical sha256 identity.
    pub step_revision: String,
    pub finish_revision: Option<String>,
    pub language_version: u32,
    pub native_version: String,
    pub profile_digest: String,
    pub initial_digest: String,
    /// Versioned, captured pure program; validation belongs to the language loader before resume.
    pub captured_program: String,
    pub program_digest: String,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WorkLedger {
    pub limit: u64,
    /// Every admitted grant remains charged, including a grant interrupted by a crash.
    pub granted: u64,
    pub completed: u64,
    /// Measured completed debit plus conservatively charged interrupted grants.
    pub charged: u64,
    /// Prepaid ceiling of the entered batch; a crash charges it in full once.
    pub outstanding: u64,
    pub grants: u64,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Checkpoint {
    pub version: u16,
    pub store: String,
    pub dataset: String,
    pub analysis: String,
    pub attempt: String,
    pub previous_attempt: Option<String>,
    pub run: String,
    pub transaction: String,
    pub bindings: CheckpointBindings,
    pub followed_source: Option<wes_engine::storage::datasets::FollowedSource>,
    pub state: InlineSnapshot,
    pub context: InlineSnapshot,
    pub next_position: u64,
    pub next_ordinal: u64,
    pub output_end: u64,
    pub decoder_carry: String,
    pub work: WorkLedger,
    pub usage: wes_engine::storage::datasets::AnalysisUsage,
    pub duration: wes_engine::storage::datasets::AnalysisDuration,
    pub finish_applied: bool,
    pub lifecycle: Lifecycle,
    pub origins: Vec<String>,
    pub dataset_reads: Vec<wes_core::flow::DatasetReadOrigin>,
}
impl Checkpoint {
    pub(super) fn bindings_extend(&self, prior: &Self) -> bool {
        let follow = match (&self.followed_source, &prior.followed_source) {
            (None, None) => return self.bindings == prior.bindings,
            (Some(next), Some(previous)) => next.extends(previous),
            _ => false,
        };
        let mut bindings = self.bindings.clone();
        bindings.source.end = prior.bindings.source.end;
        follow
            && self.bindings.source.end >= prior.bindings.source.end
            && bindings == prior.bindings
    }
    pub fn encode(&self, limits: CheckpointLimits) -> Result<Vec<u8>, FormatError> {
        self.check(limits)?;
        bounded_json(self, limits.bytes)
    }
    pub fn decode(bytes: &[u8], limits: CheckpointLimits) -> Result<Self, FormatError> {
        if bytes.len() > limits.bytes {
            return Err(FormatError::Limit("checkpoint bytes"));
        }
        let checkpoint: Self = serde_json::from_slice(bytes).map_err(|_| FormatError::Corrupt)?;
        if checkpoint.encode(limits)? != bytes {
            return Err(FormatError::Corrupt);
        }
        Ok(checkpoint)
    }
    fn check(&self, limits: CheckpointLimits) -> Result<(), FormatError> {
        if self.version != 2 {
            return Err(FormatError::Version);
        }
        if [
            &self.store,
            &self.dataset,
            &self.analysis,
            &self.attempt,
            &self.run,
            &self.transaction,
            &self.bindings.source_handle,
        ]
        .iter()
        .any(|id| !valid_uuid(id))
            || self
                .previous_attempt
                .as_ref()
                .is_some_and(|id| !valid_uuid(id))
        {
            return Err(FormatError::Corrupt);
        }
        if !self.duration.valid() {
            return Err(FormatError::Corrupt);
        }
        validate_source(&self.bindings.source)?;
        if let Some(source) = &self.followed_source {
            if !valid_uuid(&source.run)
                || !valid_uuid(&source.epoch)
                || source.epoch != source.prefix.dataset()
                || source.first == 0
                || source.prefix.store() != self.store
                || source.prefix.dataset() == self.dataset
                || source.prefix.records() != self.bindings.source.end
                || self.bindings.source.unit != super::PositionUnit::Records
                || self.bindings.source.start != 0
                || self.next_position != self.next_ordinal
            {
                return Err(FormatError::Corrupt);
            }
        }
        if self.bindings.source_bytes == 0 {
            return Err(FormatError::Corrupt);
        }
        validate_reference(&self.bindings.item_schema)?;
        validate_reference(&self.bindings.output_schema)?;
        if [
            &self.bindings.task_revision,
            &self.bindings.source_digest,
            &self.bindings.item_schema_digest,
            &self.bindings.output_schema_digest,
            &self.bindings.profile_digest,
            &self.bindings.initial_digest,
            &self.bindings.program_digest,
        ]
        .iter()
        .any(|s| !valid_digest(s))
        {
            return Err(FormatError::Corrupt);
        }
        if !valid_digest(&self.bindings.step_revision)
            || self
                .bindings
                .finish_revision
                .as_ref()
                .is_some_and(|s| !valid_digest(s))
            || self.bindings.language_version == 0
            || self.bindings.native_version.is_empty()
            || self.bindings.native_version.len() > 256
            || self.bindings.native_version.chars().any(char::is_control)
        {
            return Err(FormatError::Corrupt);
        }
        if self.bindings.captured_program.len() > limits.code_bytes {
            return Err(FormatError::Limit("captured code"));
        }
        if digest(self.bindings.captured_program.as_bytes()) != self.bindings.program_digest {
            return Err(FormatError::Corrupt);
        }
        self.state.check(limits)?;
        self.context.check(limits)?;
        check_base64(&self.decoder_carry, limits.carry_bytes)?;
        if self.next_position < self.bindings.source.start
            || self.next_position > self.bindings.source.end
            || self.work.completed > self.work.charged
            || self.work.charged > self.work.granted
            || self
                .work
                .charged
                .checked_add(self.work.outstanding)
                .is_none_or(|n| n > self.work.limit)
            || self
                .work
                .charged
                .checked_add(self.work.outstanding)
                .is_none_or(|n| n > self.work.granted)
            || self.usage.work_allowance > self.work.limit
            || self
                .work
                .charged
                .checked_add(self.work.outstanding)
                .is_none_or(|n| n > self.usage.work_allowance)
            || (self.work.grants == 0 && self.work.granted != 0)
            || (self.finish_applied && self.lifecycle != Lifecycle::Sealed)
            || (self.finish_applied && self.bindings.finish_revision.is_none())
        {
            return Err(FormatError::Corrupt);
        }
        if self.dataset_reads.len() > 128
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
fn check_base64(encoded: &str, bytes: usize) -> Result<(), FormatError> {
    let cap = bytes
        .checked_add(2)
        .and_then(|n| n.checked_div(3))
        .and_then(|n| n.checked_mul(4))
        .ok_or(FormatError::Limit("inline bytes"))?;
    if encoded.len() > cap {
        return Err(FormatError::Limit("inline bytes"));
    }
    let decoded = STANDARD.decode(encoded).map_err(|_| FormatError::Corrupt)?;
    if decoded.len() > bytes {
        return Err(FormatError::Limit("inline bytes"));
    }
    if STANDARD.encode(&decoded) != encoded {
        return Err(FormatError::Corrupt);
    }
    Ok(())
}
pub(crate) fn digest(bytes: &[u8]) -> String {
    format!("sha256:{:x}", Sha256::digest(bytes))
}
