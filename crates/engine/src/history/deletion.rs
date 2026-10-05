//! Deletion validity follows effect-bearing evidence, not observation/audit append offsets.
use super::*;
use std::collections::BTreeSet;

/// Closed, side-effect-free observations plus the explicit apply receipt. Never infer purity
/// from arbitrary provider declarations, annotations, failed parsing or mixed scripts.
pub fn observation_source(text: &str) -> bool {
    let parsed = wes_language::parse(&wes_language::SourceText::new("deletion-evidence", text));
    if !parsed.diagnostics.is_empty() || parsed.script.statements.len() != 1 {
        return false;
    }
    let s = &parsed.script.statements[0];
    let wes_language::Expression::Call(c) = &s.expression else {
        return false;
    };
    if c.marker.is_none() || !s.annotations.is_empty() {
        return false;
    }
    let Ok(invocation) = wes_language::vocabulary::commands::invocation(c) else {
        return false;
    };
    use wes_language::vocabulary::MetaCommand::*;
    matches!(
        invocation.spec.command,
        WorkspacePlan | WorkspaceDelete | Read | Inspect | Help | List | Info | Trace
    )
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DeletionEvidence {
    digest: [u8; 32],
}
impl HistoryImage {
    pub fn observation_nodes(&self) -> BTreeSet<NodeId> {
        self.journal
            .iter()
            .filter_map(|e| match e {
                JournalEntry::Command(c) if observation_source(&c.text) => {
                    Some(c.nodes.iter().cloned())
                }
                _ => None,
            })
            .flatten()
            .collect()
    }
    pub fn observation_payloads(&self) -> BTreeSet<ValueHandle> {
        let nodes = self.observation_nodes();
        let mut candidates = std::collections::BTreeMap::<ValueHandle, bool>::new();
        for entry in &self.journal {
            let Some(handle) = entry.payload_reference() else {
                continue;
            };
            let observation = match entry {
                JournalEntry::Result(r) => {
                    nodes.contains(&r.node) && r.retention != crate::storage::Retention::Protected
                }
                JournalEntry::Payload { node, .. } => nodes.contains(node),
                JournalEntry::Snapshot(s) => nodes.contains(s.observation.node()),
                _ => false,
            };
            candidates
                .entry(handle.clone())
                .and_modify(|only| *only &= observation)
                .or_insert(observation);
        }
        candidates
            .into_iter()
            .filter_map(|(h, only)| only.then_some(h))
            .collect()
    }

    pub fn deletion_evidence(&self) -> DeletionEvidence {
        let nodes = self.observation_nodes();
        let cells: BTreeSet<_> = self
            .journal
            .iter()
            .filter_map(|e| match e {
                JournalEntry::Command(c) if observation_source(&c.text) => Some(c.cell.clone()),
                JournalEntry::Submitted(s) if observation_source(&s.text) => Some(s.cell.clone()),
                _ => None,
            })
            .collect();
        let journal = self.journal.iter().filter(|e| match e {
            JournalEntry::Command(c) => !cells.contains(&c.cell),
            JournalEntry::Submitted(s) => !cells.contains(&s.cell),
            JournalEntry::Observed(o) => !nodes.contains(o.node()),
            JournalEntry::Payload { node, .. } => !nodes.contains(node),
            JournalEntry::Result(r) => {
                !nodes.contains(&r.node) || r.retention == crate::storage::Retention::Protected
            }
            JournalEntry::Snapshot(s) => !nodes.contains(s.observation.node()),
            JournalEntry::Diagnosed(d) => !cells.contains(d.cell()),
            // Explicit protection, notices, call evidence and mutations always remain relevant.
            _ => true,
        });
        let recovery = self
            .recovery
            .iter()
            .filter(|e| !matches!(e, RecoveryEntry::Accepted { cell } if cells.contains(cell)));
        // Process-local equality proof, not a persisted format. Stream bounded history evidence
        // into a digest rather than retaining another full history for every live preview.
        use sha2::{Digest, Sha256};
        use std::fmt::Write;
        struct Evidence(Sha256);
        impl std::fmt::Write for Evidence {
            fn write_str(&mut self, text: &str) -> std::fmt::Result {
                self.0.update(text.as_bytes());
                Ok(())
            }
        }
        let mut evidence = Evidence(Sha256::new());
        for entry in journal {
            write!(&mut evidence, "journal:{entry:?}\n").expect("digest writer");
        }
        for entry in recovery {
            write!(&mut evidence, "recovery:{entry:?}\n").expect("digest writer");
        }
        DeletionEvidence {
            digest: evidence.0.finalize().into(),
        }
    }
}
