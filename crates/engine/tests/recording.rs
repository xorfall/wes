use std::{
    num::{NonZeroU32, NonZeroUsize},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
    time::Duration,
};
use tokio::sync::oneshot;
use wes_engine::{
    history::{AppendReceipt, JournalSink, Persistence, Record, RecordError, RecoveryEntry},
    recording::{RecorderLimits, Rejection, spawn_recorder},
};
#[path = "recording/capture.rs"]
mod capture;
#[path = "recording/pages.rs"]
mod pages;

fn record(cell: &str) -> Arc<Record> {
    Arc::new(Record::Recovery(RecoveryEntry::Accepted {
        cell: cell.into(),
    }))
}
fn limits(records: usize, bytes: u32) -> RecorderLimits {
    RecorderLimits {
        records: NonZeroUsize::new(records).unwrap(),
        bytes: NonZeroU32::new(bytes).unwrap(),
    }
}
struct Sink {
    writes: Arc<Mutex<Vec<String>>>,
    gate: Option<(oneshot::Sender<()>, mpsc::Receiver<()>)>,
    dropped: Arc<AtomicBool>,
}
impl JournalSink for Sink {
    fn append(&mut self, record: &Record) -> Result<AppendReceipt, RecordError> {
        if let Some((entered, gate)) = self.gate.take() {
            let _ = entered.send(());
            gate.recv_timeout(Duration::from_secs(5))
                .expect("test must release the writer");
        }
        let Record::Recovery(RecoveryEntry::Accepted { cell }) = record else {
            panic!("accepted test record")
        };
        if cell == "panic" {
            panic!("private provider text must not enter returned recording errors");
        }
        self.writes.lock().unwrap().push(cell.clone());
        if cell == "fail" {
            return Err(RecordError::backend(
                "test sink",
                true,
                std::io::Error::other("injected disk failure"),
            ));
        }
        Ok(AppendReceipt {
            persistence: Persistence::FileSynced,
            end_offset: self.writes.lock().unwrap().len() as u64,
        })
    }
}
impl Drop for Sink {
    fn drop(&mut self) {
        self.dropped.store(true, Ordering::SeqCst);
    }
}
fn sink() -> Sink {
    Sink {
        writes: Arc::new(Mutex::new(vec![])),
        gate: None,
        dropped: Arc::new(AtomicBool::new(false)),
    }
}
fn gated() -> (Sink, oneshot::Receiver<()>, mpsc::Sender<()>) {
    let (entered, receive) = oneshot::channel();
    let (release, gate) = mpsc::channel();
    let mut sink = sink();
    sink.gate = Some((entered, gate));
    (sink, receive, release)
}

#[tokio::test]
async fn disk_wait_does_not_block_async_timers_and_full_queue_retains_the_record() {
    let (sink, entered, release) = gated();
    let writes = sink.writes.clone();
    let (recorder, task) = spawn_recorder(sink, limits(1, 100_000)).unwrap();
    let first = recorder.enqueue(record("one")).await.unwrap();
    entered.await.unwrap();
    let second = recorder.try_enqueue(record("two")).unwrap();
    let third = record("three");
    let rejected = recorder.try_enqueue(third.clone()).err().unwrap();
    assert_eq!(rejected.reason, Rejection::Full);
    assert!(Arc::ptr_eq(&rejected.record, &third));
    // The sync writer is still blocked; a single-thread async runtime must keep its timer alive.
    assert!(
        tokio::time::timeout(Duration::from_millis(5), recorder.flush())
            .await
            .is_err()
    );
    drop(first); // Losing interest in the receipt does not undo admission.
    release.send(()).unwrap();
    assert_eq!(second.wait().await.unwrap().end_offset, 2);
    assert_eq!(recorder.flush().await.unwrap().attempted, 2);
    assert_eq!(*writes.lock().unwrap(), ["one", "two"]);
    recorder.shutdown().await.unwrap();
    task.join().await.unwrap();
}

#[tokio::test]
async fn payload_credit_includes_the_active_write_and_wait_cancellation_does_not_admit() {
    let (sink, entered, release) = gated();
    let writes = sink.writes.clone();
    let (recorder, task) = spawn_recorder(sink, limits(10, 1500)).unwrap();
    let first = recorder.enqueue(record("one")).await.unwrap();
    entered.await.unwrap();
    assert_eq!(
        recorder.try_enqueue(record("two")).err().unwrap().reason,
        Rejection::Full
    );
    assert!(
        tokio::time::timeout(
            Duration::from_millis(5),
            recorder.enqueue(record("not-admitted"))
        )
        .await
        .is_err()
    );
    assert_eq!(
        recorder
            .try_enqueue(record(&"x".repeat(1500)))
            .err()
            .unwrap()
            .reason,
        Rejection::TooLarge
    );
    release.send(()).unwrap();
    first.wait().await.unwrap();
    recorder.append(record("after")).await.unwrap();
    let report = recorder.shutdown().await.unwrap();
    assert_eq!(report.attempted, 2);
    task.join().await.unwrap();
    assert_eq!(*writes.lock().unwrap(), ["one", "after"]);
}

