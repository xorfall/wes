use super::*;
use wes_engine::history::{HistoryCapture, HistoryCaptureLimits, HistoryCheckpoint, HistoryImage};

#[derive(Default)]
struct CapturingSink {
    entries: Vec<Record>,
    gate: Option<(oneshot::Sender<()>, mpsc::Receiver<()>)>,
    panic: bool,
    ignore_limits: bool,
}
impl JournalSink for CapturingSink {
    fn append(&mut self, record: &Record) -> Result<AppendReceipt, RecordError> {
        if matches!(record, Record::Recovery(RecoveryEntry::Accepted { cell }) if cell == "fail") {
            return Err(RecordError::Limit("synthetic append refusal"));
        }
        self.entries.push(record.clone());
        Ok(AppendReceipt {
            persistence: Persistence::FileSynced,
            end_offset: self.entries.len() as u64,
        })
    }
    fn capture(&mut self, limits: HistoryCaptureLimits) -> Result<HistoryImage, RecordError> {
        if let Some((entered, release)) = self.gate.take() {
            entered.send(()).unwrap();
            release.recv_timeout(Duration::from_secs(5)).unwrap();
        }
        assert!(!self.panic, "synthetic private capture failure");
        let mut capture = HistoryCapture::new(if self.ignore_limits {
            HistoryCaptureLimits::default()
        } else {
            limits
        });
        for record in &self.entries {
            capture.push(record.clone())?;
        }
        let receipt = AppendReceipt {
            persistence: Persistence::FileSynced,
            end_offset: self.entries.len() as u64,
        };
        Ok(capture.finish(HistoryCheckpoint {
            journal: receipt,
            recovery: receipt,
        }))
    }
}

#[tokio::test]
async fn capture_is_ordered_and_retained_reply_credit_does_not_block_appends_or_shutdown() {
    let (entered, blocked) = oneshot::channel();
    let (release, released) = mpsc::channel();
    let (recorder, writer) = spawn_recorder(
        CapturingSink {
            gate: Some((entered, released)),
            ..Default::default()
        },
        RecorderLimits::default(),
    )
    .unwrap();
    recorder.append(record("before")).await.unwrap();
    let r = recorder.clone();
    let snapshot =
        tokio::spawn(async move { r.capture(HistoryCaptureLimits::default()).await.unwrap() });
    blocked.await.unwrap();
    let after = recorder.enqueue(record("after")).await.unwrap();
    release.send(()).unwrap();
    let captured = snapshot.await.unwrap();
    assert_eq!(
        captured.image().recovery(),
        &[RecoveryEntry::Accepted {
            cell: "before".into()
        }]
    );
    assert_eq!(captured.image().checkpoint().recovery.end_offset, 1);
    after.wait().await.unwrap();
    // The first returned image owns the capture credit, but ordinary append/flush remain usable.
    assert!(
        tokio::time::timeout(
            Duration::from_millis(5),
            recorder.capture(HistoryCaptureLimits::default())
        )
        .await
        .is_err()
    );
    assert_eq!(recorder.flush().await.unwrap().attempted, 2);
    let image = captured.into_image(); // Explicitly caller-owned now.
    let next = recorder
        .capture(HistoryCaptureLimits::default())
        .await
        .unwrap();
    assert_eq!(next.image().recovery().len(), 2);
    assert_eq!(image.recovery().len(), 1);
    let r = recorder.clone();
    let waiting = tokio::spawn(async move { r.capture(HistoryCaptureLimits::default()).await });
    let report = recorder.shutdown().await.unwrap();
    assert_eq!(report.attempted, 2);
    assert_eq!(report.failed, 0); // Read attempts are not append failures.
    assert!(matches!(
        tokio::time::timeout(Duration::from_secs(5), waiting)
            .await
            .unwrap()
            .unwrap(),
        Err(RecordError::Closed)
    ));
    writer.join().await.unwrap();
    assert_eq!(next.image().recovery().len(), 2); // Shutdown does not revoke caller data.
}

