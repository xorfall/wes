use std::{
    num::NonZeroUsize,
    sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
    },
};
use wes_adapters::{
    codec::Limits,
    storage::{Durability, TieredValues},
};
use wes_core::{
    Data, Primitive, Shape,
    capability::{Capability, Parameter, ProviderDescription, Safety},
};
use wes_engine::{
    driver::CancellationToken,
    providers::{Call, InvocationFuture, Invoker, LocalScope},
    session::{
        self, RecordingMode, SessionHandle, SessionStorage,
        sandbox::{Definition, DefinitionStore, Kind},
    },
    source::SourceInput,
    storage::{AutoKeep, StoreWorkerLimits, spawn_store},
    type_sources::{TypeSourceError, TypeSourceReader},
    workspace::Workspace,
};
#[derive(Default)]
struct Definitions(Mutex<Vec<Definition>>);
impl DefinitionStore for Definitions {
    fn load(&self) -> Result<Vec<Definition>, String> {
        Ok(self.0.lock().unwrap().clone())
    }
    fn save(&self, definitions: &[Definition]) -> Result<(), String> {
        *self.0.lock().unwrap() = definitions.to_vec();
        Ok(())
    }
}
struct NoFiles;
impl TypeSourceReader for NoFiles {
    fn read(&self, _: &str, _: usize) -> Result<String, TypeSourceError> {
        panic!("unexpected file read")
    }
}
struct Counter(Arc<AtomicUsize>);
impl Invoker for Counter {
    fn invoke(&self, call: Call, _: CancellationToken) -> InvocationFuture {
        self.0.fetch_add(1, Ordering::SeqCst);
        Box::pin(async move { Ok(call.arguments["value"].clone()) })
    }
}
fn workspace(definitions: Arc<Definitions>) -> (Workspace, Arc<AtomicUsize>) {
    let calls = Arc::new(AtomicUsize::new(0));
    let mut workspace = Workspace::local(LocalScope::new("finite-analysis-fixture").unwrap())
        .with_sandbox_store(definitions);
    let mut echo = Capability::new(["echo"], Shape::Primitive(Primitive::Int), Safety::Safe);
    echo.parameters.push(Parameter::new(
        "value",
        Shape::Primitive(Primitive::Int),
        true,
    ));
    workspace
        .register_provider(
            ProviderDescription::new("numbers", [echo], vec![]).unwrap(),
            Arc::new(Counter(calls.clone())),
        )
        .unwrap();
    (workspace, calls)
}
async fn submit(handle: &SessionHandle, id: &str, source: &str) -> Arc<session::SubmissionResult> {
    handle
        .submit(SourceInput::new(id.into(), source.into()).unwrap())
        .await
        .unwrap_or_else(|e| panic!("{id}: {e:?}"))
}
async fn read(handle: &SessionHandle, reference: &str) -> Data {
    handle
        .read_sandbox_export(reference, false)
        .await
        .unwrap_or_else(|e| panic!("{reference}: {e:?}"))
        .data
}
fn files(directory: &std::path::Path) -> usize {
    std::fs::read_dir(directory)
        .unwrap()
        .filter_map(Result::ok)
        .filter(|e| e.path().extension().is_some_and(|e| e == "json"))
        .count()
}
#[tokio::test]
async fn native_external_inputs_stay_temporary_and_ordinary_auto_keep_is_unchanged() {
    let root = tempfile::tempdir().unwrap();
    let live = root.path().join("live");
    let archive = root.path().join("archive");
    let (worker, storage_task) = spawn_store(
        TieredValues::open(
            &live,
            &archive,
            Limits::default(),
            Durability::FileAndDirectory,
            None,
        )
        .unwrap(),
        StoreWorkerLimits::default(),
    )
    .unwrap();
    let definitions = Arc::new(Definitions::default());
    let (base, calls) = workspace(definitions.clone());
    let (handle, task) = session::spawn_with_storage(
        base,
        RecordingMode::Ephemeral,
        Arc::new(NoFiles),
        NonZeroUsize::new(2).unwrap(),
        SessionStorage {
            worker: worker.clone(),
            auto_keep: AutoKeep::default(),
        },
    )
    .unwrap();
    submit(&handle, "ordinary", "numbers echo value:5 > input").await;
    handle.wait_idle().await.unwrap();
    let archived = files(&archive);
    assert_eq!(archived, 1);
    let made = submit(&handle, "study", ":transient {\nnumbers echo value:$input > copied\n:calc pure { return $copied + $input; } > total\n} > study").await;
    assert_eq!(made.sandbox.as_ref().unwrap().kind, Kind::Transient);
    assert_eq!(read(&handle, "study.total").await, Data::Int(10));
    assert_eq!(calls.load(Ordering::SeqCst), 2);
    assert_eq!(files(&archive), archived);
    assert_eq!(files(&live), 1);
    assert_eq!(definitions.0.lock().unwrap()[0].kind, Kind::Transient);
    let stored = handle.values().await.unwrap().unwrap();
    assert!(stored.policy.automatic);
    handle.shutdown().await.unwrap();
    task.join().await.unwrap();
    let (base, calls) = workspace(definitions);
    let (handle, task) = session::spawn_with_storage(
        base,
        RecordingMode::Ephemeral,
        Arc::new(NoFiles),
        NonZeroUsize::new(2).unwrap(),
        SessionStorage {
            worker: worker.clone(),
            auto_keep: AutoKeep::default(),
        },
    )
    .unwrap();
    assert_eq!(calls.load(Ordering::SeqCst), 0);
    assert!(handle.read_sandbox("study.total", false).await.is_err());
    handle.shutdown().await.unwrap();
    task.join().await.unwrap();
    worker.shutdown().await.unwrap();
    storage_task.join().await.unwrap();
}

