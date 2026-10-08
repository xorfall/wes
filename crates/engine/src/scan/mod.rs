//! Pure record analysis. Sources and retained outputs are bounded independently.
mod captured;
pub mod ledger;
mod transition;
pub use transition::Transition;
mod source;
pub use source::{CapturedSource, SourceFailure, SourcePoll, frame_shape, framing_charge};
mod runner;
pub use runner::{
    Completion, Identity, Input, Phase, Poll, PreparedResume, Progress, Runner, Settings,
    SourceIdentity, Stop,
};
mod bound;
pub use bound::BoundScan;
mod resume;
pub use resume::BoundResume;

mod excerpt;
mod owned;
pub use excerpt::BoundExcerpt;
pub(crate) use excerpt::read_source_excerpt;
