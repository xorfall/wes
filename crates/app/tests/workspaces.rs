use wes::{ApplicationHandle, Config};
#[path = "workspaces/backend.rs"]
mod backend;
#[path = "workspaces/concurrent.rs"]
mod concurrent;
#[path = "workspaces/deletion.rs"]
mod deletion;
#[path = "workspaces/retention.rs"]
mod retention;
#[path = "workspaces/source_control.rs"]
mod source_control;
use std::{
    num::NonZeroUsize,
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
    time::Duration,
};
use wes_adapters::{codec::Limits, journal::Durability, storage::TieredValues};
use wes_core::{
    Data, Shape,
    capability::{Capability, Parameter, ProviderDescription, Safety},
};
use wes_engine::{
    driver::CancellationToken,
    providers::{Call, InvocationFuture, Invoker},
    session::{SessionStorage, SubmissionResult},
    source::SourceInput,
    storage::{AutoKeep, StoreWorkerLimits, spawn_store},
    type_sources::{TypeSourceError, TypeSourceReader},
    workspace::{Workspace, WorkspaceName},
};
struct NoFiles;
impl TypeSourceReader for NoFiles {
    fn read(&self, _: &str, _: usize) -> Result<String, TypeSourceError> {
        panic!("restoration must not read live source files")
    }
}
struct Echo(Arc<AtomicUsize>);
impl Invoker for Echo {
    fn invoke(&self, call: Call, _: CancellationToken) -> InvocationFuture {
        self.0.fetch_add(1, Ordering::SeqCst);
        Box::pin(async move { Ok(call.arguments["value"].clone()) })
    }
}
fn temp() -> tempfile::TempDir {
    let mut builder = tempfile::Builder::new();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        builder.permissions(std::fs::Permissions::from_mode(0o700));
    }
    builder.tempdir().unwrap()
}
fn config(
    root: &std::path::Path,
    calls: Arc<AtomicUsize>,
    drift: Arc<AtomicBool>,
    storage: Option<SessionStorage>,
) -> Config {
    Config {
        directory: root.join("workspaces"),
        initial: WorkspaceName::new("default".into()).unwrap(),
        durability: Durability::File,
        max_streams: wes_engine::driver::DEFAULT_MAX_STREAMS,
        concurrency: NonZeroUsize::new(2).unwrap(),
        type_reader: Arc::new(NoFiles),
        storage,
        workspace: Arc::new(move || {
            let mut workspace =
                Workspace::local(wes_engine::providers::LocalScope::new("fixture").unwrap());
            if !drift.load(Ordering::SeqCst) {
                let mut capability = Capability::new(["echo"], Shape::Unknown, Safety::Safe);
                capability
                    .parameters
                    .push(Parameter::new("value", Shape::Unknown, true));
                workspace.register_provider(
                    ProviderDescription::new("catalog", [capability], vec![]).unwrap(),
                    Arc::new(Echo(calls.clone())),
                )?;
            }
            Ok(workspace)
        }),
    }
}
async fn submit(handle: &ApplicationHandle, cell: &str, text: &str) -> Arc<SubmissionResult> {
    tokio::time::timeout(
        Duration::from_secs(10),
        handle.submit(SourceInput::new(cell.into(), text.into()).unwrap()),
    )
    .await
    .unwrap()
    .unwrap()
}
fn accepted(reply: &SubmissionResult) {
    assert!(!reply.accepted.is_empty(), "{:?}", reply.diagnostics);
    assert!(
        !reply
            .diagnostics
            .diagnostics
            .iter()
            .any(|d| d.severity == wes_language::Severity::Error),
        "{:?}",
        reply.diagnostics
    );
}
async fn idle(handle: &ApplicationHandle) {
    tokio::time::timeout(
        Duration::from_secs(10),
        handle.current().unwrap().session.wait_idle(),
    )
    .await
    .unwrap()
    .unwrap();
}

#[tokio::test]
async fn repeated_saves_and_startup_collect_only_superseded_generations_without_execution() {
    let root = temp();
    let calls = Arc::new(AtomicUsize::new(0));
    let drift = Arc::new(AtomicBool::new(false));
    let configuration = || config(root.path(), calls.clone(), drift.clone(), None);
    let (app, task) = wes::open(configuration()).await.unwrap();
    accepted(
        submit(&app, "source", "catalog echo value:once > result")
            .await
            .as_ref(),
    );
    idle(&app).await;
    let generations = || {
        std::fs::read_dir(root.path().join("workspaces"))
            .unwrap()
            .filter(|entry| {
                entry
                    .as_ref()
                    .unwrap()
                    .file_name()
                    .to_str()
                    .unwrap()
                    .starts_with("generation-")
            })
            .count()
    };
    for i in 0..8 {
        accepted(
            submit(&app, &format!("save-{i}"), ":workspace save \"copy\"")
                .await
                .as_ref(),
        );
        assert!(generations() <= 3); // active default, named copy, latest superseded copy
    }
    accepted(
        submit(&app, "load", ":workspace load \"copy\"")
            .await
            .as_ref(),
    );
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    app.shutdown().await;
    task.join().await.unwrap();
    let (app, task) = wes::open(configuration()).await.unwrap();
    assert_eq!(generations(), 2);
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    app.shutdown().await;
    task.join().await.unwrap();
}

