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

/// An immutable dependency on a provider read domain. Digests identify the
/// captured binding and typed scope; neither this witness nor retained bytes
/// grant permission. The live read owner must validate every joined origin.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(
    deny_unknown_fields,
    try_from = "ProviderOrigin",
    into = "ProviderOrigin"
)]
pub struct ProviderReadOrigin {
    realm: String,
    binding: String,
    scope: String,
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ProviderOrigin {
    realm: String,
    binding: String,
    scope: String,
}
impl TryFrom<ProviderOrigin> for ProviderReadOrigin {
    type Error = &'static str;
    fn try_from(raw: ProviderOrigin) -> Result<Self, Self::Error> {
        Self::new(raw.realm, raw.binding, raw.scope)
    }
}
impl From<ProviderReadOrigin> for ProviderOrigin {
    fn from(o: ProviderReadOrigin) -> Self {
        Self {
            realm: o.realm,
            binding: o.binding,
            scope: o.scope,
        }
    }
}
impl ProviderReadOrigin {
    pub fn new(realm: String, binding: String, scope: String) -> Result<Self, &'static str> {
        if !uuid::Uuid::parse_str(&realm).is_ok_and(|u| u.to_string() == realm)
            || [&binding, &scope].iter().any(|s| {
                s.len() != 64
                    || !s
                        .bytes()
                        .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
            })
        {
            return Err("invalid provider read origin");
        }
        Ok(Self {
            realm,
            binding,
            scope,
        })
    }
    pub fn realm(&self) -> &str {
        &self.realm
    }
    pub fn binding(&self) -> &str {
        &self.binding
    }
    pub fn scope(&self) -> &str {
        &self.scope
    }
}

/// Maximum residence allowed by the data policy, independently of current retention.
/// Ordering is intentional: a join always chooses the more restrictive ceiling.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Residence {
    Memory,
    Temporary,
    #[default]
    Retainable,
}

/// A declaration classifies future provider output; it never relabels an existing value.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum OutputPolicy {
    #[default]
    Public,
    Private,
    ConfidentialTemporary,
    Confidential,
}
impl OutputPolicy {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Public => "public",
            Self::Private => "private",
            Self::ConfidentialTemporary => "confidential-temporary",
            Self::Confidential => "confidential",
        }
    }
    pub fn policy(self) -> FlowPolicy {
        match self {
            Self::Public => FlowPolicy::default(),
            Self::Private => FlowPolicy::default().private(),
            Self::ConfidentialTemporary => FlowPolicy::default().confidential(Residence::Temporary),
            Self::Confidential => FlowPolicy::default().confidential(Residence::Retainable),
        }
    }
    pub fn is_sensitive(self) -> bool {
        self != Self::Public
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct FlowPolicy {
    origins: BTreeSet<String>,
    confidential: bool,
    residence: Residence,
    unknown: bool,
    dataset_reads: BTreeSet<DatasetReadOrigin>,
    provider_reads: BTreeSet<ProviderReadOrigin>,
}
impl FlowPolicy {
    pub fn origins(&self) -> &BTreeSet<String> {
        &self.origins
    }
    pub fn is_private(&self) -> bool {
        self.confidential && self.residence == Residence::Memory
    }
    pub fn is_confidential(&self) -> bool {
        self.confidential
    }
    pub fn residence(&self) -> Residence {
        self.residence
    }
    pub fn allows_retention(&self) -> bool {
        !self.unknown && self.residence == Residence::Retainable
    }
    pub fn is_unknown(&self) -> bool {
        self.unknown
    }
    pub fn is_empty(&self) -> bool {
        self.origins.is_empty()
            && self.dataset_reads.is_empty()
            && self.provider_reads.is_empty()
            && !self.confidential
            && !self.unknown
    }
    /// A trusted control-plane acknowledgement contains no read content. Preserve
    /// confidentiality and environment labels, but do not gate an effect receipt
    /// on the resource the acknowledged effect has just withdrawn.
    /// Data-plane operations must use the ordinary monotone join instead.
    pub fn for_control_acknowledgement(&self) -> Self {
        Self {
            origins: self.origins.clone(),
            confidential: self.confidential,
            residence: self.residence,
            unknown: self.unknown,
            dataset_reads: BTreeSet::new(),
            provider_reads: self.provider_reads.clone(),
        }
    }
    pub fn dataset_reads(&self) -> &BTreeSet<DatasetReadOrigin> {
        &self.dataset_reads
    }
    pub fn provider_reads(&self) -> &BTreeSet<ProviderReadOrigin> {
        &self.provider_reads
    }
    pub fn with_provider_read(mut self, origin: ProviderReadOrigin) -> Self {
        if self.provider_reads.len() < 128 || self.provider_reads.contains(&origin) {
            self.provider_reads.insert(origin);
        } else {
            self.unknown = true;
        }
        self
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
        self.confidential = true;
        self.residence = Residence::Memory;
        self
    }
    /// Adds confidentiality without relaxing an already restricted residence ceiling.
    pub fn confidential(mut self, residence: Residence) -> Self {
        self.confidential = true;
        self.residence = self.residence.min(residence);
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
        joined.confidential |= other.confidential;
        joined.residence = joined.residence.min(other.residence);
        joined.unknown |= other.unknown;
        for origin in &other.origins {
            joined = joined.from_origin(origin.clone());
        }
        for origin in &other.dataset_reads {
            joined = joined.with_dataset_read(origin.clone());
        }
        for origin in &other.provider_reads {
            joined = joined.with_provider_read(origin.clone());
        }
        joined
    }
}
