use std::{
    num::NonZeroUsize,
    sync::{Arc, Mutex, mpsc},
    time::Duration,
};
use tokio::sync::oneshot;
use wes_core::environments::{CapturedSource, CapturedSources, Package};
use wes_engine::{
    calls::{CallJournal, RequiredPersistence},
    driver::CancellationToken,
    environments::{EnvironmentRecord, Registry},
    history::{
        AppendReceipt, HistoryCapture, HistoryCaptureLimits, HistoryCheckpoint, HistoryImage,
        JournalEntry, JournalSink, Persistence, Record, RecordError,
    },
    recording::{Recorder, RecorderLimits, RecorderTask, spawn_recorder},
    session::{
        self, EnvironmentPlan, RecordingMode, RestoreError, SessionError, SessionHandle,
        SessionTask,
    },
    type_sources::{TypeSourceError, TypeSourceReader},
    workspace::Workspace,
};

struct NoFiles;
impl TypeSourceReader for NoFiles {
    fn read(&self, _: &str, _: usize) -> Result<String, TypeSourceError> {
        panic!("environment evidence must not reopen source files")
    }
}
fn yaml(body: &str) -> String {
    format!("version: 1\ntargets: {{local: {{kind: local}}}}\nenvironments:\n{body}")
}
fn basic() -> String {
    yaml(
        "  base: {imports: {api: {source: {kind: spec, file: missing.json}, bind: {target: local}}}}\n  dev: {extends: {env: base, track: latest}}",
    )
}
fn evidence(yaml: &str, bytes: &str) -> CapturedSources {
    let mut sources = CapturedSources::default();
    for key in Package::parse(yaml).unwrap().required_sources() {
        sources
            .insert(key, CapturedSource::new("synthetic/v1", bytes).unwrap())
            .unwrap();
    }
    sources
}
async fn plan(handle: &SessionHandle, yaml: &str, bytes: &str) -> EnvironmentPlan {
    handle
        .plan_environments(yaml.into(), evidence(yaml, bytes))
        .await
        .unwrap()
}
fn spawn(mode: RecordingMode) -> (SessionHandle, SessionTask) {
    session::spawn(
        Workspace::local(wes_engine::providers::LocalScope::new("fixture").unwrap()),
        mode,
        Arc::new(NoFiles),
        NonZeroUsize::new(1).unwrap(),
    )
    .unwrap()
}
async fn stop(handle: SessionHandle, task: SessionTask) {
    handle.shutdown().await.unwrap();
    tokio::time::timeout(Duration::from_secs(5), task.join())
        .await
        .unwrap()
        .unwrap();
}
fn image(records: &[Record]) -> HistoryImage {
    let mut capture = HistoryCapture::new(HistoryCaptureLimits::default());
    for record in records {
        capture.push(record.clone()).unwrap();
    }
    capture.finish(HistoryCheckpoint {
        journal: AppendReceipt {
            persistence: Persistence::FileSynced,
            end_offset: records.len() as u64,
        },
        recovery: AppendReceipt {
            persistence: Persistence::FileSynced,
            end_offset: 0,
        },
    })
}
type Gate = (oneshot::Sender<()>, mpsc::Receiver<()>);
struct Sink {
    records: Arc<Mutex<Vec<Record>>>,
    gate: Option<Gate>,
    persistence: Persistence,
    fail: bool,
}
impl JournalSink for Sink {
    fn append(&mut self, record: &Record) -> Result<AppendReceipt, RecordError> {
        if let Some((entered, release)) = self.gate.take() {
            entered.send(()).unwrap();
            release.recv_timeout(Duration::from_secs(5)).unwrap();
        }
        if self.fail {
            return Err(RecordError::backend(
                "synthetic",
                false,
                std::io::Error::other("injected failure"),
            ));
        }
        let mut records = self.records.lock().unwrap();
        records.push(record.clone());
        Ok(AppendReceipt {
            persistence: self.persistence,
            end_offset: records.len() as u64,
        })
    }
    fn capture(&mut self, _: HistoryCaptureLimits) -> Result<HistoryImage, RecordError> {
        Ok(image(&self.records.lock().unwrap()))
    }
}
struct Fixture {
    handle: SessionHandle,
    task: SessionTask,
    recorder: Recorder,
    writer: RecorderTask,
    records: Arc<Mutex<Vec<Record>>>,
}
impl Fixture {
    fn new(gate: Option<Gate>, persistence: Persistence, fail: bool) -> Self {
        let records = Arc::new(Mutex::new(vec![]));
        let (recorder, writer) = spawn_recorder(
            Sink {
                records: records.clone(),
                gate,
                persistence,
                fail,
            },
            RecorderLimits::default(),
        )
        .unwrap();
        let (handle, task) = spawn(RecordingMode::Required(CallJournal::new(
            recorder.clone(),
            RequiredPersistence::FileSynced,
        )));
        Self {
            handle,
            task,
            recorder,
            writer,
            records,
        }
    }
    async fn stop(self) {
        stop(self.handle, self.task).await;
        self.recorder.shutdown().await.unwrap();
        self.writer.join().await.unwrap();
    }
}

