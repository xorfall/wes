//! Immutable committed extent identity. A descriptor carries no filesystem or execution authority.
use serde::{Deserialize, Serialize};
use thiserror::Error;
use uuid::Uuid;

#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(deny_unknown_fields, try_from = "Wire", into = "Wire")]
pub struct DatasetRef {
    store: String,
    dataset: String,
    generation: u64,
    manifest: String,
    manifest_digest: String,
    manifest_bytes: u64,
    schema_digest: String,
    records: u64,
    authorization_generation: u64,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct Wire {
    store: String,
    dataset: String,
    generation: String,
    manifest: String,
    manifest_digest: String,
    manifest_bytes: String,
    schema_digest: String,
    records: String,
    authorization_generation: String,
}
#[derive(Clone, Debug, PartialEq, Eq, Error)]
#[error("invalid committed dataset descriptor")]
pub struct InvalidDataset;
impl DatasetRef {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        store: String,
        dataset: String,
        generation: u64,
        manifest: String,
        manifest_digest: String,
        manifest_bytes: u64,
        schema_digest: String,
        records: u64,
        authorization_generation: u64,
    ) -> Result<Self, InvalidDataset> {
        Wire {
            store,
            dataset,
            generation: generation.to_string(),
            manifest,
            manifest_digest,
            manifest_bytes: manifest_bytes.to_string(),
            schema_digest,
            records: records.to_string(),
            authorization_generation: authorization_generation.to_string(),
        }
        .try_into()
    }
    pub fn store(&self) -> &str {
        &self.store
    }
    pub fn dataset(&self) -> &str {
        &self.dataset
    }
    pub fn generation(&self) -> u64 {
        self.generation
    }
    pub fn manifest(&self) -> &str {
        &self.manifest
    }
    pub fn manifest_digest(&self) -> &str {
        &self.manifest_digest
    }
    pub fn manifest_bytes(&self) -> u64 {
        self.manifest_bytes
    }
    pub fn schema_digest(&self) -> &str {
        &self.schema_digest
    }
    pub fn records(&self) -> u64 {
        self.records
    }
    pub fn authorization_generation(&self) -> u64 {
        self.authorization_generation
    }
}
impl TryFrom<Wire> for DatasetRef {
    type Error = InvalidDataset;
    fn try_from(w: Wire) -> Result<Self, Self::Error> {
        let uuid = |s: &str| Uuid::parse_str(s).is_ok_and(|id| id.hyphenated().to_string() == s);
        let digest = |s: &str| {
            s.len() == 71
                && s.starts_with("sha256:")
                && s[7..]
                    .bytes()
                    .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        };
        let number = |s: &str| {
            s.parse::<u64>()
                .ok()
                .filter(|n| n.to_string() == s)
                .ok_or(InvalidDataset)
        };
        let generation = number(&w.generation)?;
        let manifest_bytes = number(&w.manifest_bytes)?;
        let records = number(&w.records)?;
        let authorization_generation = number(&w.authorization_generation)?;
        if !uuid(&w.store)
            || !uuid(&w.dataset)
            || !uuid(&w.manifest)
            || !digest(&w.manifest_digest)
            || !digest(&w.schema_digest)
            || generation == 0
            || authorization_generation == 0
            || manifest_bytes == 0
        {
            return Err(InvalidDataset);
        }
        Ok(Self {
            store: w.store,
            dataset: w.dataset,
            generation,
            manifest: w.manifest,
            manifest_digest: w.manifest_digest,
            manifest_bytes,
            schema_digest: w.schema_digest,
            records,
            authorization_generation,
        })
    }
}
impl From<DatasetRef> for Wire {
    fn from(r: DatasetRef) -> Self {
        Self {
            store: r.store,
            dataset: r.dataset,
            generation: r.generation.to_string(),
            manifest: r.manifest,
            manifest_digest: r.manifest_digest,
            manifest_bytes: r.manifest_bytes.to_string(),
            schema_digest: r.schema_digest,
            records: r.records.to_string(),
            authorization_generation: r.authorization_generation.to_string(),
        }
    }
}
