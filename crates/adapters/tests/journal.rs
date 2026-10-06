#[path = "journal/requests.rs"]
mod requests;
use std::{fs, path::Path};
use wes_adapters::{
    codec::Limits,
    journal::{Durability, FileHistory, ReadLimits},
    storage::FileValues,
};
use wes_engine::{
    history::{
        CommandRecord, HistoryCaptureLimits, JournalEntry, JournalSink, Persistence, Record,
        RecordError, RecoveryEntry,
    },
    storage::StoreError,
};
#[path = "journal/pages.rs"]
mod pages;

fn private_temp() -> tempfile::TempDir {
    let mut builder = tempfile::Builder::new();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        builder.permissions(fs::Permissions::from_mode(0o700));
    }
    builder.tempdir().unwrap()
}
fn open(path: &Path) -> FileHistory {
    FileHistory::open(path, ReadLimits::default(), Durability::File).unwrap()
}
fn command(text: &str) -> Record {
    Record::Journal(JournalEntry::Command(CommandRecord {
        source_name: "fixture.wes".into(),
        source_start: wes_language::Position { line: 1, column: 1 },
        changed_nodes: vec![],
        document: None,
        revision_of: None,
        environments: None,
        cell: "cell-a".into(),
        text: text.into(),
        replay: text.into(),
        nodes: vec![],
        type_sources: Default::default(),
        calculation_package: None,
        imports: vec![],
    }))
}

#[tokio::test]
async fn live_writer_capture_preserves_both_prefixes_and_lock_without_rewriting_files() {
    use std::sync::Arc;
    use wes_engine::recording::{RecorderLimits, spawn_recorder};
    let directory = private_temp();
    let (recorder, writer) =
        spawn_recorder(open(directory.path()), RecorderLimits::default()).unwrap();
    let first = command("catalog list");
    let accepted = Record::Recovery(RecoveryEntry::Accepted {
        cell: "cell-a".into(),
    });
    let journal = recorder.append(Arc::new(first.clone())).await.unwrap();
    let recovery = recorder.append(Arc::new(accepted.clone())).await.unwrap();
    let journal_bytes = fs::read(directory.path().join("journal.jsonl")).unwrap();
    let recovery_bytes = fs::read(directory.path().join("recovery.jsonl")).unwrap();
    assert!(matches!(
        recorder
            .capture(HistoryCaptureLimits {
                records: 1,
                ..Default::default()
            })
            .await,
        Err(RecordError::Limit(_))
    ));
    let captured = recorder
        .capture(HistoryCaptureLimits::default())
        .await
        .unwrap();
    assert_eq!(captured.image().checkpoint().journal, journal);
    assert_eq!(captured.image().checkpoint().recovery, recovery);
    assert!(FileHistory::open(directory.path(), ReadLimits::default(), Durability::File).is_err());
    assert_eq!(
        fs::read(directory.path().join("journal.jsonl")).unwrap(),
        journal_bytes
    );
    assert_eq!(
        fs::read(directory.path().join("recovery.jsonl")).unwrap(),
        recovery_bytes
    );
    recorder
        .append(Arc::new(command("catalog list > next")))
        .await
        .unwrap();
    assert_eq!(captured.image().journal().len(), 1);
    drop(captured);
    let next = recorder
        .capture(HistoryCaptureLimits::default())
        .await
        .unwrap()
        .into_image();
    assert_eq!(next.journal().len(), 2);
    assert_eq!(next.recovery().len(), 1);
    let report = recorder.shutdown().await.unwrap();
    assert_eq!(report.attempted, 3);
    assert_eq!(report.failed, 0);
    writer.join().await.unwrap();
    let reopened = open(directory.path())
        .capture(HistoryCaptureLimits::default())
        .unwrap();
    assert_eq!(reopened.journal(), next.journal());
    assert_eq!(reopened.recovery(), next.recovery());
}

