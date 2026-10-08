//! Enforcement labels use monotone joins, independently of descriptive provenance consensus.
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;

/// An enforcement dependency on an owned dataset read gate, not a retention
/// root or permission to resolve its data. Copies carry it across pure joins.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(deny_unknown_fields, try_from = "ReadOrigin", into = "ReadOrigin")]
pub struct DatasetReadOrigin {
    store: String,
    dataset: String,
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ReadOrigin {
    store: String,
    dataset: String,
}
impl TryFrom<ReadOrigin> for DatasetReadOrigin {
    type Error = &'static str;
    fn try_from(raw: ReadOrigin) -> Result<Self, Self::Error> {
        for id in [&raw.store, &raw.dataset] {
            if !uuid::Uuid::parse_str(id).is_ok_and(|u| u.hyphenated().to_string() == *id) {
                return Err("invalid dataset read origin");
            }
        }
        Ok(Self {
            store: raw.store,
            dataset: raw.dataset,
        })
    }
}
impl From<DatasetReadOrigin> for ReadOrigin {
    fn from(origin: DatasetReadOrigin) -> Self {
        Self {
            store: origin.store,
            dataset: origin.dataset,
        }
    }
}
impl DatasetReadOrigin {
    pub fn store(&self) -> &str {
        &self.store
    }
    pub fn dataset(&self) -> &str {
        &self.dataset
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct FlowPolicy {
    origins: BTreeSet<String>,
    private: bool,
    unknown: bool,
    dataset_reads: BTreeSet<DatasetReadOrigin>,
}
impl FlowPolicy {
    pub fn origins(&self) -> &BTreeSet<String> {
        &self.origins
    }
    pub fn is_private(&self) -> bool {
        self.private
    }
    pub fn is_unknown(&self) -> bool {
        self.unknown
    }
    pub fn is_empty(&self) -> bool {
        self.origins.is_empty() && self.dataset_reads.is_empty() && !self.private && !self.unknown
    }
    /// A trusted control-plane acknowledgement contains no read content. Preserve
    /// confidentiality and environment labels, but do not gate an effect receipt
    /// on the resource the acknowledged effect has just withdrawn.
    /// Data-plane operations must use the ordinary monotone join instead.
    pub fn for_control_acknowledgement(&self) -> Self {
        Self {
            origins: self.origins.clone(),
            private: self.private,
            unknown: self.unknown,
            dataset_reads: BTreeSet::new(),
        }
    }
    pub fn dataset_reads(&self) -> &BTreeSet<DatasetReadOrigin> {
        &self.dataset_reads
    }
    pub fn read_from_dataset(self, reference: &crate::DatasetRef) -> Self {
        self.with_dataset_read(DatasetReadOrigin {
            store: reference.store().into(),
            dataset: reference.dataset().into(),
        })
    }
    pub fn with_dataset_read(mut self, origin: DatasetReadOrigin) -> Self {
        if self.dataset_reads.len() < 128 || self.dataset_reads.contains(&origin) {
            self.dataset_reads.insert(origin);
        } else {
            self.unknown = true;
        }
        self
    }
    pub fn private(mut self) -> Self {
        self.private = true;
        self
    }
    pub fn unknown(mut self) -> Self {
        self.unknown = true;
        self
    }
    pub fn from_origin(mut self, origin: impl Into<String>) -> Self {
        let origin = origin.into();
        if origin.is_empty() || origin.len() > 256 || origin.chars().any(char::is_control) {
            self.unknown = true;
            return self;
        }
        if self.origins.len() < 128 {
            self.origins.insert(origin);
        } else if !self.origins.contains(&origin) {
            self.unknown = true;
        }
        self
    }
    pub fn join(&self, other: &Self) -> Self {
        let mut joined = self.clone();
        joined.private |= other.private;
        joined.unknown |= other.unknown;
        for origin in &other.origins {
            joined = joined.from_origin(origin.clone());
        }
        for origin in &other.dataset_reads {
            joined = joined.with_dataset_read(origin.clone());
        }
        joined
    }
}
