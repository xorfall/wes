//! Reproducible disk-growth measurement. All state is synthetic and temporary.
use std::{
    collections::BTreeSet,
    num::NonZeroUsize,
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
};
use wes::{ApplicationHandle, Config};
use wes_adapters::{codec::Limits, journal::Durability, storage::TieredValues};
use wes_core::{
    Shape,
    capability::{Capability, ProviderDescription, Safety},
};
use wes_engine::{
    driver::CancellationToken,
    history::JournalEntry,
    providers::{Call, InvocationFuture, Invoker},
    session::SessionStorage,
    source::SourceInput,
    storage::{AutoKeep, StoreWorkerLimits, spawn_store},
    trace::{TraceSink, text},
    type_sources::{TypeSourceError, TypeSourceReader},
    workspace::{Workspace, WorkspaceName},
};

struct Fixture(Arc<AtomicUsize>);
impl Invoker for Fixture {
    fn supports_trace(&self, profile: &str) -> bool {
        profile == "binary"
    }
    fn invoke(&self, _: Call, _: CancellationToken) -> InvocationFuture {
        let n = self.0.fetch_add(1, Ordering::SeqCst) + 1;
        Box::pin(async move { Ok(text(format!("synthetic response {n}"))) })
    }
    fn invoke_observed(
        &self,
        call: Call,
        cancel: CancellationToken,
        trace: TraceSink,
    ) -> InvocationFuture {
        trace.emit("synthetic", text("fixture entry; no external service"));
        self.invoke(call, cancel)
    }
}
struct NoFiles;
impl TypeSourceReader for NoFiles {
    fn read(&self, _: &str, _: usize) -> Result<String, TypeSourceError> {
        panic!("unexpected live file read")
    }
}
async fn sample(
    app: &ApplicationHandle,
    runs: usize,
    store: &wes_engine::storage::StoreWorker,
) -> serde_json::Value {
    let session = app.current().unwrap().session;
    session.wait_idle().await.unwrap();
    let observation = session.observe().await.unwrap();
    let checkpoint = session.checkpoint().await.unwrap();
    let history = checkpoint.history();
    let mut identities = BTreeSet::new();
    let mut traces = 0;
    let mut trace_bytes = 0;
    for entry in history.journal() {
        match entry {
            JournalEntry::Observed(e) => {
                if let Some(run) = e.run() {
                    identities.insert(run.clone());
                }
            }
            JournalEntry::Trace(t) => {
                traces += 1;
                trace_bytes +=
                    wes_adapters::codec::encode_display_value(&t.value, Limits::default())
                        .unwrap()
                        .len();
            }
            _ => (),
        }
    }
    let usage = store.retention_usage().await.unwrap();
    let value = serde_json::json!({
        "executions":runs, "graphNodes":observation.state.execution.graph.len(),
        "submissionAttempts":observation.cells.len(), "recordedRunIdentities":identities.len(),
        "journalBytes":history.checkpoint().journal.end_offset,
        "recoveryBytes":history.checkpoint().recovery.end_offset,
        "decodedHistoryChargeBytes":history.charged_bytes(),
        "traceRecords":traces, "traceDisplayBytes":trace_bytes,
        "payloadClasses":usage.classes.iter().map(|(class,count,bytes)| serde_json::json!({"class":class.as_str(),"count":count,"bytes":bytes})).collect::<Vec<_>>()
    });
    assert_eq!(identities.len(), runs);
    assert_eq!(observation.state.execution.graph.len(), 1);
    checkpoint.resume().await;
    value
}

