//! Enforcement labels use monotone joins, independently of descriptive provenance consensus.
use std::collections::BTreeSet;

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct FlowPolicy {
    origins: BTreeSet<String>,
    private: bool,
    unknown: bool,
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
        self.origins.is_empty() && !self.private && !self.unknown
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
        joined
    }
}