#[test]
fn captured_imports_survive_real_history_reopen_without_becoming_execution_authority() {
    use wes_engine::imports::{ImportRecipe, ImportRequest, ImportSnapshot};
    let directory = private_temp();
    let mut history = open(directory.path());
    let Record::Journal(JournalEntry::Command(mut command)) =
        command(":import spec file:deleted.json")
    else {
        unreachable!()
    };
    command.imports.push(ImportSnapshot::new(
        ImportRequest::new(
            "spec".into(),
            None,
            [(
                "file".into(),
                wes_core::Value::new(
                    wes_core::Shape::Unknown,
                    wes_core::Data::Text("deleted.json".into()),
                    Default::default(),
                )
                .unwrap(),
            )]
            .into(),
        )
        .unwrap(),
        ImportRecipe::new("spec/json/v1".into(), "{\"captured\":true}\r\n".into()).unwrap(),
    ));
    let entry = JournalEntry::Command(command);
    history.append(&Record::Journal(entry.clone())).unwrap();
    drop(history);
    let mut reopened = open(directory.path());
    let captured = reopened.capture(HistoryCaptureLimits::default()).unwrap();
    assert_eq!(captured.journal(), &[entry]);
}

#[tokio::test]
async fn operational_errors_survive_real_history_reopen_without_a_node_or_live_value_store() {
    use std::{num::NonZeroUsize, sync::Arc};
    use wes_engine::{
        calls::{CallJournal, RequiredPersistence},
        driver::CancellationToken,
        history::{NoticeContext, NoticeRecord},
        recording::{RecorderLimits, spawn_recorder},
        runtime::RuntimeCode,
        session::{self, RecordingMode},
        storage::ValueHandle,
        type_sources::{TypeSourceError, TypeSourceReader},
        workspace::Workspace,
    };
    struct NoFiles;
    impl TypeSourceReader for NoFiles {
        fn read(&self, _: &str, _: usize) -> Result<String, TypeSourceError> {
            panic!("no live source")
        }
    }
    let root = private_temp();
    let mut history = open(root.path());
    let error = RuntimeCode::RecordingFailed.error("historical keep failure", None);
    let notice = NoticeRecord::new(
        "historical-notice".into(),
        wes_core::Timestamp::new(0, 0).unwrap(),
        NoticeContext::Keep {
            handle: ValueHandle::fresh(),
            may_have_applied: true,
        },
        error.clone(),
    )
    .unwrap();
    history
        .append(&Record::Journal(JournalEntry::Noticed(notice.clone())))
        .unwrap();
    drop(history);
    let bytes = fs::read(root.path().join("journal.jsonl")).unwrap();
    let mut history = open(root.path());
    let image = history.capture(HistoryCaptureLimits::default()).unwrap();
    let (recorder, writer) = spawn_recorder(history, RecorderLimits::default()).unwrap();
    let restored = session::restore(
        Workspace::local(wes_engine::providers::LocalScope::new("fixture").unwrap()),
        RecordingMode::Required(CallJournal::new(
            recorder.clone(),
            RequiredPersistence::FileSynced,
        )),
        None,
        image,
        CancellationToken::new(),
    )
    .await
    .unwrap();
    assert!(restored.workspace().runtime().graph().is_empty());
    let (handle, task) = restored
        .spawn(Arc::new(NoFiles), NonZeroUsize::new(1).unwrap())
        .unwrap();
    handle.wait_idle().await.unwrap();
    let log = handle.log().await.unwrap();
    assert_eq!(log.entries.len(), 1);
    assert!(log.entries[0].durable());
    assert_eq!(log.entries[0].entry(), &JournalEntry::Noticed(notice));
    assert_eq!(fs::read(root.path().join("journal.jsonl")).unwrap(), bytes);
    handle.shutdown().await.unwrap();
    task.join().await.unwrap();
    recorder.shutdown().await.unwrap();
    writer.join().await.unwrap();
}

