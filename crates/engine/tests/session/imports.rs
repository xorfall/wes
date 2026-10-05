use super::*;
#[path = "../support/imports.rs"]
mod support;
use support::Fixture;

fn registered(fixture: Arc<Fixture>) -> Workspace {
    let (mut workspace, _) = workspace(None, None);
    workspace
        .register_importer("fixture".into(), fixture)
        .unwrap();
    workspace
}

#[tokio::test]
async fn import_installation_waits_for_required_recording_and_failure_never_executes() {
    for fail in [false, true] {
        let fixture = Arc::new(Fixture::default());
        let (entered, blocked) = oneshot::channel();
        let (release, released) = mpsc::channel();
        let records = Arc::new(Mutex::new(vec![]));
        let (recorder, writer) = spawn_recorder(
            Sink {
                records: records.clone(),
                block_cell: Some("import".into()),
                gate: Some((entered, released)),
                fail,
            },
            RecorderLimits::default(),
        )
        .unwrap();
        let (handle, task) = session::spawn(
            registered(fixture.clone()),
            RecordingMode::Required(CallJournal::new(
                recorder.clone(),
                RequiredPersistence::FileSynced,
            )),
            no_files(),
            NonZeroUsize::new(1).unwrap(),
        )
        .unwrap();
        let h = handle.clone();
        let pending = tokio::spawn(async move {
            h.submit(input(
                "import",
                ":import fixture file:original as:library\nlibrary get > output",
            ))
            .await
        });
        tokio::time::timeout(Duration::from_secs(5), blocked)
            .await
            .unwrap()
            .unwrap();
        let snapshot = handle.snapshot().await.unwrap();
        assert!(snapshot.admission_pending);
        assert!(snapshot.execution.graph.is_empty());
        assert!(snapshot.names.is_empty());
        assert_eq!(fixture.captures.load(Ordering::SeqCst), 1);
        assert_eq!(fixture.calls.load(Ordering::SeqCst), 0);
        assert!(!pending.is_finished());
        release.send(()).unwrap();
        let result = pending.await.unwrap();
        if fail {
            assert!(result.is_err());
            let snapshot = handle.snapshot().await.unwrap();
            assert!(snapshot.execution.graph.is_empty());
            assert!(snapshot.names.is_empty());
            assert_eq!(fixture.calls.load(Ordering::SeqCst), 0);
        } else {
            let result = result.unwrap();
            assert_eq!(result.accepted.len(), 2);
            handle.wait_idle().await.unwrap();
            let snapshot = handle.snapshot().await.unwrap();
            assert_eq!(
                snapshot.execution.values[&snapshot.names["output"].node].data(),
                &Data::Text("original".into())
            );
            assert_eq!(fixture.calls.load(Ordering::SeqCst), 1);
        }
        {
            let records = records.lock().unwrap();
            let saved = records
                .iter()
                .find_map(|record| match record {
                    Record::Journal(JournalEntry::Command(command)) if command.cell == "import" => {
                        Some(command)
                    }
                    _ => None,
                })
                .unwrap();
            assert_eq!(saved.imports.len(), 1);
            assert_eq!(saved.imports[0].recipe().source(), "original");
            assert_eq!(
                records
                    .iter()
                    .filter(|record| matches!(record, Record::Recovery(RecoveryEntry::Calling(_))))
                    .count(),
                usize::from(!fail)
            );
        }
        stop(handle, task).await;
        recorder.shutdown().await.unwrap();
        writer.join().await.unwrap();
    }
}