#[tokio::test]
async fn keep_saves_exact_members_and_reopens_old_analysis_without_provider_replay() {
    use wes_adapters::journal::{Durability as HistoryDurability, FileHistory, ReadLimits};
    use wes_engine::{
        calls::{CallJournal, RequiredPersistence},
        history::{HistoryCaptureLimits, JournalEntry},
        recording::{RecorderLimits, spawn_recorder},
    };
    let root = tempfile::tempdir().unwrap();
    let live = root.path().join("live");
    let archive = root.path().join("archive");
    let history = root.path().join("history");
    let definitions = Arc::new(Definitions::default());
    let (worker, storage_task) = spawn_store(
        TieredValues::open(
            &live,
            &archive,
            Limits::default(),
            Durability::FileAndDirectory,
            None,
        )
        .unwrap(),
        StoreWorkerLimits::default(),
    )
    .unwrap();
    let (recorder, writer) = spawn_recorder(
        FileHistory::open(
            &history,
            ReadLimits::default(),
            HistoryDurability::FileAndDirectory,
        )
        .unwrap(),
        RecorderLimits::default(),
    )
    .unwrap();
    let recording = RecordingMode::Required(CallJournal::new(
        recorder.clone(),
        RequiredPersistence::FileSynced,
    ));
    let (base, calls) = workspace(definitions.clone());
    let (handle, task) = session::spawn_with_storage(
        base,
        recording,
        Arc::new(NoFiles),
        NonZeroUsize::new(2).unwrap(),
        SessionStorage {
            worker: worker.clone(),
            auto_keep: AutoKeep::default(),
        },
    )
    .unwrap();
    submit(&handle,"A",":transient {\nnumbers echo value:11 > raw\n:calc pure { return $raw + 1; } > summary\n} > study").await;
    assert_eq!(read(&handle, "study.summary").await, Data::Int(12));
    // Join scoped storage as well as execution before exact-run Keep.
    submit(&handle, "keep-one", ":keep $study.summary > saved_summary").await;
    assert_eq!(read(&handle, "saved_summary").await, Data::Int(12));
    assert_eq!(
        files(&archive),
        1,
        "standalone summary must not archive its whole execution ancestry"
    );
    submit(&handle, "save-A", ":keep $study > saved_analysis").await;
    assert_eq!(read(&handle, "saved_analysis.raw").await, Data::Int(11));
    assert_eq!(files(&archive), 2);
    let frozen_node = handle.snapshot().await.unwrap().names["saved_summary"]
        .node
        .clone();
    assert!(handle.cancel(frozen_node.clone()).await.is_err());
    let refused = submit(
        &handle,
        "immutable-raw-id",
        &format!(":drop node:\"{frozen_node}\""),
    )
    .await;
    assert!(
        refused
            .diagnostics
            .diagnostics
            .iter()
            .any(|d| d.severity == wes_language::Severity::Error)
    );
    assert_eq!(read(&handle, "saved_summary").await, Data::Int(12));
    let before = handle
        .read_named_export("study.raw", false)
        .await
        .unwrap()
        .unwrap();
    let mut stale_ticket = before.clone();
    stale_ticket.sources.get_mut("").unwrap().revision.push('0');
    assert!(matches!(
        handle.validate_named_export(&stale_ticket).await,
        Err(session::SessionError::HistoricalReplyUnavailable)
    ));
    let (_, old_worker) = handle
        .resolve_read_scope(before.scope.as_ref().unwrap())
        .await
        .unwrap();
    submit(
        &handle,
        "B",
        ":transient { numbers echo value:22 > raw } > study",
    )
    .await;
    assert_eq!(read(&handle, "study.raw").await, Data::Int(22));
    assert!(matches!(
        handle.validate_named_export(&before).await,
        Err(session::SessionError::HistoricalReplyUnavailable)
    ));
    assert!(matches!(
        old_worker
            .read(
                wes_engine::storage::ValueHandle::new(before.sources[""].handle.as_ref().unwrap())
                    .unwrap()
            )
            .await,
        Err(wes_engine::storage::StoreError::Closed)
    ));
    assert_eq!(read(&handle, "saved_analysis.raw").await, Data::Int(11));
    assert_eq!(calls.load(Ordering::SeqCst), 2);
    let image = recorder
        .capture(HistoryCaptureLimits::default())
        .await
        .unwrap();
    let saved = image
        .image()
        .journal()
        .iter()
        .filter_map(|e| match e {
            JournalEntry::Saved(s) => Some(s),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(saved.len(), 2);
    assert_eq!(
        saved[1]
            .members
            .iter()
            .map(|m| m.name.as_str())
            .collect::<Vec<_>>(),
        ["raw", "summary"]
    );
    assert!(
        saved[1]
            .members
            .iter()
            .all(|m| m.run.is_some() && m.state == "ready")
    );
    handle.shutdown().await.unwrap();
    task.join().await.unwrap();
    recorder.shutdown().await.unwrap();
    writer.join().await.unwrap();
    worker.shutdown().await.unwrap();
    storage_task.join().await.unwrap();
    let mut journal = FileHistory::open(
        &history,
        ReadLimits::default(),
        HistoryDurability::FileAndDirectory,
    )
    .unwrap();
    let image = journal.capture(HistoryCaptureLimits::default()).unwrap();
    // A final record without its exact intent is not restore authority.
    for missing_intent in [true, false] {
        let mut capture = wes_engine::history::HistoryCapture::new(HistoryCaptureLimits::default());
        let mut changed = false;
        for entry in image.journal() {
            let mut entry = entry.clone();
            match &mut entry {
                JournalEntry::SaveIntent(_) if missing_intent => continue,
                JournalEntry::Saved(saved) if !missing_intent && !changed => {
                    saved.scope = uuid::Uuid::new_v4().to_string();
                    saved.validate().unwrap();
                    changed = true;
                }
                _ => {}
            }
            capture
                .push(wes_engine::history::Record::Journal(entry))
                .unwrap();
        }
        for entry in image.recovery() {
            capture
                .push(wes_engine::history::Record::Recovery(entry.clone()))
                .unwrap();
        }
        let (base, calls) = workspace(definitions.clone());
        let result = session::restore(
            base,
            RecordingMode::Ephemeral,
            None,
            capture.finish(image.checkpoint()),
            CancellationToken::new(),
        )
        .await;
        assert!(matches!(result, Err(session::RestoreError::Saved)));
        assert_eq!(calls.load(Ordering::SeqCst), 0);
    }
    // Refusing a read budget keeps the saved artifact intact and does not become
    // missing data or a reason to call its original provider.
    let (limited_worker, limited_storage_task) = spawn_store(
        TieredValues::open(
            &live,
            &archive,
            Limits {
                bytes: 1,
                ..Limits::default()
            },
            Durability::FileAndDirectory,
            None,
        )
        .unwrap(),
        StoreWorkerLimits::default(),
    )
    .unwrap();
    let (limited_base, limited_calls) = workspace(definitions.clone());
    let limited = session::restore(
        limited_base,
        RecordingMode::Ephemeral,
        Some(SessionStorage {
            worker: limited_worker.clone(),
            auto_keep: AutoKeep::default(),
        }),
        image.clone(),
        CancellationToken::new(),
    )
    .await
    .unwrap();
    assert!(!limited.report().problems.is_empty());
    assert!(
        limited
            .report()
            .problems
            .iter()
            .all(|p| p.kind == session::RestoreProblemKind::ValueCapacity)
    );
    let output = limited.workspace().resolve("saved_summary").unwrap();
    assert_eq!(
        limited.workspace().runtime().stale_reason(&output.node),
        Some(wes_engine::runtime::StaleReason::RestoreCapacity)
    );
    let (limited_handle, limited_task) = limited
        .spawn(Arc::new(NoFiles), NonZeroUsize::new(2).unwrap())
        .unwrap();
    assert!(
        limited_handle
            .read_sandbox("saved_summary", true)
            .await
            .is_ok()
    );
    assert!(matches!(
        limited_handle
            .read_sandbox_export("saved_summary", false)
            .await,
        Err(session::SessionError::Capacity)
    ));
    assert_eq!(limited_calls.load(Ordering::SeqCst), 0);
    assert_eq!(files(&archive), 2);
    limited_handle.shutdown().await.unwrap();
    limited_task.join().await.unwrap();
    limited_worker.shutdown().await.unwrap();
    limited_storage_task.join().await.unwrap();
    let (recorder, writer) = spawn_recorder(journal, RecorderLimits::default()).unwrap();
    let (worker, storage_task) = spawn_store(
        TieredValues::open(
            &live,
            &archive,
            Limits::default(),
            Durability::FileAndDirectory,
            None,
        )
        .unwrap(),
        StoreWorkerLimits::default(),
    )
    .unwrap();
    let (base, calls) = workspace(definitions);
    let restored = session::restore(
        base,
        RecordingMode::Required(CallJournal::new(
            recorder.clone(),
            RequiredPersistence::FileSynced,
        )),
        Some(SessionStorage {
            worker: worker.clone(),
            auto_keep: AutoKeep::default(),
        }),
        image,
        CancellationToken::new(),
    )
    .await
    .unwrap();
    assert!(restored.report().problems.is_empty());
    let (handle, task) = restored
        .spawn(Arc::new(NoFiles), NonZeroUsize::new(2).unwrap())
        .unwrap();
    assert_eq!(read(&handle, "saved_analysis.raw").await, Data::Int(11));
    assert_eq!(read(&handle, "saved_summary").await, Data::Int(12));
    assert!(
        handle.read_sandbox("study.raw", false).await.is_err(),
        "unsaved B must not restore"
    );
    let continued = submit(
        &handle,
        "use-saved",
        ":calc pure { return $saved_analysis.summary + $saved_summary; } > combined",
    )
    .await;
    handle.wait_idle().await.unwrap();
    let observation = handle.observe().await.unwrap();
    assert_eq!(
        observation
            .state
            .execution
            .values
            .get(&continued.nodes[0])
            .unwrap()
            .data(),
        &Data::Int(24)
    );
    assert_eq!(calls.load(Ordering::SeqCst), 0);
    handle.shutdown().await.unwrap();
    task.join().await.unwrap();
    recorder.shutdown().await.unwrap();
    writer.join().await.unwrap();
    worker.shutdown().await.unwrap();
    storage_task.join().await.unwrap();
    let child = std::process::Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "separate_process_saved_analysis_reader",
            "--nocapture",
        ])
        .env("WES_TRANSIENT_REOPEN_FIXTURE", root.path())
        .status()
        .unwrap();
    assert!(child.success());
}