#[tokio::test]
async fn real_session_restart_reopens_owned_history_and_archive_without_reexecution_or_rewriting() {
    use std::{num::NonZeroUsize, sync::Arc};
    use wes_adapters::storage::TieredValues;
    use wes_core::Data;
    use wes_engine::{
        calls::{CallJournal, RequiredPersistence},
        driver::CancellationToken,
        recording::{RecorderLimits, spawn_recorder},
        session::{self, RecordingMode, SessionStorage, ValuePublication},
        source::SourceInput,
        storage::{AutoKeep, StoreWorkerLimits, spawn_store},
        type_sources::{TypeSourceError, TypeSourceReader},
        workspace::Workspace,
    };
    struct NoFiles;
    impl TypeSourceReader for NoFiles {
        fn read(&self, _: &str, _: usize) -> Result<String, TypeSourceError> {
            panic!("unexpected type source read")
        }
    }
    let history_root = private_temp();
    let values_root = private_temp();
    let open_values = || {
        TieredValues::open(
            &values_root.path().join("live"),
            &values_root.path().join("archive"),
            Limits::default(),
            Durability::File,
            None,
        )
        .unwrap()
    };
    let (recorder, writer) =
        spawn_recorder(open(history_root.path()), RecorderLimits::default()).unwrap();
    let (store, storage_task) = spawn_store(open_values(), StoreWorkerLimits::default()).unwrap();
    let (session, task) = session::spawn_with_storage(
        Workspace::local(wes_engine::providers::LocalScope::new("fixture").unwrap()),
        RecordingMode::Required(CallJournal::new(
            recorder.clone(),
            RequiredPersistence::FileSynced,
        )),
        Arc::new(NoFiles),
        NonZeroUsize::new(1).unwrap(),
        SessionStorage {
            worker: store.clone(),
            auto_keep: AutoKeep::default(),
        },
    )
    .unwrap();
    let text = ":type check \"hello\" as:Text > greeting";
    let input = || SourceInput::new("original-cell".into(), text.into()).unwrap();
    let reply = session.submit(input()).await.unwrap();
    session.wait_idle().await.unwrap();
    let node = reply.nodes[0].clone();
    let before = session.values().await.unwrap().unwrap();
    let ValuePublication::Complete(published) = &before.outputs[&node] else {
        panic!("completed publication")
    };
    assert!(published.durably_retained());
    let handle = published.stored.as_ref().unwrap().handle.clone();
    let run = published.run.clone();
    let log_ids: Vec<_> = session
        .log()
        .await
        .unwrap()
        .entries
        .iter()
        .map(|entry| entry.id().to_owned())
        .collect();
    session.shutdown().await.unwrap();
    task.join().await.unwrap();
    store.shutdown().await.unwrap();
    storage_task.join().await.unwrap();
    recorder.shutdown().await.unwrap();
    writer.join().await.unwrap();
    let journal_bytes = fs::read(history_root.path().join("journal.jsonl")).unwrap();
    let recovery_bytes = fs::read(history_root.path().join("recovery.jsonl")).unwrap();

    let mut history = open(history_root.path());
    let image = history.capture(HistoryCaptureLimits::default()).unwrap();
    let (recorder, writer) = spawn_recorder(history, RecorderLimits::default()).unwrap();
    let (store, storage_task) = spawn_store(open_values(), StoreWorkerLimits::default()).unwrap();
    let restored = session::restore(
        Workspace::local(wes_engine::providers::LocalScope::new("fixture").unwrap()),
        RecordingMode::Required(CallJournal::new(
            recorder.clone(),
            RequiredPersistence::FileSynced,
        )),
        Some(SessionStorage {
            worker: store.clone(),
            auto_keep: AutoKeep::default(),
        }),
        image,
        CancellationToken::new(),
    )
    .await
    .unwrap();
    assert_eq!(restored.report().values, 1);
    assert_eq!(
        restored
            .workspace()
            .runtime()
            .value_of(&node)
            .unwrap()
            .data(),
        &Data::Text("hello".into())
    );
    let (session, task) = restored
        .spawn(Arc::new(NoFiles), NonZeroUsize::new(1).unwrap())
        .unwrap();
    session.wait_idle().await.unwrap();
    assert_eq!(
        session
            .snapshot()
            .await
            .unwrap()
            .restoration
            .unwrap()
            .values,
        1
    );
    assert!(session.submit(input()).await.unwrap().restored);
    let after = session.values().await.unwrap().unwrap();
    let ValuePublication::Recovered(recovered) = &after.outputs[&node] else {
        panic!("recovered publication")
    };
    assert_eq!(recovered.handle, handle);
    assert_eq!(recovered.run, run);
    assert_eq!(
        session
            .log()
            .await
            .unwrap()
            .entries
            .iter()
            .map(|entry| entry.id().to_owned())
            .collect::<Vec<_>>(),
        log_ids
    );
    assert_eq!(
        fs::read(history_root.path().join("journal.jsonl")).unwrap(),
        journal_bytes
    );
    assert_eq!(
        fs::read(history_root.path().join("recovery.jsonl")).unwrap(),
        recovery_bytes
    );
    // Explicit actions after the zero-write restart may confirm retention or remove the value.
    let kept = session.keep(handle.clone()).await.unwrap();
    assert!(kept.problem.is_none());
    assert_eq!(kept.journal.len(), 1);
    assert_eq!(kept.journal[0].run, run);
    let removed = session.release(handle.clone()).await.unwrap();
    assert!(removed.problem.is_none());
    assert!(removed.affected);
    assert!(store.read(handle).await.unwrap().is_none());
    assert!(
        !session
            .snapshot()
            .await
            .unwrap()
            .execution
            .values
            .contains_key(&node)
    );
    session.shutdown().await.unwrap();
    task.join().await.unwrap();
    store.shutdown().await.unwrap();
    storage_task.join().await.unwrap();
    recorder.shutdown().await.unwrap();
    writer.join().await.unwrap();
}

