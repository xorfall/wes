use std::collections::{BTreeMap, BTreeSet};

/// Facts require agreement at joins; cautions accumulate by union.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Provenance {
    policy: crate::flow::FlowPolicy,
    facts: BTreeMap<String, String>,
    cautions: BTreeSet<String>,
}

impl Provenance {
    pub fn new(facts: BTreeMap<String, String>, cautions: BTreeSet<String>) -> Self {
        Self {
            facts,
            cautions,
            policy: Default::default(),
        }
    }
    pub fn policy(&self) -> &crate::flow::FlowPolicy {
        &self.policy
    }
    /// Only adds restrictions. Descriptive fact replacement cannot remove enforcement labels.
    pub fn with_policy(mut self, policy: &crate::flow::FlowPolicy) -> Self {
        self.policy = self.policy.join(policy);
        self
    }

    pub fn facts(&self) -> &BTreeMap<String, String> {
        &self.facts
    }
    pub fn cautions(&self) -> &BTreeSet<String> {
        &self.cautions
    }
    pub fn fact(&self, key: &str) -> Option<&str> {
        self.facts.get(key).map(String::as_str)
    }
    pub fn is_empty(&self) -> bool {
        self.facts.is_empty() && self.cautions.is_empty() && self.policy.is_empty()
    }

    pub fn with_fact(mut self, key: impl Into<String>, value: impl Into<String>) -> Self {
        self.facts.insert(key.into(), value.into());
        self
    }

    pub fn cautioned(mut self, added: impl IntoIterator<Item = String>) -> Self {
        self.cautions.extend(added);
        self
    }

    pub fn merge(&self, other: &Self) -> Self {
        Self {
            policy: self.policy.join(&other.policy),
            facts: self
                .facts
                .iter()
                .filter(|(key, value)| other.facts.get(*key) == Some(*value))
                .map(|(key, value)| (key.clone(), value.clone()))
                .collect(),
            cautions: self.cautions.union(&other.cautions).cloned().collect(),
        }
    }

    /// This producer's declared facts override input consensus, without erasing cautions.
    pub fn inheriting(&self, carried: &Self) -> Self {
        let mut facts = carried.facts.clone();
        facts.extend(self.facts.clone());
        Self {
            policy: self.policy.join(&carried.policy),
            facts,
            cautions: self.cautions.union(&carried.cautions).cloned().collect(),
        }
    }

    pub fn agreed_by<'a>(inputs: impl IntoIterator<Item = &'a Self>) -> Self {
        inputs
            .into_iter()
            .cloned()
            .reduce(|left, right| left.merge(&right))
            .unwrap_or_default()
    }
}
