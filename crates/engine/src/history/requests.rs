//! Durable correlation only: no result payload, source copy, or restored authority.
use super::{InvalidRecord, Persistence};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RequestRecord {
    pub namespace: String,
    pub request: String,
    pub cell: String,
    pub fingerprint: String,
    /// Sequential step identities; empty for an ordinary submission. Never a replay grant.
    pub steps: Vec<String>,
}
impl RequestRecord {
    pub fn validate(&self) -> Result<(), InvalidRecord> {
        if !self.steps.is_empty()
            && (self.steps.len() > 64
                || self.steps.first() != Some(&self.cell)
                || self.steps.iter().any(|step| {
                    step.is_empty() || step.len() > 256 || step.chars().any(char::is_control)
                })
                || self
                    .steps
                    .iter()
                    .collect::<std::collections::BTreeSet<_>>()
                    .len()
                    != self.steps.len())
        {
            return Err(InvalidRecord("invalid sequential request steps"));
        }
        for (text, max) in [
            (&self.namespace, 128),
            (&self.request, 128),
            (&self.cell, 256),
        ] {
            if text.is_empty() || text.len() > max || text.chars().any(char::is_control) {
                return Err(InvalidRecord("invalid request identity"));
            }
        }
        if self.fingerprint.len() != 64
            || !self
                .fingerprint
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        {
            return Err(InvalidRecord("invalid request fingerprint"));
        }
        Ok(())
    }
    pub fn matches(&self, other: &Self) -> bool {
        self.namespace == other.namespace
            && self.request == other.request
            && self.fingerprint == other.fingerprint
    }
}
#[derive(Clone, Debug)]
pub struct RequestClaim {
    pub record: RequestRecord,
    pub fresh: bool,
    pub persistence: Persistence,
}