#[test]
fn owned_capture_covers_both_streams_without_rewriting_them_or_disturbing_append_positions() {
    let dir = private_temp();
    let mut history = open(dir.path());
    let empty = history.capture(HistoryCaptureLimits::default()).unwrap();
    assert!(empty.journal().is_empty());
    assert!(empty.recovery().is_empty());
    assert_eq!(empty.checkpoint().journal.end_offset, 0);
    assert!(!dir.path().join("journal.jsonl").exists());
    assert!(!dir.path().join("recovery.jsonl").exists());
    let entry = command("catalog list");
    let journal = history.append(&entry).unwrap();
    let recovery = history
        .append(&Record::Recovery(RecoveryEntry::Accepted {
            cell: "cell-a".into(),
        }))
        .unwrap();
    let journal_bytes = fs::read(dir.path().join("journal.jsonl")).unwrap();
    let recovery_bytes = fs::read(dir.path().join("recovery.jsonl")).unwrap();
    for _ in 0..2 {
        let captured = history.capture(HistoryCaptureLimits::default()).unwrap();
        assert_eq!(Record::Journal(captured.journal()[0].clone()), entry);
        assert_eq!(captured.recovery().len(), 1);
        assert_eq!(captured.checkpoint().journal, journal);
        assert_eq!(captured.checkpoint().recovery, recovery);

        assert!(captured.charged_bytes() > 0);
        assert_eq!(
            fs::read(dir.path().join("journal.jsonl")).unwrap(),
            journal_bytes
        );
        assert_eq!(
            fs::read(dir.path().join("recovery.jsonl")).unwrap(),
            recovery_bytes
        );
    }
    let next = history.append(&command("catalog list > next")).unwrap();
    let captured = history.capture(HistoryCaptureLimits::default()).unwrap();
    assert_eq!(captured.journal().len(), 2);
    assert_eq!(captured.checkpoint().journal, next);
    assert!(next.end_offset > journal.end_offset);
}

