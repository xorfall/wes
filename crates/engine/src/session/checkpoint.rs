//! A scoped admission pause; the session continues owning and joining all execution and I/O.
use super::{RecordingMode, SessionError};
use crate::{
    driver::CancellationToken,
    history::{HistoryCaptureLimits, HistoryImage},
    recording::CapturedHistory,
    source::{ParsedSource, SourcePreparation},
};
use std::sync::Arc;
use tokio::{sync::oneshot, task::JoinSet};

/// Admission resumes when this guard is dropped. Shutdown does not wait for caller-owned guards.
pub struct SessionPause {
    _resume: oneshot::Sender<()>,
    authority: Arc<()>,
}
pub struct SessionCheckpoint {
    history: CapturedHistory,
    pause: SessionPause,
    resumed: oneshot::Receiver<()>,
}
impl SessionCheckpoint {
    pub(super) fn authority(&self) -> Arc<()> {
        self.pause.authority.clone()
    }
    pub fn history(&self) -> &HistoryImage {
        self.history.image()
    }
    /// The caller now owns image memory; retain the pause until save/switch has been decided.
    pub fn into_parts(self) -> (HistoryImage, SessionPause) {
        (self.history.into_image(), self.pause)
    }
    /// Release this pause and wait until the actor has joined its checkpoint task.
    /// Useful for owner workflows returning a reply that permits the next mutation.
    pub async fn resume(self) {
        let Self {
            history,
            pause,
            resumed,
        } = self;
        drop(history);
        drop(pause);
        let _ = resumed.await;
    }
}
type Reply = oneshot::Sender<Result<SessionCheckpoint, SessionError>>;

#[derive(Default)]
pub(super) struct Checkpoints {
    pending: Option<Reply>,
    work: JoinSet<Option<oneshot::Sender<()>>>,
    shutdown: CancellationToken,
    authority: Option<Arc<()>>,
    retirement_fenced: bool,
}
impl Checkpoints {
    pub fn permits(&self, authority: &Arc<()>) -> bool {
        !self.work.is_empty()
            && self
                .authority
                .as_ref()
                .is_some_and(|a| Arc::ptr_eq(a, authority))
    }
    pub fn fence_retirement(&mut self, authority: &Arc<()>) -> bool {
        if !self.permits(authority) {
            return false;
        }
        self.retirement_fenced = true;
        true
    }
    pub fn rejects_sources(&self) -> bool {
        self.retirement_fenced && self.busy()
    }
    pub fn retirement_fenced(&self) -> bool {
        self.retirement_fenced
    }
    pub fn pending(&self) -> bool {
        self.pending.is_some()
    }
    pub fn busy(&self) -> bool {
        self.pending.is_some() || !self.work.is_empty()
    }
    pub fn request(&mut self, reply: Reply, recording: &RecordingMode) {
        if self.busy() {
            let _ = reply.send(Err(SessionError::CheckpointBusy));
        } else if matches!(recording, RecordingMode::Ephemeral) {
            let _ = reply.send(Err(SessionError::NoHistory));
        } else {
            self.retirement_fenced = false;
            self.pending = Some(reply);
            self.authority = Some(Arc::new(()));
        }
    }
    pub fn start_if_settled(&mut self, settled: bool, failed: bool, recording: &RecordingMode) {
        let Some(reply) = &self.pending else { return };
        if reply.is_closed() {
            self.pending = None;
            return;
        }
        if failed {
            let _ = self
                .pending
                .take()
                .expect("pending request")
                .send(Err(SessionError::Recording));
            return;
        }
        if !settled {
            return;
        }
        let reply = self.pending.take().expect("pending request");
        let RecordingMode::Required(journal) = recording else {
            unreachable!("checked at request")
        };
        let journal = journal.clone();
        let shutdown = self.shutdown.clone();
        let authority = self.authority.clone().expect("checkpoint authority");
        self.work.spawn(async move {
            let (recorder, _) = journal.recording();
            // Do not select cancellation against capture: an entered physical read must be joined.
            let history = recorder.capture(HistoryCaptureLimits::default()).await;
            if shutdown.is_cancelled() {
                let _ = reply.send(Err(SessionError::Stopped));
                return None;
            }
            let history = match history {
                Ok(history)
                    if history.append_report().failed == 0
                        && journal.checkpoint(history.image().checkpoint()).is_ok() =>
                {
                    history
                }
                _ => {
                    let _ = reply.send(Err(SessionError::Recording));
                    return None;
                }
            };
            let (resume, resumed) = oneshot::channel();
            let (acknowledge, acknowledged) = oneshot::channel();
            if reply
                .send(Ok(SessionCheckpoint {
                    history,
                    pause: SessionPause {
                        _resume: resume,
                        authority,
                    },
                    resumed: acknowledged,
                }))
                .is_ok()
            {
                tokio::select! {
                    _ = resumed => {},
                    _ = shutdown.cancelled() => {},
                }
            }
            Some(acknowledge)
        });
    }
    pub async fn completed(&mut self) -> Option<oneshot::Sender<()>> {
        if let Some(reply) = &mut self.pending {
            reply.closed().await;
            self.pending = None;
            None
        } else {
            self.work.join_next().await.and_then(Result::ok).flatten()
        }
    }
    pub fn close(&mut self) {
        self.shutdown.cancel();
        if let Some(reply) = self.pending.take() {
            let _ = reply.send(Err(SessionError::Stopped));
        }
    }
    pub fn allows_cancel(&self, source: &SourcePreparation<ParsedSource>) -> bool {
        // Only while settling. During/after capture the quiescent state must remain unchanged.
        self.pending.is_some()
            && matches!(source,
            SourcePreparation::Immediate { statement, .. }
                if matches!(&statement.expression, wes_language::Expression::Call(call)
                    if wes_language::vocabulary::commands::invocation(call).is_ok_and(|i|i.spec.command == wes_language::vocabulary::MetaCommand::Cancel)))
    }
}