#[tokio::test]
async fn plans_are_inert_bounded_and_noop_does_not_append() {
    let f = Fixture::new(None, Persistence::FileSynced, false);
    let text = basic();
    let first = plan(&f.handle, &text, "one").await;
    let second = plan(&f.handle, &text, "one").await;
    assert!(f.handle.environment_revisions().await.unwrap().is_empty());
    assert!(f.records.lock().unwrap().is_empty());
    assert!(matches!(
        f.handle
            .plan_environments(text.clone(), evidence(&text, "one"))
            .await,
        Err(SessionError::Capacity)
    ));
    drop(second);
    let third = plan(&f.handle, &text, "one").await;
    let revisions = first.revisions().clone();
    let publication = f.handle.apply_environments(first).await.unwrap();
    assert!(publication.recorded);
    assert_eq!(publication.changes.len(), 2);
    assert_eq!(f.handle.environment_revisions().await.unwrap(), revisions);
    assert!(matches!(
        f.handle.apply_environments(third).await,
        Err(SessionError::Environment(_))
    ));
    let noop = plan(&f.handle, &text, "one").await;
    assert!(noop.changes().is_empty());
    assert!(!f.handle.apply_environments(noop).await.unwrap().recorded);
    assert_eq!(f.records.lock().unwrap().len(), 1);
    f.stop().await;
}

#[tokio::test]
async fn foreign_plan_rejects_without_recording_and_shutdown_does_not_wait_for_held_plan() {
    let f = Fixture::new(None, Persistence::FileSynced, false);
    let (other, task) = spawn(RecordingMode::Ephemeral);
    let foreign = plan(&other, &basic(), "one").await;
    assert!(matches!(
        f.handle.apply_environments(foreign).await,
        Err(SessionError::Environment(_))
    ));
    assert!(f.records.lock().unwrap().is_empty());
    let held = plan(&other, &basic(), "one").await;
    stop(other, task).await;
    drop(held);
    f.stop().await;
}

#[tokio::test]
async fn append_ack_precedes_publication_and_checkpoint_joins_it() {
    let (entered, blocked) = oneshot::channel();
    let (release, released) = mpsc::channel();
    let f = Fixture::new(Some((entered, released)), Persistence::FileSynced, false);
    let pending = plan(&f.handle, &basic(), "one").await;
    let expected = pending.revisions().clone();
    let handle = f.handle.clone();
    let apply = tokio::spawn(async move { handle.apply_environments(pending).await });
    blocked.await.unwrap();
    assert!(f.handle.environment_revisions().await.unwrap().is_empty());
    assert!(f.handle.snapshot().await.unwrap().admission_pending);
    assert!(!apply.is_finished());
    assert!(matches!(
        f.handle
            .plan_environments(basic(), evidence(&basic(), "one"))
            .await,
        Err(SessionError::AdmissionBusy)
    ));
    let handle = f.handle.clone();
    let checkpoint = tokio::spawn(async move { handle.checkpoint().await });
    while !f.handle.snapshot().await.unwrap().checkpoint_pending {
        tokio::task::yield_now().await;
    }
    assert!(!checkpoint.is_finished());
    release.send(()).unwrap();
    assert!(apply.await.unwrap().unwrap().recorded);
    let checkpoint = checkpoint.await.unwrap().unwrap();
    assert_eq!(checkpoint.history().journal().len(), 1);
    assert_eq!(f.handle.environment_revisions().await.unwrap(), expected);
    assert!(matches!(
        f.handle
            .plan_environments(basic(), evidence(&basic(), "one"))
            .await,
        Err(SessionError::CheckpointBusy)
    ));
    drop(checkpoint);
    f.stop().await;
}

