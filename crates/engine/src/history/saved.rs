//! Immutable saved selections contain identities and paths, never generated payloads.
use super::{InvalidRecord, ValueHandle};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SavedSelection {
    pub id: String,
    pub name: String,
    pub owner: String,
    pub scope: String,
    pub definition: String,
    pub single: bool,
    pub alias_shape: Option<wes_core::Shape>,
    pub members: Vec<SavedMember>,
    pub lineage: Vec<SavedLineage>,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SavedMember {
    pub name: String,
    pub node: String,
    pub run: Option<String>,
    pub revision: u64,
    pub port: String,
    pub state: String,
    pub typing: String,
    pub operation: String,
    pub fields: Vec<String>,
    pub payload: Option<SavedPayload>,
    pub evidence: Option<String>,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SavedPayload {
    pub handle: String,
    pub digest: String,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SavedLineage {
    pub node: String,
    pub run: Option<String>,
    pub revision: u64,
    pub inputs: Vec<SavedEdge>,
    /// A native snapshot crosses a scope boundary without recreating its producer.
    pub origin: Option<SavedEdge>,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SavedEdge {
    pub node: String,
    pub run: Option<String>,
    pub revision: u64,
    pub port: String,
}
impl SavedSelection {
    pub fn validate(&self) -> Result<(), InvalidRecord> {
        let invalid = || InvalidRecord("invalid saved selection");
        let name = |n: &str| n.len() <= 128 && wes_language::binding_name(n);
        let identity = |n: &str| n.len() <= 256 && crate::graph::NodeId::new(n).is_ok();
        let run = |n: &Option<String>| n.as_ref().is_none_or(|n| identity(n));
        let digest = |n: &str| {
            n.strip_prefix("sha256:").is_some_and(|s| {
                s.len() == 64
                    && s.bytes()
                        .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
            })
        };
        if ValueHandle::new(&self.id).is_err()
            || ValueHandle::new(&self.scope).is_err()
            || !name(&self.name)
            || !identity(&self.owner)
            || !digest(&self.definition)
            || (self.single && self.members.len() != 1)
            || self.members.len() > wes_budgets::get("values.outputs") as usize
            || self.lineage.len() > wes_budgets::get("values.outputs") as usize
        {
            return Err(invalid());
        }
        let mut names = BTreeSet::new();
        if let Some(shape) = &self.alias_shape {
            let mut pending = vec![(shape, 0usize)];
            let mut count = 0usize;
            while let Some((shape, depth)) = pending.pop() {
                count += 1;
                if depth > 64 || count > wes_budgets::get("values.outputs") as usize {
                    return Err(invalid());
                }
                match shape {
                    wes_core::Shape::Meta(_) => return Err(invalid()),
                    wes_core::Shape::Record(record) => {
                        if record.name().len() > 64 * 1024 {
                            return Err(invalid());
                        }
                        for (field, shape) in record.fields() {
                            if field.len() > 64 * 1024 {
                                return Err(invalid());
                            }
                            pending.push((shape, depth + 1));
                        }
                    }
                    wes_core::Shape::List(element)
                    | wes_core::Shape::Option(element)
                    | wes_core::Shape::Iter(element)
                    | wes_core::Shape::Dataset(element) => pending.push((element, depth + 1)),
                    _ => {}
                }
            }
        }
        let mut nodes = BTreeSet::new();
        let mut edges = 0usize;
        for entry in &self.lineage {
            if !identity(&entry.node) || !run(&entry.run) || !nodes.insert(&entry.node) {
                return Err(invalid());
            }
            edges = edges.saturating_add(entry.inputs.len());
            if edges > wes_budgets::get("workspace.changes") as usize * 128 {
                return Err(invalid());
            }
            let mut inputs = BTreeSet::new();
            if entry.origin.as_ref().is_some_and(|e| {
                !identity(&e.node)
                    || !run(&e.run)
                    || !matches!(e.port.as_str(), "data" | "error" | "cancel")
            }) {
                return Err(invalid());
            }
            if entry.inputs.iter().any(|e| {
                !identity(&e.node)
                    || !run(&e.run)
                    || !matches!(e.port.as_str(), "data" | "error" | "cancel")
                    || !inputs.insert(&e.node)
            }) {
                return Err(invalid());
            }
        }
        let by_node: BTreeMap<_, _> = self.lineage.iter().map(|e| (e.node.as_str(), e)).collect();
        let mut dependents: BTreeMap<&str, Vec<&str>> = BTreeMap::new();
        let mut remaining: BTreeMap<&str, usize> = BTreeMap::new();
        for entry in &self.lineage {
            remaining.insert(&entry.node, entry.inputs.len());
            for edge in &entry.inputs {
                if !by_node
                    .get(edge.node.as_str())
                    .is_some_and(|input| input.run == edge.run && input.revision == edge.revision)
                {
                    return Err(invalid());
                }
                dependents.entry(&edge.node).or_default().push(&entry.node);
            }
        }
        let mut ready: Vec<_> = remaining
            .iter()
            .filter_map(|(n, count)| (*count == 0).then_some(*n))
            .collect();
        let mut visited = 0usize;
        while let Some(node) = ready.pop() {
            visited += 1;
            for dependent in dependents.get(node).into_iter().flatten() {
                let count = remaining.get_mut(dependent).expect("validated lineage");
                *count -= 1;
                if *count == 0 {
                    ready.push(*dependent);
                }
            }
        }
        if visited != self.lineage.len() {
            return Err(invalid());
        }
        for member in &self.members {
            if !name(&member.name)
                || !names.insert(&member.name)
                || !identity(&member.node)
                || !run(&member.run)
                || member.typing.len() > 64 * 1024
                || member.fields.len() > 128
                || !matches!(member.port.as_str(), "data" | "error" | "cancel")
                || !matches!(
                    member.operation.as_str(),
                    "scan" | "scan_attempt" | "call" | "calculation" | "input" | "view" | "other"
                )
                || member.fields.iter().any(|f| !name(f))
                || !matches!(
                    member.state.as_str(),
                    "pending" | "running" | "ready" | "stale" | "failed" | "cancelled" | "skipped"
                )
                || !by_node
                    .get(member.node.as_str())
                    .is_some_and(|e| e.run == member.run && e.revision == member.revision)
                || member.evidence.as_ref().is_some_and(|k| {
                    !matches!(k.as_str(), "incomplete" | "stopped_stream")
                        || !matches!(member.state.as_str(), "cancelled" | "failed" | "skipped")
                })
                || member.payload.as_ref().is_some_and(|p| {
                    (member.state != "ready" && member.evidence.is_none())
                        || member.run.is_none()
                        || ValueHandle::new(&p.handle).is_err()
                        || !digest(&p.digest)
                })
            {
                return Err(invalid());
            }
        }
        Ok(())
    }
    pub fn handles(&self) -> Vec<ValueHandle> {
        self.members
            .iter()
            .filter_map(|m| m.payload.as_ref())
            .filter_map(|p| ValueHandle::new(&p.handle).ok())
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect()
    }
    pub(crate) fn charge(&self) -> u64 {
        let mut bytes = 1024u64;
        for text in [
            &self.id,
            &self.name,
            &self.owner,
            &self.scope,
            &self.definition,
        ] {
            bytes = bytes.saturating_add(text.len() as u64 * 6 + 64);
        }
        for member in &self.members {
            bytes = bytes.saturating_add(
                1024 + (member.name.len() + member.node.len() + member.typing.len()) as u64 * 6,
            );
            if let Some(run) = &member.run {
                bytes = bytes.saturating_add(run.len() as u64 * 6);
            }
            bytes = bytes.saturating_add(
                member
                    .fields
                    .iter()
                    .map(|s| s.len() as u64 * 6 + 64)
                    .sum::<u64>(),
            );
        }
        for entry in &self.lineage {
            bytes = bytes.saturating_add(
                1024 + entry.node.len() as u64 * 6
                    + entry.run.as_ref().map_or(0, |r| r.len() as u64 * 6),
            );
            if let Some(origin) = &entry.origin {
                bytes = bytes.saturating_add(
                    512 + origin.node.len() as u64 * 6
                        + origin.run.as_ref().map_or(0, |r| r.len() as u64 * 6),
                );
            }
            for edge in &entry.inputs {
                bytes = bytes.saturating_add(
                    512 + edge.node.len() as u64 * 6
                        + edge.run.as_ref().map_or(0, |r| r.len() as u64 * 6),
                );
            }
        }
        let mut pending: Vec<_> = self.alias_shape.iter().collect();
        while let Some(shape) = pending.pop() {
            bytes = bytes.saturating_add(512);
            match shape {
                wes_core::Shape::Record(record) => {
                    bytes = bytes.saturating_add(record.name().len() as u64 * 6);
                    for (field, shape) in record.fields() {
                        bytes = bytes.saturating_add(field.len() as u64 * 6 + 64);
                        pending.push(shape);
                    }
                }
                wes_core::Shape::List(element)
                | wes_core::Shape::Option(element)
                | wes_core::Shape::Iter(element)
                | wes_core::Shape::Dataset(element) => pending.push(element),
                _ => {}
            }
        }
        bytes
    }
}
