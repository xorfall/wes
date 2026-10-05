//! A single bounded writer. Filesystem waits never occupy the runtime actor or async timer thread.
use crate::history::{
    AppendReceipt, HistoryCaptureLimits, HistoryCursor, HistoryImage, HistoryPage,
    HistoryPageLimits, JournalEntry, JournalSink, Record, RecordError, RecoveryEntry,
};
use std::{
    num::{NonZeroU32, NonZeroUsize},
    sync::Arc,
    thread,
};
use thiserror::Error;
use tokio::sync::{OwnedSemaphorePermit, Semaphore, mpsc, oneshot};

#[derive(Clone, Copy, Debug)]
pub struct RecorderLimits {
    pub records: NonZeroUsize,
    /// Conservative payload charge, not an OS allocator/RSS guarantee. External Arc owners and
    /// records still waiting to acquire admission remain the submitting caller's responsibility.
    pub bytes: NonZeroU32,
}
impl Default for RecorderLimits {
    fn default() -> Self {
        Self {
            records: NonZeroUsize::new(wes_budgets::get("journal.pending") as _).unwrap(),
            bytes: NonZeroU32::new(wes_budgets::get("journal.pending.bytes") as _).unwrap(),
        }
    }
}

#[derive(Clone)]
pub struct Recorder {
    sender: mpsc::Sender<Request>,
    budget: Arc<Semaphore>,
    limits: RecorderLimits,
    capture_credit: Arc<Semaphore>,
    page_credit: Arc<Semaphore>,
}
pub struct RecorderTask {
    thread: thread::JoinHandle<()>,
    stopped: oneshot::Receiver<()>,
}
pub struct PendingAppend(oneshot::Receiver<Result<AppendReceipt, RecordError>>);
/// One page credit spans admission, physical read and retained reply. Extracting transfers memory ownership.
#[derive(Debug)]
pub struct CapturedPage {
    page: HistoryPage,
    append_report: DrainReport,
    _credit: OwnedSemaphorePermit,
}
impl CapturedPage {
    pub fn page(&self) -> &HistoryPage {
        &self.page
    }
    pub fn append_report(&self) -> DrainReport {
        self.append_report
    }
    pub fn into_page(self) -> HistoryPage {
        self.page
    }
}
/// Retains the single capture credit through queueing, physical reading and reply consumption.
/// Explicitly extracting the image transfers its memory ownership to the caller.
#[derive(Debug)]
pub struct CapturedHistory {
    image: HistoryImage,
    append_report: DrainReport,
    _credit: OwnedSemaphorePermit,
}
impl CapturedHistory {
    pub fn image(&self) -> &HistoryImage {
        &self.image
    }
    /// Earlier append attempts, including failures, at this exact queue boundary. A successful
    /// capture does not make previously failed writes successful. Read failures are not appends.
    pub fn append_report(&self) -> DrainReport {
        self.append_report
    }
    pub fn into_image(self) -> HistoryImage {
        self.image
    }
}
impl PendingAppend {
    pub async fn wait(self) -> Result<AppendReceipt, RecordError> {
        self.0.await.map_err(|_| RecordError::Closed)?
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Rejection {
    Full,
    TooLarge,
    Closed,
}
#[derive(Debug, Error)]
#[error("recording admission failed: {reason:?}")]
pub struct RejectedRecord {
    pub reason: Rejection,
    pub record: Arc<Record>,
}

#[derive(Clone, Copy, Default, Debug, PartialEq, Eq)]
pub struct DrainReport {
    pub attempted: u64,
    pub failed: u64,
}
enum Request {
    Replace(Box<dyn JournalSink>, oneshot::Sender<()>),
    ClaimRequest {
        record: crate::history::RequestRecord,
        current: bool,
        _credit: OwnedSemaphorePermit,
        reply: oneshot::Sender<Result<crate::history::RequestClaim, RecordError>>,
    },
    FindRequest {
        actor: String,
        request: String,
        _credit: OwnedSemaphorePermit,
        reply: oneshot::Sender<Result<Option<crate::history::RequestRecord>, RecordError>>,
    },
    Page {
        cursor: Option<HistoryCursor>,
        limits: HistoryPageLimits,
        credit: OwnedSemaphorePermit,
        reply: oneshot::Sender<Result<CapturedPage, RecordError>>,
    },
    Append {
        record: Arc<Record>,
        credit: OwnedSemaphorePermit,
        reply: oneshot::Sender<Result<AppendReceipt, RecordError>>,
    },
    Barrier(oneshot::Sender<DrainReport>),
    Capture {
        limits: HistoryCaptureLimits,
        credit: OwnedSemaphorePermit,
        reply: oneshot::Sender<Result<CapturedHistory, RecordError>>,
    },
    Shutdown(oneshot::Sender<DrainReport>),
}

pub fn spawn_recorder(
    sink: impl JournalSink + 'static,
    limits: RecorderLimits,
) -> Result<(Recorder, RecorderTask), RecordError> {
    if limits.records.get() > Semaphore::MAX_PERMITS
        || limits.bytes.get() as usize > Semaphore::MAX_PERMITS
    {
        return Err(RecordError::Limit("queue capacity"));
    }
    let (sender, receiver) = mpsc::channel(limits.records.get());
    let budget = Arc::new(Semaphore::new(limits.bytes.get() as usize));
    let (stopped_tx, stopped) = oneshot::channel();
    let thread = thread::Builder::new()
        .name("wes-history".into())
        .spawn(move || {
            write_loop(sink, receiver);
            let _ = stopped_tx.send(());
        })
        .map_err(|e| RecordError::backend("starting the recording worker", false, e))?;
    Ok((
        Recorder {
            sender,
            budget,
            limits,
            capture_credit: Arc::new(Semaphore::new(1)),
            page_credit: Arc::new(Semaphore::new(1)),
        },
        RecorderTask { thread, stopped },
    ))
}
impl RecorderTask {
    /// Requires shutdown or dropping all senders. Dropping this handle never aborts accepted writes.
    pub async fn join(self) -> Result<(), RecordError> {
        let _ = self.stopped.await;
        // Completion/destruction already happened; use the blocking pool only to join the OS thread.
        tokio::task::spawn_blocking(move || self.thread.join())
            .await
            .map_err(|_| RecordError::Closed)?
            .map_err(|_| RecordError::Closed)
    }
}
impl Recorder {
    pub async fn claim_request(
        &self,
        record: crate::history::RequestRecord,
        current: bool,
    ) -> Result<crate::history::RequestClaim, RecordError> {
        record
            .validate()
            .map_err(|_| RecordError::Limit("request identity"))?;
        let credit = self
            .budget
            .clone()
            .try_acquire_many_owned(8192)
            .map_err(|_| RecordError::ReadBusy)?;
        let (reply, receive) = oneshot::channel();
        self.sender
            .send(Request::ClaimRequest {
                record,
                current,
                _credit: credit,
                reply,
            })
            .await
            .map_err(|_| RecordError::Closed)?;
        receive.await.map_err(|_| RecordError::Closed)?
    }
    pub async fn find_request(
        &self,
        actor: &str,
        request: &str,
    ) -> Result<Option<crate::history::RequestRecord>, RecordError> {
        let probe = crate::history::RequestRecord {
            namespace: actor.into(),
            request: request.into(),
            cell: "lookup".into(),
            fingerprint: "0".repeat(64),
            steps: Vec::new(),
        };
        probe
            .validate()
            .map_err(|_| RecordError::Limit("request identity"))?;
        let credit = self
            .budget
            .clone()
            .try_acquire_many_owned(8192)
            .map_err(|_| RecordError::ReadBusy)?;
        let (reply, receive) = oneshot::channel();
        self.sender
            .send(Request::FindRequest {
                actor: actor.into(),
                request: request.into(),
                _credit: credit,
                reply,
            })
            .await
            .map_err(|_| RecordError::Closed)?;
        receive.await.map_err(|_| RecordError::Closed)?
    }

    /// Composition-root operation under a session checkpoint. The replacement has
    /// already been durably published. All earlier reads/writes are joined before
    /// closing the old sink; clones and call-admission identities remain valid.
    pub async fn replace_sink(&self, sink: impl JournalSink + 'static) -> Result<(), RecordError> {
        let (reply, receive) = oneshot::channel();
        self.sender
            .send(Request::Replace(Box::new(sink), reply))
            .await
            .map_err(|_| RecordError::Closed)?;
        receive.await.map_err(|_| RecordError::Closed)
    }
    /// Backpressured read on the owned writer, separate from append/capture payload credits.
    /// Abandoning an admitted wait does not abort the read; writer shutdown drains and joins it.
    pub async fn page(
        &self,
        cursor: Option<HistoryCursor>,
        limits: HistoryPageLimits,
    ) -> Result<CapturedPage, RecordError> {
        limits.validate()?;
        let credit = tokio::select! {
            _ = self.sender.closed() => return Err(RecordError::Closed),
            credit = self.page_credit.clone().acquire_owned() => credit.map_err(|_| RecordError::Closed)?,
        };
        let (reply, receiver) = oneshot::channel();
        self.sender
            .send(Request::Page {
                cursor,
                limits,
                credit,
                reply,
            })
            .await
            .map_err(|_| RecordError::Closed)?;
        receiver.await.map_err(|_| RecordError::Closed)?
    }
    /// Capacity-refusing page admission for transports. Never wait for a caller-retained page or
    /// a full writer queue; after admission the same physical-read ownership contract applies.
    pub async fn try_page(
        &self,
        cursor: Option<HistoryCursor>,
        limits: HistoryPageLimits,
    ) -> Result<CapturedPage, RecordError> {
        limits.validate()?;
        if self.sender.is_closed() {
            return Err(RecordError::Closed);
        }
        let credit = self
            .page_credit
            .clone()
            .try_acquire_owned()
            .map_err(|_| RecordError::ReadBusy)?;
        let (reply, receiver) = oneshot::channel();
        self.sender
            .try_send(Request::Page {
                cursor,
                limits,
                credit,
                reply,
            })
            .map_err(|error| match error {
                mpsc::error::TrySendError::Full(_) => RecordError::ReadBusy,
                mpsc::error::TrySendError::Closed(_) => RecordError::Closed,
            })?;
        receiver.await.map_err(|_| RecordError::Closed)?
    }
    /// Queue a synchronized snapshot behind earlier admitted writes. Later admissions are excluded.
    /// The reader runs on the existing writer thread, retaining its exclusive filesystem ownership.
    /// At most one capture/reply is retained; limits may lower, but not raise, the default
    /// 100,000-record / 128-MiB logical image budgets. Append admission has separate byte credit.
    /// Cancellation after enqueue discards the reply, not an entered read; shutdown joins it.
    pub async fn capture(
        &self,
        limits: HistoryCaptureLimits,
    ) -> Result<CapturedHistory, RecordError> {
        let maximum = HistoryCaptureLimits::default();
        if limits.records > maximum.records || limits.bytes > maximum.bytes {
            return Err(RecordError::Limit("captured history policy"));
        }
        let credit = tokio::select! {
            _ = self.sender.closed() => return Err(RecordError::Closed),
            credit = self.capture_credit.clone().acquire_owned() => credit.map_err(|_| RecordError::Closed)?,
        };
        let (reply, receiver) = oneshot::channel();
        self.sender
            .send(Request::Capture {
                limits,
                credit,
                reply,
            })
            .await
            .map_err(|_| RecordError::Closed)?;
        receiver.await.map_err(|_| RecordError::Closed)?
    }
    /// Backpressured admission. Cancellation before enqueue is harmless; cancelling a receipt wait
    /// after admission does not retract the write. The receipt alone establishes its sync outcome.
    pub async fn enqueue(&self, record: Arc<Record>) -> Result<PendingAppend, RejectedRecord> {
        let cost = self.cost(&record)?;
        let credit = self
            .budget
            .clone()
            .acquire_many_owned(cost)
            .await
            .map_err(|_| rejected(Rejection::Closed, &record))?;
        let (reply, receiver) = oneshot::channel();
        self.sender
            .send(Request::Append {
                record: record.clone(),
                credit,
                reply,
            })
            .await
            .map_err(|_| rejected(Rejection::Closed, &record))?;
        Ok(PendingAppend(receiver))
    }
    /// Nonblocking observation path. On overflow the caller retains the complete record and must
    /// surface an unsaved-history event; it must not turn cancellation into an execution failure.
    pub fn try_enqueue(&self, record: Arc<Record>) -> Result<PendingAppend, RejectedRecord> {
        let cost = self.cost(&record)?;
        let credit = self
            .budget
            .clone()
            .try_acquire_many_owned(cost)
            .map_err(|e| {
                rejected(
                    match e {
                        tokio::sync::TryAcquireError::Closed => Rejection::Closed,
                        tokio::sync::TryAcquireError::NoPermits => Rejection::Full,
                    },
                    &record,
                )
            })?;
        let (reply, receiver) = oneshot::channel();
        self.sender
            .try_send(Request::Append {
                record: record.clone(),
                credit,
                reply,
            })
            .map_err(|e| {
                rejected(
                    match e {
                        mpsc::error::TrySendError::Full(_) => Rejection::Full,
                        mpsc::error::TrySendError::Closed(_) => Rejection::Closed,
                    },
                    &record,
                )
            })?;
        Ok(PendingAppend(receiver))
    }
    pub async fn append(&self, record: Arc<Record>) -> Result<AppendReceipt, RecordError> {
        self.enqueue(record)
            .await
            .map_err(|error| match error.reason {
                Rejection::TooLarge => RecordError::Limit("queue payload"),
                Rejection::Closed => RecordError::Closed,
                Rejection::Full => RecordError::Limit("queue capacity"),
            })?
            .wait()
            .await
    }
    /// All earlier admissions have been attempted when this resolves; inspect `failed`, not just
    /// completion, before describing history as saved. Concurrent later submissions are excluded.
    pub async fn flush(&self) -> Result<DrainReport, RecordError> {
        let (reply, receiver) = oneshot::channel();
        self.sender
            .send(Request::Barrier(reply))
            .await
            .map_err(|_| RecordError::Closed)?;
        receiver.await.map_err(|_| RecordError::Closed)
    }
    /// Close admission and drain all accepted records, including those already queued behind this
    /// request. The acknowledgement follows sink destruction and therefore releases its file lock.
    pub async fn shutdown(&self) -> Result<DrainReport, RecordError> {
        let (reply, receiver) = oneshot::channel();
        self.sender
            .send(Request::Shutdown(reply))
            .await
            .map_err(|_| RecordError::Closed)?;
        receiver.await.map_err(|_| RecordError::Closed)
    }
    fn cost(&self, record: &Arc<Record>) -> Result<u32, RejectedRecord> {
        if self.sender.is_closed() {
            return Err(rejected(Rejection::Closed, record));
        }
        let cost = record_charge(record);
        if cost > u64::from(self.limits.bytes.get()) {
            return Err(rejected(Rejection::TooLarge, record));
        }
        Ok(cost as u32)
    }
}
fn rejected(reason: Rejection, record: &Arc<Record>) -> RejectedRecord {
    RejectedRecord {
        reason,
        record: record.clone(),
    }
}
fn write_loop(sink: impl JournalSink + 'static, mut receiver: mpsc::Receiver<Request>) {
    let mut sink: Box<dyn JournalSink> = Box::new(sink);
    let mut report = DrainReport::default();
    let mut shutdown = vec![];
    while let Some(request) = receiver.blocking_recv() {
        match request {
            Request::ClaimRequest {
                record,
                current,
                _credit,
                reply,
            } => {
                let result = sink.claim_request(record, current);
                // A failed reservation can be an uncertain append; checkpoints must see it.
                if result.as_ref().is_ok_and(|r| r.fresh)
                    || result.as_ref().is_err_and(|e| {
                        !matches!(
                            e,
                            RecordError::RequestConflict | RecordError::RequestContext
                        )
                    })
                {
                    report.attempted = report.attempted.saturating_add(1);
                    report.failed = report.failed.saturating_add(u64::from(result.is_err()));
                }
                let _ = reply.send(result);
            }
            Request::FindRequest {
                actor,
                request,
                _credit,
                reply,
            } => {
                let _ = reply.send(sink.find_request(&actor, &request));
            }
            Request::Replace(replacement, reply) => {
                sink = replacement;
                let _ = reply.send(());
            }
            Request::Page {
                cursor,
                limits,
                credit,
                reply,
            } => {
                let result = sink.page(cursor, limits).and_then(|page| {
                    page.validate(cursor, limits)?;
                    Ok(CapturedPage {
                        page,
                        append_report: report,
                        _credit: credit,
                    })
                });
                let _ = reply.send(result);
            }
            Request::Append {
                record,
                credit,
                reply,
            } => {
                let timing = crate::diagnostics::Operation::start("journal");
                let result = sink.append(&record);
                timing.finish(if result.is_ok() { "ok" } else { "error" });
                report.attempted = report.attempted.saturating_add(1);
                report.failed = report.failed.saturating_add(u64::from(result.is_err()));
                // Release accounting only after the sink returns; an active disk write consumes it.
                drop(record);
                drop(credit);
                let _ = reply.send(result);
            }
            Request::Barrier(reply) => {
                let _ = reply.send(report);
            }
            Request::Capture {
                limits,
                credit,
                reply,
            } => {
                let captured = sink.capture(limits).and_then(|image| {
                    if image.charged_bytes() > limits.bytes
                        || image.journal().len().saturating_add(image.recovery().len())
                            > limits.records
                    {
                        return Err(RecordError::Limit("captured history response"));
                    }
                    Ok(CapturedHistory {
                        image,
                        append_report: report,
                        _credit: credit,
                    })
                });
                let _ = reply.send(captured);
            }
            Request::Shutdown(reply) => {
                receiver.close();
                shutdown.push(reply);
            }
        }
    }
    drop(sink);
    for reply in shutdown {
        let _ = reply.send(report);
    }
}

// Charge fixed object overhead, six bytes per input byte (JSON escaping upper bound), and
// collection element overhead. This bounds admitted logical payloads without serializing in engine.
pub(crate) fn record_charge(record: &Record) -> u64 {
    fn text(value: &str) -> u64 {
        64u64.saturating_add((value.len() as u64).saturating_mul(6))
    }
    fn add(total: &mut u64, value: &str) {
        *total = total.saturating_add(text(value));
    }
    fn error_charge(size: &mut u64, error: &wes_core::ErrorValue) {
        for field in [error.id().as_str(), error.code(), error.message()] {
            add(size, field);
        }
        if let Some(cause) = error.cause() {
            add(size, cause.as_str());
        }
        for issue in error.issues() {
            for field in [&issue.path, &issue.code, &issue.message] {
                add(size, field);
            }
        }
    }
    let mut size = 1024u64;
    match record {
        Record::Journal(
            JournalEntry::Payload { node, run, handle }
            | JournalEntry::ProtectedRun { node, run, handle },
        ) => {
            for field in [node.as_str(), run.as_str(), handle.as_str()] {
                add(&mut size, field);
            }
        }
        Record::Journal(JournalEntry::Requested(r)) => {
            for field in [&r.namespace, &r.request, &r.cell, &r.fingerprint] {
                add(&mut size, field);
            }
        }
        Record::Journal(JournalEntry::Retired(r)) => {
            for node in &r.nodes {
                add(&mut size, node.as_str());
            }
            for handle in r.payloads.iter().chain(&r.protected) {
                add(&mut size, handle.as_str());
            }
        }
        Record::Journal(JournalEntry::Submitted(s)) => {
            if let Some(document) = &s.document {
                add(&mut size, document);
            }
            if let Some(origin) = &s.revision_of {
                add(&mut size, origin);
            }
            for field in [&s.id, &s.cell, &s.source_name, &s.text, &s.client] {
                add(&mut size, field);
            }
            for node in &s.nodes {
                add(&mut size, node.as_str());
            }
            if let Some(repeat) = &s.repeat {
                add(&mut size, &repeat.origin);
                if let Some(from) = &repeat.from {
                    add(&mut size, from.as_str());
                }
            }
            if let Some(run) = &s.run {
                add(&mut size, run.as_str());
            }
            if let Some(context) = &s.context {
                size = size.saturating_add(context.revisions.len() as u64 * 1024 + 2048);
            }
        }
        Record::Journal(JournalEntry::Views(record)) => {
            add(&mut size, &record.id);
            size = size.saturating_add(
                crate::value_size::value_charge(&record.value, u64::MAX)
                    .unwrap_or(u64::MAX)
                    .saturating_mul(6),
            );
        }
        Record::Journal(JournalEntry::Trace(record)) => {
            size = size.saturating_add(
                crate::value_size::value_charge(&record.value, u64::MAX).unwrap_or(u64::MAX),
            );
            add(&mut size, record.node.as_str());
            add(&mut size, record.run.as_str());
        }
        Record::Journal(JournalEntry::Environments(record)) => {
            size = size.saturating_add(record.charge());
        }
        Record::Journal(JournalEntry::Command(c)) => {
            if let Some(origin) = &c.revision_of {
                add(&mut size, origin);
            }
            if let Some(context) = &c.environments {
                size = size.saturating_add(context.revisions.len() as u64 * 1024 + 2048);
            }
            if let Some(document) = &c.document {
                add(&mut size, document);
            }
            for s in [&c.cell, &c.source_name, &c.text, &c.replay] {
                add(&mut size, s);
            }
            for node in c.nodes.iter().chain(&c.changed_nodes) {
                add(&mut size, node.as_str());
            }
            if let Some(package) = &c.calculation_package {
                add(&mut size, package);
            }
            for (path, source) in &c.type_sources {
                add(&mut size, path);
                add(&mut size, source);
            }
            for snapshot in &c.imports {
                add(&mut size, snapshot.request().kind());
                if let Some(alias) = snapshot.request().alias() {
                    add(&mut size, alias);
                }
                add(&mut size, snapshot.recipe().format());
                add(&mut size, snapshot.recipe().source());
                for (key, value) in snapshot.request().arguments() {
                    add(&mut size, key);
                    size = size.saturating_add(
                        crate::value_size::value_charge(value, u64::MAX).unwrap_or(u64::MAX),
                    );
                }
            }
        }
        Record::Journal(JournalEntry::Snapshot(s)) => {
            add(&mut size, s.source.as_str());
            add(&mut size, s.epoch.as_str());
            size = size.saturating_add(record_charge(&Record::Journal(JournalEntry::Observed(
                s.observation.clone(),
            ))));
            if let Some(r) = &s.result {
                size = size.saturating_add(record_charge(&Record::Journal(JournalEntry::Result(
                    r.clone(),
                ))));
            }
        }
        Record::Journal(JournalEntry::Result(r)) => {
            add(&mut size, r.node.as_str());
            add(&mut size, r.handle.as_str());
            add(&mut size, r.run.as_str());
        }
        Record::Journal(JournalEntry::Observed(o)) => {
            add(&mut size, o.id());
            add(&mut size, o.node().as_str());
            if let Some(run) = o.run() {
                add(&mut size, run.as_str());
            }
            if let Some(error) = o.error() {
                error_charge(&mut size, error);
            }
        }
        Record::Journal(JournalEntry::Noticed(notice)) => {
            add(&mut size, notice.id());
            if let Some(node) = notice.context().node() {
                add(&mut size, node.as_str());
            }
            if let Some(run) = notice.context().run() {
                add(&mut size, run.as_str());
            }
            if let Some(handle) = notice.context().handle() {
                add(&mut size, handle.as_str());
            }
            error_charge(&mut size, notice.error());
        }
        Record::Journal(JournalEntry::Diagnosed(d)) => {
            for s in [
                d.id(),
                d.cell(),
                d.source(),
                d.diagnostic().code.as_ref(),
                &d.diagnostic().message,
            ] {
                add(&mut size, s);
            }
            if let Some(message) = &d.diagnostic().public_message {
                add(&mut size, message);
            }
            for hint in &d.diagnostic().hints {
                add(&mut size, hint);
            }
        }
        Record::Recovery(RecoveryEntry::Accepted { cell }) => add(&mut size, cell),
        Record::Recovery(RecoveryEntry::Calling(c)) => {
            for s in [c.node.as_str(), &c.cell, &c.capability] {
                add(&mut size, s);
            }
            add(&mut size, c.run.as_str());
        }
        Record::Recovery(RecoveryEntry::Called { node, run, .. }) => {
            add(&mut size, node.as_str());
            add(&mut size, run.as_str());
        }
    }
    size
}

#[cfg(test)]
mod charge_tests {
    use super::*;
    use crate::{
        history::{CommandRecord, HistoryCapture, HistoryCaptureLimits},
        imports::{ImportRecipe, ImportRequest, ImportSnapshot},
    };
    #[test]
    fn captured_import_bytes_and_exact_argument_values_consume_recording_capacity() {
        let mut command = CommandRecord {
            source_name: "fixture.wes".into(),
            source_start: wes_language::Position { line: 1, column: 1 },
            changed_nodes: vec![],
            document: None,
            revision_of: None,
            environments: None,
            cell: "cell".into(),
            text: "source".into(),
            replay: "source".into(),
            nodes: vec![],
            type_sources: Default::default(),
            calculation_package: None,
            imports: vec![],
        };
        let baseline = record_charge(&Record::Journal(JournalEntry::Command(command.clone())));
        command.imports.push(ImportSnapshot::new(
            ImportRequest::new(
                "fixture".into(),
                Some("name".into()),
                [(
                    "value".into(),
                    wes_core::Value::new(
                        wes_core::Shape::Unknown,
                        wes_core::Data::Bytes(vec![0; 2048].into()),
                        Default::default(),
                    )
                    .unwrap(),
                )]
                .into(),
            )
            .unwrap(),
            ImportRecipe::new("fixture/v1".into(), "\0".repeat(1024)).unwrap(),
        ));
        let record = Record::Journal(JournalEntry::Command(command));
        let charge = record_charge(&record);
        assert!(charge >= baseline + 6 * 1024 + 2 * 2048);
        let mut capture = HistoryCapture::new(HistoryCaptureLimits {
            records: 1,
            bytes: charge - 1,
        });
        assert!(capture.push(record.clone()).is_err());
        let mut capture = HistoryCapture::new(HistoryCaptureLimits {
            records: 1,
            bytes: charge,
        });
        capture.push(record).unwrap();
    }
}
