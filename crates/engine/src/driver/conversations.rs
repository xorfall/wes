//! Bounded acknowledged live I/O relay. The completion future retains the physical execution lease.
use super::{CancellationToken, ExecutionReport, Executor};
use crate::{
    conversations::{ConversationHandle, Output},
    runtime::{Outcome, Run, RunTicket, RuntimeCode},
};
use std::sync::Arc;
use tokio::sync::{mpsc, oneshot};

/// Transient conversation events, never history records. `Started` acknowledges the driver's
/// ownership of the port; it does not promise the process has displayed its first prompt yet.
#[derive(Clone)]
pub enum ConversationEvent {
    Started(Run),
    Output { run: Run, batch: Arc<Output> },
    Ended(Run),
}
pub(crate) enum Update {
    Started {
        run: Run,
        handle: ConversationHandle,
        reply: oneshot::Sender<bool>,
    },
    Output {
        run: Run,
        batch: Output,
        reply: oneshot::Sender<bool>,
    },
}
impl Update {
    pub fn reject(self) {
        let reply = match self {
            Self::Started { reply, .. } | Self::Output { reply, .. } => reply,
        };
        let _ = reply.send(false);
    }
}
fn failed(message: &str) -> ExecutionReport {
    Outcome::Failed(RuntimeCode::ExecutionFailed.error(message, None)).into()
}

pub(super) async fn execute<T: Send + 'static>(
    executor: Arc<dyn Executor<T>>,
    ticket: RunTicket<T>,
    cancellation: CancellationToken,
    updates: mpsc::Sender<Update>,
) -> ExecutionReport {
    let run = ticket.run.clone();
    let active = match executor
        .execute_interactive(ticket, cancellation.clone())
        .await
    {
        Ok(active) => active,
        Err(report) => return report,
    };
    let handle = active.handle;
    if handle.snapshot().run != run {
        handle.cancel();
        let mut report = active.completion.await;
        report.outcome = failed("The conversation was started for a different run.").outcome;
        return report;
    }
    let (reply, receive) = oneshot::channel();
    let accepted = updates
        .send(Update::Started {
            run: run.clone(),
            handle: handle.clone(),
            reply,
        })
        .await
        .is_ok()
        && receive.await.unwrap_or(false);
    if !accepted {
        handle.cancel();
    }
    let mut stopped = !accepted;
    let mut changes = handle.subscribe();
    let completion = active.completion;
    tokio::pin!(completion);
    loop {
        if cancellation.is_cancelled() {
            stopped = true;
            handle.cancel();
        }
        changes.borrow_and_update();
        if !stopped && !flush(&run, &handle, &updates).await {
            stopped = true;
            handle.cancel();
        }
        tokio::select! {
            biased;
            report = &mut completion => {
                // The provider flushes decoder tails before completing. Deliver the last accepted
                // batch before the worker completion can advance the node to Ready/Failed.
                if !stopped { let _ = flush(&run, &handle, &updates).await; }
                return report;
            },
            _ = cancellation.cancelled(), if !stopped => { stopped = true; handle.cancel(); },
            changed = changes.changed() => {
                if changed.is_err() {
                    // Physical output closes before a potentially blocked Called acknowledgement.
                    // Flush now, then join that acknowledgement without a closed-watch busy loop.
                    if !stopped { let _ = flush(&run, &handle, &updates).await; }
                    return completion.await;
                }
            },
        }
    }
}
async fn flush(run: &Run, handle: &ConversationHandle, updates: &mpsc::Sender<Update>) -> bool {
    let batch = handle.take_output();
    if batch.is_empty() {
        return true;
    }
    let (reply, receive) = oneshot::channel();
    updates
        .send(Update::Output {
            run: run.clone(),
            batch,
            reply,
        })
        .await
        .is_ok()
        && receive.await.unwrap_or(false)
}
