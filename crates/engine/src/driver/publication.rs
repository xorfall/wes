//! One acknowledged ordinary result while a separate physical owner remains joined.
use super::{ExecutionReport, capacity::Permit};
use crate::{
    runtime::{Effect, Outcome, Run, Runtime, RuntimeCode},
    tasks::BoundTask,
    workspace::Workspace,
};
use std::{
    sync::{Arc, Mutex},
    time::Duration,
};
use tokio::sync::{OwnedSemaphorePermit, Semaphore, mpsc, oneshot};
#[cfg(test)]
mod tests;

/// Native, read-only generation check. This cannot confirm a probe or grant a read.
#[derive(Clone)]
pub struct Guard(Arc<dyn Fn() -> bool + Send + Sync>);
impl std::fmt::Debug for Guard {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("PublicationGuard")
    }
}
impl Guard {
    pub(crate) fn new(check: impl Fn() -> bool + Send + Sync + 'static) -> Self {
        Self(Arc::new(check))
    }
    pub(crate) fn current(&self) -> bool {
        (self.0)()
    }
}
pub(crate) fn checked(report: &ExecutionReport) -> Outcome {
    if report.publication_guard.as_ref().is_none_or(Guard::current) {
        return report.outcome.clone();
    }
    let policy = report.outcome.flow_policy();
    Outcome::Failed(
        RuntimeCode::ResultForbidden
            .error(
                "Result read authority changed before publication; no source was replayed.",
                None,
            )
            .with_policy(&policy),
    )
}

type CallSlot = Arc<Mutex<Option<Permit>>>;
pub(crate) struct Update {
    pub run: Run,
    pub report: ExecutionReport,
    reply: oneshot::Sender<bool>,
    call_slot: CallSlot,
    _credit: OwnedSemaphorePermit,
}
impl Update {
    fn acknowledge(self, effects: Option<Vec<Effect<BoundTask>>>) -> Vec<Effect<BoundTask>> {
        if effects.is_some() {
            self.call_slot
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .take();
        }
        let _ = self.reply.send(effects.is_some());
        effects.unwrap_or_default()
    }
    pub fn apply<T: Clone>(self, runtime: &mut Runtime<T>, now: Duration) -> Vec<Effect<T>> {
        if self.reply.is_closed() {
            return vec![];
        }
        let effects = runtime.publish_result(&self.run, checked(&self.report), now);
        if effects.is_some() {
            self.call_slot
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .take();
        }
        let _ = self.reply.send(effects.is_some());
        effects.unwrap_or_default()
    }
    pub fn apply_workspace(
        self,
        workspace: &mut Workspace,
        now: Duration,
    ) -> Vec<Effect<BoundTask>> {
        if self.reply.is_closed() {
            return vec![];
        }
        let effects = workspace.publish_result(&self.run, checked(&self.report), now);
        self.acknowledge(effects)
    }
}

#[derive(Clone, Default)]
pub struct Reporter {
    target: Option<(Run, mpsc::Sender<Update>, Arc<Semaphore>, CallSlot)>,
    sources: Option<mpsc::Sender<super::streaming::Update>>,
}
impl Reporter {
    pub(crate) fn new(
        run: Run,
        sender: mpsc::Sender<Update>,
        credit: Arc<Semaphore>,
        call_slot: CallSlot,
    ) -> Self {
        Self {
            target: Some((run, sender, credit, call_slot)),
            sources: None,
        }
    }
    pub(crate) fn with_sources(mut self, sources: mpsc::Sender<super::streaming::Update>) -> Self {
        self.sources = Some(sources);
        self
    }
    pub fn silent() -> Self {
        Self::default()
    }
    /// Attach one separate source to this original joined run before publishing its
    /// ordinary result. This sends no source windows or events to the data port.
    /// The worker retains and physically joins the source even if admission is refused
    /// or its single acknowledgement is lost; do not reacquire or replay it.
    pub async fn attach_source(&self, handle: &crate::streams::StreamHandle) -> bool {
        let (Some((run, ..)), Some(sources)) = (&self.target, &self.sources) else {
            return false;
        };
        super::streaming::attach(run.clone(), handle.clone(), sources).await
    }
    /// One attempt, never retry a lost acknowledgement. The original report and
    /// its shared holds stay with the physical owner until it joins on every path.
    pub async fn publish(&self, report: &ExecutionReport) -> bool {
        let Some((run, sender, pool, call_slot)) = &self.target else {
            return false;
        };
        let Some(charge) = charge(report).and_then(|n| u32::try_from(n).ok()) else {
            return false;
        };
        let Ok(credit) = pool.clone().try_acquire_many_owned(charge) else {
            return false;
        };
        let (reply, receive) = oneshot::channel();
        sender
            .send(Update {
                run: run.clone(),
                report: report.clone(),
                reply,
                call_slot: call_slot.clone(),
                _credit: credit,
            })
            .await
            .is_ok()
            && receive.await.unwrap_or(false)
    }
}
fn charge(report: &ExecutionReport) -> Option<u64> {
    let limit = wes_budgets::get("execution.publication.value.bytes");
    if report.holds.len() > 1024 || report.notices.len() > 128 {
        return None;
    }
    let mut total = 256u64.checked_add(report.holds.len() as u64 * 64)?;
    let outcome = match &report.outcome {
        Outcome::Produced(v) => crate::value_size::value_charge(v, limit)?,
        Outcome::Stored(output) => output.snapshot.as_ref()?.metadata_charge(),
        Outcome::Failed(e) | Outcome::Cancelled(e) => error_charge(e, limit)?,
        Outcome::Incomplete { value, error } => crate::value_size::value_charge(value, limit)?
            .checked_add(error_charge(error, limit)?)?,
        Outcome::Skipped => 0,
    };
    total = total.checked_add(outcome)?;
    for notice in &report.notices {
        total = total.checked_add(error_charge(notice, limit)?)?;
        if total > limit {
            return None;
        }
    }
    // Terminal publication carries ordinary results, not source window/progress mutations.
    (total <= limit && report.stream_start.is_none() && report.progress.is_none()).then_some(total)
}
fn error_charge(error: &wes_core::ErrorValue, limit: u64) -> Option<u64> {
    if error.issues().len() > wes_budgets::get("value_size.nodes") as usize {
        return None;
    }
    let mut total = 256u64;
    let mut text = |s: &str| -> Option<()> {
        total = total.checked_add((s.len() as u64).checked_mul(6)?.checked_add(64)?)?;
        (total <= limit).then_some(())
    };
    for s in [error.id().as_str(), error.code(), error.message()] {
        text(s)?;
    }
    if let Some(cause) = error.cause() {
        text(cause.as_str())?;
    }
    for issue in error.issues() {
        for s in [&issue.path, &issue.code, &issue.message] {
            text(s)?;
        }
    }
    for origin in error.policy().origins() {
        text(origin)?;
    }
    for origin in error.policy().dataset_reads() {
        for s in [origin.store(), origin.dataset()] {
            text(s)?;
        }
    }
    for origin in error.policy().provider_reads() {
        for s in [origin.realm(), origin.binding(), origin.scope()] {
            text(s)?;
        }
    }
    for location in error.locations() {
        text(&location.source)?;
    }
    Some(total.saturating_add(error.locations().len() as u64 * 1024))
}