#[tokio::test]
async fn abandoned_capture_is_joined_and_keeps_credit_until_physical_completion() {
    let (entered, blocked) = oneshot::channel();
    let (release, released) = mpsc::channel();
    let (recorder, writer) = spawn_recorder(
        CapturingSink {
            gate: Some((entered, released)),
            ..Default::default()
        },
        RecorderLimits::default(),
    )
    .unwrap();
    let r = recorder.clone();
    let pending = tokio::spawn(async move { r.capture(HistoryCaptureLimits::default()).await });
    blocked.await.unwrap();
    pending.abort();
    assert!(pending.await.unwrap_err().is_cancelled());
    assert!(
        tokio::time::timeout(
            Duration::from_millis(5),
            recorder.capture(HistoryCaptureLimits::default())
        )
        .await
        .is_err()
    );
    let r = recorder.clone();
    let closing = tokio::spawn(async move { r.shutdown().await });
    tokio::task::yield_now().await;
    assert!(!closing.is_finished());
    release.send(()).unwrap();
    assert_eq!(closing.await.unwrap().unwrap().attempted, 0);
    writer.join().await.unwrap();
}

#[tokio::test]
async fn unsupported_and_excessive_capture_fail_explicitly_without_poisoning_append_admission() {
    let (recorder, writer) = spawn_recorder(sink(), RecorderLimits::default()).unwrap();
    assert!(matches!(
        recorder.capture(HistoryCaptureLimits::default()).await,
        Err(RecordError::CaptureUnsupported)
    ));
    for limits in [
        HistoryCaptureLimits {
            records: usize::MAX,
            ..Default::default()
        },
        HistoryCaptureLimits {
            bytes: u64::MAX,
            ..Default::default()
        },
    ] {
        assert!(matches!(
            recorder.capture(limits).await,
            Err(RecordError::Limit("captured history policy"))
        ));
    }
    recorder.append(record("still-usable")).await.unwrap();
    assert_eq!(recorder.shutdown().await.unwrap().failed, 0);
    writer.join().await.unwrap();
    for ignore_limits in [false, true] {
        let (recorder, writer) = spawn_recorder(
            CapturingSink {
                ignore_limits,
                ..Default::default()
            },
            RecorderLimits::default(),
        )
        .unwrap();
        recorder.append(record("one")).await.unwrap();
        for limits in [
            HistoryCaptureLimits {
                records: 0,
                ..Default::default()
            },
            HistoryCaptureLimits {
                bytes: 0,
                ..Default::default()
            },
        ] {
            assert!(matches!(
                recorder.capture(limits).await,
                Err(RecordError::Limit(_))
            ));
        }
        recorder.append(record("two")).await.unwrap();
        let captured = recorder
            .capture(HistoryCaptureLimits::default())
            .await
            .unwrap();
        assert_eq!(captured.image().recovery().len(), 2);
        assert_eq!(recorder.shutdown().await.unwrap().failed, 0);
        writer.join().await.unwrap();
    }
}

#[tokio::test]
async fn capture_panic_closes_the_writer_without_returning_private_panic_payload() {
    let (recorder, writer) = spawn_recorder(
        CapturingSink {
            panic: true,
            ..Default::default()
        },
        RecorderLimits::default(),
    )
    .unwrap();
    assert!(matches!(
        recorder.capture(HistoryCaptureLimits::default()).await,
        Err(RecordError::Closed)
    ));
    assert!(matches!(
        recorder.append(record("after")).await,
        Err(RecordError::Closed)
    ));
    assert!(matches!(writer.join().await, Err(RecordError::Closed)));
}

#[tokio::test]
async fn successful_capture_retains_evidence_of_earlier_append_failures() {
    let (recorder, writer) =
        spawn_recorder(CapturingSink::default(), RecorderLimits::default()).unwrap();
    recorder.append(record("saved")).await.unwrap();
    assert!(recorder.append(record("fail")).await.is_err());
    let captured = recorder
        .capture(HistoryCaptureLimits::default())
        .await
        .unwrap();
    assert_eq!(captured.image().recovery().len(), 1);
    assert_eq!(captured.append_report().attempted, 2);
    assert_eq!(captured.append_report().failed, 1);
    recorder.append(record("later")).await.unwrap();
    assert_eq!(captured.append_report().attempted, 2);
    assert_eq!(recorder.shutdown().await.unwrap().attempted, 3);
    writer.join().await.unwrap();
}
