use std::{
    num::{NonZeroU32, NonZeroUsize},
    sync::{Arc, Mutex, mpsc},
    time::Duration,
};
use tokio::sync::oneshot;
use wes_core::{ErrorId, ErrorValue, Timestamp};
use wes_engine::{
    graph::{NodeId, NodeState},
    history::{
        AppendReceipt, JournalEntry, JournalSink, Persistence, Record, RecordError,
        RequiredPersistence,
    },
    log::{LogRecorder, LogStatus},
    recording::{RecorderLimits, spawn_recorder},
    runtime::{Observation, RunId},
};
use wes_language::{Diagnostic, Span};

type Gate = (oneshot::Sender<()>, mpsc::Receiver<()>);
struct Sink {
    records: Arc<Mutex<Vec<Record>>>,
    persistence: Persistence,
    gate: Option<Gate>,
    failure: Option<bool>,
    panic: bool,
}
impl JournalSink for Sink {
    fn append(&mut self, record: &Record) -> Result<AppendReceipt, RecordError> {
        if let Some((entered, release)) = self.gate.take() {
            entered.send(()).unwrap();
            release.recv_timeout(Duration::from_secs(5)).unwrap();
        }
        if self.failure != Some(false) {
            self.records.lock().unwrap().push(record.clone());
        }
        assert!(!self.panic, "synthetic private panic");
        if let Some(may_have_appended) = self.failure {
            return Err(RecordError::backend(
                "injected failure",
                may_have_appended,
                std::io::Error::other("synthetic private disk detail"),
            ));
        }
        Ok(AppendReceipt {
            persistence: self.persistence,
            end_offset: 123,
        })
    }
}
fn sink() -> Sink {
    Sink {
        records: Arc::default(),
        persistence: Persistence::FileSynced,
        gate: None,
        failure: None,
        panic: false,
    }
}
fn at() -> Timestamp {
    Timestamp::new(0, 42).unwrap()
}
#[tokio::test]
async fn operational_notice_failure_preserves_original_error_without_recursive_logging() {
    use wes_engine::{history::NoticeContext, log::PreparedLog, storage::ValueHandle};
    let original =
        wes_engine::runtime::RuntimeCode::RecordingFailed.error("original retention failure", None);
    let mut backend = sink();
    backend.failure = Some(true);
    let records = backend.records.clone();
    let (recorder, task) = spawn_recorder(backend, RecorderLimits::default()).unwrap();
    let log = LogRecorder::recorded(recorder.clone(), RequiredPersistence::FileSynced);
    let logged = log
        .record(
            PreparedLog::notice(
                at(),
                NoticeContext::Keep {
                    handle: ValueHandle::fresh(),
                    may_have_applied: true,
                },
                original.clone(),
            )
            .unwrap(),
        )
        .wait()
        .await;
    let JournalEntry::Noticed(notice) = logged.entry() else {
        panic!("operational log")
    };
    assert_eq!(notice.error(), &original);
    assert_ne!(notice.id(), original.id().as_str());
    let LogStatus::Unconfirmed {
        error,
        may_have_appended,
        ..
    } = logged.status()
    else {
        panic!("unconfirmed log")
    };
    assert!(*may_have_appended);
    assert_ne!(error.id(), original.id());
    assert!(!error.message().contains("private"));
    assert_eq!(records.lock().unwrap().len(), 1);
    recorder.shutdown().await.unwrap();
    task.join().await.unwrap();
}
fn observation() -> Observation {
    Observation {
        revision: 0,
        stale_reason: None,
        delivery: None,
        stopped: None,
        node: NodeId::new("node").unwrap(),
        run: Some(RunId::new("run").unwrap()),
        state: NodeState::Cancelled,
        value: None,
        error: Some(
            ErrorValue::new(
                ErrorId::new("original-error").unwrap(),
                "RUN003",
                "cancelled",
                vec![],
                None,
            )
            .unwrap(),
        ),
    }
}