#[tokio::test]
async fn source_save_load_and_restart_preserve_retained_values_without_reexecuting_providers() {
    let root = temp();
    let calls = Arc::new(AtomicUsize::new(0));
    let drift = Arc::new(AtomicBool::new(false));
    let values = TieredValues::open(
        &root.path().join("live"),
        &root.path().join("archive"),
        Limits::default(),
        Durability::File,
        None,
    )
    .unwrap();
    let (worker, worker_task) = spawn_store(values, StoreWorkerLimits::default()).unwrap();
    let storage = Some(SessionStorage {
        worker: worker.clone(),
        auto_keep: AutoKeep::default(),
    });
    let (handle, task) = wes::open(config(
        root.path(),
        calls.clone(),
        drift.clone(),
        storage.clone(),
    ))
    .await
    .unwrap();
    let original_session = handle.current().unwrap();
    let original = submit(&handle, "original", "catalog echo value:first > answer").await;
    accepted(&original);
    idle(&handle).await;
    accepted(
        submit(&handle, "save", ":workspace save \"kept\"")
            .await
            .as_ref(),
    );
    accepted(
        submit(&handle, "extra", "catalog echo value:second > extra")
            .await
            .as_ref(),
    );
    idle(&handle).await;
    assert_eq!(calls.load(Ordering::SeqCst), 2);
    accepted(
        submit(&handle, "load", ":workspace load \"kept\"")
            .await
            .as_ref(),
    );
    let current = handle.current().unwrap();
    assert_eq!(current.name.as_str(), "kept");
    assert_ne!(current.generation, original_session.generation);
    let snapshot = current.session.snapshot().await.unwrap();
    assert!(snapshot.names.contains_key("answer"));
    assert!(!snapshot.names.contains_key("extra"));
    assert_eq!(
        snapshot.execution.values[&original.nodes[0]].data(),
        &Data::Text("first".into())
    );
    assert_eq!(calls.load(Ordering::SeqCst), 2);
    assert_eq!(snapshot.restoration.unwrap().values, 1);
    // Loading selects another workspace; the original live owner remains available.
    assert!(
        original_session
            .session
            .snapshot()
            .await
            .unwrap()
            .names
            .contains_key("extra")
    );
    accepted(
        submit(&handle, "later", "catalog echo value:third > later")
            .await
            .as_ref(),
    );
    idle(&handle).await;
    accepted(
        submit(&handle, "save-self", ":workspace save \"kept\"")
            .await
            .as_ref(),
    );
    let old_generation = handle.current().unwrap().generation;
    accepted(
        submit(&handle, "reload", ":workspace load \"kept\"")
            .await
            .as_ref(),
    );
    assert_ne!(handle.current().unwrap().generation, old_generation);
    assert!(
        handle
            .current()
            .unwrap()
            .session
            .snapshot()
            .await
            .unwrap()
            .names
            .contains_key("later")
    );
    assert_eq!(calls.load(Ordering::SeqCst), 3);
    handle.shutdown().await;
    task.join().await.unwrap();
    let mut next = config(root.path(), calls.clone(), drift, storage);
    next.initial = WorkspaceName::new("kept".into()).unwrap();
    let (handle, task) = wes::open(next).await.unwrap();
    let snapshot = handle.current().unwrap().session.snapshot().await.unwrap();
    assert!(snapshot.names.contains_key("later"));
    assert_eq!(snapshot.restoration.unwrap().values, 2);
    assert_eq!(calls.load(Ordering::SeqCst), 3);
    handle.shutdown().await;
    task.join().await.unwrap();
    worker.shutdown().await.unwrap();
    worker_task.join().await.unwrap();
}