#[tokio::main]
async fn main() {
    let root = tempfile::tempdir().unwrap();
    let calls = Arc::new(AtomicUsize::new(0));
    let values = TieredValues::open(
        &root.path().join("live"),
        &root.path().join("archive"),
        Limits::default(),
        Durability::File,
        None,
    )
    .unwrap();
    let (store, storage_task) = spawn_store(values, StoreWorkerLimits::default()).unwrap();
    let counted = calls.clone();
    let config = || Config {
        directory: root.path().join("workspaces"),
        initial: WorkspaceName::new("measurement".into()).unwrap(),
        durability: Durability::File,
        max_streams: wes_engine::driver::DEFAULT_MAX_STREAMS,
        concurrency: NonZeroUsize::new(1).unwrap(),
        type_reader: Arc::new(NoFiles),
        storage: Some(SessionStorage {
            worker: store.clone(),
            auto_keep: AutoKeep::default(),
        }),
        workspace: Arc::new({
            let counted = counted.clone();
            move || {
                let mut workspace = Workspace::new();
                workspace.register_provider(
                    ProviderDescription::new(
                        "fixture",
                        [Capability::new(["read"], Shape::Unknown, Safety::Safe)],
                        vec![],
                    )
                    .unwrap(),
                    Arc::new(Fixture(counted.clone())),
                )?;
                Ok(workspace)
            }
        }),
    };
    let (app, task) = wes::open(config()).await.unwrap();
    let source = include_str!("../../../examples/developer-workflow/work.wes").trim();
    let original = app
        .submit(SourceInput::new("original".into(), source.into()).unwrap())
        .await
        .unwrap();
    assert_eq!(original.nodes.len(), 1);
    let mut measurements = vec![sample(&app, 1, &store).await];
    let first_run = app
        .current()
        .unwrap()
        .session
        .snapshot()
        .await
        .unwrap()
        .execution
        .runs[&original.nodes[0]]
        .to_string();
    let mut protected_runs = vec![];
    for n in 2..=80 {
        let request = SourceInput::new(format!("repeat-{n}"), source.into())
            .unwrap()
            .with_repeat("original".into(), false)
            .unwrap();
        let accepted = app.submit(request.clone()).await.unwrap();
        assert_eq!(accepted.nodes, original.nodes);
        assert!(Arc::ptr_eq(&accepted, &app.submit(request).await.unwrap()));
        app.current().unwrap().session.wait_idle().await.unwrap();
        if [20, 60].contains(&n) {
            let current = app.current().unwrap();
            let checkpoint = current.session.checkpoint().await.unwrap();
            checkpoint.resume().await;
            let run = current.session.snapshot().await.unwrap().execution.runs[&original.nodes[0]]
                .to_string();
            let evidence = app
                .protect_run(
                    current.generation,
                    "measurement".into(),
                    "original".into(),
                    run.clone(),
                )
                .await
                .unwrap();
            assert_eq!(evidence["run"]["protected"], true);
            assert!(!evidence["trace"].is_null());
            protected_runs.push(run);
        }
        if [20, 60, 80].contains(&n) {
            measurements.push(sample(&app, n, &store).await);
        }
    }
    assert_eq!(calls.load(Ordering::SeqCst), 80);
    app.shutdown().await;
    task.join().await.unwrap();
    let (app, task) = wes::open(config()).await.unwrap();
    app.current().unwrap().session.wait_idle().await.unwrap();
    assert_eq!(
        calls.load(Ordering::SeqCst),
        80,
        "reopen must not execute providers"
    );
    let observation = app.current().unwrap().session.observe().await.unwrap();
    assert!(
        observation
            .traces
            .get(&original.nodes[0], Some(&first_run))
            .is_none()
    );
    let oldest = app
        .work_history(
            app.current().unwrap().generation,
            "measurement".into(),
            "original".into(),
            Some(first_run),
        )
        .await
        .unwrap();
    assert!(
        !oldest["trace"].is_null(),
        "history uses recorded evidence even after live trace eviction"
    );
    for run in protected_runs {
        let evidence = app
            .work_history(
                app.current().unwrap().generation,
                "measurement".into(),
                "original".into(),
                Some(run),
            )
            .await
            .unwrap();
        assert_eq!(evidence["run"]["protected"], true);
        assert!(
            !evidence["trace"].is_null(),
            "selected protection remains discoverable after reopen"
        );
        let handle =
            wes_engine::storage::ValueHandle::new(evidence["run"]["handle"].as_str().unwrap())
                .unwrap();
        assert_eq!(
            store.retention(handle.clone()).await.unwrap(),
            wes_engine::storage::Retention::Protected
        );
        assert!(store.read(handle).await.unwrap().is_some());
    }
    app.shutdown().await;
    task.join().await.unwrap();
    store.shutdown().await.unwrap();
    storage_task.join().await.unwrap();
    println!("{}", serde_json::to_string_pretty(&serde_json::json!({"scenario":"80 explicit repeats; one node; no automatic cleanup", "providerEntries":80, "reopenProviderEntries":0, "samples":measurements})).unwrap());
}
