use super::*;
use wes::backend::{BackendError, BackendHistory, FileBackend, WorkspaceBackend};
use wes_adapters::journal::ReadLimits;
use wes_engine::history::HistoryImage;

struct Injected {
    files: FileBackend,
    saves: Arc<AtomicUsize>,
    uncertain: bool,
}
impl WorkspaceBackend for Injected {
    fn names(&self) -> Result<Vec<WorkspaceName>, BackendError> {
        if self.uncertain {
            return Err(BackendError::Invalid);
        }
        self.files.names()
    }
    fn load(
        &mut self,
        name: &WorkspaceName,
    ) -> Result<(BackendHistory, HistoryImage), BackendError> {
        self.files.load(name)
    }
    fn save(&mut self, name: &WorkspaceName, image: &HistoryImage) -> Result<(), BackendError> {
        self.files.save(name, image)
    }
    fn save_current(
        &mut self,
        name: &WorkspaceName,
        image: &HistoryImage,
    ) -> Result<(), BackendError> {
        self.saves.fetch_add(1, Ordering::SeqCst);
        if self.saves.load(Ordering::SeqCst) == usize::MAX {
            self.uncertain = true;
            return Err(BackendError::Published(Box::new(std::io::Error::other(
                "synthetic lost reply",
            ))));
        }
        self.files.save_current(name, image)
    }
    // Default unsupported maintenance must neither run on open/save nor pretend success.
}
fn injected(root: &std::path::Path, saves: Arc<AtomicUsize>) -> Injected {
    Injected {
        files: FileBackend::open(
            &root.join("injected"),
            ReadLimits::default(),
            Durability::File,
        )
        .unwrap(),
        saves,
        uncertain: false,
    }
}
#[tokio::test]
async fn injected_live_backend_survives_restart_without_file_fallback_or_implicit_management() {
    let root = temp();
    let calls = Arc::new(AtomicUsize::new(0));
    let saves = Arc::new(AtomicUsize::new(0));
    let configuration = || {
        config(
            root.path(),
            calls.clone(),
            Arc::new(AtomicBool::new(false)),
            None,
        )
    };
    let (app, task) = wes::open_with_backend(configuration(), injected(root.path(), saves.clone()))
        .await
        .unwrap();
    assert!(!root.path().join("workspaces").exists());
    accepted(
        submit(&app, "a", "catalog echo value:one > first")
            .await
            .as_ref(),
    );
    idle(&app).await;
    accepted(
        submit(&app, "self", ":workspace save \"default\"")
            .await
            .as_ref(),
    );
    assert_eq!(saves.load(Ordering::SeqCst), 1);
    accepted(
        submit(&app, "copy", ":workspace save \"kept\"")
            .await
            .as_ref(),
    );
    let before = app.current().unwrap();
    assert!(
        submit(&app, "bad", ":workspace load \"missing\"")
            .await
            .accepted
            .is_empty()
    );
    assert_eq!(app.current().unwrap().generation, before.generation);
    // A mutation preview must fail before collecting references, publishing or deleting values.
    assert!(matches!(
        app.preview_delete_work(before.generation.clone(), "client".into(), "a".into())
            .await,
        Err(wes::retention::ManagementError::UnsupportedBackend)
    ));
    accepted(
        submit(&app, "load", ":workspace load \"kept\"")
            .await
            .as_ref(),
    );
    accepted(
        submit(&app, "later", "catalog echo value:two > later")
            .await
            .as_ref(),
    );
    idle(&app).await;
    app.shutdown().await;
    task.join().await.unwrap();
    let mut next = configuration();
    next.initial = WorkspaceName::new("kept".into()).unwrap();
    let (app, task) = wes::open_with_backend(next, injected(root.path(), saves.clone()))
        .await
        .unwrap();
    let snapshot = app.current().unwrap().session.snapshot().await.unwrap();
    assert!(snapshot.names.contains_key("first") && snapshot.names.contains_key("later"));
    assert_eq!(calls.load(Ordering::SeqCst), 2);
    assert!(!root.path().join("workspaces").exists());
    app.shutdown().await;
    task.join().await.unwrap();
}
#[tokio::test]
async fn uncertain_current_save_stops_admission_even_when_names_refresh_fails() {
    let root = temp();
    let saves = Arc::new(AtomicUsize::new(usize::MAX - 1));
    let (app, task) =
        wes::open_with_backend(config_for_reopen(root.path()), injected(root.path(), saves))
            .await
            .unwrap();
    let result = app
        .submit(SourceInput::new("save".into(), ":workspace save \"default\"".into()).unwrap())
        .await;
    assert!(result.is_err() || result.unwrap().accepted.is_empty());
    tokio::time::timeout(Duration::from_secs(10), task.join())
        .await
        .unwrap()
        .unwrap();
    assert!(app.current().is_err());
    assert!(!root.path().join("workspaces").exists());
}