#[tokio::test]
async fn missing_corrupt_or_drifted_load_keeps_current_session_usable_and_releases_candidate_lock()
{
    let root = temp();
    let calls = Arc::new(AtomicUsize::new(0));
    let drift = Arc::new(AtomicBool::new(false));
    let (handle, task) = wes::open(config(root.path(), calls.clone(), drift.clone(), None))
        .await
        .unwrap();
    accepted(
        submit(&handle, "original", "catalog echo value:first > answer")
            .await
            .as_ref(),
    );
    idle(&handle).await;
    accepted(
        submit(&handle, "save", ":workspace save \"kept\"")
            .await
            .as_ref(),
    );
    let original = handle.current().unwrap();
    assert!(
        submit(&handle, "missing", ":workspace load \"missing\"")
            .await
            .accepted
            .is_empty()
    );
    assert_eq!(handle.current().unwrap().generation, original.generation);
    drift.store(true, Ordering::SeqCst);
    assert!(
        submit(&handle, "drift", ":workspace load \"kept\"")
            .await
            .accepted
            .is_empty()
    );
    assert_eq!(handle.current().unwrap().generation, original.generation);
    assert!(
        handle
            .current()
            .unwrap()
            .session
            .snapshot()
            .await
            .unwrap()
            .names
            .contains_key("answer")
    );
    drift.store(false, Ordering::SeqCst);
    accepted(
        submit(&handle, "continue", "catalog echo value:second > extra")
            .await
            .as_ref(),
    );
    idle(&handle).await;
    accepted(
        submit(&handle, "retry", ":workspace load \"kept\"")
            .await
            .as_ref(),
    ); // Failed candidate released the writer lock.
    assert_eq!(calls.load(Ordering::SeqCst), 2);
    accepted(
        submit(&handle, "save-broken", ":workspace save \"broken\"")
            .await
            .as_ref(),
    );
    let manifest =
        std::fs::read_to_string(root.path().join("workspaces/workspace-62726f6b656e")).unwrap();
    let generation = manifest.lines().last().unwrap();
    std::fs::write(
        root.path()
            .join(format!("workspaces/generation-{generation}/journal.jsonl")),
        b"{broken\n",
    )
    .unwrap();
    let before = handle.current().unwrap().generation;
    assert!(
        submit(&handle, "corrupt", ":workspace load \"broken\"")
            .await
            .accepted
            .is_empty()
    );
    assert_eq!(handle.current().unwrap().generation, before);
    handle.shutdown().await;
    task.join().await.unwrap();
}

#[tokio::test]
async fn normal_validation_and_exact_save_retries_do_not_duplicate_effects() {
    let root = temp();
    let calls = Arc::new(AtomicUsize::new(0));
    let (handle, task) = wes::open(config(
        root.path(),
        calls.clone(),
        Arc::new(AtomicBool::new(false)),
        None,
    ))
    .await
    .unwrap();
    assert!(
        submit(
            &handle,
            "mixed",
            ":workspace save \"forbidden\"\ncatalog echo value:never"
        )
        .await
        .accepted
        .is_empty()
    );
    assert!(
        submit(&handle, "path", ":workspace save \"../outside\"")
            .await
            .accepted
            .is_empty()
    );
    assert_eq!(calls.load(Ordering::SeqCst), 0);
    accepted(
        submit(&handle, "save", ":workspace save \"kept\"")
            .await
            .as_ref(),
    );
    let saved = std::fs::read_dir(root.path().join("workspaces"))
        .unwrap()
        .count();
    accepted(
        submit(&handle, "save", ":workspace save \"kept\"")
            .await
            .as_ref(),
    );
    assert_eq!(
        std::fs::read_dir(root.path().join("workspaces"))
            .unwrap()
            .count(),
        saved
    );
    assert!(
        handle
            .submit(SourceInput::new("save".into(), ":workspace save \"other\"".into()).unwrap())
            .await
            .is_err()
    );
    handle.shutdown().await;
    task.join().await.unwrap();
}

