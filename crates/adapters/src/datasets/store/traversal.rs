//! One bounded index walk for recovery proofs and physical reachability.
use super::*;
use crate::datasets::{IndexEntry, IndexSummary, SourceRange, Stream};

pub(super) enum IndexVisit<'a> {
    Node(&'a ObjectRef),
    Segment(&'a IndexEntry),
}
#[derive(Clone, Copy)]
pub(super) enum Visit {
    Descend,
    /// The caller already holds the same immutable subtree's complete proof or inventory.
    Skip,
}

#[allow(clippy::too_many_arguments)]
pub(super) fn walk_index(
    files: &ObjectFiles,
    dataset: &str,
    stream: Stream,
    root: Option<&ObjectRef>,
    summary: &IndexSummary,
    source: &SourceRange,
    limit: usize,
    work: &mut usize,
    mut visit: impl FnMut(IndexVisit<'_>) -> Result<Visit, DatasetError>,
) -> Result<(), DatasetError> {
    let Some(root) = root else {
        return if summary.first == 0 && summary.end == 0 && summary.segment_bytes == 0 {
            Ok(())
        } else {
            Err(DatasetError::StorageCorrupt)
        };
    };
    // Height comes from the validated root. Every subsequent edge carries its expected height.
    let mut pending = vec![(root.clone(), None, summary.clone())];
    let mut seen = BTreeSet::new();
    while let Some((reference, height, expected)) = pending.pop() {
        admit(&mut seen, &reference, limit, work)?;
        let node = files.read_index(&reference, dataset, stream, None)?;
        if node.summary != expected || height.is_some_and(|height| node.height != height) {
            return Err(DatasetError::StorageCorrupt);
        }
        if matches!(visit(IndexVisit::Node(&reference))?, Visit::Skip) {
            continue;
        }
        match node.entries {
            Entries::Branch { children } => {
                if seen
                    .len()
                    .saturating_add(pending.len())
                    .saturating_add(children.len())
                    > limit
                {
                    return Err(DatasetError::Limit("index traversal"));
                }
                let height = node
                    .height
                    .checked_sub(1)
                    .ok_or(DatasetError::StorageCorrupt)?;
                for child in children.into_iter().rev() {
                    pending.push((child.node, Some(height), child.summary));
                }
            }
            Entries::Leaf { entries } => {
                for entry in entries {
                    admit(&mut seen, &entry.segment, limit, work)?;
                    if entry.source.identity != source.identity
                        || entry.source.unit != source.unit
                        || entry.source.start < source.start
                        || entry.source.end > source.end
                    {
                        return Err(DatasetError::StorageCorrupt);
                    }
                    visit(IndexVisit::Segment(&entry))?;
                }
            }
        }
    }
    Ok(())
}

fn admit(
    seen: &mut BTreeSet<String>,
    reference: &ObjectRef,
    limit: usize,
    work: &mut usize,
) -> Result<(), DatasetError> {
    if seen.len() >= limit || *work == 0 {
        return Err(DatasetError::Limit("index traversal"));
    }
    if !seen.insert(reference.id.clone()) {
        return Err(DatasetError::StorageCorrupt);
    }
    *work -= 1;
    Ok(())
}