#[tokio::test]
async fn memory_logging_is_explicit_and_observation_and_error_identities_remain_distinct() {
    let original = observation();
    let log = LogRecorder::memory();
    let pending = log.observation(at(), &original).unwrap();
    let id = pending.initial().id().to_string();
    assert!(matches!(pending.initial().status(), LogStatus::Memory));
    let logged = pending.wait().await;
    assert_eq!(logged.id(), id);
    assert!(!logged.durable());
    let JournalEntry::Observed(record) = logged.entry() else {
        panic!("execution record")
    };
    assert_eq!(record.node(), &original.node);
    assert_eq!(record.run(), original.run.as_ref());
    assert_eq!(record.state(), NodeState::Cancelled);
    assert_eq!(record.at(), at());
    assert_eq!(record.error(), original.error.as_ref());
    assert_ne!(record.id(), "original-error");
}

#[tokio::test]
async fn logging_validates_diagnostic_source_spans_and_preserves_unicode_source_and_severity() {
    let source = "🦀\n:help";
    let diagnostic = Diagnostic::error("EXAMPLE", Span::new(5, 10).unwrap(), "description");
    let logged = LogRecorder::memory()
        .diagnostic(at(), "cell".into(), source.into(), diagnostic.clone())
        .unwrap()
        .wait()
        .await;
    let JournalEntry::Diagnosed(record) = logged.entry() else {
        panic!("diagnostic")
    };
    assert_eq!(record.source(), source);
    assert_eq!(record.cell(), "cell");
    assert_eq!(record.diagnostic(), &diagnostic);
    assert!(
        LogRecorder::memory()
            .diagnostic(
                at(),
                "cell".into(),
                source.into(),
                Diagnostic::error("bad", Span::at(1), "splits UTF-8")
            )
            .is_err()
    );
}

#[tokio::test]
async fn every_persistence_policy_checks_the_actual_receipt_and_never_labels_memory_as_durable() {
    for actual in [
        Persistence::Volatile,
        Persistence::FileSynced,
        Persistence::FileAndDirectorySynced,
    ] {
        for required in [
            RequiredPersistence::Volatile,
            RequiredPersistence::FileSynced,
            RequiredPersistence::FileAndDirectorySynced,
        ] {
            let mut destination = sink();
            destination.persistence = actual;
            let records = destination.records.clone();
            let (recorder, task) = spawn_recorder(destination, RecorderLimits::default()).unwrap();
            let pending = LogRecorder::recorded(recorder.clone(), required)
                .observation(at(), &observation())
                .unwrap();
            let id = pending.initial().id().to_string();
            assert!(matches!(pending.initial().status(), LogStatus::Pending));
            let logged = pending.wait().await;
            assert_eq!(logged.id(), id);
            assert_eq!(
                logged.durable(),
                required.accepts(actual) && actual != Persistence::Volatile
            );
            if required.accepts(actual) {
                assert!(
                    matches!(logged.status(), LogStatus::Acknowledged(receipt) if receipt.persistence == actual && receipt.end_offset == 123)
                );
            } else {
                assert!(
                    matches!(logged.status(), LogStatus::Unconfirmed { acknowledgement: Some(receipt), may_have_appended: true, .. } if receipt.persistence == actual)
                );
            }
            assert_eq!(records.lock().unwrap().len(), 1);
            recorder.shutdown().await.unwrap();
            task.join().await.unwrap();
        }
    }
}