#[test]
fn capture_capacity_is_atomic_and_does_not_invalidate_an_unchanged_writer() {
    let dir = private_temp();
    let mut history = open(dir.path());
    history.append(&command("catalog list")).unwrap();
    history
        .append(&Record::Recovery(RecoveryEntry::Accepted {
            cell: "cell-a".into(),
        }))
        .unwrap();
    for limits in [
        HistoryCaptureLimits {
            records: 1,
            bytes: u64::MAX,
        },
        HistoryCaptureLimits {
            records: usize::MAX,
            bytes: 1,
        },
    ] {
        assert!(matches!(
            history.capture(limits),
            Err(RecordError::Limit(_))
        ));
    }
    history.append(&command("catalog list > next")).unwrap();
    let captured = history.capture(HistoryCaptureLimits::default()).unwrap();
    assert_eq!(captured.journal().len(), 2);
    assert_eq!(captured.recovery().len(), 1);
}

#[test]
fn capture_detects_out_of_band_changes_and_invalidates_further_appends_without_repair() {
    use std::io::Write;
    let dir = private_temp();
    let mut history = open(dir.path());
    history.append(&command("catalog list")).unwrap();
    let path = dir.path().join("journal.jsonl");
    let original = fs::read(&path).unwrap();
    fs::OpenOptions::new()
        .append(true)
        .open(&path)
        .unwrap()
        .write_all(&original)
        .unwrap();
    let changed = fs::read(&path).unwrap();
    assert!(matches!(
        history.capture(HistoryCaptureLimits::default()),
        Err(RecordError::Backend {
            may_have_appended: false,
            ..
        })
    ));
    assert!(matches!(
        history.append(&command("catalog list > next")),
        Err(RecordError::Poisoned)
    ));
    assert_eq!(fs::read(&path).unwrap(), changed);
}

#[test]
fn empty_history_is_lazy_and_reads_without_creating_journal_files() {
    let dir = private_temp();
    let history = open(dir.path());
    assert!(history.journal().unwrap().next().is_none());
    assert!(history.recovery().unwrap().next().is_none());
    assert!(!dir.path().join("journal.jsonl").exists());
    assert!(!dir.path().join("recovery.jsonl").exists());
}

#[test]
fn synced_acknowledgements_and_stream_boundaries_survive_reopening() {
    let dir = private_temp();
    let mut history = open(dir.path());
    let first = history.append(&command("catalog list")).unwrap();
    assert_eq!(first.persistence, Persistence::FileSynced);
    assert_eq!(
        first.end_offset,
        fs::metadata(dir.path().join("journal.jsonl"))
            .unwrap()
            .len()
    );
    let accepted = Record::Recovery(RecoveryEntry::Accepted {
        cell: "cell-a".into(),
    });
    history.append(&accepted).unwrap();
    let second = history.append(&command("catalog list > list")).unwrap();
    assert!(second.end_offset > first.end_offset);
    drop(history);
    let mut restored = open(dir.path());
    assert_eq!(restored.journal().unwrap().count(), 2);
    let recovery = restored
        .recovery()
        .unwrap()
        .collect::<Result<Vec<_>, _>>()
        .unwrap();
    assert_eq!(recovery.len(), 1);
    assert_eq!(Record::Recovery(recovery[0].entry.clone()), accepted);
    let third = restored.append(&command("catalog list > other")).unwrap();
    assert!(third.end_offset > second.end_offset);
    let records = restored
        .journal()
        .unwrap()
        .collect::<Result<Vec<_>, _>>()
        .unwrap();
    assert_eq!(records.len(), 3);
}

