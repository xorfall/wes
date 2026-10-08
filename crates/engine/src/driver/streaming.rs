//! Deliver events and coalesced observations through distinct messages, retaining physical ownership.
use super::{CancellationToken, ExecutionReport, Executor};
use crate::{
    runtime::{Effect, Outcome, Run, RunTicket, Runtime, RuntimeCode},
    streams::{Phase, Snapshot, delivery::Event},
};
use std::{sync::Arc, time::Duration};
use tokio::sync::{mpsc, oneshot};

enum Publication {
    Window { snapshot: Arc<Snapshot> },
    Event { run: Run, event: Event },
}
pub(crate) struct Update {
    publication: Publication,
    reply: oneshot::Sender<bool>,
}
impl Update {
    pub fn deferrable<T: Clone>(&self, runtime: &Runtime<T>) -> bool {
        let (run, candidate) = match &self.publication {
            Publication::Event { run, .. } => (run, true),
            Publication::Window { snapshot, .. } => (
                &snapshot.run,
                snapshot.phase == Phase::Open
                    || (snapshot.phase == Phase::Ended
                        && runtime.ordered_root(snapshot.run.node()).is_some()),
            ),
        };
        candidate
            && runtime.is_streaming(run.node())
            && runtime.run_of(run.node()) == Some(run.id())
    }
    pub fn apply<T: Clone>(self, runtime: &mut Runtime<T>, now: Duration) -> Vec<Effect<T>> {
        let effects = match self.publication {
            Publication::Event { run, event } => runtime.ingest_event(&run, event, now),
            Publication::Window { snapshot } => {
                let items = match snapshot.window.data() {
                    wes_core::Data::List(items) => items.len(),
                    _ => 0,
                };
                runtime.set_stream_counts(
                    &snapshot.run,
                    snapshot.omitted,
                    snapshot.rejected,
                    items,
                );
                if snapshot.phase == Phase::Ended
                    && runtime.ordered_root(snapshot.run.node()).is_none()
                {
                    let _ = self.reply.send(true);
                    return vec![];
                }
                match &snapshot.phase {
                    Phase::Open | Phase::Ended => {
                        runtime.set_stream_start(&snapshot.run, snapshot.omitted);
                        let mut effects = vec![];
                        match runtime.stream_window(&snapshot.run, snapshot.window.clone(), now) {
                            Some(next) => {
                                effects.extend(next);
                                Some(effects)
                            }
                            None if !effects.is_empty() => Some(effects),
                            None => None,
                        }
                    }
                    Phase::Closing => runtime.stream_closing(&snapshot.run),
                    _ => None,
                }
            }
        };
        let _ = self.reply.send(effects.is_some());
        effects.unwrap_or_default()
    }
}
async fn publish(publication: Publication, updates: &mpsc::Sender<Update>) -> bool {
    let (reply, receive) = oneshot::channel();
    updates.send(Update { publication, reply }).await.is_ok() && receive.await.unwrap_or(false)
}
fn failed(message: &str) -> ExecutionReport {
    Outcome::Failed(RuntimeCode::ExecutionFailed.error(message, None)).into()
}
pub(super) async fn execute<T: Send + 'static>(
    executor: Arc<dyn Executor<T>>,
    ticket: RunTicket<T>,
    cancellation: CancellationToken,
    permit: super::capacity::Permit,
    updates: mpsc::Sender<Update>,
) -> ExecutionReport {
    let run = ticket.run.clone();
    let (handle, task) = match executor.execute_stream(ticket, cancellation.clone()).await {
        Ok(stream) => stream,
        Err(report) => return report,
    };
    if handle.snapshot().run != run {
        let _ = task.shutdown().await;
        return failed("The stream was opened for a different run.");
    }
    let mut events = handle.take_events();
    let direct = events.is_some();
    let mut snapshots = handle.subscribe();
    let joined = task.join();
    tokio::pin!(joined);
    let mut permit = Some(permit);
    let mut stopped = false;
    let mut published: Option<Arc<Snapshot>> = None;
    loop {
        if cancellation.is_cancelled() {
            handle.cancel();
            stopped = true;
        }
        let snapshot = snapshots.borrow_and_update().clone();
        if matches!(snapshot.phase, Phase::Open | Phase::Closing)
            && !stopped
            && published
                .as_ref()
                .is_none_or(|old| !Arc::ptr_eq(old, &snapshot))
        {
            if !publish(
                Publication::Window {
                    snapshot: snapshot.clone(),
                },
                &updates,
            )
            .await
            {
                handle.cancel();
                stopped = true;
            } else if snapshot.phase == Phase::Open {
                drop(permit.take());
            }
            published = Some(snapshot.clone());
        }
        tokio::select! {
            biased;
            () = cancellation.cancelled(), if !stopped => { handle.cancel(); stopped = true; }
            result = &mut joined => {
                if result.is_err() { return failed("The stream lifetime owner terminated unexpectedly."); }
                break;
            }
            change = snapshots.changed() => {
                if change.is_err() {
                    if joined.as_mut().await.is_err() { return failed("The stream lifetime owner terminated unexpectedly."); }
                    break;
                }
            }
            event = async { events.as_mut().expect("guarded event receiver").recv().await },
                if direct && !stopped && snapshot.phase == Phase::Open => {
                match event {
                    Some(event) => {
                        if !publish(Publication::Event { run: run.clone(), event }, &updates).await { handle.cancel(); stopped = true; }
                    }
                    None => {
                        if joined.as_mut().await.is_err() { return failed("The stream lifetime owner terminated unexpectedly."); }
                        break;
                    }
                }
            }
        }
    }
    let final_ = handle.snapshot();
    match &final_.phase {
        Phase::Ended => {
            // A synchronous provider can finish before the driver observes its open projection.
            // Release the finite permit before draining events so a single-slot driver can run them.
            let accepted = publish(
                Publication::Window {
                    snapshot: final_.clone(),
                },
                &updates,
            )
            .await;
            drop(permit.take());
            if accepted && !stopped {
                if let Some(events) = &mut events {
                    while let Some(event) = events.recv().await {
                        if !publish(
                            Publication::Event {
                                run: run.clone(),
                                event,
                            },
                            &updates,
                        )
                        .await
                        {
                            break;
                        }
                    }
                }
            }
            ExecutionReport {
                outcome: Outcome::Produced(final_.window.clone()),
                notices: vec![],
                stream_start: Some(final_.omitted),
                holds: vec![],
                progress: None,
            }
        }
        Phase::Failed(error) => Outcome::Failed(error.clone()).into(),
        Phase::Cancelled => Outcome::Cancelled(
            RuntimeCode::Cancelled.error("The local stream was cancelled.", None),
        )
        .with_policy(final_.window.provenance().policy())
        .into(),
        _ => failed("The stream ended without a terminal observation."),
    }
}