fn gated_config(
    root: &std::path::Path,
) -> (
    Config,
    Arc<AtomicBool>,
    tokio::sync::oneshot::Receiver<()>,
    std::sync::mpsc::Sender<()>,
    Arc<AtomicUsize>,
) {
    let mut config = config(
        root,
        Arc::new(AtomicUsize::new(0)),
        Arc::new(AtomicBool::new(false)),
        None,
    );
    let factory = config.workspace.clone();
    let enabled = Arc::new(AtomicBool::new(false));
    let gate = enabled.clone();
    let (entered, blocked) = tokio::sync::oneshot::channel();
    let (release, released) = std::sync::mpsc::channel();
    let entered = std::sync::Mutex::new(Some(entered));
    let released = std::sync::Mutex::new(released);
    let calls = Arc::new(AtomicUsize::new(0));
    let count = calls.clone();
    config.workspace = Arc::new(move || {
        count.fetch_add(1, Ordering::SeqCst);
        if gate.load(Ordering::SeqCst) {
            if let Some(entered) = entered.lock().unwrap().take() {
                let _ = entered.send(());
            }
            released
                .lock()
                .unwrap()
                .recv_timeout(Duration::from_secs(10))
                .unwrap();
        }
        factory()
    });
    (config, enabled, blocked, release, calls)
}
#[tokio::test]
async fn disconnected_load_and_duplicate_requests_share_one_joined_switch() {
    let root = temp();
    let (config, enabled, blocked, release, factory_calls) = gated_config(root.path());
    let (handle, task) = wes::open(config).await.unwrap();
    accepted(
        submit(&handle, "save", ":workspace save \"kept\"")
            .await
            .as_ref(),
    );
    enabled.store(true, Ordering::SeqCst);
    let h = handle.clone();
    let request = tokio::spawn(async move {
        h.submit(SourceInput::new("load".into(), ":workspace load \"kept\"".into()).unwrap())
            .await
    });
    blocked.await.unwrap();
    let current = handle.current().unwrap();
    assert_eq!(current.name.as_str(), "default");
    assert!(!current.session.snapshot().await.unwrap().checkpoint_pending);
    assert!(
        handle
            .submit(SourceInput::new("load".into(), ":workspace load \"other\"".into()).unwrap())
            .await
            .is_err()
    );
    let h = handle.clone();
    let duplicate = tokio::spawn(async move {
        h.submit(SourceInput::new("load".into(), ":workspace load \"kept\"".into()).unwrap())
            .await
    });
    request.abort();
    let _ = request.await;
    release.send(()).unwrap();
    accepted(
        tokio::time::timeout(Duration::from_secs(10), duplicate)
            .await
            .unwrap()
            .unwrap()
            .unwrap()
            .as_ref(),
    );
    assert_eq!(handle.current().unwrap().name.as_str(), "kept");
    assert_eq!(factory_calls.load(Ordering::SeqCst), 2); // Startup and exactly one load.
    handle.shutdown().await;
    task.join().await.unwrap();
}
#[tokio::test]
async fn shutdown_cancels_candidate_startup_and_joins_the_entered_worker() {
    let root = temp();
    let (config, enabled, blocked, release, _) = gated_config(root.path());
    let (handle, task) = wes::open(config).await.unwrap();
    accepted(
        submit(&handle, "save", ":workspace save \"kept\"")
            .await
            .as_ref(),
    );
    enabled.store(true, Ordering::SeqCst);
    let h = handle.clone();
    let pending = tokio::spawn(async move {
        h.submit(SourceInput::new("load".into(), ":workspace load \"kept\"".into()).unwrap())
            .await
    });
    blocked.await.unwrap();
    handle.shutdown().await;
    let mut joining = tokio::spawn(task.join());
    assert!(
        tokio::time::timeout(Duration::from_millis(10), &mut joining)
            .await
            .is_err()
    );
    release.send(()).unwrap();
    tokio::time::timeout(Duration::from_secs(10), joining)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    let result = pending.await.unwrap();
    assert!(result.is_err() || result.unwrap().accepted.is_empty());
    assert!(handle.current().is_err());
    // Both the candidate and original history locks have been released.
    let (handle, task) = wes::open(config_for_reopen(root.path())).await.unwrap();
    handle.shutdown().await;
    task.join().await.unwrap();
}
fn config_for_reopen(root: &std::path::Path) -> Config {
    config(
        root,
        Arc::new(AtomicUsize::new(0)),
        Arc::new(AtomicBool::new(false)),
        None,
    )
}

#[tokio::test]
async fn saved_names_are_captured_at_execution_entry_and_refresh_without_provider_work() {
    let root = temp();
    let calls = Arc::new(AtomicUsize::new(0));
    let (handle, task) = wes::open(config(
        root.path(),
        calls.clone(),
        Arc::new(AtomicBool::new(false)),
        None,
    ))
    .await
    .unwrap();
    let list = submit(&handle, "list", ":list workspaces > saved").await;
    accepted(&list);
    idle(&handle).await;
    let names = || {
        Data::List(vec![
            Data::Text("default".into()),
            Data::Text("kept".into()),
        ])
    };
    accepted(
        submit(&handle, "save", ":workspace save \"kept\"")
            .await
            .as_ref(),
    );
    let snapshot = handle.current().unwrap().session.snapshot().await.unwrap();
    assert_eq!(
        snapshot.execution.values[&list.nodes[0]].data(),
        &Data::List(vec![Data::Text("default".into())])
    );
    accepted(submit(&handle, "refresh", ":refresh $saved").await.as_ref());
    idle(&handle).await;
    let snapshot = handle.current().unwrap().session.snapshot().await.unwrap();
    assert_eq!(snapshot.execution.values[&list.nodes[0]].data(), &names());
    assert_eq!(calls.load(Ordering::SeqCst), 0);
    handle.shutdown().await;
    task.join().await.unwrap();
}