#[test]
fn corruption_and_unterminated_tails_require_explicit_recovery_without_truncation() {
    for tail in [
        b"{\"record\":".as_slice(),
        b"{oops}\n",
        b"{}\n",
        b"  ",
        b"{\"record\":\"command\",\"text\":\"x\",\"nodes\":[]}",
    ] {
        let dir = private_temp();
        drop(open(dir.path()));
        let path = dir.path().join("journal.jsonl");
        fs::write(&path, tail).unwrap();
        assert!(FileHistory::open(dir.path(), ReadLimits::default(), Durability::File).is_err());
        assert_eq!(fs::read(&path).unwrap(), tail);
        assert!(!dir.path().join("recovery.jsonl").exists());
    }
}

#[test]
fn limits_fail_before_mutation_and_do_not_poison_the_writer() {
    let dir = private_temp();
    let limits = ReadLimits {
        record: Limits {
            bytes: 200,
            nodes: 100,
        },
        lines: 1,
        ..ReadLimits::default()
    };
    let mut history = FileHistory::open(dir.path(), limits, Durability::File).unwrap();
    assert!(history.append(&command(&"x".repeat(201))).is_err());
    assert!(!dir.path().join("journal.jsonl").exists());
    history.append(&command("x")).unwrap();
    let before = fs::read(dir.path().join("journal.jsonl")).unwrap();
    assert!(matches!(
        history.append(&command("y")),
        Err(RecordError::Limit("line count"))
    ));
    assert_eq!(fs::read(dir.path().join("journal.jsonl")).unwrap(), before);
    // Recovery has its own count, not a copied workspace-journal position.
    assert!(
        history
            .append(&Record::Recovery(RecoveryEntry::Accepted {
                cell: "cell".into()
            }))
            .is_ok()
    );
}

#[test]
fn total_budget_and_blank_line_accounting_persist_across_reopen() {
    let dir = private_temp();
    drop(open(dir.path()));
    fs::write(dir.path().join("journal.jsonl"), b"\n\n").unwrap();
    let limits = ReadLimits {
        lines: 2,
        ..ReadLimits::default()
    };
    let mut history = FileHistory::open(dir.path(), limits, Durability::File).unwrap();
    assert!(matches!(
        history.append(&command("x")),
        Err(RecordError::Limit("line count"))
    ));
    drop(history);
    let mut history = FileHistory::open(
        dir.path(),
        ReadLimits {
            bytes: 2,
            ..ReadLimits::default()
        },
        Durability::File,
    )
    .unwrap();
    assert!(matches!(
        history.append(&command("x")),
        Err(RecordError::Limit("total byte"))
    ));
    assert_eq!(fs::read(dir.path().join("journal.jsonl")).unwrap(), b"\n\n");
}

#[test]
fn one_writer_owns_the_directory_and_history_cannot_be_a_value_store() {
    let dir = private_temp();
    let history = open(dir.path());
    assert!(FileHistory::open(dir.path(), ReadLimits::default(), Durability::File).is_err());
    assert!(matches!(
        FileValues::open(dir.path(), Limits::default(), Durability::File, None),
        Err(StoreError::UnownedDirectory)
    ));
    drop(history);
    assert!(FileHistory::open(dir.path(), ReadLimits::default(), Durability::File).is_ok());
}

#[cfg(unix)]
#[test]
fn private_file_permissions_and_directory_sync_are_explicit() {
    use std::os::unix::fs::PermissionsExt;
    let dir = private_temp();
    let mut history = FileHistory::open(
        dir.path(),
        ReadLimits::default(),
        Durability::FileAndDirectory,
    )
    .unwrap();
    let ack = history.append(&command("catalog list")).unwrap();
    assert_eq!(ack.persistence, Persistence::FileAndDirectorySynced);
    assert_eq!(
        fs::metadata(dir.path().join("journal.jsonl"))
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o600
    );
}