#[tokio::test]
async fn separate_process_saved_analysis_reader() {
    let Some(root) = std::env::var_os("WES_TRANSIENT_REOPEN_FIXTURE") else {
        return;
    };
    use wes_adapters::journal::{Durability as HistoryDurability, FileHistory, ReadLimits};
    use wes_engine::{
        calls::{CallJournal, RequiredPersistence},
        history::HistoryCaptureLimits,
        recording::{RecorderLimits, spawn_recorder},
    };
    let root = std::path::PathBuf::from(root);
    let mut journal = FileHistory::open(
        &root.join("history"),
        ReadLimits::default(),
        HistoryDurability::FileAndDirectory,
    )
    .unwrap();
    let image = journal.capture(HistoryCaptureLimits::default()).unwrap();
    let (recorder, writer) = spawn_recorder(journal, RecorderLimits::default()).unwrap();
    let (worker, storage_task) = spawn_store(
        TieredValues::open(
            &root.join("live"),
            &root.join("archive"),
            Limits::default(),
            Durability::FileAndDirectory,
            None,
        )
        .unwrap(),
        StoreWorkerLimits::default(),
    )
    .unwrap();
    let (base, calls) = workspace(Arc::new(Definitions::default()));
    let restored = session::restore(
        base,
        RecordingMode::Required(CallJournal::new(
            recorder.clone(),
            RequiredPersistence::FileSynced,
        )),
        Some(SessionStorage {
            worker: worker.clone(),
            auto_keep: AutoKeep::Never,
        }),
        image,
        CancellationToken::new(),
    )
    .await
    .unwrap();
    assert!(restored.report().problems.is_empty());
    let (handle, task) = restored
        .spawn(Arc::new(NoFiles), NonZeroUsize::new(2).unwrap())
        .unwrap();
    assert_eq!(read(&handle, "saved_analysis.raw").await, Data::Int(11));
    assert_eq!(read(&handle, "saved_summary").await, Data::Int(12));
    assert_eq!(calls.load(Ordering::SeqCst), 0);
    handle.shutdown().await.unwrap();
    task.join().await.unwrap();
    recorder.shutdown().await.unwrap();
    writer.join().await.unwrap();
    worker.shutdown().await.unwrap();
    storage_task.join().await.unwrap();
}