#[tokio::test]
async fn queue_overflow_is_nonblocking_and_keeps_the_full_original_cancel_record() {
    let mut destination = sink();
    let (entered, entered_rx) = oneshot::channel();
    let (release, release_rx) = mpsc::channel();
    destination.gate = Some((entered, release_rx));
    let records = destination.records.clone();
    let (recorder, task) = spawn_recorder(
        destination,
        RecorderLimits {
            records: NonZeroUsize::new(1).unwrap(),
            bytes: NonZeroU32::new(1024 * 1024).unwrap(),
        },
    )
    .unwrap();
    let log = LogRecorder::recorded(recorder.clone(), RequiredPersistence::FileSynced);
    let first = log.observation(at(), &observation()).unwrap();
    entered_rx.await.unwrap();
    let second = log.observation(at(), &observation()).unwrap();
    let refused = log.observation(at(), &observation()).unwrap();
    let initial_id = refused.initial().id().to_string();
    let refused = tokio::time::timeout(Duration::from_secs(1), refused.wait())
        .await
        .unwrap();
    assert_eq!(refused.id(), initial_id);
    assert!(!refused.durable());
    let LogStatus::Unconfirmed {
        error,
        may_have_appended,
        acknowledgement,
    } = refused.status()
    else {
        panic!("unsaved")
    };
    assert!(error.message().contains("full"));
    assert_eq!(error.code(), "RUN005");
    assert!(!may_have_appended);
    assert!(acknowledgement.is_none());
    let JournalEntry::Observed(record) = refused.entry() else {
        panic!("original record")
    };
    assert_eq!(record.state(), NodeState::Cancelled);
    assert_eq!(record.error().unwrap().id().as_str(), "original-error");
    assert_ne!(error.id().as_str(), "original-error");
    release.send(()).unwrap();
    assert!(first.wait().await.durable());
    assert!(second.wait().await.durable());
    recorder.shutdown().await.unwrap();
    task.join().await.unwrap();
    assert_eq!(records.lock().unwrap().len(), 2);
}

#[tokio::test]
async fn backend_failure_and_panics_keep_uncertainty_without_exposing_backend_payloads() {
    for failure in [Some(false), Some(true), None] {
        let mut destination = sink();
        destination.failure = failure;
        destination.panic = failure.is_none();
        let (recorder, task) = spawn_recorder(destination, RecorderLimits::default()).unwrap();
        let logged = LogRecorder::recorded(recorder.clone(), RequiredPersistence::FileSynced)
            .observation(at(), &observation())
            .unwrap()
            .wait()
            .await;
        let LogStatus::Unconfirmed {
            error,
            may_have_appended,
            acknowledgement,
        } = logged.status()
        else {
            panic!("failed acknowledgement")
        };
        assert_eq!(*may_have_appended, failure.unwrap_or(true));
        assert!(acknowledgement.is_none());
        assert!(!error.message().contains("private"));
        assert!(!format!("{logged:?}").contains("private"));
        assert!(!logged.durable());
        if failure.is_some() {
            recorder.shutdown().await.unwrap();
            task.join().await.unwrap();
        } else {
            assert!(task.join().await.is_err());
        }
    }
}

#[tokio::test]
async fn oversize_and_closed_refusals_are_explicit_and_dropped_attempts_do_not_retract_admitted_records()
 {
    let destination = sink();
    let records = destination.records.clone();
    let (recorder, task) = spawn_recorder(destination, RecorderLimits::default()).unwrap();
    let log = LogRecorder::recorded(recorder.clone(), RequiredPersistence::FileSynced);
    let pending = log.observation(at(), &observation()).unwrap();
    let id = pending.initial().id().to_string();
    drop(pending);
    assert_eq!(recorder.flush().await.unwrap().attempted, 1);
    assert!(
        matches!(&records.lock().unwrap()[0], Record::Journal(JournalEntry::Observed(record)) if record.id() == id)
    );
    recorder.shutdown().await.unwrap();
    task.join().await.unwrap();
    let closed = log.observation(at(), &observation()).unwrap().wait().await;
    assert!(matches!(
        closed.status(),
        LogStatus::Unconfirmed {
            may_have_appended: false,
            ..
        }
    ));
    let (recorder, task) = spawn_recorder(
        sink(),
        RecorderLimits {
            bytes: NonZeroU32::new(1).unwrap(),
            ..RecorderLimits::default()
        },
    )
    .unwrap();
    let too_large = LogRecorder::recorded(recorder.clone(), RequiredPersistence::FileSynced)
        .observation(at(), &observation())
        .unwrap()
        .wait()
        .await;
    assert!(
        matches!(too_large.status(), LogStatus::Unconfirmed { error, may_have_appended: false, .. } if error.message().contains("budget"))
    );
    assert_eq!(recorder.shutdown().await.unwrap().attempted, 0);
    task.join().await.unwrap();
}