#[cfg(unix)]
#[test]
fn a_symlinked_journal_cannot_read_or_append_outside_its_directory() {
    let dir = private_temp();
    let outside = private_temp();
    let target = outside.path().join("private.txt");
    fs::write(&target, b"untouched").unwrap();
    drop(open(dir.path()));
    std::os::unix::fs::symlink(&target, dir.path().join("journal.jsonl")).unwrap();
    assert!(FileHistory::open(dir.path(), ReadLimits::default(), Durability::File).is_err());
    assert_eq!(fs::read(target).unwrap(), b"untouched");
}

#[tokio::test]
async fn the_bounded_worker_persists_both_streams_and_shutdown_releases_the_real_lock() {
    use std::sync::Arc;
    use wes_engine::recording::{RecorderLimits, spawn_recorder};
    let dir = private_temp();
    let history = open(dir.path());
    let (recorder, task) = spawn_recorder(history, RecorderLimits::default()).unwrap();
    let first = recorder
        .enqueue(Arc::new(command("catalog list")))
        .await
        .unwrap();
    let second = recorder
        .enqueue(Arc::new(Record::Recovery(RecoveryEntry::Accepted {
            cell: "cell-a".into(),
        })))
        .await
        .unwrap();
    let report = recorder.shutdown().await.unwrap();
    assert_eq!((report.attempted, report.failed), (2, 0));
    // The acknowledgement follows sink destruction, even if another Recorder handle still exists.
    let restored = open(dir.path());
    assert_eq!(restored.journal().unwrap().count(), 1);
    assert_eq!(restored.recovery().unwrap().count(), 1);
    assert_eq!(
        first.wait().await.unwrap().persistence,
        Persistence::FileSynced
    );
    assert_eq!(
        second.wait().await.unwrap().persistence,
        Persistence::FileSynced
    );
    task.join().await.unwrap();
}

#[tokio::test]
async fn provider_entry_sees_its_recorded_command_and_run_before_returning_a_value() {
    use std::{io::BufReader, path::PathBuf, sync::Arc, time::Duration};
    use wes_core::{
        Shape,
        capability::{Capability, Parameter, ProviderDescription, Safety},
    };
    use wes_engine::{
        calls::{CallJournal, RequiredPersistence},
        driver::{CancellationToken, Executor},
        providers::{Call, InvocationFuture, Invoker},
        recording::{RecorderLimits, spawn_recorder},
        runtime::{Effect, Outcome},
        tasks::TaskExecutor,
        workspace::{Preparation, Workspace},
    };
    use wes_language::{SourceText, parse};
    struct Inspect(PathBuf);
    impl Invoker for Inspect {
        fn invoke(&self, call: Call, _: CancellationToken) -> InvocationFuture {
            let path = self.0.clone();
            Box::pin(async move {
                tokio::task::spawn_blocking(move || {
                    // Test-only inspection of our temporary directory; no live/user store access.
                    let journal = wes_adapters::journal::read_journal(
                        BufReader::new(fs::File::open(path.join("journal.jsonl")).unwrap()), ReadLimits::default())
                        .collect::<Result<Vec<_>, _>>().unwrap();
                    assert!(matches!(&journal[0].entry, JournalEntry::Command(record) if record.nodes == [call.run.node().clone()]));
                    let recovery = wes_adapters::journal::read_recovery(
                        BufReader::new(fs::File::open(path.join("recovery.jsonl")).unwrap()), ReadLimits::default())
                        .collect::<Result<Vec<_>, _>>().unwrap();
                    assert_eq!(recovery.len(), 2);
                    assert!(matches!(&recovery[1].entry, RecoveryEntry::Calling(record) if &record.run == call.run.id() && &record.node == call.run.node()));
                    Ok(call.arguments["value"].clone())
                }).await.unwrap()
            })
        }
    }
    let dir = private_temp();
    let (recorder, task) = spawn_recorder(open(dir.path()), RecorderLimits::default()).unwrap();
    let journal = CallJournal::new(recorder.clone(), RequiredPersistence::FileSynced);
    let mut workspace =
        Workspace::local(wes_engine::providers::LocalScope::new("fixture").unwrap());
    let mut capability = Capability::new(["echo"], Shape::Unknown, Safety::Safe);
    capability.parameters = vec![Parameter::new("value", Shape::Unknown, true)];
    workspace
        .register_provider(
            ProviderDescription::new("catalog", [capability], vec![]).unwrap(),
            Arc::new(Inspect(dir.path().to_owned())),
        )
        .unwrap();
    let text = "catalog echo value:hello";
    let parsed = parse(&SourceText::new("cell-a", text));
    let Preparation::Change(prepared) = workspace.prepare(&parsed.script.statements[0]).unwrap()
    else {
        panic!("call")
    };
    let admission = journal
        .admit(CommandRecord {
            source_name: "fixture.wes".into(),
            source_start: wes_language::Position { line: 1, column: 1 },
            changed_nodes: vec![],
            document: None,
            revision_of: None,
            environments: None,
            cell: "cell-a".into(),
            text: text.into(),
            replay: text.into(),
            nodes: vec![prepared.node().unwrap().clone()],
            type_sources: Default::default(),
            calculation_package: None,
            imports: vec![],
        })
        .await
        .unwrap();
    workspace
        .commit(prepared.with_admission(admission).unwrap())
        .unwrap();
    let ticket = workspace
        .start(Duration::ZERO)
        .into_iter()
        .find_map(|effect| match effect {
            Effect::Spawn(ticket) => Some(ticket),
            _ => None,
        })
        .unwrap();
    let run = ticket.run.clone();
    workspace.enter(&run);
    let report = TaskExecutor::recorded(journal)
        .execute(ticket, CancellationToken::new())
        .await;
    assert!(matches!(report.outcome, Outcome::Produced(_)));
    assert!(report.notices.is_empty());
    workspace.complete(&run, report.outcome, Duration::from_secs(1));
    assert_eq!(recorder.shutdown().await.unwrap().failed, 0);
    task.join().await.unwrap();
    let restored = open(dir.path());
    let recovery = restored
        .recovery()
        .unwrap()
        .collect::<Result<Vec<_>, _>>()
        .unwrap();
    assert_eq!(recovery.len(), 3);
    assert!(
        matches!(&recovery[2].entry, RecoveryEntry::Called { node, run: id, produced: true } if node == run.node() && id == run.id())
    );
}