struct DeferredCleanup(FileBackend);
impl WorkspaceBackend for DeferredCleanup {
    fn set_retained_dataset_publication(
        &mut self,
        publication: Arc<dyn wes_engine::history::RetainedDatasetPublication>,
    ) -> Result<(), BackendError> {
        self.0.set_retained_dataset_publication(publication)
    }
    fn capabilities(&self) -> wes::backend::BackendCapabilities {
        wes::backend::BackendCapabilities {
            automatic_cleanup: true,
            retention: false,
        }
    }
    fn names(&self) -> Result<Vec<WorkspaceName>, BackendError> {
        self.0.names()
    }
    fn load(
        &mut self,
        name: &WorkspaceName,
    ) -> Result<(BackendHistory, HistoryImage), BackendError> {
        self.0.load(name)
    }
    fn save(&mut self, name: &WorkspaceName, image: &HistoryImage) -> Result<(), BackendError> {
        self.0.save(name, image)
    }
    fn save_current(
        &mut self,
        name: &WorkspaceName,
        image: &HistoryImage,
    ) -> Result<(), BackendError> {
        self.0.save_current(name, image)
    }
    fn collect_unused(&mut self) -> Result<wes::backend::CollectionReport, BackendError> {
        Err(BackendError::Storage(Box::new(std::io::Error::other(
            "synthetic deferred root cleanup",
        ))))
    }
}

#[tokio::test]
async fn pending_startup_cleanup_keeps_the_owned_session_available_for_explicit_store_recovery() {
    let root = temp();
    let calls = Arc::new(AtomicUsize::new(0));
    let datasets = wes_adapters::datasets::DatasetStore::open(
        &root.path().join("datasets"),
        Durability::File,
        wes_adapters::datasets::StoreLimits::default(),
    )
    .unwrap();
    let values = TieredValues::open(
        &root.path().join("live"),
        &root.path().join("archive"),
        Limits::default(),
        Durability::File,
        None,
    )
    .unwrap();
    let (worker, storage_task) =
        wes_engine::storage::spawn_storage(values, datasets, StoreWorkerLimits::default()).unwrap();
    let backend = DeferredCleanup(
        FileBackend::open(
            &root.path().join("workspaces"),
            ReadLimits::default(),
            Durability::File,
        )
        .unwrap(),
    );
    let configuration = config(
        root.path(),
        calls.clone(),
        Arc::new(AtomicBool::new(false)),
        Some(SessionStorage {
            worker: worker.clone(),
            auto_keep: AutoKeep::default(),
        }),
    );
    let (app, task) = wes::open_with_backend(configuration, backend)
        .await
        .unwrap();
    assert!(
        app.cleanup_warning()
            .unwrap()
            .contains("Workspace cleanup is pending")
    );
    accepted(
        submit(&app, "repair", ":dataset reconcile > localRepair")
            .await
            .as_ref(),
    );
    idle(&app).await;
    let current = app.current().unwrap();
    let state = current.session.snapshot().await.unwrap();
    let node = &state.names["localRepair"].node;
    assert!(state.execution.values.contains_key(node));
    assert_eq!(calls.load(Ordering::SeqCst), 0);
    app.shutdown().await;
    task.join().await.unwrap();
    worker.shutdown().await.unwrap();
    storage_task.join().await.unwrap();
}