#[tokio::test]
async fn kept_incomplete_checkpoint_reopens_and_resumes_in_a_new_transient_scope() {
    use wes_adapters::{
        datasets::{DatasetStore, StoreLimits},
        journal::{Durability as HistoryDurability, FileHistory, ReadLimits},
    };
    use wes_engine::{
        calls::{CallJournal, RequiredPersistence},
        history::{HistoryCaptureLimits, JournalEntry},
        recording::{RecorderLimits, spawn_recorder},
        storage::spawn_storage,
    };
    let root = tempfile::tempdir().unwrap();
    let definitions = Arc::new(Definitions::default());
    let storage = || {
        spawn_storage(
            TieredValues::open(
                &root.path().join("live"),
                &root.path().join("archive"),
                Limits::default(),
                Durability::FileAndDirectory,
                None,
            )
            .unwrap(),
            DatasetStore::open(
                &root.path().join("datasets"),
                Durability::FileAndDirectory,
                StoreLimits::default(),
            )
            .unwrap(),
            StoreWorkerLimits::default(),
        )
        .unwrap()
    };
    let (worker, storage_task) = storage();
    let (recorder, writer) = spawn_recorder(
        FileHistory::open(
            &root.path().join("history"),
            ReadLimits::default(),
            HistoryDurability::FileAndDirectory,
        )
        .unwrap(),
        RecorderLimits::default(),
    )
    .unwrap();
    let (base, calls) = workspace(definitions.clone());
    let (handle, task) = session::spawn_with_storage(
        base,
        RecordingMode::Required(CallJournal::new(
            recorder.clone(),
            RequiredPersistence::FileSynced,
        )),
        Arc::new(NoFiles),
        NonZeroUsize::new(2).unwrap(),
        SessionStorage {
            worker: worker.clone(),
            auto_keep: AutoKeep::default(),
        },
    )
    .unwrap();
    submit(&handle,"types",r#":package load source:"types: {Step: {base: Record, fields: {state: Int, outputs: 'List<Int>'}}}"
:def step(state:Int, context:Int, item:Int) -> Step as :calc pure { return {state:state+item,outputs:[item]}; }"#).await;
    let committed = wes_engine::scan::Settings::capture().commit_records;
    let rows = std::iter::repeat_n("1", committed + 1)
        .collect::<Vec<_>>()
        .join(",");
    submit(&handle, "partial-study", &format!(":transient {{\n:calc pure {{ return [{rows}]; }} > raw\n:scan source:$raw transition:step initial:0 context:0 profile:TypedRecords sink:dataset records:{committed} > analysis\n}} > study")).await;
    handle.read_sandbox_export("study", true).await.unwrap();
    submit(
        &handle,
        "keep-partial",
        ":keep $study.analysis > saved_partial",
    )
    .await;
    let saved = read(&handle, "saved_partial").await;
    let Data::Record(saved) = saved else {
        panic!("scan evidence")
    };
    assert_eq!(saved["state"], Data::Int(committed as i64));
    let Data::Dataset(prefix) = &saved["outputs"] else {
        panic!("dataset")
    };
    let prefix = prefix.as_ref().clone();
    assert_eq!(prefix.records(), committed as u64);
    let image = recorder
        .capture(HistoryCaptureLimits::default())
        .await
        .unwrap();
    let record = image
        .image()
        .journal()
        .iter()
        .find_map(|e| match e {
            JournalEntry::Saved(s) => Some(s),
            _ => None,
        })
        .unwrap();
    assert_eq!(record.members[0].state, "failed");
    assert_eq!(record.members[0].evidence.as_deref(), Some("incomplete"));
    assert_eq!(record.members[0].operation, "scan");
    let scope = handle
        .read_sandbox("study", true)
        .await
        .unwrap()
        .scope
        .unwrap();
    let (_, scoped) = handle.resolve_read_scope(&scope).await.unwrap();
    assert!(
        scoped
            .dataset_checkpoint(prefix.clone())
            .await
            .unwrap()
            .is_some()
    );
    assert_eq!(
        files(&root.path().join("live")),
        0,
        "inline values and captured scan inputs must not force live files"
    );
    assert_eq!(calls.load(Ordering::SeqCst), 0);
    handle.shutdown().await.unwrap();
    task.join().await.unwrap();
    recorder.shutdown().await.unwrap();
    writer.join().await.unwrap();
    worker.shutdown().await.unwrap();
    storage_task.join().await.unwrap();
    let (worker, storage_task) = storage();
    let mut history = FileHistory::open(
        &root.path().join("history"),
        ReadLimits::default(),
        HistoryDurability::FileAndDirectory,
    )
    .unwrap();
    let image = history.capture(HistoryCaptureLimits::default()).unwrap();
    let (recorder, writer) = spawn_recorder(history, RecorderLimits::default()).unwrap();
    let (base, calls) = workspace(definitions);
    let restored = session::restore(
        base,
        RecordingMode::Required(CallJournal::new(
            recorder.clone(),
            RequiredPersistence::FileSynced,
        )),
        Some(SessionStorage {
            worker: worker.clone(),
            auto_keep: AutoKeep::default(),
        }),
        image,
        CancellationToken::new(),
    )
    .await
    .unwrap();
    assert!(restored.report().problems.is_empty());
    let (handle, task) = restored
        .spawn(Arc::new(NoFiles), NonZeroUsize::new(2).unwrap())
        .unwrap();
    assert!(
        worker
            .dataset_checkpoint(prefix.clone())
            .await
            .unwrap()
            .is_some()
    );
    let before = files(&root.path().join("archive"));
    submit(
        &handle,
        "resume-study",
        ":transient {\n:scan resume $saved_partial > continued\n} > next",
    )
    .await;
    let data = handle.read_sandbox_export("next", true).await.unwrap();
    let scope = data.scope.unwrap();
    let (child, _) = handle.resolve_read_scope(&scope).await.unwrap();
    let observation = child.snapshot().await.unwrap();
    let node = &observation.names["continued"].node;
    let error = observation.execution.errors.get(node);
    // The original record limit stays cumulative; resume must read the kept checkpoint,
    // then stop at that real limit rather than using a new source or resetting allowances.
    assert!(
        observation.execution.evidence_values.contains_key(node),
        "{error:?}"
    );
    assert_eq!(
        files(&root.path().join("archive")),
        before,
        "new continuation remains temporary"
    );
    assert_eq!(calls.load(Ordering::SeqCst), 0);
    assert_eq!(
        read(&handle, "saved_partial.outputs").await,
        Data::Dataset(Arc::new(prefix))
    );
    handle.shutdown().await.unwrap();
    task.join().await.unwrap();
    recorder.shutdown().await.unwrap();
    writer.join().await.unwrap();
    worker.shutdown().await.unwrap();
    storage_task.join().await.unwrap();
}

#[tokio::test]
async fn an_unacknowledged_final_keep_is_queryable_but_never_claimed_as_saved() {
    use wes_adapters::journal::{Durability as HistoryDurability, FileHistory, ReadLimits};
    use wes_engine::{
        calls::{CallJournal, RequiredPersistence},
        history::{self, HistoryCaptureLimits, JournalEntry, JournalSink, Record},
        recording::{RecorderLimits, spawn_recorder},
    };
    struct RefuseFinal(FileHistory);
    impl JournalSink for RefuseFinal {
        fn append(
            &mut self,
            record: &Record,
        ) -> Result<history::AppendReceipt, history::RecordError> {
            if matches!(record, Record::Journal(JournalEntry::Saved(_))) {
                return Err(history::RecordError::Limit("synthetic final refusal"));
            }
            JournalSink::append(&mut self.0, record)
        }
        fn capture(
            &mut self,
            limits: HistoryCaptureLimits,
        ) -> Result<history::HistoryImage, history::RecordError> {
            self.0.capture(limits)
        }
    }
    let root = tempfile::tempdir().unwrap();
    let definitions = Arc::new(Definitions::default());
    let (worker, storage_task) = spawn_store(
        TieredValues::open(
            &root.path().join("live"),
            &root.path().join("archive"),
            Limits::default(),
            Durability::FileAndDirectory,
            None,
        )
        .unwrap(),
        StoreWorkerLimits::default(),
    )
    .unwrap();
    let (recorder, writer) = spawn_recorder(
        RefuseFinal(
            FileHistory::open(
                &root.path().join("history"),
                ReadLimits::default(),
                HistoryDurability::FileAndDirectory,
            )
            .unwrap(),
        ),
        RecorderLimits::default(),
    )
    .unwrap();
    let (base, calls) = workspace(definitions.clone());
    let (handle, task) = session::spawn_with_storage(
        base,
        RecordingMode::Required(CallJournal::new(
            recorder.clone(),
            RequiredPersistence::FileSynced,
        )),
        Arc::new(NoFiles),
        NonZeroUsize::new(2).unwrap(),
        SessionStorage {
            worker: worker.clone(),
            auto_keep: AutoKeep::default(),
        },
    )
    .unwrap();
    submit(
        &handle,
        "study",
        ":transient { numbers echo value:3 > value } > study",
    )
    .await;
    assert_eq!(read(&handle, "study.value").await, Data::Int(3));
    let input = SourceInput::new("save-once".into(), ":keep $study > unconfirmed".into()).unwrap();
    let failure = handle.submit(input.clone()).await.unwrap_err();
    let identity = match failure {
        session::SessionError::KeepIncomplete {
            identity,
            may_have_applied: true,
            ..
        } => identity,
        error => panic!("{error:?}"),
    };
    assert!(handle.submit(input).await.is_err());
    let pending = handle.read_sandbox("unconfirmed", true).await.unwrap();
    assert_eq!(pending.state.as_str(), "unconfirmed");
    assert!(format!("{:?}", pending.data).contains(&identity));
    assert!(
        handle
            .read_sandbox("unconfirmed.value", false)
            .await
            .is_err()
    );
    assert_eq!(
        files(&root.path().join("archive")),
        1,
        "payload retention can succeed before final history fails"
    );
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    let image = recorder
        .capture(HistoryCaptureLimits::default())
        .await
        .unwrap()
        .image()
        .clone();
    assert!(
        image
            .journal()
            .iter()
            .any(|e| matches!(e, JournalEntry::SaveIntent(_)))
    );
    assert!(
        !image
            .journal()
            .iter()
            .any(|e| matches!(e, JournalEntry::Saved(_)))
    );
    handle.shutdown().await.unwrap();
    task.join().await.unwrap();
    recorder.shutdown().await.unwrap();
    writer.join().await.unwrap();
    let (base, calls) = workspace(definitions);
    let restored = session::restore(
        base,
        RecordingMode::Ephemeral,
        Some(SessionStorage {
            worker: worker.clone(),
            auto_keep: AutoKeep::default(),
        }),
        image,
        CancellationToken::new(),
    )
    .await
    .unwrap();
    let (handle, task) = restored
        .spawn(Arc::new(NoFiles), NonZeroUsize::new(2).unwrap())
        .unwrap();
    assert_eq!(
        handle
            .read_sandbox("unconfirmed", true)
            .await
            .unwrap()
            .state
            .as_str(),
        "unconfirmed"
    );
    assert!(
        handle
            .read_sandbox("unconfirmed.value", false)
            .await
            .is_err()
    );
    assert_eq!(calls.load(Ordering::SeqCst), 0);
    handle.shutdown().await.unwrap();
    task.join().await.unwrap();
    worker.shutdown().await.unwrap();
    storage_task.join().await.unwrap();
}

struct BytesCounter(Arc<AtomicUsize>);
impl Invoker for BytesCounter {
    fn invoke(&self, call: Call, _: CancellationToken) -> InvocationFuture {
        self.0.fetch_add(1, Ordering::SeqCst);
        Box::pin(async move {
            let Data::Int(n) = call.arguments["value"].data() else {
                unreachable!()
            };
            Ok(wes_core::Value::new(
                Shape::Primitive(Primitive::Bytes),
                Data::Bytes(vec![*n as u8; 6000].into()),
                Default::default(),
            )
            .unwrap())
        })
    }
}
#[tokio::test]
async fn decoded_spill_prepares_a_later_consumer_without_changing_the_source_run() {
    use wes_engine::storage::ResidentMemory;
    let root = tempfile::tempdir().unwrap();
    let (worker, storage_task) = spawn_store(
        TieredValues::open(
            &root.path().join("live"),
            &root.path().join("archive"),
            Limits::default(),
            Durability::FileAndDirectory,
            None,
        )
        .unwrap(),
        StoreWorkerLimits::default(),
    )
    .unwrap();
    let worker = worker.with_resident_memory(ResidentMemory::new(20000, 10000));
    let calls = Arc::new(AtomicUsize::new(0));
    let mut base = Workspace::local(LocalScope::new("residency-fixture").unwrap())
        .with_sandbox_store(Arc::new(Definitions::default()));
    let mut cap = Capability::new(["bytes"], Shape::Primitive(Primitive::Bytes), Safety::Safe);
    cap.parameters.push(Parameter::new(
        "value",
        Shape::Primitive(Primitive::Int),
        true,
    ));
    base.register_provider(
        ProviderDescription::new("payload", [cap], vec![]).unwrap(),
        Arc::new(BytesCounter(calls.clone())),
    )
    .unwrap();
    let (handle, task) = session::spawn_with_storage(
        base,
        RecordingMode::Ephemeral,
        Arc::new(NoFiles),
        NonZeroUsize::new(2).unwrap(),
        SessionStorage {
            worker: worker.clone(),
            auto_keep: AutoKeep::default(),
        },
    )
    .unwrap();
    let reply = submit(&handle, "study", ":transient {\npayload bytes value:7 > a\npayload bytes value:8 > b\npayload bytes value:9 > c\n} > study").await;
    assert!(reply.sandbox.is_some(), "{:?}", reply.diagnostics);
    let running = handle.read_sandbox_export("study", false).await.unwrap();
    let scope = running.scope.clone().unwrap();
    drop(running);
    let (child, _) = handle.resolve_read_scope(&scope).await.unwrap();
    child.wait_idle().await.unwrap();
    let before = child.snapshot().await.unwrap();
    let a = before.names["a"].node.clone();
    let run = before.execution.runs[&a].clone();
    assert!(before.execution.stored_values.contains_key(&a));
    assert!(
        !before.execution.values.contains_key(&a),
        "the decoded payload must actually leave Runtime"
    );
    drop(before);
    submit(&child, "reuse", ":calc pure { return $a; } > reused").await;
    child.wait_idle().await.unwrap();
    let after = child.snapshot().await.unwrap();
    assert_eq!(after.execution.runs[&a], run);
    assert_eq!(calls.load(Ordering::SeqCst), 3);
    assert!(
        !after
            .execution
            .errors
            .contains_key(&after.names["reused"].node),
        "{:?}",
        after.execution.errors
    );
    drop(after);
    assert_eq!(
        read(&handle, "study.a").await,
        Data::Bytes(vec![7; 6000].into())
    );
    handle.shutdown().await.unwrap();
    task.join().await.unwrap();
    worker.shutdown().await.unwrap();
    storage_task.join().await.unwrap();
}

#[path = "transient_session/bounded.rs"]
mod bounded;