#[test]
fn historical_journal_is_rejected_without_rewriting() {
    let dir = private_temp();
    drop(open(dir.path()));
    let bytes = b"{\"record\":\"command\",\"text\":\"catalog list\",\"nodes\":[]}\n";
    let path = dir.path().join("journal.jsonl");
    fs::write(&path, bytes).unwrap();
    assert!(FileHistory::open(dir.path(), ReadLimits::default(), Durability::File).is_err());
    assert_eq!(fs::read(path).unwrap(), bytes);
}

/// A history acknowledges only what this host establishes. Where directory-entry durability
/// is not offered the requirement is refused, no stronger receipt exists, and the same
/// history continues with file durability.
#[test]
fn a_history_acknowledges_only_the_durability_this_host_establishes() {
    let dir = private_temp();
    let mut history = open(dir.path());
    assert_eq!(
        history.append(&command("first")).unwrap().persistence,
        Persistence::FileSynced
    );
    drop(history);
    let strong = FileHistory::open(
        dir.path(),
        ReadLimits::default(),
        Durability::FileAndDirectory,
    );
    if Durability::FileAndDirectory.supported() {
        assert_eq!(
            strong
                .unwrap()
                .append(&command("second"))
                .unwrap()
                .persistence,
            Persistence::FileAndDirectorySynced
        );
        return;
    }
    assert!(strong.is_err());
    let mut history = open(dir.path());
    assert_eq!(
        history.append(&command("second")).unwrap().persistence,
        Persistence::FileSynced
    );
    assert_eq!(
        history
            .capture(HistoryCaptureLimits::default())
            .unwrap()
            .journal()
            .len(),
        2
    );
}