#[tokio::test]
async fn shutdown_drains_admissions_behind_its_request_and_releases_the_sink_before_ack() {
    let (sink, entered, release) = gated();
    let dropped = sink.dropped.clone();
    let (recorder, task) = spawn_recorder(sink, limits(4, 100_000)).unwrap();
    let first = recorder.enqueue(record("one")).await.unwrap();
    entered.await.unwrap();
    let shutdown = recorder.shutdown();
    tokio::pin!(shutdown);
    assert!(
        tokio::time::timeout(Duration::from_millis(5), &mut shutdown)
            .await
            .is_err()
    );
    let after = recorder
        .enqueue(record("already-accepted-behind-shutdown"))
        .await
        .unwrap();
    release.send(()).unwrap();
    let report = shutdown.await.unwrap();
    assert_eq!(report.attempted, 2);
    assert_eq!(report.failed, 0);
    assert!(dropped.load(Ordering::SeqCst));
    first.wait().await.unwrap();
    after.wait().await.unwrap();
    assert_eq!(
        recorder.try_enqueue(record("closed")).err().unwrap().reason,
        Rejection::Closed
    );
    task.join().await.unwrap();
}

#[tokio::test]
async fn dropping_all_handles_drains_accepted_writes_instead_of_aborting_the_worker() {
    let sink = sink();
    let writes = sink.writes.clone();
    let dropped = sink.dropped.clone();
    let (recorder, task) = spawn_recorder(sink, RecorderLimits::default()).unwrap();
    let first = recorder.enqueue(record("one")).await.unwrap();
    let second = recorder.enqueue(record("two")).await.unwrap();
    drop(recorder);
    task.join().await.unwrap();
    first.wait().await.unwrap();
    second.wait().await.unwrap();
    assert_eq!(*writes.lock().unwrap(), ["one", "two"]);
    assert!(dropped.load(Ordering::SeqCst));
}

#[tokio::test]
async fn individual_failures_and_drain_reports_never_claim_all_records_were_saved() {
    let (recorder, task) = spawn_recorder(sink(), RecorderLimits::default()).unwrap();
    assert!(matches!(
        recorder.append(record("fail")).await,
        Err(RecordError::Backend {
            may_have_appended: true,
            ..
        })
    ));
    recorder.append(record("ok")).await.unwrap();
    let report = recorder.flush().await.unwrap();
    assert_eq!((report.attempted, report.failed), (2, 1));
    assert_eq!(recorder.shutdown().await.unwrap(), report);
    task.join().await.unwrap();
}

#[tokio::test]
async fn a_panicked_writer_closes_receipts_and_join_without_exposing_its_payload() {
    let (recorder, task) = spawn_recorder(sink(), RecorderLimits::default()).unwrap();
    let error = recorder.append(record("panic")).await.unwrap_err();
    assert!(matches!(error, RecordError::Closed));
    assert!(!error.to_string().contains("private"));
    assert!(matches!(task.join().await, Err(RecordError::Closed)));
    assert!(matches!(
        recorder.append(record("later")).await,
        Err(RecordError::Closed)
    ));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn concurrent_producers_are_serialized_with_exact_acknowledgement_counts() {
    let sink = sink();
    let writes = sink.writes.clone();
    let (recorder, task) = spawn_recorder(sink, limits(8, 16_000)).unwrap();
    let mut producers = tokio::task::JoinSet::new();
    for i in 0..200 {
        let recorder = recorder.clone();
        producers
            .spawn(async move { recorder.append(record(&format!("cell-{i}"))).await.unwrap() });
    }
    let mut offsets = std::collections::HashSet::new();
    while let Some(result) = producers.join_next().await {
        offsets.insert(result.unwrap().end_offset);
    }
    assert_eq!(offsets.len(), 200);
    let report = recorder.shutdown().await.unwrap();
    assert_eq!((report.attempted, report.failed), (200, 0));
    task.join().await.unwrap();
    assert_eq!(
        writes
            .lock()
            .unwrap()
            .iter()
            .collect::<std::collections::HashSet<_>>()
            .len(),
        200
    );
}

#[test]
fn invalid_platform_capacities_return_an_error_instead_of_panicking_in_a_channel() {
    assert!(matches!(
        spawn_recorder(sink(), limits(usize::MAX, 4096)),
        Err(RecordError::Limit("queue capacity"))
    ));
}
