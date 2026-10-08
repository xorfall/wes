//! Bounded dataset formats. Filesystem ownership and execution stay outside the codec.
pub mod catalog;
mod checkpoint;
mod format;
mod manifest;
mod objects;
mod store;
mod tree;
pub use checkpoint::{
    Checkpoint, CheckpointBindings, CheckpointLimits, InlineSnapshot, WorkLedger,
};
pub use format::{
    FormatError, FormatLimits, PositionUnit, Record, SegmentCoverage, SegmentHeader, SegmentReader,
    SourceRange, Stream, encode_segment,
};
pub use manifest::{
    DatasetKind, Lifecycle, Manifest, ManifestLimits, Persistence as DatasetPersistence,
};
pub use objects::{IndexRange, ObjectError, ObjectFiles, ObjectLimits};
pub use store::{
    CommitReceipt, DatasetError, DatasetStore, Reconciliation, ReferenceReceipt, StoreLimits,
};
pub use store::{DatasetPage, PageLimits};
pub use tree::{
    Branch as IndexBranch, Entries as IndexEntries, IndexEntry, IndexLimits, IndexNode,
    IndexSummary,
};
