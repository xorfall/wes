use super::*;
use indexmap::IndexMap;
use std::collections::HashMap;
use wes_core::{Provenance, Timestamp, Value};
use wes_engine::{
    graph::NodeId,
    history::{
        CallRecord, CommandRecord, DiagnosticRecord, ExecutionRecord, HistoryCapture,
        HistoryCaptureLimits, HistoryCheckpoint, HistoryImage, RetainedResult,
    },
    runtime::{RunId, RuntimeCode},
    session::{RestoreError, RestoreProblemKind, SessionStorage, ValuePublication},
    storage::{
        AutoKeep, LoadedValue, StoreError, StoreWorkerLimits, ValueHandle, ValueStore, spawn_store,
    },
};
use wes_language::{Diagnostic, Span};

fn id(value: &str) -> NodeId {
    NodeId::new(value).unwrap()
}
fn run(value: &str) -> RunId {
    RunId::new(value).unwrap()
}
fn now() -> Timestamp {
    Timestamp::new(0, 0).unwrap()
}
fn command(cell: &str, text: &str, nodes: &[&str]) -> JournalEntry {
    JournalEntry::Command(CommandRecord {
        source_name: format!("cell {cell}"),
        source_start: wes_language::Position { line: 1, column: 1 },
        changed_nodes: vec![],
        document: None,
        revision_of: None,
        environments: None,
        cell: cell.into(),
        text: text.into(),
        replay: text.into(),
        nodes: nodes.iter().map(|node| id(node)).collect(),
        type_sources: IndexMap::new(),
        calculation_package: None,
        imports: vec![],
    })
}
fn changed_command(cell: &str, text: &str, target: &str) -> JournalEntry {
    let JournalEntry::Command(mut record) = command(cell, text, &[]) else {
        unreachable!()
    };
    record.changed_nodes = vec![id(target)];
    JournalEntry::Command(record)
}
fn observed(event: &str, node: &str, attempt: Option<&str>, state: NodeState) -> JournalEntry {
    JournalEntry::Observed(
        ExecutionRecord::new(event.into(), id(node), attempt.map(run), now(), state, None).unwrap(),
    )
}
fn image(
    entries: Vec<JournalEntry>,
    recovery: Vec<RecoveryEntry>,
    persistence: Persistence,
) -> HistoryImage {
    let mut capture = HistoryCapture::new(HistoryCaptureLimits::default());
    for entry in entries {
        capture.push(Record::Journal(entry)).unwrap();
    }
    for entry in recovery {
        capture.push(Record::Recovery(entry)).unwrap();
    }
    capture.finish(HistoryCheckpoint {
        journal: AppendReceipt {
            persistence,
            end_offset: 100,
        },
        recovery: AppendReceipt {
            persistence,
            end_offset: 200,
        },
    })
}
fn sink() -> (
    wes_engine::recording::Recorder,
    wes_engine::recording::RecorderTask,
    Arc<Mutex<Vec<Record>>>,
) {
    let records = Arc::new(Mutex::new(vec![]));
    let (recorder, task) = spawn_recorder(
        LogSink {
            records: records.clone(),
            gate: None,
            fail_observation: false,
        },
        RecorderLimits::default(),
    )
    .unwrap();
    (recorder, task, records)
}

#[tokio::test]
async fn downstream_refresh_uses_call_recording_but_never_replays_execution_intent() {
    let (base, calls) = workspace(None, None);
    let (recorder, writer, records) = sink();
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
    for (cell, source) in [
        ("root", "catalog echo value:hello > root"),
        ("child", "catalog echo value:$root > child"),
    ] {
        submit(&handle, cell, source).await;
        handle.wait_idle().await.unwrap();
    }
    let request = submit(&handle, "refresh", ":refresh $root scope:downstream").await;
    assert_eq!(request.accepted.len(), 1);
    assert!(!request.recorded);
    handle.wait_idle().await.unwrap();
    assert_eq!(calls.load(Ordering::SeqCst), 4);
    stop(handle, task).await;
    recorder.shutdown().await.unwrap();
    writer.join().await.unwrap();
    let records = records.lock().unwrap().clone();
    assert!(!records.iter().any(|record| matches!(record, Record::Journal(JournalEntry::Command(command)) if command.cell == "refresh")));
    assert_eq!(
        records
            .iter()
            .filter(|record| matches!(record, Record::Recovery(RecoveryEntry::Calling(_))))
            .count(),
        4
    );
    let journal = records
        .iter()
        .filter_map(|record| match record {
            Record::Journal(entry) => Some(entry.clone()),
            _ => None,
        })
        .collect();
    let recovery = records
        .iter()
        .filter_map(|record| match record {
            Record::Recovery(entry) => Some(entry.clone()),
            _ => None,
        })
        .collect();
    let (base, restored_calls) = workspace(None, None);
    let restored = session::restore(
        base,
        RecordingMode::Ephemeral,
        None,
        image(journal, recovery, Persistence::FileSynced),
        CancellationToken::new(),
    )
    .await
    .unwrap();
    let (handle, task) = restored
        .spawn(no_files(), NonZeroUsize::new(1).unwrap())
        .unwrap();
    handle.wait_idle().await.unwrap();
    assert_eq!(restored_calls.load(Ordering::SeqCst), 0);
    assert_eq!(handle.snapshot().await.unwrap().execution.graph.len(), 2);
    stop(handle, task).await;
}

