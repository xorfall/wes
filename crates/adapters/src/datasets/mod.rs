//! Bounded dataset formats. Filesystem ownership and execution stay outside the codec.
pub mod catalog;
mod format;
mod objects;
pub use format::{
    FormatError, FormatLimits, PositionUnit, Record, SegmentHeader, SegmentReader, SourceRange,
    encode_segment,
};
pub use objects::{ObjectError, ObjectFiles, ObjectLimits};
