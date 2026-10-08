//! Owner-side Log retention. Broadcasts are wakeups, never the source of durable history.
use super::RecordingMode;
use crate::{
    history::InvalidRecord,
    log::{LogRecorder, LogStatus, Logged, PreparedLog},
};
use std::collections::BTreeMap;
use tokio::{sync::broadcast, task::JoinSet};

fn max_entries() -> usize {
    wes_budgets::get("history.window.entries") as usize
}
fn max_bytes() -> u64 {
    wes_budgets::get("history.window.bytes") as u64
}
fn max_pending() -> usize {
    wes_budgets::get("history.pending") as usize
}

#[derive(Clone, Debug)]
pub struct LogSnapshot {
    /// Recent capture order. Acknowledgements update entries in place, never reorder them.
    pub entries: Vec<Logged>,
    pub pending: usize,
    /// Entries removed from this memory window, not necessarily from persistent history.
    pub omitted: u64,
    /// Omitted entries whose final status is known not to be durable, including memory mode.
    pub omitted_not_durable: u64,
    /// Lifetime recording refusals/failures, including entries no longer in the memory window.
    pub unconfirmed: u64,
    pub capture_failures: u64,
}

pub(super) struct SessionLog {
    view_checkpoint: Option<wes_core::Value>,
    recorder: LogRecorder,
    live: BTreeMap<crate::graph::NodeId, (crate::history::LiveSnapshot, bool, u64)>,
    live_bytes: u64,
    entries: BTreeMap<u64, Logged>,
    bytes: u64,
    pending_bytes: u64,
    next: u64,
    pending: JoinSet<(u64, Logged, u64)>,
    updates: broadcast::Sender<()>,
    omitted: u64,
    omitted_not_durable: u64,
    unconfirmed: u64,
    capture_failures: u64,
}
impl SessionLog {
    pub fn replace_history(&mut self, image: &crate::history::HistoryImage) {
        debug_assert!(self.is_idle());
        self.entries.clear();
        self.view_checkpoint = None;
        self.bytes = 0;
        self.next = 0;
        self.omitted = 0;
        self.omitted_not_durable = 0;
        for entry in image.journal() {
            self.restore(entry.clone(), image.checkpoint().journal);
        }
        let _ = self.updates.send(());
    }
    /// Startup-only retention: preserve IDs/order without enqueueing writes or live wakeups.
    pub fn restore(
        &mut self,
        entry: crate::history::JournalEntry,
        checkpoint: crate::history::AppendReceipt,
    ) {
        if let crate::history::JournalEntry::Views(record) = &entry {
            self.view_checkpoint = Some(record.value.clone());
        }
        if let Some(entry) = Logged::recover(entry, checkpoint) {
            let sequence = self.next;
            self.next = self.next.checked_add(1).expect("bounded startup history");
            self.retain(sequence, entry);
        }
    }
    pub fn new(mode: &RecordingMode) -> Self {
        let recorder = match mode {
            RecordingMode::Ephemeral => LogRecorder::memory(),
            RecordingMode::Required(journal) => {
                let (recorder, required) = journal.recording();
                LogRecorder::recorded(recorder, required)
            }
        };
        Self {
            view_checkpoint: None,
            recorder,
            live: BTreeMap::new(),
            live_bytes: 0,
            entries: BTreeMap::new(),
            bytes: 0,
            pending_bytes: 0,
            next: 0,
            pending: JoinSet::new(),
            updates: broadcast::channel(1).0,
            omitted: 0,
            omitted_not_durable: 0,
            unconfirmed: 0,
            capture_failures: 0,
        }
    }
    pub fn updates(&self) -> broadcast::WeakSender<()> {
        self.updates.downgrade()
    }
    pub fn snapshot(&self) -> LogSnapshot {
        LogSnapshot {
            entries: self.entries.values().cloned().collect(),
            pending: self.pending.len(),
            omitted: self.omitted,
            omitted_not_durable: self.omitted_not_durable,
            unconfirmed: self.unconfirmed,
            capture_failures: self.capture_failures,
        }
    }
    pub fn is_idle(&self) -> bool {
        self.pending.is_empty()
    }
    pub fn failed(&self) -> bool {
        self.unconfirmed != 0 || self.capture_failures != 0
    }
    pub fn observe_live(&mut self, snapshot: crate::history::LiveSnapshot) {
        let record = snapshot.observation.clone();
        // Metadata has its own bounded latest-state budget, independent of the recent log window.
        if let Some((_, _, charge)) = self.live.remove(record.node()) {
            self.live_bytes -= charge;
        }
        let charge = crate::recording::record_charge(&crate::history::Record::Journal(
            crate::history::JournalEntry::Snapshot(snapshot.clone()),
        ));
        let bytes = self.live_bytes.saturating_add(charge);
        if self.live.len() >= max_entries() || bytes > max_bytes() {
            self.capture_failures = self.capture_failures.saturating_add(1);
            return;
        }
        self.live_bytes = bytes;
        self.live
            .insert(record.node().clone(), (snapshot, true, charge));
        self.capture_with(PreparedLog::observation_record(record), true);
    }
    /// The actor has paused ingress and joined event work. Ready payload evidence is committed
    /// by the value owner; outcomes without retained payloads are captured here.
    pub fn checkpoint_views(&mut self, workspace: &crate::workspace::Workspace) {
        match workspace.capture_views() {
            Ok(record)
                if self.view_checkpoint.as_ref() == Some(&record.value)
                    || (self.view_checkpoint.is_none()
                        && matches!(record.value.data(),wes_core::Data::List(v) if v.is_empty())) =>
                {}
            Ok(record) => {
                self.view_checkpoint = Some(record.value.clone());
                self.capture(PreparedLog::views(record));
            }
            Err(error) => self.capture(Err(error)),
        }
    }
    pub fn checkpoint_live(
        &mut self,
        workspace: &crate::workspace::Workspace,
        values: Option<&super::SessionValues>,
    ) {
        self.retain_nodes(workspace);
        // One at a time: checkpoint metadata must not flood the shared required writer.
        loop {
            let snapshot = self.live.values_mut().find_map(|(snapshot, dirty, _)| {
                if !*dirty {
                    return None;
                }
                if values.is_some_and(|v| v.retained_live(snapshot)) {
                    *dirty = false;
                    return None;
                }
                *dirty = false;
                Some(snapshot.clone())
            });
            let Some(snapshot) = snapshot else { break };
            self.capture(PreparedLog::snapshot(snapshot));
            if self.recorder.is_recorded() {
                break;
            }
        }
    }
    pub fn retain_nodes(&mut self, workspace: &crate::workspace::Workspace) {
        self.live.retain(|n, (_, _, charge)| {
            let keep = workspace.runtime().graph().node(n).is_some();
            if !keep {
                self.live_bytes -= *charge;
            }
            keep
        });
    }
    pub fn live_pending(&self) -> bool {
        self.live.values().any(|(_, dirty, _)| *dirty)
    }
    pub fn capture(&mut self, prepared: Result<PreparedLog, InvalidRecord>) {
        self.capture_with(prepared, false);
    }
    fn capture_with(&mut self, prepared: Result<PreparedLog, InvalidRecord>, live: bool) {
        let Ok(prepared) = prepared else {
            self.capture_failures = self.capture_failures.saturating_add(1);
            let _ = self.updates.send(());
            return;
        };
        let Some(next) = self.next.checked_add(1) else {
            self.capture_failures = self.capture_failures.saturating_add(1);
            self.omitted = self.omitted.saturating_add(1);
            self.omitted_not_durable = self.omitted_not_durable.saturating_add(1);
            let _ = self.updates.send(());
            return;
        };
        let sequence = self.next;
        self.next = next;
        let charge = prepared.charge();
        let entry = if live {
            LogRecorder::memory().record(prepared).initial().clone()
        } else if self.recorder.is_recorded()
            && (self.pending.len() >= max_pending()
                || charge > max_bytes().saturating_sub(self.pending_bytes))
        {
            // Refuse BEFORE queue admission. Never enqueue and then abandon an owned receipt.
            prepared.refuse()
        } else {
            let attempt = self.recorder.record(prepared);
            let initial = attempt.initial().clone();
            if matches!(initial.status(), LogStatus::Pending) {
                self.pending_bytes += charge;
                self.pending
                    .spawn(async move { (sequence, attempt.wait().await, charge) });
            }
            initial
        };
        self.count_failure(&entry);
        self.retain(sequence, entry);
        let _ = self.updates.send(());
    }
    fn count_failure(&mut self, entry: &Logged) {
        if matches!(entry.status(), LogStatus::Unconfirmed { .. }) {
            self.unconfirmed = self.unconfirmed.saturating_add(1);
        }
    }
    fn omit(&mut self, entry: &Logged) {
        self.omitted = self.omitted.saturating_add(1);
        if !matches!(entry.status(), LogStatus::Pending) && !entry.durable() {
            self.omitted_not_durable = self.omitted_not_durable.saturating_add(1);
        }
    }
    fn retain(&mut self, sequence: u64, entry: Logged) {
        // Submission presentation shares bounded, joined receipt ownership, not the
        // observation/diagnostic display window. Cells own its in-memory projection.
        if matches!(
            entry.entry(),
            crate::history::JournalEntry::Submitted(_) | crate::history::JournalEntry::Views(_)
        ) {
            return;
        }
        let charge = entry.charge();
        if charge > max_bytes() {
            self.omit(&entry);
            return;
        }
        while self.entries.len() >= max_entries() || charge > max_bytes() - self.bytes {
            let (_, removed) = self
                .entries
                .pop_first()
                .expect("retention contains charged entries");
            self.bytes -= removed.charge();
            self.omit(&removed);
        }
        self.bytes += charge;
        self.entries.insert(sequence, entry);
    }
    /// The actor selects this future only while nonempty and joins all attempts before exiting.
    pub async fn completed(&mut self) {
        match self.pending.join_next().await {
            Some(Ok((sequence, entry, charge))) => {
                self.pending_bytes -= charge;
                self.count_failure(&entry);
                if let Some(retained) = self.entries.get_mut(&sequence) {
                    *retained = entry;
                } else if !entry.durable()
                    && !matches!(entry.entry(), crate::history::JournalEntry::Submitted(_))
                {
                    self.omitted_not_durable = self.omitted_not_durable.saturating_add(1);
                }
            }
            Some(Err(_)) => {
                // No receipt task intentionally panics or is aborted. Fail closed if it does;
                // keep its charge reserved rather than pretending its unknown outcome was saved.
                self.capture_failures = self.capture_failures.saturating_add(1);
            }
            None => {}
        }
        let _ = self.updates.send(());
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        calls::{CallJournal, RequiredPersistence},
        graph::{NodeId, NodeState},
        history::{AppendReceipt, JournalSink, Persistence, Record, RecordError},
        recording::{RecorderLimits, spawn_recorder},
        runtime::Observation,
    };
    use std::sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
        mpsc,
    };
    use tokio::sync::oneshot;
    use wes_core::Timestamp;
    use wes_language::{Diagnostic, Span};

    fn prepared() -> Result<PreparedLog, InvalidRecord> {
        PreparedLog::observation(
            Timestamp::new(0, 0).unwrap(),
            &Observation {
                revision: 0,
                stale_reason: None,
                delivery: None,
                evidence: None,
                node: NodeId::new("node").unwrap(),
                run: None,
                state: NodeState::Ready,
                value: None,
                error: None,
            },
        )
    }
    #[tokio::test]
    async fn recovered_window_is_bounded_preserves_event_ids_and_never_enqueues_or_notifies() {
        let mut log = SessionLog::new(&RecordingMode::Ephemeral);
        let mut wakeups = log.updates().upgrade().unwrap().subscribe();
        for number in 0..max_entries() + 2 {
            let entry = crate::history::ExecutionRecord::new(
                number.to_string(),
                NodeId::new("historical").unwrap(),
                None,
                Timestamp::new(0, 0).unwrap(),
                NodeState::Ready,
                None,
            )
            .unwrap();
            log.restore(
                crate::history::JournalEntry::Observed(entry),
                AppendReceipt {
                    persistence: Persistence::FileSynced,
                    end_offset: 123,
                },
            );
        }
        let snapshot = log.snapshot();
        assert_eq!(snapshot.entries.len(), max_entries());
        assert_eq!(snapshot.entries[0].id(), "2");
        assert_eq!(snapshot.omitted, 2);
        assert_eq!(snapshot.omitted_not_durable, 0);
        assert_eq!(snapshot.pending, 0);
        assert_eq!(snapshot.unconfirmed, 0);
        assert!(log.is_idle());
        assert!(!log.failed());
        assert!(matches!(
            wakeups.try_recv(),
            Err(broadcast::error::TryRecvError::Empty)
        ));
    }
    struct Sink {
        count: Arc<AtomicUsize>,
        gate: Option<(oneshot::Sender<()>, mpsc::Receiver<()>)>,
    }
    impl JournalSink for Sink {
        fn append(&mut self, _: &Record) -> Result<AppendReceipt, RecordError> {
            if let Some((entered, released)) = self.gate.take() {
                let _ = entered.send(());
                released
                    .recv_timeout(std::time::Duration::from_secs(10))
                    .unwrap();
            }
            Ok(AppendReceipt {
                persistence: Persistence::FileSynced,
                end_offset: self.count.fetch_add(1, Ordering::SeqCst) as u64 + 1,
            })
        }
    }
    #[tokio::test]
    async fn bounded_pending_admission_retains_refusals_and_late_receipts_do_not_reinsert_old_entries()
     {
        let count = Arc::new(AtomicUsize::new(0));
        let (entered, blocked) = oneshot::channel();
        let (release, released) = mpsc::channel();
        let (recorder, task) = spawn_recorder(
            Sink {
                count: count.clone(),
                gate: Some((entered, released)),
            },
            RecorderLimits::default(),
        )
        .unwrap();
        let mut log = SessionLog::new(&RecordingMode::Required(CallJournal::new(
            recorder.clone(),
            RequiredPersistence::FileSynced,
        )));
        log.capture(prepared());
        blocked.await.unwrap();
        for _ in 0..=max_entries() {
            log.capture(prepared());
        }
        let before = log.snapshot();
        assert_eq!(before.entries.len(), max_entries());
        assert_eq!(before.pending, max_pending());
        assert_eq!(before.omitted, 2);
        assert_eq!(before.omitted_not_durable, 0); // Evicted pending receipts are still owned.
        assert_eq!(
            before.unconfirmed,
            (max_entries() + 2 - max_pending()) as u64
        );
        assert!(matches!(
            before.entries.last().unwrap().status(),
            LogStatus::Unconfirmed {
                may_have_appended: false,
                ..
            }
        ));
        let ids: Vec<_> = before
            .entries
            .iter()
            .map(|entry| entry.id().to_owned())
            .collect();
        release.send(()).unwrap();
        while !log.is_idle() {
            log.completed().await;
        }
        let after = log.snapshot();
        assert_eq!(count.load(Ordering::SeqCst), max_pending());
        assert_eq!(
            after
                .entries
                .iter()
                .map(|entry| entry.id())
                .collect::<Vec<_>>(),
            ids
        );
        assert_eq!(after.pending, 0);
        assert_eq!(log.pending_bytes, 0);
        assert_eq!(after.omitted_not_durable, 0);
        assert_eq!(after.unconfirmed, before.unconfirmed);
        recorder.shutdown().await.unwrap();
        task.join().await.unwrap();
    }
    #[tokio::test]
    async fn memory_retention_and_oversize_capture_have_explicit_omission_accounting() {
        let mut log = SessionLog::new(&RecordingMode::Ephemeral);
        for _ in 0..=max_entries() {
            log.capture(prepared());
        }
        assert_eq!(log.snapshot().omitted_not_durable, 1);
        let oversized = || {
            PreparedLog::diagnostic(
                Timestamp::new(0, 0).unwrap(),
                "cell".into(),
                "x".repeat((max_bytes() / 6) as usize),
                Diagnostic::error("TEST", Span::at(0), "oversized source"),
            )
        };
        log.capture(oversized());
        assert_eq!(log.snapshot().omitted, 2);
        assert_eq!(log.snapshot().omitted_not_durable, 2);
        assert!(!log.failed()); // Deliberately memory-only is not a failed persistence attempt.
        assert!(log.bytes <= max_bytes());
        let count = Arc::new(AtomicUsize::new(0));
        let (recorder, task) = spawn_recorder(
            Sink {
                count: count.clone(),
                gate: None,
            },
            RecorderLimits::default(),
        )
        .unwrap();
        let mut log = SessionLog::new(&RecordingMode::Required(CallJournal::new(
            recorder.clone(),
            RequiredPersistence::FileSynced,
        )));
        log.capture(oversized());
        assert_eq!(log.snapshot().omitted_not_durable, 1);
        assert_eq!(log.snapshot().unconfirmed, 1);
        assert!(log.is_idle());
        assert_eq!(count.load(Ordering::SeqCst), 0);
        recorder.shutdown().await.unwrap();
        task.join().await.unwrap();
    }
    #[tokio::test]
    async fn invalid_capture_and_sequence_exhaustion_are_visible_without_fabricated_records() {
        let mut log = SessionLog::new(&RecordingMode::Ephemeral);
        log.capture(Err(InvalidRecord("invalid synthetic record")));
        log.next = u64::MAX;
        log.capture(prepared());
        assert_eq!(log.snapshot().capture_failures, 2);
        assert_eq!(log.snapshot().omitted_not_durable, 1);
        assert!(log.snapshot().entries.is_empty());
    }
}