#[tokio::test]
async fn disconnected_apply_client_does_not_cancel_admitted_publication() {
    let (entered, blocked) = oneshot::channel();
    let (release, released) = mpsc::channel();
    let f = Fixture::new(Some((entered, released)), Persistence::FileSynced, false);
    let pending = plan(&f.handle, &basic(), "one").await;
    let expected = pending.revisions().clone();
    let handle = f.handle.clone();
    let client = tokio::spawn(async move { handle.apply_environments(pending).await });
    blocked.await.unwrap();
    client.abort();
    assert!(client.await.unwrap_err().is_cancelled());
    release.send(()).unwrap();
    f.handle.wait_idle().await.unwrap();
    assert_eq!(f.handle.environment_revisions().await.unwrap(), expected);
    assert_eq!(f.records.lock().unwrap().len(), 1);
    f.stop().await;
}

#[tokio::test]
async fn shutdown_joins_physical_append_and_record_remains_recoverable() {
    let (entered, blocked) = oneshot::channel();
    let (release, released) = mpsc::channel();
    let f = Fixture::new(Some((entered, released)), Persistence::FileSynced, false);
    let pending = plan(&f.handle, &basic(), "one").await;
    let expected = pending.revisions().clone();
    let handle = f.handle.clone();
    let apply = tokio::spawn(async move { handle.apply_environments(pending).await });
    blocked.await.unwrap();
    f.handle.shutdown().await.unwrap();
    let joined = tokio::spawn(f.task.join());
    assert!(f.handle.environment_revisions().await.unwrap().is_empty());
    assert!(!joined.is_finished());
    release.send(()).unwrap();
    assert!(matches!(apply.await.unwrap(), Err(SessionError::Stopped)));
    joined.await.unwrap().unwrap();
    let history = image(&f.records.lock().unwrap());
    let restored = session::restore(
        Workspace::local(wes_engine::providers::LocalScope::new("fixture").unwrap()),
        RecordingMode::Ephemeral,
        None,
        history,
        CancellationToken::new(),
    )
    .await
    .unwrap();
    assert_eq!(restored.workspace().environments().revisions(), expected);
    f.recorder.shutdown().await.unwrap();
    f.writer.join().await.unwrap();
}

#[tokio::test]
async fn failed_or_insufficient_persistence_never_publishes_and_fails_closed() {
    for (persistence, fail) in [
        (Persistence::FileSynced, true),
        (Persistence::Volatile, false),
    ] {
        let f = Fixture::new(None, persistence, fail);
        let pending = plan(&f.handle, &basic(), "one").await;
        assert!(matches!(
            f.handle.apply_environments(pending).await,
            Err(SessionError::Recording)
        ));
        assert!(f.handle.environment_revisions().await.unwrap().is_empty());
        assert!(matches!(
            f.handle
                .plan_environments(basic(), evidence(&basic(), "one"))
                .await,
            Err(SessionError::Recording)
        ));
        assert!(matches!(
            f.handle.checkpoint().await,
            Err(SessionError::Recording)
        ));
        f.stop().await;
    }
}

#[tokio::test]
async fn malformed_or_oversized_plans_release_credit_and_leave_state_unchanged() {
    let (handle, task) = spawn(RecordingMode::Ephemeral);
    for _ in 0..3 {
        assert!(matches!(
            handle
                .plan_environments("version: 9".into(), CapturedSources::default())
                .await,
            Err(SessionError::Environment(_))
        ));
    }
    assert!(matches!(
        handle
            .plan_environments("x".repeat(1_048_577), CapturedSources::default())
            .await,
        Err(SessionError::Capacity)
    ));
    assert!(handle.environment_revisions().await.unwrap().is_empty());
    let pending = plan(&handle, &basic(), "one").await;
    assert!(!handle.apply_environments(pending).await.unwrap().recorded);
    stop(handle, task).await;
}

fn record(registry: &mut Registry, yaml: String, bytes: &str) -> EnvironmentRecord {
    let sources = evidence(&yaml, bytes);
    let package = Package::parse(&yaml).unwrap();
    let before = registry.revisions();
    let plan = registry.plan(&package, &sources).unwrap();
    registry.apply(plan).unwrap();
    EnvironmentRecord::new(
        uuid::Uuid::new_v4().to_string(),
        yaml,
        sources,
        before,
        registry.revisions(),
    )
    .unwrap()
}
fn entry(record: EnvironmentRecord) -> Record {
    Record::Journal(JournalEntry::Environments(record))
}