#[tokio::test]
async fn restored_pipeline_requires_explicit_intent_and_preserves_definition_drift_checks() {
    let source = "catalog echo value:hello | catalog echo value:input";
    let (base, calls) = workspace(None, None);
    let restored = session::restore(
        base,
        RecordingMode::Ephemeral,
        None,
        image(
            vec![command("original", source, &["id1", "id2"])],
            vec![],
            Persistence::FileSynced,
        ),
        CancellationToken::new(),
    )
    .await
    .unwrap();
    let (handle, task) = restored
        .spawn(no_files(), NonZeroUsize::new(1).unwrap())
        .unwrap();
    handle.wait_idle().await.unwrap();
    assert_eq!(calls.load(Ordering::SeqCst), 0);
    let result = handle
        .submit(
            input("again", source)
                .with_repeat("original".into(), true)
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(result.nodes, vec![id("id1"), id("id2")]);
    handle.wait_idle().await.unwrap();
    assert_eq!(calls.load(Ordering::SeqCst), 2);
    stop(handle, task).await;

    let (base, calls) = workspace(None, None);
    let restored = session::restore(
        base,
        RecordingMode::Ephemeral,
        None,
        image(
            vec![
                command("original", "catalog echo value:hello > answer", &["id1"]),
                changed_command("changed", ":change $answer value:other", "id1"),
            ],
            vec![],
            Persistence::FileSynced,
        ),
        CancellationToken::new(),
    )
    .await
    .unwrap();
    let (handle, task) = restored
        .spawn(no_files(), NonZeroUsize::new(1).unwrap())
        .unwrap();
    assert!(matches!(
        handle
            .submit(
                input("again", "catalog echo value:hello > answer")
                    .with_repeat("original".into(), true)
                    .unwrap()
            )
            .await,
        Err(SessionError::RepeatRefused(_))
    ));
    assert_eq!(calls.load(Ordering::SeqCst), 0);
    stop(handle, task).await;
}

#[tokio::test]
async fn restored_diagnostic_cells_keep_journal_order_instead_of_burying_recent_commands() {
    let (base, calls) = workspace(None, None);
    let diagnostic = JournalEntry::Diagnosed(
        DiagnosticRecord::new(
            "old-error".into(),
            now(),
            "first".into(),
            ":env use \"missing\"".into(),
            Diagnostic::error("ENV001", Span::at(0), "original environment diagnostic"),
        )
        .unwrap(),
    );
    let restored = session::restore(
        base,
        RecordingMode::Ephemeral,
        None,
        image(
            vec![
                diagnostic,
                JournalEntry::Diagnosed(
                    DiagnosticRecord::new(
                        "new-error".into(),
                        now(),
                        "second".into(),
                        "secret".into(),
                        Diagnostic::error("FUTURE000", Span::new(0, 6).unwrap(), "private detail")
                            .with_public_message("Unknown environment name."),
                    )
                    .unwrap(),
                ),
                command("latest", "catalog echo value:new > testData", &["id1"]),
            ],
            vec![],
            Persistence::FileSynced,
        ),
        CancellationToken::new(),
    )
    .await
    .unwrap();
    let (handle, task) = restored
        .spawn(no_files(), NonZeroUsize::new(1).unwrap())
        .unwrap();
    let observed = handle.observe().await.unwrap();
    assert_eq!(
        observed
            .cells
            .iter()
            .map(|c| c.input.cell())
            .collect::<Vec<_>>(),
        ["first", "second", "latest"]
    );
    assert_eq!(
        observed.cells[0]
            .reply
            .as_ref()
            .unwrap()
            .as_ref()
            .unwrap()
            .diagnostics
            .diagnostics[0]
            .message,
        "original environment diagnostic"
    );
    assert_eq!(
        observed.cells[2]
            .reply
            .as_ref()
            .unwrap()
            .as_ref()
            .unwrap()
            .nodes,
        [id("id1")]
    );
    assert_eq!(
        observed.cells[1]
            .reply
            .as_ref()
            .unwrap()
            .as_ref()
            .unwrap()
            .diagnostics
            .diagnostics[0]
            .public_summary(),
        "Unknown environment name."
    );
    assert_eq!(calls.load(Ordering::SeqCst), 0);
    stop(handle, task).await;
}

#[tokio::test]
async fn restart_preserves_failure_and_log_ids_deduplicates_source_and_only_explicit_refresh_executes()
 {
    let (base, calls) = workspace(None, None);
    let (recorder, writer, records) = sink();
    let text = "catalog echo value:old > answer *> failure\ncatalog echo value:$failure > handler";
    let error = RuntimeCode::ExecutionFailed.error("original failure", None);
    let failed = JournalEntry::Observed(
        ExecutionRecord::new(
            "original-event".into(),
            id("id9000"),
            Some(run("old-run")),
            now(),
            NodeState::Failed,
            Some(error.clone()),
        )
        .unwrap(),
    );
    let diagnostic = JournalEntry::Diagnosed(
        DiagnosticRecord::new(
            "original-diagnostic".into(),
            now(),
            "old-cell".into(),
            text.into(),
            Diagnostic::error("TEST001", Span::at(0), "historical diagnostic"),
        )
        .unwrap(),
    );
    let pending = CallRecord {
        node: id("id9000"),
        run: run("uncertain-run"),
        cell: "old-cell".into(),
        capability: "echo".into(),
        safe: true,
        at: now(),
    };
    let restored = session::restore(
        base,
        RecordingMode::Required(CallJournal::new(
            recorder.clone(),
            RequiredPersistence::FileSynced,
        )),
        None,
        image(
            vec![
                command("old-cell", text, &["id9000", "id9001"]),
                failed,
                diagnostic,
            ],
            vec![
                RecoveryEntry::Accepted {
                    cell: "old-cell".into(),
                },
                RecoveryEntry::Calling(pending.clone()),
                RecoveryEntry::Called {
                    node: id("id9000"),
                    run: run("different-run"),
                    produced: false,
                },
            ],
            Persistence::FileSynced,
        ),
        CancellationToken::new(),
    )
    .await
    .unwrap();
    assert_eq!(restored.report().commands, 1);
    assert_eq!(restored.report().interrupted, vec![pending]);
    assert_eq!(
        restored.workspace().runtime().error_of(&id("id9000")),
        Some(&error)
    );
    assert!(records.lock().unwrap().is_empty());
    let (handle, task) = restored
        .spawn(no_files(), NonZeroUsize::new(2).unwrap())
        .unwrap();
    handle.wait_idle().await.unwrap();
    assert_eq!(calls.load(Ordering::SeqCst), 0);
    let log = handle.log().await.unwrap();
    assert_eq!(
        log.entries
            .iter()
            .map(|entry| entry.id())
            .collect::<Vec<_>>(),
        ["original-event", "original-diagnostic"]
    );
    assert!(
        log.entries
            .iter()
            .all(|entry| entry.durable() && matches!(entry.status(), LogStatus::Recovered(_)))
    );
    let reply = submit(&handle, "old-cell", text).await;
    assert!(reply.restored);
    assert!(reply.accepted.is_empty()); // Do not invent the original transient span/report data.
    assert!(Arc::ptr_eq(
        &reply,
        &submit(&handle, "old-cell", text).await
    ));
    assert!(matches!(
        handle.submit(input("old-cell", "different")).await,
        Err(SessionError::Conflict)
    ));
    assert!(records.lock().unwrap().is_empty());
    submit(&handle, "new-cell", "catalog echo value:new > fresh").await;
    handle.wait_idle().await.unwrap();
    assert_eq!(calls.load(Ordering::SeqCst), 1); // The old failure handler stays held.
    submit(&handle, "refresh-cell", ":refresh $answer").await;
    handle.wait_idle().await.unwrap();
    assert_eq!(calls.load(Ordering::SeqCst), 2);
    let snapshot = records.lock().unwrap().clone();
    assert!(snapshot.iter().any(|entry| matches!(entry, Record::Recovery(RecoveryEntry::Calling(call)) if call.cell == "old-cell" && call.node == id("id9000") && call.run != run("old-run"))));
    assert_eq!(snapshot.iter().filter(|entry| matches!(entry, Record::Recovery(RecoveryEntry::Accepted { cell }) if cell == "old-cell")).count(), 0);
    stop(handle, task).await;
    recorder.shutdown().await.unwrap();
    writer.join().await.unwrap();
}

#[tokio::test]
async fn ephemeral_restore_rejects_unidentified_commands_without_execution() {
    for cell in [
        "".to_owned(),
        " ".to_owned(),
        "bad\ncell".to_owned(),
        "x".repeat(257),
    ] {
        let (base, calls) = workspace(None, None);
        let restored = session::restore(
            base,
            RecordingMode::Ephemeral,
            None,
            image(
                vec![command(&cell, "catalog echo value:old > answer", &["id80"])],
                vec![],
                Persistence::Volatile,
            ),
            CancellationToken::new(),
        )
        .await;
        assert!(matches!(restored, Err(RestoreError::Source { .. })));
        assert_eq!(calls.load(Ordering::SeqCst), 0);
    }
}

#[tokio::test]
async fn missing_accepted_is_acknowledged_only_on_explicit_execution_and_unidentified_origins_fail()
{
    for cell in ["known-cell", ""] {
        let (base, calls) = workspace(None, None);
        let (recorder, writer, records) = sink();
        let restored = session::restore(
            base,
            RecordingMode::Required(CallJournal::new(
                recorder.clone(),
                RequiredPersistence::FileSynced,
            )),
            None,
            image(
                vec![command(cell, "catalog echo value:old > answer", &["id80"])],
                vec![],
                Persistence::FileSynced,
            ),
            CancellationToken::new(),
        )
        .await;
        if cell.is_empty() {
            assert!(matches!(
                restored,
                Err(session::RestoreError::Source { .. })
            ));
            assert!(records.lock().unwrap().is_empty());
            assert_eq!(calls.load(Ordering::SeqCst), 0);
            recorder.shutdown().await.unwrap();
            writer.join().await.unwrap();
            continue;
        }
        let restored = restored.unwrap();
        let (handle, task) = restored
            .spawn(no_files(), NonZeroUsize::new(1).unwrap())
            .unwrap();
        handle.wait_idle().await.unwrap();
        assert!(records.lock().unwrap().is_empty());
        assert_eq!(calls.load(Ordering::SeqCst), 0);
        submit(&handle, "explicit", ":refresh $answer").await;
        handle.wait_idle().await.unwrap();
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        let snapshot = records.lock().unwrap().clone();
        let accepted = snapshot
            .iter()
            .position(|entry| matches!(entry, Record::Recovery(RecoveryEntry::Accepted { .. })))
            .unwrap();
        let calling = snapshot
            .iter()
            .position(|entry| matches!(entry, Record::Recovery(RecoveryEntry::Calling(_))))
            .unwrap();
        assert!(accepted < calling);
        let Record::Recovery(RecoveryEntry::Calling(call)) = &snapshot[calling] else {
            panic!()
        };
        assert_eq!(call.cell, cell);
        assert!(
            !snapshot
                .iter()
                .any(|entry| matches!(entry, Record::Journal(JournalEntry::Command(_))))
        );
        stop(handle, task).await;
        recorder.shutdown().await.unwrap();
        writer.join().await.unwrap();
    }
}

#[tokio::test]
async fn diagnostic_only_and_source_less_recovery_cells_are_retained_as_non_retryable_identities() {
    let (base, calls) = workspace(None, None);
    let text = "catalog echo value:not-an-authorized-retry > result";
    let diagnostic = JournalEntry::Diagnosed(
        DiagnosticRecord::new(
            "diagnostic".into(),
            now(),
            "diagnosed".into(),
            text.into(),
            Diagnostic::error("TEST001", Span::at(0), "old failure"),
        )
        .unwrap(),
    );
    let restored = session::restore(
        base,
        RecordingMode::Ephemeral,
        None,
        image(
            vec![diagnostic],
            vec![RecoveryEntry::Accepted {
                cell: "source-lost".into(),
            }],
            Persistence::FileSynced,
        ),
        CancellationToken::new(),
    )
    .await
    .unwrap();
    assert_eq!(restored.report().unidentified_cells, 1);
    let (handle, task) = restored
        .spawn(no_files(), NonZeroUsize::new(1).unwrap())
        .unwrap();
    let reply = handle.submit(input("diagnosed", text)).await.unwrap();
    assert!(reply.restored);
    assert!(reply.accepted.is_empty());
    assert_eq!(reply.diagnostics.diagnostics[0].code, "TEST001");
    assert_eq!(reply.diagnostics.diagnostics[0].message, "old failure");
    assert!(matches!(
        handle.submit(input("source-lost", text)).await,
        Err(SessionError::HistoricalReplyUnavailable)
    ));
    assert!(matches!(
        handle.submit(input("diagnosed", "changed")).await,
        Err(SessionError::Conflict)
    ));
    assert_eq!(calls.load(Ordering::SeqCst), 0);
    stop(handle, task).await;
}

#[tokio::test]
async fn conflicting_or_drifted_structure_and_weak_checkpoints_never_start_or_append() {
    let text = "catalog echo value:old > answer";
    let cases = [
        vec![
            command("same", text, &["id80"]),
            command("same", "catalog echo value:changed > answer", &["id80"]),
        ],
        vec![
            command("first", text, &["id80"]),
            command("second", "catalog echo value:other > other", &["id80"]),
        ],
        vec![command(
            "drift",
            "catalog removed value:old > answer",
            &["id80"],
        )],
        vec![command("bad-count", text, &[])],
    ];
    for entries in cases {
        let (base, calls) = workspace(None, None);
        let (recorder, writer, records) = sink();
        let result = session::restore(
            base,
            RecordingMode::Required(CallJournal::new(
                recorder.clone(),
                RequiredPersistence::FileSynced,
            )),
            None,
            image(entries, vec![], Persistence::FileSynced),
            CancellationToken::new(),
        )
        .await;
        assert!(result.is_err());
        assert_eq!(calls.load(Ordering::SeqCst), 0);
        assert!(records.lock().unwrap().is_empty());
        recorder.shutdown().await.unwrap();
        writer.join().await.unwrap();
    }
    let (base, _) = workspace(None, None);
    let (recorder, writer, records) = sink();
    let result = session::restore(
        base,
        RecordingMode::Required(CallJournal::new(
            recorder.clone(),
            RequiredPersistence::FileSynced,
        )),
        None,
        image(vec![], vec![], Persistence::Volatile),
        CancellationToken::new(),
    )
    .await;
    assert!(matches!(result, Err(RestoreError::Recording(_))));
    assert!(records.lock().unwrap().is_empty());
    recorder.shutdown().await.unwrap();
    writer.join().await.unwrap();
}

struct ReadStore {
    values: HashMap<ValueHandle, LoadedValue>,
    kept: bool,
    fail: bool,
    reads: Arc<AtomicUsize>,
}
impl ValueStore for ReadStore {
    fn store(&mut self, _: &Value) -> Result<ValueHandle, StoreError> {
        panic!("startup must not publish")
    }
    fn read(&self, handle: &ValueHandle) -> Result<Option<LoadedValue>, StoreError> {
        self.reads.fetch_add(1, Ordering::SeqCst);
        if self.fail {
            return Err(StoreError::backend(
                "test",
                std::io::Error::other("private corrupt bytes"),
            ));
        }
        Ok(self.values.get(handle).cloned())
    }
    fn encoded(&self, _: &ValueHandle) -> Result<Option<Vec<u8>>, StoreError> {
        panic!("no encoding during startup")
    }
    fn size(&self, _: &ValueHandle) -> Result<Option<u64>, StoreError> {
        Ok(Some(123))
    }
    fn is_kept(&self, _: &ValueHandle) -> Result<bool, StoreError> {
        Ok(self.kept)
    }
    fn keep(&mut self, _: &ValueHandle) -> Result<bool, StoreError> {
        panic!("startup must not retain")
    }
    fn release(&mut self, _: &ValueHandle) -> Result<bool, StoreError> {
        panic!("startup must not delete")
    }
}
fn value() -> Value {
    Value::new(
        Shape::Unknown,
        Data::Text("retained 🦀".into()),
        Provenance::default().with_fact("source", "synthetic-history"),
    )
    .unwrap()
}

#[tokio::test]
async fn retained_values_preserve_run_identity_and_provenance_without_republication() {
    for attempt in [Some("old-run"), Some("second-run")] {
        let (base, calls) = workspace(None, None);
        let handle = ValueHandle::fresh();
        let reads = Arc::new(AtomicUsize::new(0));
        let (store, store_task) = spawn_store(
            ReadStore {
                values: [(handle.clone(), LoadedValue { value: value() })].into(),
                kept: true,
                fail: false,
                reads: reads.clone(),
            },
            StoreWorkerLimits::default(),
        )
        .unwrap();
        let restored = session::restore(
            base,
            RecordingMode::Ephemeral,
            Some(SessionStorage {
                worker: store.clone(),
                auto_keep: AutoKeep::default(),
            }),
            image(
                vec![
                    command("old-cell", "catalog echo value:old > answer", &["id80"]),
                    observed("ready", "id80", attempt, NodeState::Ready),
                    JournalEntry::Result(RetainedResult {
                        retention: wes_engine::storage::Retention::Unknown,
                        node: id("id80"),
                        handle: handle.clone(),
                        run: run(attempt.unwrap()),
                    }),
                ],
                vec![],
                Persistence::FileSynced,
            ),
            CancellationToken::new(),
        )
        .await
        .unwrap();
        assert_eq!(restored.report().values, 1);
        assert_eq!(
            restored.workspace().runtime().value_of(&id("id80")),
            Some(&value())
        );
        assert_eq!(
            restored.workspace().runtime().run_of(&id("id80")),
            attempt.map(run).as_ref()
        );
        let (session, task) = restored
            .spawn(no_files(), NonZeroUsize::new(1).unwrap())
            .unwrap();
        session.wait_idle().await.unwrap();
        let snapshot = session.values().await.unwrap().unwrap();
        let ValuePublication::Recovered(recovered) = &snapshot.outputs[&id("id80")] else {
            panic!()
        };
        assert_eq!(recovered.run, run(attempt.unwrap()));
        assert_eq!(recovered.handle, handle);
        assert_eq!(recovered.bytes, 123);

        assert_eq!(calls.load(Ordering::SeqCst), 0);
        assert_eq!(reads.load(Ordering::SeqCst), 1);
        stop(session, task).await;
        store.shutdown().await.unwrap();
        store_task.join().await.unwrap();
    }
}

#[tokio::test]
async fn missing_corrupt_live_only_and_superseded_results_never_recover_the_wrong_bytes() {
    for (kept, fail, latest, expected) in [
        (false, false, "old", Some(RestoreProblemKind::MissingValue)),
        (true, true, "old", Some(RestoreProblemKind::UnreadableValue)),
        (true, false, "new", None),
    ] {
        let (base, calls) = workspace(None, None);
        let handle = ValueHandle::fresh();
        let reads = Arc::new(AtomicUsize::new(0));
        let (store, store_task) = spawn_store(
            ReadStore {
                values: [(handle.clone(), LoadedValue { value: value() })].into(),
                kept,
                fail,
                reads: reads.clone(),
            },
            StoreWorkerLimits::default(),
        )
        .unwrap();
        let restored = session::restore(
            base,
            RecordingMode::Ephemeral,
            Some(SessionStorage {
                worker: store.clone(),
                auto_keep: AutoKeep::default(),
            }),
            image(
                vec![
                    command("old-cell", "catalog echo value:old > answer", &["id80"]),
                    observed("ready", "id80", Some(latest), NodeState::Ready),
                    JournalEntry::Result(RetainedResult {
                        retention: wes_engine::storage::Retention::Unknown,
                        node: id("id80"),
                        handle,
                        run: run("old"),
                    }),
                ],
                vec![],
                Persistence::FileSynced,
            ),
            CancellationToken::new(),
        )
        .await
        .unwrap();
        assert_eq!(
            restored
                .workspace()
                .runtime()
                .graph()
                .node(&id("id80"))
                .unwrap()
                .state(),
            NodeState::Stale
        );
        assert_eq!(
            restored
                .report()
                .problems
                .first()
                .map(|problem| problem.kind),
            expected
        );
        assert_eq!(
            restored.workspace().runtime().stale_reason(&id("id80")),
            Some(if expected.is_some() {
                wes_engine::runtime::StaleReason::RestoreUnavailable
            } else {
                wes_engine::runtime::StaleReason::RestoreNotRetained
            })
        );
        assert!(!format!("{:?}", restored.report()).contains("private corrupt bytes"));
        assert_eq!(
            reads.load(Ordering::SeqCst),
            usize::from(kept && latest == "old")
        );
        assert_eq!(calls.load(Ordering::SeqCst), 0);
        drop(restored);
        store.shutdown().await.unwrap();
        store_task.join().await.unwrap();
    }
}

#[tokio::test]
async fn later_change_invalidates_prior_and_delayed_old_runs_but_accepts_a_new_run() {
    for new_run in [false, true] {
        let (base, _) = workspace(None, None);
        let handle = ValueHandle::fresh();
        let reads = Arc::new(AtomicUsize::new(0));
        let (store, store_task) = spawn_store(
            ReadStore {
                values: [(handle.clone(), LoadedValue { value: value() })].into(),
                kept: true,
                fail: false,
                reads: reads.clone(),
            },
            StoreWorkerLimits::default(),
        )
        .unwrap();
        let mut entries = vec![
            command(
                "create",
                "catalog echo value:old > root\ncatalog echo value:$root > dependent",
                &["id80", "id81"],
            ),
            observed("old-start", "id80", Some("old"), NodeState::Running),
            observed("old-ready", "id80", Some("old"), NodeState::Ready),
            changed_command("change", ":change $root value:changed", "id80"),
            observed("delayed-ready", "id80", Some("old"), NodeState::Ready),
        ];
        if new_run {
            entries.extend([
                observed("new-start", "id80", Some("new"), NodeState::Running),
                observed("new-ready", "id80", Some("new"), NodeState::Ready),
            ]);
        }
        entries.push(JournalEntry::Result(RetainedResult {
            retention: wes_engine::storage::Retention::Unknown,
            node: id("id80"),
            handle,
            run: run(if new_run { "new" } else { "old" }),
        }));
        let restored = session::restore(
            base,
            RecordingMode::Ephemeral,
            Some(SessionStorage {
                worker: store.clone(),
                auto_keep: AutoKeep::Never,
            }),
            image(entries, vec![], Persistence::FileSynced),
            CancellationToken::new(),
        )
        .await
        .unwrap();
        assert_eq!(
            restored
                .workspace()
                .runtime()
                .graph()
                .node(&id("id80"))
                .unwrap()
                .state(),
            if new_run {
                NodeState::Ready
            } else {
                NodeState::Stale
            }
        );
        assert_eq!(reads.load(Ordering::SeqCst), usize::from(new_run));
        assert!(restored.report().unconfirmed_changes.contains(&id("id81")));
        assert_eq!(
            restored.workspace().runtime().stale_reason(&id("id81")),
            Some(wes_engine::runtime::StaleReason::RestoreChanged)
        );
        let message = restored
            .workspace()
            .runtime()
            .stale_reason(&id("id81"))
            .unwrap()
            .message();
        assert!(message.starts_with("A definition or dependency changed;"));
        assert!(message.contains("Reopening preserved this stale state"));
        assert_eq!(
            restored.report().unconfirmed_changes.contains(&id("id80")),
            !new_run
        );
        drop(restored);
        store.shutdown().await.unwrap();
        store_task.join().await.unwrap();
    }
}

#[tokio::test]
async fn identical_command_copies_are_inert_and_pre_cancelled_startup_returns_no_session() {
    let (base, calls) = workspace(None, None);
    let record = command("old", "catalog echo value:old > answer", &["id80"]);
    let restored = session::restore(
        base,
        RecordingMode::Ephemeral,
        None,
        image(
            vec![record.clone(), record.clone()],
            vec![],
            Persistence::FileSynced,
        ),
        CancellationToken::new(),
    )
    .await
    .unwrap();
    assert_eq!(restored.report().commands, 1);
    assert_eq!(restored.report().duplicate_commands, 1);
    assert_eq!(calls.load(Ordering::SeqCst), 0);
    let (base, _) = workspace(None, None);
    let token = CancellationToken::new();
    token.cancel();
    assert!(matches!(
        session::restore(
            base,
            RecordingMode::Ephemeral,
            None,
            image(vec![record], vec![], Persistence::FileSynced),
            token
        )
        .await,
        Err(RestoreError::Cancelled)
    ));
}

#[tokio::test]
async fn repeated_event_ids_cannot_reorder_history_or_replace_their_original_meaning() {
    let old = observed("old-event", "id80", Some("old-run"), NodeState::Running);
    let current = observed(
        "current-event",
        "id80",
        Some("current-run"),
        NodeState::Running,
    );
    let (base, _) = workspace(None, None);
    let restored = session::restore(
        base,
        RecordingMode::Ephemeral,
        None,
        image(
            vec![
                command("source", "catalog echo value:old > answer", &["id80"]),
                old.clone(),
                current.clone(),
                old.clone(),
            ],
            vec![],
            Persistence::FileSynced,
        ),
        CancellationToken::new(),
    )
    .await
    .unwrap();
    assert_eq!(restored.report().duplicate_logs, 1);
    assert_eq!(
        restored.workspace().runtime().run_of(&id("id80")),
        Some(&run("current-run"))
    );
    let (handle, task) = restored
        .spawn(no_files(), NonZeroUsize::new(1).unwrap())
        .unwrap();
    assert_eq!(
        handle
            .log()
            .await
            .unwrap()
            .entries
            .iter()
            .map(|entry| entry.id().to_owned())
            .collect::<Vec<_>>(),
        ["old-event", "current-event"]
    );
    stop(handle, task).await;
    let (base, _) = workspace(None, None);
    let changed = observed(
        "old-event",
        "id80",
        Some("different-run"),
        NodeState::Running,
    );
    assert!(matches!(
        session::restore(
            base,
            RecordingMode::Ephemeral,
            None,
            image(vec![old, changed], vec![], Persistence::FileSynced),
            CancellationToken::new()
        )
        .await,
        Err(RestoreError::ConflictingLog)
    ));
}

struct SlowRead {
    gate: Mutex<Option<Gate>>,
}
impl ValueStore for SlowRead {
    fn store(&mut self, _: &Value) -> Result<ValueHandle, StoreError> {
        panic!("no publication")
    }
    fn read(&self, _: &ValueHandle) -> Result<Option<LoadedValue>, StoreError> {
        let (entered, released) = self.gate.lock().unwrap().take().unwrap();
        entered.send(()).unwrap();
        released.recv_timeout(Duration::from_secs(5)).unwrap();
        Ok(Some(LoadedValue { value: value() }))
    }
    fn is_kept(&self, _: &ValueHandle) -> Result<bool, StoreError> {
        Ok(true)
    }
    fn encoded(&self, _: &ValueHandle) -> Result<Option<Vec<u8>>, StoreError> {
        panic!("no encoding")
    }
    fn size(&self, _: &ValueHandle) -> Result<Option<u64>, StoreError> {
        Ok(Some(1))
    }
    fn release(&mut self, _: &ValueHandle) -> Result<bool, StoreError> {
        panic!("no removal")
    }
}
#[tokio::test]
async fn startup_cancellation_joins_an_admitted_read_and_returns_no_partial_session() {
    let (base, calls) = workspace(None, None);
    let (entered, blocked) = oneshot::channel();
    let (release, released) = mpsc::channel();
    let (store, storage_task) = spawn_store(
        SlowRead {
            gate: Mutex::new(Some((entered, released))),
        },
        StoreWorkerLimits::default(),
    )
    .unwrap();
    let token = CancellationToken::new();
    let work = tokio::spawn(session::restore(
        base,
        RecordingMode::Ephemeral,
        Some(SessionStorage {
            worker: store.clone(),
            auto_keep: AutoKeep::Never,
        }),
        image(
            vec![
                command("source", "catalog echo value:old > answer", &["id80"]),
                JournalEntry::Result(RetainedResult {
                    retention: wes_engine::storage::Retention::Unknown,
                    node: id("id80"),
                    handle: ValueHandle::fresh(),
                    run: wes_engine::runtime::RunId::new("test-run").unwrap(),
                }),
            ],
            vec![],
            Persistence::FileSynced,
        ),
        token.clone(),
    ));
    blocked.await.unwrap();
    token.cancel();
    tokio::task::yield_now().await;
    assert!(!work.is_finished());
    release.send(()).unwrap();
    assert!(matches!(work.await.unwrap(), Err(RestoreError::Cancelled)));
    assert_eq!(calls.load(Ordering::SeqCst), 0);
    store.shutdown().await.unwrap();
    storage_task.join().await.unwrap();
}

struct RejectAccepted;
impl JournalSink for RejectAccepted {
    fn append(&mut self, record: &Record) -> Result<AppendReceipt, RecordError> {
        assert!(!matches!(
            record,
            Record::Recovery(RecoveryEntry::Calling(_))
        ));
        if matches!(record, Record::Recovery(RecoveryEntry::Accepted { .. })) {
            return Err(RecordError::backend(
                "synthetic",
                true,
                std::io::Error::other("private failure"),
            ));
        }
        Ok(AppendReceipt {
            persistence: Persistence::FileSynced,
            end_offset: 1,
        })
    }
}
#[tokio::test]
async fn unacknowledged_recovered_admission_never_enters_the_provider() {
    let (base, calls) = workspace(None, None);
    let (recorder, writer) = spawn_recorder(RejectAccepted, RecorderLimits::default()).unwrap();
    let restored = session::restore(
        base,
        RecordingMode::Required(CallJournal::new(
            recorder.clone(),
            RequiredPersistence::FileSynced,
        )),
        None,
        image(
            vec![command("old", "catalog echo value:old > answer", &["id80"])],
            vec![],
            Persistence::FileSynced,
        ),
        CancellationToken::new(),
    )
    .await
    .unwrap();
    let (handle, task) = restored
        .spawn(no_files(), NonZeroUsize::new(1).unwrap())
        .unwrap();
    submit(&handle, "explicit", ":refresh $answer").await;
    handle.wait_idle().await.unwrap();
    assert_eq!(calls.load(Ordering::SeqCst), 0);
    assert!(handle.log().await.unwrap().entries.iter().any(|entry| matches!(entry.entry(), JournalEntry::Observed(record) if record.state() == NodeState::Failed)));
    stop(handle, task).await;
    recorder.shutdown().await.unwrap();
    writer.join().await.unwrap();
}

#[tokio::test]
async fn admitted_revision_restores_grouping_without_presentation_receipt_or_execution() {
    let mut revision = command("revision", "catalog echo value:new > result", &["id3"]);
    if let JournalEntry::Command(c) = &mut revision {
        c.revision_of = Some("original".into());
    }
    let (base, calls) = workspace(None, None);
    let restored = session::restore(
        base,
        RecordingMode::Ephemeral,
        None,
        image(
            vec![
                command("original", "catalog echo value:old > result", &["id1"]),
                command("dependent", "catalog echo value:$result", &["id2"]),
                JournalEntry::Submitted(wes_engine::history::SubmissionRecord {
                    source_name: "cell original".into(),
                    source_start: wes_language::Position { line: 1, column: 1 },
                    refreshed: vec![],
                    document: None,
                    revision_of: None,
                    id: "receipt-original".into(),
                    cell: "original".into(),
                    text: "catalog echo value:old > result".into(),
                    client: "terminal".into(),
                    context: None,
                    order: 0,
                    nodes: vec![id("id1")],
                    repeat: None,
                    run: None,
                }),
                revision,
            ],
            vec![],
            Persistence::FileSynced,
        ),
        CancellationToken::new(),
    )
    .await
    .unwrap();
    let (handle, task) = restored
        .spawn(no_files(), NonZeroUsize::new(1).unwrap())
        .unwrap();
    handle.wait_idle().await.unwrap();
    let observation = handle.observe().await.unwrap();
    assert_eq!(observation.work_roots().get("revision"), Some(&"original"));
    assert_eq!(calls.load(Ordering::SeqCst), 0);
    assert_eq!(
        observation
            .cells
            .iter()
            .find(|c| c.input.cell() == "revision")
            .unwrap()
            .input
            .revision_of(),
        Some("original")
    );
    assert!(
        observation
            .state
            .execution
            .graph
            .node(&id("id2"))
            .unwrap()
            .dependencies()
            .iter()
            .any(|(n, _)| n == &id("id1"))
    );
    let revised = handle
        .submit(
            input("revision2", "catalog echo value:latest > result")
                .with_revision("revision".into())
                .unwrap(),
        )
        .await
        .unwrap();
    handle.wait_idle().await.unwrap();
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    assert_eq!(revised.nodes.len(), 1);
    stop(handle, task).await;
}

#[tokio::test]
async fn typed_calculations_capture_definition_package_and_restore_held() {
    let (base, calls) = workspace(None, None);
    let (recorder, writer, records) = sink();
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
    let defined = submit(
        &handle,
        "definition",
        ":def twice(input: Int) -> Int as :calc pure { return input * 2; }",
    )
    .await;
    assert!(
        !defined
            .diagnostics
            .diagnostics
            .iter()
            .any(|d| d.severity == wes_language::Severity::Error),
        "{:?}",
        defined.diagnostics
    );
    submit(
        &handle,
        "call",
        ":calc { return 21; } > seed | twice > answer",
    )
    .await;
    handle.wait_idle().await.unwrap();
    let snapshot = handle.snapshot().await.unwrap();
    assert_eq!(
        snapshot.execution.values[&snapshot.names["answer"].node].data(),
        &Data::Int(42)
    );
    let node = snapshot
        .execution
        .graph
        .node(&snapshot.names["answer"].node)
        .unwrap();
    let wes_engine::tasks::BoundTask::Calculation(calculation) = node.payload() else {
        panic!()
    };
    let revision = calculation.definition().unwrap().revision.clone();
    stop(handle, task).await;
    recorder.shutdown().await.unwrap();
    writer.join().await.unwrap();
    let records = records.lock().unwrap().clone();
    assert!(records.iter().any(|r| matches!(r, Record::Journal(JournalEntry::Command(c)) if c.cell == "definition" && c.calculation_package.is_some())));
    let journal = records
        .into_iter()
        .filter_map(|r| {
            if let Record::Journal(e) = r {
                Some(e)
            } else {
                None
            }
        })
        .collect();
    let (base, restored_calls) = workspace(None, None);
    let restored = session::restore(
        base,
        RecordingMode::Ephemeral,
        None,
        image(journal, vec![], Persistence::FileSynced),
        CancellationToken::new(),
    )
    .await
    .unwrap();
    let (handle, task) = restored
        .spawn(no_files(), NonZeroUsize::new(1).unwrap())
        .unwrap();
    handle.wait_idle().await.unwrap();
    let snapshot = handle.snapshot().await.unwrap();
    let node = snapshot
        .execution
        .graph
        .node(&snapshot.names["answer"].node)
        .unwrap();
    let wes_engine::tasks::BoundTask::Calculation(calculation) = node.payload() else {
        panic!()
    };
    assert_eq!(calculation.definition().unwrap().revision, revision);
    assert!(!snapshot.execution.values.contains_key(node.id()));
    assert_eq!(calls.load(Ordering::SeqCst), 0);
    assert_eq!(restored_calls.load(Ordering::SeqCst), 0);
    // Replay is held: restoring a conversion never restarts its producer.
    assert!(
        !snapshot
            .execution
            .values
            .contains_key(&snapshot.names["seed"].node)
    );
    submit(&handle, "refresh-seed", ":refresh $seed").await;
    handle.wait_idle().await.unwrap();
    submit(&handle, "refresh", ":refresh $answer").await;
    handle.wait_idle().await.unwrap();
    let snapshot = handle.snapshot().await.unwrap();
    assert_eq!(
        snapshot.execution.values[&snapshot.names["answer"].node].data(),
        &Data::Int(42)
    );
    stop(handle, task).await;
}
