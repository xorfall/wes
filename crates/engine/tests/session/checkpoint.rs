use super::*;
use wes_engine::history::{HistoryCapture, HistoryCaptureLimits, HistoryCheckpoint, HistoryImage};

#[derive(Default)]
struct Sink {
    records: Vec<Record>,
    capture_gate: Option<Gate>,
    fail_capture: bool,
}
impl JournalSink for Sink {
    fn append(&mut self, record: &Record) -> Result<AppendReceipt, RecordError> {
        self.records.push(record.clone());
        Ok(AppendReceipt {
            persistence: Persistence::FileSynced,
            end_offset: self.records.len() as u64,
        })
    }
    fn capture(&mut self, limits: HistoryCaptureLimits) -> Result<HistoryImage, RecordError> {
        if let Some((entered, released)) = self.capture_gate.take() {
            let _ = entered.send(());
            released.recv_timeout(Duration::from_secs(5)).unwrap();
        }
        if self.fail_capture {
            return Err(RecordError::CaptureUnsupported);
        }
        let mut image = HistoryCapture::new(limits);
        for record in &self.records {
            image.push(record.clone())?;
        }
        let receipt = AppendReceipt {
            persistence: Persistence::FileSynced,
            end_offset: self.records.len() as u64,
        };
        Ok(image.finish(HistoryCheckpoint {
            journal: receipt,
            recovery: receipt,
        }))
    }
}
fn recorded(
    base: Workspace,
    sink: Sink,
) -> (
    SessionHandle,
    session::SessionTask,
    wes_engine::recording::Recorder,
    wes_engine::recording::RecorderTask,
) {
    let (recorder, writer) = spawn_recorder(sink, RecorderLimits::default()).unwrap();
    let (handle, task) = session::spawn(
        base,
        RecordingMode::Required(CallJournal::new(
            recorder.clone(),
            RequiredPersistence::FileSynced,
        )),
        no_files(),
        NonZeroUsize::new(1).unwrap(),
    )
    .unwrap();
    (handle, task, recorder, writer)
}
async fn until_paused(handle: &SessionHandle) {
    tokio::time::timeout(Duration::from_secs(5), async {
        while !handle.snapshot().await.unwrap().checkpoint_pending {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
}

#[tokio::test]
async fn checkpoint_settles_admitted_work_keeps_both_streams_and_resumes_queued_source() {
    let gate = Arc::new(Notify::new());
    let (entered, running) = oneshot::channel();
    let (base, calls) = workspace(Some(gate.clone()), Some(entered));
    let (handle, task, recorder, writer) = recorded(base, Sink::default());
    submit(&handle, "before", "catalog echo value:blocked > before").await;
    running.await.unwrap();
    let h = handle.clone();
    let pending = tokio::spawn(async move { h.checkpoint().await });
    until_paused(&handle).await;
    let h = handle.clone();
    let after = tokio::spawn(async move {
        h.submit(input("after", ":def later as catalog echo value:after"))
            .await
    });
    assert!(!pending.is_finished());
    assert!(matches!(
        handle.checkpoint().await,
        Err(SessionError::CheckpointBusy)
    ));
    gate.notify_one();
    let checkpoint = tokio::time::timeout(Duration::from_secs(5), pending)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    assert!(
        checkpoint
            .history()
            .journal()
            .iter()
            .any(|entry| matches!(entry, JournalEntry::Command(c) if c.cell == "before"))
    );
    assert!(
        !checkpoint
            .history()
            .journal()
            .iter()
            .any(|entry| matches!(entry, JournalEntry::Command(c) if c.cell == "after"))
    );
    assert!(
        checkpoint
            .history()
            .recovery()
            .iter()
            .any(|entry| matches!(entry, RecoveryEntry::Called { .. }))
    );
    assert!(handle.snapshot().await.unwrap().execution.idle);
    assert!(!after.is_finished());
    assert!(matches!(
        handle.submit(input("refresh", ":refresh $before")).await,
        Err(SessionError::CheckpointBusy)
    ));
    drop(checkpoint);
    assert!(
        tokio::time::timeout(Duration::from_secs(5), after)
            .await
            .unwrap()
            .unwrap()
            .is_ok()
    );
    stop(handle, task).await;
    recorder.shutdown().await.unwrap();
    writer.join().await.unwrap();
}

#[tokio::test]
async fn cancel_works_while_settling_and_abandoning_a_pending_checkpoint_unpauses() {
    let gate = Arc::new(Notify::new());
    let (entered, running) = oneshot::channel();
    let (base, _) = workspace(Some(gate.clone()), Some(entered));
    let (handle, task, recorder, writer) = recorded(base, Sink::default());
    submit(&handle, "before", "catalog echo value:blocked > before").await;
    running.await.unwrap();
    let h = handle.clone();
    let pending = tokio::spawn(async move { h.checkpoint().await });
    until_paused(&handle).await;
    let cancelled = submit(&handle, "cancel", ":cancel $before").await;
    assert!(!cancelled.accepted.is_empty());
    pending.abort();
    let _ = pending.await;
    // No physical exit yet. Disconnect itself must wake the actor and release source admission.
    assert!(
        !submit(
            &handle,
            "definition",
            ":def later as catalog echo value:after"
        )
        .await
        .accepted
        .is_empty()
    );
    gate.notify_one();
    stop(handle, task).await;
    recorder.shutdown().await.unwrap();
    writer.join().await.unwrap();
}

#[tokio::test]
async fn entered_capture_is_joined_on_disconnect_and_shutdown_but_snapshots_stay_responsive() {
    let (entered, blocked) = oneshot::channel();
    let (release, released) = mpsc::channel();
    let (base, _) = workspace(None, None);
    let (handle, task, recorder, writer) = recorded(
        base,
        Sink {
            capture_gate: Some((entered, released)),
            ..Default::default()
        },
    );
    let h = handle.clone();
    let pending = tokio::spawn(async move { h.checkpoint().await });
    blocked.await.unwrap();
    pending.abort();
    let _ = pending.await;
    assert!(handle.snapshot().await.unwrap().checkpoint_pending);
    handle.shutdown().await.unwrap();
    let mut joining = tokio::spawn(task.join());
    assert!(
        tokio::time::timeout(Duration::from_millis(10), &mut joining)
            .await
            .is_err()
    );
    release.send(()).unwrap();
    tokio::time::timeout(Duration::from_secs(5), joining)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    recorder.shutdown().await.unwrap();
    writer.join().await.unwrap();
}

#[tokio::test]
async fn failed_capture_releases_admission_and_held_caller_does_not_prevent_shutdown() {
    let (base, _) = workspace(None, None);
    let (handle, task, recorder, writer) = recorded(
        base,
        Sink {
            fail_capture: true,
            ..Default::default()
        },
    );
    assert!(matches!(
        handle.checkpoint().await,
        Err(SessionError::Recording)
    ));
    assert!(
        !submit(
            &handle,
            "definition",
            ":def later as catalog echo value:after"
        )
        .await
        .accepted
        .is_empty()
    );
    stop(handle, task).await;
    recorder.shutdown().await.unwrap();
    writer.join().await.unwrap();

    let (base, _) = workspace(None, None);
    let (handle, task, recorder, writer) = recorded(base, Sink::default());
    let checkpoint = handle.checkpoint().await.unwrap();
    stop(handle, task).await;
    recorder.shutdown().await.unwrap();
    writer.join().await.unwrap();
    assert!(checkpoint.history().journal().is_empty());
}

#[tokio::test]
async fn ephemeral_sessions_refuse_checkpoint_without_freezing() {
    let (base, _) = workspace(None, None);
    let (handle, task) = session::spawn(
        base,
        RecordingMode::Ephemeral,
        no_files(),
        NonZeroUsize::new(1).unwrap(),
    )
    .unwrap();
    assert!(matches!(
        handle.checkpoint().await,
        Err(SessionError::NoHistory)
    ));
    assert!(
        !submit(
            &handle,
            "definition",
            ":def later as catalog echo value:after"
        )
        .await
        .accepted
        .is_empty()
    );
    stop(handle, task).await;
}