#[tokio::test]
async fn chronological_replay_preserves_historical_pins_and_exact_duplicates_are_inert() {
    let mut registry = Registry::default();
    let first = record(&mut registry, basic(), "one");
    let pin = registry.revisions()["base"];
    let pinned = format!(
        "{}\n  prod: {{extends: {{env: base, revision: '{pin}'}}}}",
        basic()
    );
    let second = record(&mut registry, pinned.clone(), "one");
    let third = record(&mut registry, pinned, "two");
    let history = image(&[
        entry(first),
        entry(second.clone()),
        entry(third),
        entry(second),
    ]);
    let restored = session::restore(
        Workspace::local(wes_engine::providers::LocalScope::new("fixture").unwrap()),
        RecordingMode::Ephemeral,
        None,
        history,
        CancellationToken::new(),
    )
    .await
    .unwrap();
    assert_eq!(restored.report().environments, 3);
    assert_eq!(restored.report().duplicate_environments, 1);
    assert_eq!(
        restored.workspace().environments().revisions(),
        registry.revisions()
    );
    assert_eq!(
        restored
            .workspace()
            .environments()
            .inspect("prod")
            .unwrap()
            .imports()["api"]
            .source()
            .bytes(),
        "one"
    );
    assert_eq!(
        restored
            .workspace()
            .environments()
            .inspect("dev")
            .unwrap()
            .imports()["api"]
            .source()
            .bytes(),
        "two"
    );
    let (handle, task) = restored
        .spawn(Arc::new(NoFiles), NonZeroUsize::new(1).unwrap())
        .unwrap();
    handle.wait_idle().await.unwrap();
    assert!(handle.snapshot().await.unwrap().names.is_empty());
    stop(handle, task).await;
}

#[tokio::test]
async fn missing_reordered_tampered_and_conflicting_evidence_rejects_without_partial_session() {
    let mut registry = Registry::default();
    let first = record(&mut registry, basic(), "one");
    let second = record(&mut registry, basic(), "two");
    let tampered = EnvironmentRecord::new(
        first.id().into(),
        first.yaml().into(),
        first.sources().clone(),
        first.before().clone(),
        second.after().clone(),
    )
    .unwrap();
    for records in [
        vec![entry(second.clone())],
        vec![entry(second), entry(first.clone())],
        vec![entry(tampered.clone())],
    ] {
        let result = session::restore(
            Workspace::local(wes_engine::providers::LocalScope::new("fixture").unwrap()),
            RecordingMode::Ephemeral,
            None,
            image(&records),
            CancellationToken::new(),
        )
        .await;
        assert!(matches!(result, Err(RestoreError::Environment { .. })));
    }
    let result = session::restore(
        Workspace::local(wes_engine::providers::LocalScope::new("fixture").unwrap()),
        RecordingMode::Ephemeral,
        None,
        image(&[entry(first), entry(tampered)]),
        CancellationToken::new(),
    )
    .await;
    assert!(matches!(result, Err(RestoreError::ConflictingEnvironment)));
}

#[tokio::test]
async fn restored_registry_has_fresh_plan_ownership_even_with_identical_revisions() {
    let f = Fixture::new(None, Persistence::FileSynced, false);
    let pending = plan(&f.handle, &basic(), "one").await;
    f.handle.apply_environments(pending).await.unwrap();
    let old = plan(&f.handle, &basic(), "two").await;
    let history = image(&f.records.lock().unwrap());
    let restored = session::restore(
        Workspace::local(wes_engine::providers::LocalScope::new("fixture").unwrap()),
        RecordingMode::Ephemeral,
        None,
        history,
        CancellationToken::new(),
    )
    .await
    .unwrap();
    let (handle, task) = restored
        .spawn(Arc::new(NoFiles), NonZeroUsize::new(1).unwrap())
        .unwrap();
    assert!(matches!(
        handle.apply_environments(old).await,
        Err(SessionError::Environment(_))
    ));
    stop(handle, task).await;
    f.stop().await;
}

#[tokio::test]
async fn environment_observation_does_not_emit_self_triggering_update_notifications() {
    let (handle, task) = spawn(RecordingMode::Ephemeral);
    let mut updates = handle.subscribe_updates().unwrap();
    for _ in 0..3 {
        assert!(handle.environment_revisions().await.unwrap().is_empty());
    }
    assert!(matches!(
        updates.try_recv(),
        Err(tokio::sync::broadcast::error::TryRecvError::Empty)
    ));
    stop(handle, task).await;
}

