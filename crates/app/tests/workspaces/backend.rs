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