#[tokio::test]
async fn closing_session_joins_import_capture_and_discards_its_late_product() {
    let fixture = Arc::new(Fixture::default());
    let (entered, blocked) = oneshot::channel();
    let (release, released) = mpsc::channel();
    *fixture.gate.lock().unwrap() = Some((entered, released));
    let (handle, task) = session::spawn(
        registered(fixture.clone()),
        RecordingMode::Ephemeral,
        no_files(),
        NonZeroUsize::new(1).unwrap(),
    )
    .unwrap();
    let h = handle.clone();
    let pending = tokio::spawn(async move {
        h.submit(input(
            "import",
            ":import fixture file:late as:library\nlibrary get > output",
        ))
        .await
    });
    tokio::time::timeout(Duration::from_secs(5), blocked)
        .await
        .unwrap()
        .unwrap();
    handle.shutdown().await.unwrap();
    let joined = tokio::spawn(task.join());
    tokio::task::yield_now().await;
    assert!(!joined.is_finished());
    assert!(!pending.is_finished());
    let snapshot = handle.snapshot().await.unwrap();
    assert!(snapshot.execution.closed);
    assert!(snapshot.execution.graph.is_empty());
    release.send(()).unwrap();
    assert!(matches!(pending.await.unwrap(), Err(SessionError::Stopped)));
    joined.await.unwrap().unwrap();
    assert_eq!(fixture.captures.load(Ordering::SeqCst), 1);
    assert!(fixture.modes.lock().unwrap().is_empty());
    assert_eq!(fixture.calls.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn disconnected_import_submission_keeps_its_identity_and_is_not_read_twice() {
    let fixture = Arc::new(Fixture::default());
    let (entered, blocked) = oneshot::channel();
    let (release, released) = mpsc::channel();
    *fixture.gate.lock().unwrap() = Some((entered, released));
    let (handle, task) = session::spawn(
        registered(fixture.clone()),
        RecordingMode::Ephemeral,
        no_files(),
        NonZeroUsize::new(1).unwrap(),
    )
    .unwrap();
    let text = ":import fixture file:original as:library\nlibrary get > output";
    let h = handle.clone();
    let pending = tokio::spawn(async move { h.submit(input("import", text)).await });
    tokio::time::timeout(Duration::from_secs(5), blocked)
        .await
        .unwrap()
        .unwrap();
    pending.abort();
    assert!(pending.await.unwrap_err().is_cancelled());
    release.send(()).unwrap();
    let reply = submit(&handle, "import", text).await;
    assert_eq!(reply.accepted.len(), 2);
    assert!(Arc::ptr_eq(&reply, &submit(&handle, "import", text).await));
    handle.wait_idle().await.unwrap();
    assert_eq!(fixture.captures.load(Ordering::SeqCst), 1);
    assert_eq!(fixture.calls.load(Ordering::SeqCst), 1);
    stop(handle, task).await;
}

#[tokio::test]
async fn recorded_session_rebuilds_imports_inertly_and_refresh_uses_captured_source() {
    use wes_engine::{
        history::{HistoryCapture, HistoryCaptureLimits, HistoryCheckpoint},
        imports::ImportMode,
    };
    let fixture = Arc::new(Fixture::default());
    let records = Arc::new(Mutex::new(vec![]));
    let (recorder, writer) = spawn_recorder(
        Sink {
            records: records.clone(),
            block_cell: None,
            gate: None,
            fail: false,
        },
        RecorderLimits::default(),
    )
    .unwrap();
    let (handle, task) = session::spawn(
        registered(fixture.clone()),
        RecordingMode::Required(CallJournal::new(
            recorder.clone(),
            RequiredPersistence::FileSynced,
        )),
        no_files(),
        NonZeroUsize::new(1).unwrap(),
    )
    .unwrap();
    let text = ":import fixture file:original as:library\nlibrary get > output";
    let original = submit(&handle, "import", text).await;
    handle.wait_idle().await.unwrap();
    stop(handle, task).await;
    recorder.shutdown().await.unwrap();
    writer.join().await.unwrap();
    let mut capture = HistoryCapture::new(HistoryCaptureLimits::default());
    for record in records.lock().unwrap().iter() {
        capture.push(record.clone()).unwrap();
    }
    let checkpoint = AppendReceipt {
        persistence: Persistence::FileSynced,
        end_offset: 100,
    };
    let image = capture.finish(HistoryCheckpoint {
        journal: checkpoint,
        recovery: checkpoint,
    });
    let replay_fixture = Arc::new(Fixture::default());
    let restored = session::restore(
        registered(replay_fixture.clone()),
        RecordingMode::Ephemeral,
        None,
        image,
        CancellationToken::new(),
    )
    .await
    .unwrap();
    assert_eq!(restored.report().commands, 1);
    assert!(
        restored
            .workspace()
            .catalogue()
            .provider("library")
            .is_some()
    );
    assert_eq!(replay_fixture.captures.load(Ordering::SeqCst), 0);
    assert_eq!(*replay_fixture.modes.lock().unwrap(), [ImportMode::Replay]);
    let (handle, task) = restored
        .spawn(no_files(), NonZeroUsize::new(1).unwrap())
        .unwrap();
    handle.wait_idle().await.unwrap();
    assert_eq!(replay_fixture.calls.load(Ordering::SeqCst), 0);
    let restored_reply = submit(&handle, "import", text).await;
    assert!(restored_reply.restored);
    assert_eq!(restored_reply.nodes, original.nodes);
    assert_eq!(replay_fixture.captures.load(Ordering::SeqCst), 0);
    submit(&handle, "refresh", ":refresh $output").await;
    handle.wait_idle().await.unwrap();
    assert_eq!(replay_fixture.calls.load(Ordering::SeqCst), 1);
    let snapshot = handle.snapshot().await.unwrap();
    assert_eq!(
        snapshot.execution.values[&snapshot.names["output"].node].data(),
        &Data::Text("original".into())
    );
    stop(handle, task).await;
}

#[tokio::test]
async fn import_plan_freezes_fields_reads_once_and_records_applied_evidence() {
    let fixture = Arc::new(Fixture::default());
    let records = Arc::new(Mutex::new(vec![]));
    let (recorder, writer) = spawn_recorder(
        Sink {
            records: records.clone(),
            block_cell: None,
            gate: None,
            fail: false,
        },
        RecorderLimits::default(),
    )
    .unwrap();
    let (handle, task) = session::spawn(
        registered(fixture.clone()),
        RecordingMode::Required(CallJournal::new(
            recorder.clone(),
            RequiredPersistence::FileSynced,
        )),
        no_files(),
        NonZeroUsize::new(2).unwrap(),
    )
    .unwrap();
    let reply = submit(
        &handle,
        "config",
        r#":calc { return {path:"original"}; } > cfg"#,
    )
    .await;
    assert_eq!(reply.accepted.len(), 1, "{:?}", reply.diagnostics);
    handle.wait_idle().await.unwrap();
    let planned = submit(
        &handle,
        "plan",
        ":import plan fixture file:$cfg.path as:library > p",
    )
    .await;
    assert_eq!(planned.accepted.len(), 1, "{:?}", planned.diagnostics);
    handle.wait_idle().await.unwrap();
    let snapshot = handle.snapshot().await.unwrap();
    let plan = &snapshot.execution.values[&snapshot.names["p"].node];
    assert_eq!(plan.shape(), &Shape::Meta(wes_core::MetaType::ImportPlan));
    let Data::Record(details) = plan.data() else {
        panic!("plan projection");
    };
    assert_eq!(
        details.get("contentsReadWhenPlanned"),
        Some(&Data::Bool(false))
    );
    assert!(plan.management_authority().is_some());
    assert_eq!(fixture.captures.load(Ordering::SeqCst), 0);
    assert_eq!(fixture.calls.load(Ordering::SeqCst), 0);
    let applied = submit(&handle, "apply", ":import apply $p").await;
    assert_eq!(applied.accepted.len(), 1, "{:?}", applied.diagnostics);
    assert!(Arc::ptr_eq(
        &applied,
        &submit(&handle, "apply", ":import apply $p").await
    ));
    assert!(
        handle
            .submit(input("again", ":import apply $p"))
            .await
            .is_err()
    );
    assert_eq!(fixture.captures.load(Ordering::SeqCst), 1);
    assert_eq!(fixture.calls.load(Ordering::SeqCst), 0);
    let saved = records
        .lock()
        .unwrap()
        .iter()
        .find_map(|record| match record {
            Record::Journal(JournalEntry::Command(command)) if command.cell == "apply" => {
                Some(command.clone())
            }
            _ => None,
        })
        .unwrap();
    assert_eq!(saved.text, ":import apply $p");
    assert_eq!(saved.replay, saved.text);
    assert_eq!(
        saved.imports[0].origin(),
        wes_engine::imports::ImportOrigin::Applied
    );
    let fresh = Arc::new(Fixture::default());
    let mut replay =
        wes_engine::workspace::ReplayWorkspace::new(registered(fresh.clone())).unwrap();
    let prepared = replay
        .prepare(&saved, CancellationToken::new())
        .await
        .unwrap();
    assert!(replay.apply(prepared).unwrap().effects.is_empty());
    assert!(replay.workspace().catalogue().provider("library").is_some());
    assert_eq!(fresh.captures.load(Ordering::SeqCst), 0);
    assert_eq!(fresh.calls.load(Ordering::SeqCst), 0);
    stop(handle, task).await;
    recorder.shutdown().await.unwrap();
    writer.join().await.unwrap();
}

#[tokio::test]
async fn refreshed_arguments_and_other_principals_refuse_apply_without_content_reads() {
    let fixture = Arc::new(Fixture::default());
    let (handle, task) = session::spawn(
        registered(fixture.clone()),
        RecordingMode::Ephemeral,
        no_files(),
        NonZeroUsize::new(2).unwrap(),
    )
    .unwrap();
    submit(&handle, "config", r#":calc { return "original"; } > path"#).await;
    handle.wait_idle().await.unwrap();
    submit(
        &handle,
        "plan",
        ":import plan fixture file:$path as:library > p",
    )
    .await;
    handle.wait_idle().await.unwrap();
    let before = handle.snapshot().await.unwrap();
    assert!(
        before.execution.values[&before.names["p"].node]
            .management_authority()
            .is_some()
    );
    let other = input("other", ":import apply $p")
        .with_client("other".into())
        .unwrap();
    assert!(handle.submit(other).await.is_err());
    assert_eq!(fixture.captures.load(Ordering::SeqCst), 0);
    submit(&handle, "refresh", ":refresh $path").await;
    handle.wait_idle().await.unwrap();
    assert!(
        handle
            .submit(input("changed", ":import apply $p"))
            .await
            .is_err()
    );
    assert_eq!(fixture.captures.load(Ordering::SeqCst), 0);
    stop(handle, task).await;
}

#[tokio::test]
async fn failed_apply_consumes_authority_without_automatic_replan() {
    let fixture = Arc::new(Fixture::default());
    let (handle, task) = session::spawn(
        registered(fixture.clone()),
        RecordingMode::Ephemeral,
        no_files(),
        NonZeroUsize::new(1).unwrap(),
    )
    .unwrap();
    submit(
        &handle,
        "plan",
        ":import plan fixture file:bad as:library > p",
    )
    .await;
    handle.wait_idle().await.unwrap();
    let failed = submit(&handle, "apply", ":import apply $p").await;
    assert_eq!(failed.accepted.len(), 0);
    assert_eq!(fixture.captures.load(Ordering::SeqCst), 1);
    assert!(
        handle
            .submit(input("again", ":import apply $p"))
            .await
            .is_err()
    );
    assert_eq!(fixture.captures.load(Ordering::SeqCst), 1);
    assert_eq!(fixture.calls.load(Ordering::SeqCst), 0);
    stop(handle, task).await;
}

#[tokio::test]
async fn late_import_capture_cannot_commit_after_input_refresh_or_cancel() {
    for cancel in [false, true] {
        let fixture = Arc::new(Fixture::default());
        let records = Arc::new(Mutex::new(vec![]));
        let (recorder, writer) = spawn_recorder(
            Sink {
                records: records.clone(),
                block_cell: None,
                gate: None,
                fail: false,
            },
            RecorderLimits::default(),
        )
        .unwrap();
        let (handle, task) = session::spawn(
            registered(fixture.clone()),
            RecordingMode::Required(CallJournal::new(
                recorder.clone(),
                RequiredPersistence::FileSynced,
            )),
            no_files(),
            NonZeroUsize::new(2).unwrap(),
        )
        .unwrap();
        submit(&handle, "config", r#":calc { return "original"; } > path"#).await;
        handle.wait_idle().await.unwrap();
        submit(
            &handle,
            "plan",
            ":import plan fixture file:$path as:library > p",
        )
        .await;
        handle.wait_idle().await.unwrap();
        let (entered, blocked) = oneshot::channel();
        let (release, released) = mpsc::channel();
        *fixture.gate.lock().unwrap() = Some((entered, released));
        let h = handle.clone();
        let pending =
            tokio::spawn(async move { h.submit(input("apply", ":import apply $p")).await });
        tokio::time::timeout(Duration::from_secs(5), blocked)
            .await
            .unwrap()
            .unwrap();
        if cancel {
            handle.cancel_work("apply".into()).await.unwrap();
        } else {
            submit(&handle, "refresh", ":refresh $path").await;
            submit(&handle, "ready", ":wait $path").await;
        }
        tokio::task::yield_now().await;
        assert!(
            !pending.is_finished(),
            "The entered content reader must be joined"
        );
        release.send(()).unwrap();
        let failure = pending.await.unwrap().unwrap_err();
        if cancel {
            assert!(matches!(failure, SessionError::Cancelled));
        } else {
            assert!(
                failure.to_string().contains("new producing run"),
                "{failure}"
            );
        }
        handle.wait_idle().await.unwrap();
        assert!(
            handle
                .observe()
                .await
                .unwrap()
                .catalogue
                .provider("library")
                .is_none()
        );
        assert!(
            records
                .lock()
                .unwrap()
                .iter()
                .all(|r| !matches!(r,Record::Journal(JournalEntry::Command(c)) if c.cell=="apply"))
        );
        assert!(
            handle
                .submit(input("again", ":import apply $p"))
                .await
                .is_err()
        );
        assert_eq!(fixture.captures.load(Ordering::SeqCst), 1);
        assert_eq!(fixture.calls.load(Ordering::SeqCst), 0);
        stop(handle, task).await;
        recorder.shutdown().await.unwrap();
        writer.join().await.unwrap();
    }
}