#[tokio::test]
async fn definitions_coexist_with_source_history_without_reexecuting_held_nodes() {
    use std::sync::atomic::{AtomicUsize, Ordering};
    use wes_core::{
        Data, Shape,
        capability::{Capability, ProviderDescription, Safety},
    };
    use wes_engine::{
        providers::{Call, InvocationFuture, Invoker},
        source::SourceInput,
    };
    struct Counter(Arc<AtomicUsize>);
    impl Invoker for Counter {
        fn invoke(&self, _: Call, _: CancellationToken) -> InvocationFuture {
            self.0.fetch_add(1, Ordering::SeqCst);
            Box::pin(async {
                Ok(wes_core::Value::new(Shape::Unknown, Data::Int(7), Default::default()).unwrap())
            })
        }
    }
    fn workspace(calls: Arc<AtomicUsize>) -> Workspace {
        let mut workspace =
            Workspace::local(wes_engine::providers::LocalScope::new("fixture").unwrap());
        workspace
            .register_provider(
                ProviderDescription::new(
                    "fixture",
                    [Capability::new(["count"], Shape::Unknown, Safety::Safe)],
                    vec![],
                )
                .unwrap(),
                Arc::new(Counter(calls)),
            )
            .unwrap();
        workspace
    }
    let calls = Arc::new(AtomicUsize::new(0));
    let records = Arc::new(Mutex::new(vec![]));
    let (recorder, writer) = spawn_recorder(
        Sink {
            records: records.clone(),
            gate: None,
            persistence: Persistence::FileSynced,
            fail: false,
        },
        RecorderLimits::default(),
    )
    .unwrap();
    let (handle, task) = session::spawn(
        workspace(calls.clone()),
        RecordingMode::Required(CallJournal::new(
            recorder.clone(),
            RequiredPersistence::FileSynced,
        )),
        Arc::new(NoFiles),
        NonZeroUsize::new(1).unwrap(),
    )
    .unwrap();
    let first = plan(&handle, &basic(), "one").await;
    handle.apply_environments(first).await.unwrap();
    handle
        .submit(SourceInput::new("call-once".into(), "fixture count > answer".into()).unwrap())
        .await
        .unwrap();
    handle.wait_idle().await.unwrap();
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    let answer = handle.snapshot().await.unwrap().names["answer"].clone();
    let second = plan(&handle, &basic(), "two").await;
    handle.apply_environments(second).await.unwrap();
    handle.wait_idle().await.unwrap();
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    assert_eq!(handle.snapshot().await.unwrap().names["answer"], answer);
    let history = image(&records.lock().unwrap());
    let expected = handle.environment_revisions().await.unwrap();
    let restored = session::restore(
        workspace(calls.clone()),
        RecordingMode::Ephemeral,
        None,
        history,
        CancellationToken::new(),
    )
    .await
    .unwrap();
    assert_eq!(restored.report().environments, 2);
    assert_eq!(restored.report().commands, 1);
    let (loaded, loaded_task) = restored
        .spawn(Arc::new(NoFiles), NonZeroUsize::new(1).unwrap())
        .unwrap();
    loaded.wait_idle().await.unwrap();
    assert_eq!(loaded.environment_revisions().await.unwrap(), expected);
    assert_eq!(loaded.snapshot().await.unwrap().names["answer"], answer);
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    stop(loaded, loaded_task).await;
    stop(handle, task).await;
    recorder.shutdown().await.unwrap();
    writer.join().await.unwrap();
}

#[tokio::test]
async fn environment_target_and_authority_failures_retain_distinct_safe_causes() {
    use wes_engine::source::SourceInput;
    let (handle, task) = spawn(RecordingMode::Ephemeral);
    let proposed = plan(&handle, &basic(), "synthetic").await;
    handle.apply_environments(proposed).await.unwrap();
    for (index, source, reason) in [
        (0, ":env use \"missing\"", "Unknown environment name."),
        (
            1,
            ":env disable \"base\"",
            "environment authority is unavailable",
        ),
        (2, ":env enable \"missing\"", "Unknown environment name."),
    ] {
        let result = handle
            .submit(SourceInput::new(format!("cause-{index}"), source.into()).unwrap())
            .await
            .unwrap();
        let diagnostic = &result.diagnostics.diagnostics[0];
        assert_eq!(diagnostic.message, reason);
        assert_eq!(diagnostic.public_summary(), reason);
        if index != 1 {
            assert_eq!(
                source[diagnostic.span.start()..diagnostic.span.end()].trim_matches('"'),
                "missing"
            );
        }
    }
    stop(handle, task).await;
}
