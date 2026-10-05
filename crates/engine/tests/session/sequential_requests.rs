use super::*;
use wes_engine::session::SequentialState;

async fn finish(handle: &SessionHandle, cell: &str) -> SequentialState {
    tokio::time::timeout(Duration::from_secs(5), async {
        let mut updates = handle.subscribe_updates().unwrap();
        loop {
            if let Some(state) = handle.sequential_state(cell)
                && state != SequentialState::Running
            {
                return state;
            }
            updates.recv().await.unwrap_or(());
        }
    })
    .await
    .unwrap()
}
fn source(cell: &str, text: &str) -> SourceInput {
    input(cell, text)
        .with_client("owner".into())
        .unwrap()
        .cooperative()
}
async fn sequence(
    handle: &SessionHandle,
    request: &str,
    text: &str,
) -> wes_engine::history::RequestRecord {
    handle
        .submit_sequential_request(
            "pane".into(),
            request.into(),
            "context".into(),
            true,
            source(request, text),
            CancellationToken::new(),
        )
        .await
        .unwrap()
        .claim
        .record
}
#[tokio::test]
async fn sequential_waits_for_finite_results_and_deduplicates_mode_and_unicode_source() {
    let gate = Arc::new(Notify::new());
    let (entered, blocked) = oneshot::channel();
    let (base, calls) = workspace(Some(gate.clone()), Some(entered));
    let (handle, task) = session::spawn(
        base,
        RecordingMode::Ephemeral,
        no_files(),
        NonZeroUsize::new(2).unwrap(),
    )
    .unwrap();
    let text = "catalog echo value:blocked > first\n:calc { return $first + 'é'; } > second\ncatalog echo value:$second > third";
    let record = sequence(&handle, "run", text).await;
    blocked.await.unwrap();
    assert_eq!(handle.observe().await.unwrap().cells.len(), 1);
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    let duplicate = handle
        .submit_sequential_request(
            "pane".into(),
            "run".into(),
            "context".into(),
            false,
            source("unused", text),
            CancellationToken::new(),
        )
        .await
        .unwrap();
    assert!(!duplicate.claim.fresh);
    assert_eq!(duplicate.claim.record.steps, record.steps);
    assert!(matches!(
        handle
            .submit_request(
                "pane".into(),
                "run".into(),
                "context".into(),
                true,
                source("unused", text)
            )
            .await,
        Err(RecordError::RequestConflict)
    ));
    gate.notify_one();
    assert_eq!(
        finish(&handle, &record.cell).await,
        SequentialState::Completed
    );
    let observed = handle.observe().await.unwrap();
    assert_eq!(observed.cells.len(), 3);
    assert_eq!(calls.load(Ordering::SeqCst), 2);
    assert_eq!(
        observed.state.execution.values[&observed.state.names["third"].node].data(),
        &Data::Text("blockedé".into())
    );
    assert_eq!(observed.cells[1].input.source_start().line, 2);
    stop(handle, task).await;
}
#[tokio::test]
async fn parse_preflight_is_atomic_and_runtime_failure_stops_later_effects() {
    let (base, calls) = workspace(None, None);
    let (handle, task) = session::spawn(
        base,
        RecordingMode::Ephemeral,
        no_files(),
        NonZeroUsize::new(2).unwrap(),
    )
    .unwrap();
    assert!(
        handle
            .submit_sequential_request(
                "pane".into(),
                "bad".into(),
                "context".into(),
                true,
                source("bad", "catalog echo value:hello\n:calc {"),
                CancellationToken::new()
            )
            .await
            .is_err()
    );
    assert!(handle.find_request("pane", "bad").await.unwrap().is_none());
    assert_eq!(calls.load(Ordering::SeqCst), 0);
    let record = sequence(
        &handle,
        "failed",
        "catalog echo value:hello\n:calc { return 1 / 0; }\ncatalog echo value:never",
    )
    .await;
    assert_eq!(finish(&handle, &record.cell).await, SequentialState::Failed);
    assert_eq!(handle.observe().await.unwrap().cells.len(), 2);
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    stop(handle, task).await;
}
#[tokio::test]
async fn cancellation_is_authorized_and_blocks_future_steps_without_replay() {
    let gate = Arc::new(Notify::new());
    let (entered, blocked) = oneshot::channel();
    let (base, calls) = workspace(Some(gate.clone()), Some(entered));
    let (handle, task) = session::spawn(
        base,
        RecordingMode::Ephemeral,
        no_files(),
        NonZeroUsize::new(2).unwrap(),
    )
    .unwrap();
    let text = "catalog echo value:blocked > first\ncatalog echo value:never";
    let record = sequence(&handle, "cancel", text).await;
    blocked.await.unwrap();
    assert!(
        handle
            .cancel_request_work("foreign".into(), &record)
            .await
            .is_err()
    );
    assert_eq!(
        handle.sequential_state(&record.cell),
        Some(SequentialState::Running)
    );
    handle
        .cancel_request_work("owner".into(), &record)
        .await
        .unwrap();
    assert_eq!(
        finish(&handle, &record.cell).await,
        SequentialState::Cancelled
    );
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    let duplicate = handle
        .submit_sequential_request(
            "pane".into(),
            "cancel".into(),
            "context".into(),
            false,
            source("unused", text),
            CancellationToken::new(),
        )
        .await
        .unwrap();
    assert!(!duplicate.claim.fresh);
    assert_eq!(handle.observe().await.unwrap().cells.len(), 1);
    gate.notify_waiters();
    stop(handle, task).await;
}
#[tokio::test]
async fn terminal_revocation_stops_waiting_work_and_capacity_leaves_controls_available() {
    let gate = Arc::new(Notify::new());
    let (base, calls) = workspace(Some(gate.clone()), None);
    let (handle, task) = session::spawn(
        base,
        RecordingMode::Ephemeral,
        no_files(),
        NonZeroUsize::new(4).unwrap(),
    )
    .unwrap();
    let stop_token = CancellationToken::new();
    let first = handle
        .submit_sequential_request(
            "pane".into(),
            "one".into(),
            "context".into(),
            true,
            source(
                "one",
                "catalog echo value:blocked\ncatalog echo value:never",
            ),
            stop_token.clone(),
        )
        .await
        .unwrap()
        .claim
        .record;
    for i in 0..3 {
        sequence(
            &handle,
            &format!("pending{i}"),
            "catalog echo value:blocked\ncatalog echo value:never",
        )
        .await;
    }
    assert!(matches!(
        handle
            .submit_sequential_request(
                "pane".into(),
                "excess".into(),
                "context".into(),
                true,
                source("excess", ":calc { return 1; }"),
                CancellationToken::new()
            )
            .await,
        Err(RecordError::ReadBusy)
    ));
    assert!(
        handle
            .find_request("pane", "excess")
            .await
            .unwrap()
            .is_none()
    );
    tokio::time::timeout(Duration::from_secs(5), async {
        while calls.load(Ordering::SeqCst) != 4 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    stop_token.cancel();
    assert_eq!(
        finish(&handle, &first.cell).await,
        SequentialState::Cancelled
    );
    for i in 0..3 {
        let record = handle
            .find_request("pane", &format!("pending{i}"))
            .await
            .unwrap()
            .unwrap();
        handle
            .cancel_request_work("owner".into(), &record)
            .await
            .unwrap();
        assert_eq!(
            finish(&handle, &record.cell).await,
            SequentialState::Cancelled
        );
    }
    assert_eq!(calls.load(Ordering::SeqCst), 4);
    gate.notify_waiters();
    stop(handle, task).await;
}

#[tokio::test]
async fn open_streams_stop_the_finite_workflow_and_do_not_dispatch_the_next_effect() {
    use wes_engine::streams::{StreamFuture, StreamSink, StreamingInvoker};
    struct Open;
    impl Invoker for Open {
        fn invoke(&self, _: Call, _: CancellationToken) -> InvocationFuture {
            panic!("finite port");
        }
    }
    impl StreamingInvoker for Open {
        fn subscribe(&self, _: Call, sink: StreamSink, stop: CancellationToken) -> StreamFuture {
            Box::pin(async move {
                sink.opened().unwrap();
                stop.cancelled().await;
                Ok(())
            })
        }
    }
    let (mut base, calls) = workspace(None, None);
    let mut capability = Capability::new(["watch"], Shape::Unknown, Safety::Safe);
    capability.streaming = true;
    base.register_provider_ports(
        ProviderDescription::new("streams", [capability], vec![]).unwrap(),
        Arc::new(Open),
        Some(Arc::new(Open)),
    )
    .unwrap();
    let (handle, task) = session::spawn(
        base,
        RecordingMode::Ephemeral,
        no_files(),
        NonZeroUsize::new(2).unwrap(),
    )
    .unwrap();
    let record = sequence(
        &handle,
        "stream",
        "streams watch > live\ncatalog echo value:never",
    )
    .await;
    assert_eq!(
        finish(&handle, &record.cell).await,
        SequentialState::Interrupted
    );
    assert_eq!(calls.load(Ordering::SeqCst), 0);
    assert_eq!(handle.observe().await.unwrap().cells.len(), 1);
    stop(handle, task).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn blocked_step_admission_keeps_request_reads_and_cancellation_bounded() {
    use wes_engine::history::{RequestClaim, RequestRecord};
    struct Sink {
        records: Arc<Mutex<Vec<Record>>>,
        gate: Option<Gate>,
    }
    impl JournalSink for Sink {
        fn claim_request(
            &mut self,
            record: RequestRecord,
            _: bool,
        ) -> Result<RequestClaim, RecordError> {
            self.append(&Record::Journal(JournalEntry::Requested(record.clone())))?;
            Ok(RequestClaim {
                record,
                fresh: true,
                persistence: Persistence::FileSynced,
            })
        }
        fn append(&mut self, record: &Record) -> Result<AppendReceipt, RecordError> {
            if matches!(record, Record::Journal(JournalEntry::Command(_)))
                && let Some((entered, released)) = self.gate.take()
            {
                entered.send(()).unwrap();
                released.recv_timeout(Duration::from_secs(5)).unwrap();
            }
            self.records.lock().unwrap().push(record.clone());
            Ok(AppendReceipt {
                persistence: Persistence::FileSynced,
                end_offset: 1,
            })
        }
    }
    let (entered, blocked) = oneshot::channel();
    let (release, released) = mpsc::channel();
    let (recorder, writer) = spawn_recorder(
        Sink {
            records: Arc::default(),
            gate: Some((entered, released)),
        },
        RecorderLimits::default(),
    )
    .unwrap();
    let provider_gate = Arc::new(Notify::new());
    let (started, running) = oneshot::channel();
    let (base, calls) = workspace(Some(provider_gate.clone()), Some(started));
    let (handle, task) = session::spawn(
        base,
        RecordingMode::Required(CallJournal::new(
            recorder.clone(),
            RequiredPersistence::FileSynced,
        )),
        no_files(),
        NonZeroUsize::new(2).unwrap(),
    )
    .unwrap();
    let record = sequence(
        &handle,
        "blocked",
        "catalog echo value:blocked\ncatalog echo value:never",
    )
    .await;
    blocked.await.unwrap();
    assert_eq!(
        tokio::time::timeout(
            Duration::from_secs(1),
            handle.find_request("pane", "blocked")
        )
        .await
        .unwrap()
        .unwrap(),
        Some(record.clone())
    );
    assert!(matches!(
        tokio::time::timeout(
            Duration::from_secs(1),
            handle.cancel_request_work("owner".into(), &record)
        )
        .await
        .unwrap(),
        Err(SessionError::AdmissionBusy)
    ));
    release.send(()).unwrap();
    running.await.unwrap();
    handle
        .cancel_request_work("owner".into(), &record)
        .await
        .unwrap();
    assert_eq!(
        finish(&handle, &record.cell).await,
        SequentialState::Cancelled
    );
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    provider_gate.notify_waiters();
    stop(handle, task).await;
    recorder.shutdown().await.unwrap();
    writer.join().await.unwrap();
}
