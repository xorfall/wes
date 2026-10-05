//! Transport-independent execution endpoints. Concrete process/PTY/network I/O is adapter-owned.
use crate::driver::CancellationToken;
use std::{io, sync::Arc, time::Duration};
use wes_core::environments::{Revision, Target};

#[derive(Clone, Copy)]
pub struct TerminalSize {
    pub cols: u16,
    pub rows: u16,
}
#[derive(Clone, Copy)]
pub struct TerminalExit {
    pub code: u32,
    /// Closing the local endpoint does not establish the remote process outcome.
    pub uncertain: bool,
}
/// Nonblocking byte endpoint, exclusively owned by one terminal worker. Shutdown joins
/// entered local execution. An endpoint must release its resources if its owner unwinds.
pub trait TerminalIo: io::Read + io::Write + Send {
    /// Bounded display evidence for the actually opened destination, frozen for this endpoint.
    fn identity(&self) -> Option<String> {
        None
    }
    fn resize(&mut self, size: TerminalSize) -> io::Result<()>;
    fn wait_ready(&mut self, writing: bool, timeout: Duration) -> io::Result<()>;
    fn try_exit(&mut self) -> io::Result<Option<TerminalExit>>;
    fn shutdown(&mut self) -> io::Result<TerminalExit>;
}
/// Constructed by the session owner after revision and authority validation. Not serializable.
pub struct TargetLease {
    pub(crate) _reference: Arc<()>,
    pub environment: String,
    pub revision: Revision,
    pub target: Arc<Target>,
    pub cancelled: CancellationToken,
}

/// A changed definition is a review, not permission to acquire a process/target lease.
pub enum TargetPreparation {
    Ready(TargetLease),
    Review(TargetReview),
}
pub struct TargetReview {
    pub previous_revision: Revision,
    pub previous: Option<Arc<wes_core::environments::EffectiveEnvironment>>,
    pub current: Arc<wes_core::environments::EffectiveEnvironment>,
    pub target: Arc<Target>,
}

/// Ephemeral references for admitted execution outside graph nodes. Persistence never
/// restores these references or their authority. Environment deletion shares this ledger.
#[derive(Clone, Default)]
pub(crate) struct TargetLeases(Arc<std::sync::Mutex<Vec<(String, std::sync::Weak<()>)>>>);
impl TargetLeases {
    pub fn acquire(&self, identity: &str) -> Result<Arc<()>, &'static str> {
        let mut references = self.0.lock().expect("execution leases");
        references.retain(|(_, reference)| reference.strong_count() > 0);
        if references.len() >= 1024 {
            return Err("Execution target session capacity is exhausted");
        }
        let reference = Arc::new(());
        references.push((identity.into(), Arc::downgrade(&reference)));
        Ok(reference)
    }
    pub fn references(&self, identity: &str) -> bool {
        self.0
            .lock()
            .expect("execution leases")
            .iter()
            .any(|(name, reference)| name == identity && reference.strong_count() > 0)
    }
}
