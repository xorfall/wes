//! Lazy ordinal ranges visit an immutable index path once per physical page.
use super::*;
use wes_engine::storage::datasets::ReadWork;

enum Edge {
    Node {
        reference: ObjectRef,
        height: Option<u8>,
        summary: IndexSummary,
    },
    Segment(IndexEntry),
}

/// Operation-local pending edges, not a decoded-node cache or read authority.
/// Dropping a page or refusing its work drops this walk; retries start with the
/// current owned reader gate and charge every physical read they actually enter.
pub struct IndexRange<'a> {
    files: &'a ObjectFiles,
    dataset: &'a str,
    stream: Stream,
    work: Option<&'a ReadWork>,
    next: u64,
    end: u64,
    pending: Vec<Edge>,
    pending_limit: usize,
    failed: bool,
}

impl ObjectFiles {
    pub fn range<'a>(
        &'a self,
        root: &ObjectRef,
        dataset: &'a str,
        stream: Stream,
        summary: &IndexSummary,
        from: u64,
        work: Option<&'a ReadWork>,
    ) -> Result<IndexRange<'a>, ObjectError> {
        if from < summary.first || from > summary.end {
            return Err(ObjectError::Limit("index ordinal range"));
        }
        let pending_limit = self
            .limits
            .index
            .fanout
            .checked_mul(self.limits.index.depth as usize + 1)
            .filter(|n| *n > 0)
            .ok_or(ObjectError::Limit("index pending edges"))?;
        Ok(IndexRange {
            files: self,
            dataset,
            stream,
            work,
            next: from,
            end: summary.end,
            pending: if from == summary.end {
                vec![]
            } else {
                vec![Edge::Node {
                    reference: root.clone(),
                    height: None,
                    summary: summary.clone(),
                }]
            },
            pending_limit,
            failed: false,
        })
    }
}

impl IndexRange<'_> {
    /// No future subtree is read until the page asks for its next segment.
    /// A refusal poisons this operation; there is no way to skip the failed edge.
    pub fn next_entry(&mut self) -> Result<Option<IndexEntry>, ObjectError> {
        if self.failed {
            return Err(ObjectError::Corrupt);
        }
        let result = self.advance();
        if result.is_err() {
            self.failed = true;
        }
        result
    }

    fn advance(&mut self) -> Result<Option<IndexEntry>, ObjectError> {
        if self.next == self.end {
            return Ok(None);
        }
        while let Some(edge) = self.pending.pop() {
            match edge {
                Edge::Segment(entry) => {
                    if entry.summary.first > self.next
                        || entry.summary.end <= self.next
                        || entry.summary.end > self.end
                    {
                        return Err(ObjectError::Corrupt);
                    }
                    self.next = entry.summary.end;
                    return Ok(Some(entry));
                }
                Edge::Node {
                    reference,
                    height,
                    summary,
                } => {
                    if summary.first > self.next
                        || summary.end <= self.next
                        || summary.end > self.end
                    {
                        return Err(ObjectError::Corrupt);
                    }
                    let node =
                        self.files
                            .read_index(&reference, self.dataset, self.stream, self.work)?;
                    if node.summary != summary || height.is_some_and(|height| height != node.height)
                    {
                        return Err(ObjectError::Corrupt);
                    }
                    let count = match &node.entries {
                        Entries::Leaf { entries } => entries.len(),
                        Entries::Branch { children } => children.len(),
                    };
                    if self
                        .pending
                        .len()
                        .checked_add(count)
                        .is_none_or(|n| n > self.pending_limit)
                    {
                        return Err(ObjectError::Limit("index pending edges"));
                    }
                    match node.entries {
                        Entries::Leaf { entries } => {
                            self.pending.extend(
                                entries
                                    .into_iter()
                                    .rev()
                                    .filter(|entry| entry.summary.end > self.next)
                                    .map(Edge::Segment),
                            );
                        }
                        Entries::Branch { children } => {
                            let height = node.height.checked_sub(1).ok_or(ObjectError::Corrupt)?;
                            self.pending.extend(
                                children
                                    .into_iter()
                                    .rev()
                                    .filter(|child| child.summary.end > self.next)
                                    .map(|child| Edge::Node {
                                        reference: child.node,
                                        height: Some(height),
                                        summary: child.summary,
                                    }),
                            );
                        }
                    }
                }
            }
        }
        Err(ObjectError::Corrupt)
    }
}
