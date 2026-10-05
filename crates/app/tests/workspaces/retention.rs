use super::*;
use wes::retention::{ManagementError, ReleasePreview};
use wes_engine::{
    graph::NodeState,
    session::ValuePublication,
    storage::{Retention, StoreWorker, StoreWorkerTask, ValueHandle},
};

struct Fixture {
    root: tempfile::TempDir,
    app: ApplicationHandle,
    task: wes::ApplicationTask,
    worker: StoreWorker,
    writer: StoreWorkerTask,
    calls: Arc<AtomicUsize>,
}
impl Fixture {
    async fn work_preview(&self, cell: &str) -> wes::retirement::WorkPreview {
        self.app
            .preview_delete_work(
                self.app.current().unwrap().generation,
                "alice".into(),
                cell.into(),
            )
            .await
            .unwrap()
    }
    async fn delete_work(
        &self,
        preview: wes::retirement::WorkPreview,
        dependents: bool,
        protected: bool,
    ) -> Result<(), ManagementError> {
        tokio::time::timeout(
            Duration::from_secs(10),
            self.app.delete_work(
                self.app.current().unwrap().generation,
                "alice".into(),
                preview.token,
                dependents,
                protected,
            ),
        )
        .await
        .unwrap()
    }
    async fn new() -> Self {
        Self::with_store(|values| spawn_store(values, StoreWorkerLimits::default()).unwrap()).await
    }
    async fn with_store(
        factory: impl FnOnce(TieredValues) -> (StoreWorker, StoreWorkerTask),
    ) -> Self {
        let root = temp();
        let calls = Arc::new(AtomicUsize::new(0));
        let values = TieredValues::open(
            &root.path().join("live"),
            &root.path().join("archive"),
            Limits::default(),
            Durability::File,
            None,
        )
        .unwrap();
        let (worker, writer) = factory(values);
        let (app, task) = wes::open(config(
            root.path(),
            calls.clone(),
            Arc::new(AtomicBool::new(false)),
            Some(SessionStorage {
                worker: worker.clone(),
                auto_keep: AutoKeep::default(),
            }),
        ))
        .await
        .unwrap();
        Self {
            root,
            app,
            task,
            worker,
            writer,
            calls,
        }
    }
    async fn source(&self, id: &str, source: &str) {
        accepted(submit(&self.app, id, source).await.as_ref());
        idle(&self.app).await;
    }
    async fn finding(&self) -> ValueHandle {
        self.source("finding", "catalog echo value:original > finding")
            .await;
        let current = self.app.current().unwrap();
        let checkpoint = current.session.checkpoint().await.unwrap();
        let observation = current.session.observe().await.unwrap();
        let result = observation
            .values
            .unwrap()
            .outputs
            .values()
            .next()
            .unwrap()
            .handle()
            .unwrap()
            .clone();
        checkpoint.resume().await;
        result
    }
    async fn preview(&self, handle: &ValueHandle) -> ReleasePreview {
        // Yield lets a just-dropped checkpoint relinquish its scoped pause.
        tokio::task::yield_now().await;
        self.app
            .preview_release(
                self.app.current().unwrap().generation,
                "alice".into(),
                handle.clone(),
            )
            .await
            .unwrap()
    }
    async fn confirm(&self, preview: ReleasePreview) -> Result<(), ManagementError> {
        self.app
            .confirm_release(
                self.app.current().unwrap().generation,
                "alice".into(),
                preview.token,
            )
            .await
    }
    async fn close(self) {
        self.app.shutdown().await;
        self.task.join().await.unwrap();
        self.worker.shutdown().await.unwrap();
        self.writer.join().await.unwrap();
    }
}

#[tokio::test]
async fn deleting_work_erases_source_runs_payload_and_reserves_identity_across_reopen() {
    let f = Fixture::new().await;
    let handle = f.finding().await;
    let old_session = f.app.current().unwrap().session;
    let old_generation = f.app.current().unwrap().generation;
    let preview = f.work_preview("finding").await;
    assert_eq!(preview.cells, ["finding"]);
    assert_eq!(preview.payloads, [handle.to_string()]);
    let deleted_node = preview.nodes[0].clone();
    f.delete_work(preview, false, false).await.unwrap();
    assert_eq!(old_generation, f.app.current().unwrap().generation);
    let observed = old_session.observe().await.unwrap();
    assert!(observed.cells.is_empty());
    assert!(observed.state.execution.graph.is_empty());
    assert!(f.worker.read(handle.clone()).await.unwrap().is_none());
    assert!(matches!(
        old_session
            .submit(
                SourceInput::new(
                    "finding".into(),
                    "catalog echo value:original > finding".into()
                )
                .unwrap()
            )
            .await,
        Err(wes_engine::session::SessionError::HistoricalReplyUnavailable)
    ));
    for entry in std::fs::read_dir(f.root.path().join("workspaces")).unwrap() {
        let path = entry.unwrap().path();
        if path.is_dir() {
            for name in ["journal.jsonl", "recovery.jsonl"] {
                if let Ok(bytes) = std::fs::read_to_string(path.join(name)) {
                    assert!(!bytes.contains("value:original"));
                    assert!(!bytes.contains("\"record\":\"calling\""));
                    assert!(!bytes.contains("\"record\":\"observation\""));
                }
            }
        }
    }
    assert!(
        !f.root
            .path()
            .join("archive")
            .join(format!("{handle}.json"))
            .exists()
    );
    assert!(
        !f.root
            .path()
            .join("live")
            .join(format!("{handle}.json"))
            .exists()
    );
    f.app.shutdown().await;
    f.task.join().await.unwrap();
    let (app, task) = wes::open(config(
        f.root.path(),
        f.calls.clone(),
        Arc::new(AtomicBool::new(false)),
        Some(SessionStorage {
            worker: f.worker.clone(),
            auto_keep: AutoKeep::default(),
        }),
    ))
    .await
    .unwrap();
    assert!(
        app.current()
            .unwrap()
            .session
            .observe()
            .await
            .unwrap()
            .cells
            .is_empty()
    );
    assert!(matches!(
        app.submit(
            SourceInput::new(
                "finding".into(),
                "catalog echo value:original > finding".into()
            )
            .unwrap()
        )
        .await,
        Err(wes_engine::session::SessionError::HistoricalReplyUnavailable)
    ));
    let reply = submit(&app, "fresh", "catalog echo value:new").await;
    idle(&app).await;
    assert_ne!(reply.nodes[0].as_str(), deleted_node);
    assert_eq!(f.calls.load(Ordering::SeqCst), 2);
    app.shutdown().await;
    task.join().await.unwrap();
    f.worker.shutdown().await.unwrap();
    f.writer.join().await.unwrap();
}

#[tokio::test]
async fn work_deletion_requires_dependent_and_protected_approval_without_losing_other_live_values()
{
    let f = Fixture::new().await;
    let handle = f.finding().await;
    f.source("consumer", ":calc { return $finding; } > report")
        .await;
    f.app
        .current()
        .unwrap()
        .session
        .keep(handle.clone())
        .await
        .unwrap();
    f.app
        .current()
        .unwrap()
        .session
        .set_retention_policy(wes_engine::session::RetentionPolicy {
            automatic: false,
            under: 0,
        })
        .await
        .unwrap();
    f.source("unrelated", "catalog echo value:temporary > other")
        .await;
    let before = f.app.current().unwrap().session.observe().await.unwrap();
    let other = before.state.names["other"].node.clone();
    let other_handle = before.values.unwrap().outputs[&other]
        .handle()
        .unwrap()
        .clone();
    let preview = f.work_preview("finding").await;
    assert_eq!(preview.dependents, ["consumer"]);
    assert_eq!(preview.protected, [handle.to_string()]);
    assert!(matches!(
        f.delete_work(preview, false, true).await,
        Err(ManagementError::Approval)
    ));
    let preview = f.work_preview("finding").await;
    assert!(matches!(
        f.delete_work(preview, true, false).await,
        Err(ManagementError::Approval)
    ));
    assert!(f.worker.read(handle.clone()).await.unwrap().is_some());
    let preview = f.work_preview("finding").await;
    f.delete_work(preview, true, true).await.unwrap();
    let after = f.app.current().unwrap().session.observe().await.unwrap();
    assert_eq!(after.cells.len(), 1);
    assert_eq!(after.cells[0].input.cell(), "unrelated");
    assert_eq!(
        after.state.execution.graph.node(&other).unwrap().state(),
        NodeState::Ready
    );
    assert_eq!(
        after.values.unwrap().outputs[&other].handle(),
        Some(&other_handle)
    );
    assert!(f.worker.read(other_handle).await.unwrap().is_some());
    assert!(f.worker.read(handle).await.unwrap().is_none());
    f.close().await;
}

#[tokio::test]
async fn shared_workspace_keeps_protected_content_until_approved_last_reference_deletion() {
    let f = Fixture::new().await;
    let handle = f.finding().await;
    f.app
        .current()
        .unwrap()
        .session
        .keep(handle.clone())
        .await
        .unwrap();
    f.source("save", ":workspace save \"copy\"").await;
    let preview = f.work_preview("finding").await;
    assert!(preview.protected.is_empty());
    assert!(preview.payloads.is_empty());
    assert_eq!(preview.shared_workspaces, ["copy"]);
    f.delete_work(preview, false, false).await.unwrap();
    assert!(f.worker.read(handle.clone()).await.unwrap().is_some());
    f.source("switch", ":workspace load \"copy\"").await;
    let preview = f.work_preview("finding").await;
    assert_eq!(preview.protected, [handle.to_string()]);
    assert!(preview.shared_workspaces.is_empty());
    f.delete_work(preview, false, true).await.unwrap();
    assert!(f.worker.read(handle).await.unwrap().is_none());
    assert_eq!(f.calls.load(Ordering::SeqCst), 1);
    f.close().await;
}

#[tokio::test]
async fn work_preview_is_client_bound_one_shot_and_stale_after_keep_or_new_history() {
    let f = Fixture::new().await;
    let handle = f.finding().await;
    let preview = f.work_preview("finding").await;
    assert!(matches!(
        f.app
            .delete_work(
                f.app.current().unwrap().generation,
                "bob".into(),
                preview.token.clone(),
                true,
                true
            )
            .await,
        Err(ManagementError::Stale)
    ));
    f.app
        .current()
        .unwrap()
        .session
        .keep(handle.clone())
        .await
        .unwrap();
    assert!(matches!(
        f.delete_work(preview, true, true).await,
        Err(ManagementError::Stale)
    ));
    let preview = f.work_preview("finding").await;
    f.source("new", "catalog echo value:unrelated").await;
    let token = preview.token.clone();
    assert!(matches!(
        f.delete_work(preview, true, true).await,
        Err(ManagementError::Stale)
    ));
    assert!(matches!(
        f.app
            .delete_work(
                f.app.current().unwrap().generation,
                "alice".into(),
                token,
                true,
                true
            )
            .await,
        Err(ManagementError::Stale)
    ));
    assert!(f.worker.read(handle).await.unwrap().is_some());
    f.close().await;
}

#[tokio::test]
async fn repeat_attempts_and_multi_node_batches_are_deleted_as_one_work_group() {
    let f = Fixture::new().await;
    f.finding().await;
    let repeated = SourceInput::new(
        "again".into(),
        "catalog echo value:original > finding".into(),
    )
    .unwrap()
    .with_repeat("finding".into(), false)
    .unwrap();
    f.app.submit(repeated).await.unwrap();
    idle(&f.app).await;
    let preview = f.work_preview("again").await;
    assert_eq!(preview.cells, ["again", "finding"]);
    assert!(matches!(
        f.delete_work(preview, false, false).await,
        Err(ManagementError::Approval)
    ));
    let preview = f.work_preview("finding").await;
    assert!(preview.dependents.is_empty());
    f.delete_work(preview, true, false).await.unwrap();
    f.source(
        "batch",
        "catalog echo value:first > one\ncatalog echo value:second > two",
    )
    .await;
    f.source("downstream", ":calc { return $one; } > three")
        .await;
    let preview = f.work_preview("batch").await;
    assert_eq!(preview.nodes.len(), 3);
    f.delete_work(preview, true, false).await.unwrap();
    assert!(
        f.app
            .current()
            .unwrap()
            .session
            .observe()
            .await
            .unwrap()
            .state
            .execution
            .graph
            .is_empty()
    );
    f.close().await;
}

#[tokio::test]
async fn deletion_refuses_shadowed_binding_resurrection_and_unreadable_saved_references() {
    let f = Fixture::new().await;
    f.source("earlier", "catalog echo value:first > named")
        .await;
    f.source("later", "catalog echo value:second > named").await;
    assert!(matches!(
        f.app
            .preview_delete_work(
                f.app.current().unwrap().generation,
                "alice".into(),
                "later".into()
            )
            .await,
        Err(ManagementError::Work(_))
    ));
    let preview = f.work_preview("earlier").await;
    f.delete_work(preview, false, false).await.unwrap();
    f.source("save", ":workspace save \"copy\"").await;
    std::fs::write(
        f.root.path().join("workspaces/workspace-636f7079"),
        "invalid synthetic pointer",
    )
    .unwrap();
    assert!(matches!(
        f.app
            .preview_delete_work(
                f.app.current().unwrap().generation,
                "alice".into(),
                "later".into()
            )
            .await,
        Err(ManagementError::References)
    ));
    assert_eq!(
        f.app
            .current()
            .unwrap()
            .session
            .observe()
            .await
            .unwrap()
            .state
            .execution
            .graph
            .nodes()
            .count(),
        1
    );
    f.close().await;
}

#[tokio::test]
async fn published_retirement_recovers_cleanup_after_interruption_without_reexecution() {
    use wes_adapters::{journal::ReadLimits, workspaces::FileWorkspaces};
    use wes_engine::session::retirement::RetirementPlan;
    let f = Fixture::new().await;
    let handle = f.finding().await;
    f.app
        .current()
        .unwrap()
        .session
        .keep(handle.clone())
        .await
        .unwrap();
    let current = f.app.current().unwrap();
    let checkpoint = current.session.checkpoint().await.unwrap();
    let observation = current.session.observe().await.unwrap();
    let plan = RetirementPlan::prepare(checkpoint.history(), &observation, "finding").unwrap();
    let compacted = plan
        .compact(checkpoint.history(), vec![handle.clone()])
        .unwrap();
    // Model process loss after pointer publication but before live retirement/release.
    f.app.shutdown().await;
    f.task.join().await.unwrap();
    drop(checkpoint);
    let mut files = FileWorkspaces::open(
        &f.root.path().join("workspaces"),
        ReadLimits::default(),
        Durability::File,
    )
    .unwrap();
    files
        .save(&WorkspaceName::new("default".into()).unwrap(), &compacted)
        .unwrap();
    drop(files);
    assert!(f.worker.read(handle.clone()).await.unwrap().is_some());
    let (app, task) = wes::open(config(
        f.root.path(),
        f.calls.clone(),
        Arc::new(AtomicBool::new(false)),
        Some(SessionStorage {
            worker: f.worker.clone(),
            auto_keep: AutoKeep::default(),
        }),
    ))
    .await
    .unwrap();
    assert!(app.current().unwrap().storage_warning.is_none());
    assert!(
        app.current()
            .unwrap()
            .session
            .observe()
            .await
            .unwrap()
            .cells
            .is_empty()
    );
    assert!(f.worker.read(handle).await.unwrap().is_none());
    assert_eq!(f.calls.load(Ordering::SeqCst), 1);
    app.shutdown().await;
    task.join().await.unwrap();
    f.worker.shutdown().await.unwrap();
    f.writer.join().await.unwrap();
}

#[tokio::test]
async fn unresolved_call_evidence_blocks_work_deletion() {
    use wes_engine::history::{
        CallRecord, HistoryCapture, HistoryCaptureLimits, Record, RecoveryEntry,
    };
    use wes_engine::session::retirement::{RetirementError, RetirementPlan};
    let f = Fixture::new().await;
    f.finding().await;
    let current = f.app.current().unwrap();
    let checkpoint = current.session.checkpoint().await.unwrap();
    let observed = current.session.observe().await.unwrap();
    let mut capture = HistoryCapture::new(HistoryCaptureLimits::default());
    for entry in checkpoint.history().journal() {
        capture.push(Record::Journal(entry.clone())).unwrap();
    }
    for entry in checkpoint.history().recovery() {
        capture.push(Record::Recovery(entry.clone())).unwrap();
    }
    capture
        .push(Record::Recovery(RecoveryEntry::Calling(CallRecord {
            cell: "finding".into(),
            node: observed.state.names["finding"].node.clone(),
            run: wes_engine::runtime::RunId::new("interrupted").unwrap(),
            capability: "catalog echo".into(),
            safe: true,
            at: wes_core::Timestamp::new(1_700_000_000, 0).unwrap(),
        })))
        .unwrap();
    let image = capture.finish(checkpoint.history().checkpoint());
    assert!(matches!(
        RetirementPlan::prepare(&image, &observed, "finding"),
        Err(RetirementError::Uncertain)
    ));
    checkpoint.resume().await;
    f.close().await;
}

#[tokio::test]
async fn delete_work_example_executes_actual_file_without_external_providers() {
    let f = Fixture::new().await;
    for (index, line) in include_str!("../../../../examples/delete-work/work.wes")
        .lines()
        .enumerate()
    {
        f.source(&format!("example-{index}"), line).await;
    }
    let current = f.app.current().unwrap();
    let before = current.session.observe().await.unwrap();
    let node = &before.state.names["testData"].node;
    let handle = before.values.unwrap().outputs[node]
        .handle()
        .unwrap()
        .clone();
    current.session.keep(handle.clone()).await.unwrap();
    let preview = f.work_preview("example-0").await;
    assert_eq!(preview.dependents, ["example-1"]);
    assert_eq!(preview.protected, [handle.to_string()]);
    f.delete_work(preview, true, true).await.unwrap();
    let after = f.app.current().unwrap().session.observe().await.unwrap();
    assert_eq!(after.cells.len(), 1);
    assert_eq!(after.cells[0].input.cell(), "example-2");
    assert_eq!(
        after.state.execution.graph.nodes().next().unwrap().state(),
        NodeState::Ready
    );
    assert_eq!(f.calls.load(Ordering::SeqCst), 0);
    f.close().await;
}

#[tokio::test]
async fn temporary_attempt_ownership_survives_restart_but_pending_restoration_cannot_be_deleted() {
    use wes_engine::session::RetentionPolicy;
    let f = Fixture::new().await;
    f.app
        .current()
        .unwrap()
        .session
        .set_retention_policy(RetentionPolicy {
            automatic: false,
            under: 0,
        })
        .await
        .unwrap();
    let first = f.finding().await;
    f.app
        .submit(
            SourceInput::new(
                "again".into(),
                "catalog echo value:original > finding".into(),
            )
            .unwrap()
            .with_repeat("finding".into(), false)
            .unwrap(),
        )
        .await
        .unwrap();
    idle(&f.app).await;
    let checkpoint = f.app.current().unwrap().session.checkpoint().await.unwrap();
    let owned: Vec<_> = checkpoint
        .history()
        .journal()
        .iter()
        .filter_map(|e| match e {
            wes_engine::history::JournalEntry::Payload { handle, .. } => Some(handle.clone()),
            _ => None,
        })
        .collect();
    assert_eq!(owned.len(), 2);
    assert!(owned.contains(&first));
    assert!(
        !checkpoint
            .history()
            .journal()
            .iter()
            .any(|e| matches!(e, wes_engine::history::JournalEntry::Result(_)))
    );
    checkpoint.resume().await;
    f.app.shutdown().await;
    f.task.join().await.unwrap();
    let (app, task) = wes::open(config(
        f.root.path(),
        f.calls.clone(),
        Arc::new(AtomicBool::new(false)),
        Some(SessionStorage {
            worker: f.worker.clone(),
            auto_keep: AutoKeep::default(),
        }),
    ))
    .await
    .unwrap();
    let observed = app.current().unwrap().session.observe().await.unwrap();
    assert!(
        observed
            .state
            .execution
            .graph
            .nodes()
            .all(|n| n.state() != NodeState::Ready)
    );
    let result = app
        .preview_delete_work(
            app.current().unwrap().generation,
            "alice".into(),
            "finding".into(),
        )
        .await;
    assert!(matches!(
        result,
        Err(ManagementError::Work(
            wes_engine::session::retirement::RetirementError::Blocked(_)
        ))
    ));
    // Missing current results are restored as pending work. Deletion must neither
    // reexecute them nor mistake their historical payloads for completed execution.
    for handle in owned {
        assert!(f.worker.read(handle).await.unwrap().is_some());
    }
    assert_eq!(f.calls.load(Ordering::SeqCst), 2);
    app.shutdown().await;
    task.join().await.unwrap();
    f.worker.shutdown().await.unwrap();
    f.writer.join().await.unwrap();
}

#[tokio::test]
async fn refused_generation_cleanup_is_visible_and_reopening_completes_it() {
    let f = Fixture::new().await;
    let handle = f.finding().await;
    let old = std::fs::read_dir(f.root.path().join("workspaces"))
        .unwrap()
        .map(|e| e.unwrap().path())
        .find(|p| {
            p.file_name()
                .unwrap()
                .to_string_lossy()
                .starts_with("generation-")
        })
        .unwrap();
    let unknown = old.join("unexpected-fixture.txt");
    std::fs::write(&unknown, "must not be deleted by collection").unwrap();
    let preview = f.work_preview("finding").await;
    assert!(matches!(
        f.delete_work(preview, false, false).await,
        Err(ManagementError::CleanupPending)
    ));
    assert!(unknown.exists());
    assert!(
        f.app
            .current()
            .unwrap()
            .storage_warning
            .as_deref()
            .unwrap()
            .contains("STO010")
    );
    assert!(
        f.app
            .current()
            .unwrap()
            .session
            .observe()
            .await
            .unwrap()
            .cells
            .is_empty()
    );
    assert!(f.worker.read(handle.clone()).await.unwrap().is_some());
    // Remove only the test-owned obstacle; restart must finish the already authorized job.
    std::fs::remove_file(unknown).unwrap();
    f.app.shutdown().await;
    f.task.join().await.unwrap();
    let (app, task) = wes::open(config(
        f.root.path(),
        f.calls.clone(),
        Arc::new(AtomicBool::new(false)),
        Some(SessionStorage {
            worker: f.worker.clone(),
            auto_keep: AutoKeep::default(),
        }),
    ))
    .await
    .unwrap();
    assert!(app.current().unwrap().storage_warning.is_none());
    assert!(!old.exists());
    assert!(f.worker.read(handle).await.unwrap().is_none());
    assert_eq!(f.calls.load(Ordering::SeqCst), 1);
    app.shutdown().await;
    task.join().await.unwrap();
    f.worker.shutdown().await.unwrap();
    f.writer.join().await.unwrap();
}

#[tokio::test]
async fn preview_shared_references_and_explicit_release_preserve_history_without_provider_replay() {
    let f = Fixture::new().await;
    let handle = f.finding().await;
    f.source("consumer", ":calc { return $finding; } > report")
        .await;
    f.source("save-a", ":workspace save \"alpha\"").await;
    f.source("save-b", ":workspace save \"beta\"").await;
    let preview = f.preview(&handle).await;
    assert_eq!(preview.retention, "automatic");
    assert_eq!(preview.workspaces, ["alpha", "beta", "default"]);
    assert_eq!(preview.nodes.len(), 1);
    assert_eq!(preview.downstream.len(), 1);
    let token = preview.token.clone();
    f.confirm(preview).await.unwrap();
    assert!(f.worker.read(handle.clone()).await.unwrap().is_none());
    assert!(matches!(
        f.app
            .confirm_release(f.app.current().unwrap().generation, "alice".into(), token)
            .await,
        Err(ManagementError::Stale)
    ));
    let after = f.app.current().unwrap().session.observe().await.unwrap();
    assert!(
        after
            .state
            .execution
            .graph
            .nodes()
            .any(|n| n.state() == NodeState::Stale)
    );
    assert!(after.cells.iter().any(|c| c.input.cell() == "finding"));
    f.source("load", ":workspace load \"alpha\"").await;
    let reopened = f.app.current().unwrap().session.observe().await.unwrap();
    assert!(
        reopened
            .state
            .execution
            .graph
            .nodes()
            .any(|n| n.state() == NodeState::Stale)
    );
    assert_eq!(f.calls.load(Ordering::SeqCst), 1);
    f.close().await;
}

#[tokio::test]
async fn keep_save_new_work_and_load_invalidate_preview_authority() {
    let f = Fixture::new().await;
    let handle = f.finding().await;
    let preview = f.preview(&handle).await;
    let receipt = f
        .app
        .current()
        .unwrap()
        .session
        .keep(handle.clone())
        .await
        .unwrap();
    assert!(receipt.problem.is_none());
    assert_eq!(
        receipt.stored.as_ref().unwrap().retention,
        Retention::Protected
    );
    assert!(matches!(
        f.confirm(preview).await,
        Err(ManagementError::Stale)
    ));
    let preview = f.preview(&handle).await;
    f.source("save", ":workspace save \"protected\"").await;
    assert!(matches!(
        f.confirm(preview).await,
        Err(ManagementError::Stale)
    ));
    let preview = f.preview(&handle).await;
    f.source("new", "catalog echo value:other").await;
    assert!(matches!(
        f.confirm(preview).await,
        Err(ManagementError::Stale)
    ));
    let preview = f.preview(&handle).await;
    let generation = f.app.current().unwrap().generation;
    assert!(matches!(
        f.app
            .confirm_release(generation.clone(), "ben".into(), preview.token.clone())
            .await,
        Err(ManagementError::Stale)
    ));
    f.source("load", ":workspace load \"protected\"").await;
    assert!(matches!(
        f.app
            .confirm_release(generation, "alice".into(), preview.token)
            .await,
        Err(ManagementError::Stale)
    ));
    let values = f
        .app
        .current()
        .unwrap()
        .session
        .values()
        .await
        .unwrap()
        .unwrap();
    assert!(values.outputs.values().any(
        |v| matches!(v, ValuePublication::Recovered(v) if v.retention == Retention::Protected)
    ));
    assert!(f.worker.read(handle).await.unwrap().is_some());
    assert_eq!(f.calls.load(Ordering::SeqCst), 2);
    f.close().await;
}

#[tokio::test]
async fn unreadable_named_reference_refuses_deletion_and_never_becomes_no_references() {
    let f = Fixture::new().await;
    let handle = f.finding().await;
    f.source("save", ":workspace save \"broken\"").await;
    let preview = f.preview(&handle).await;
    // Corrupt only this test's exact synthetic pointer. No live user files involved.
    std::fs::write(
        f.root.path().join("workspaces/workspace-62726f6b656e"),
        "invalid fixture",
    )
    .unwrap();
    assert!(matches!(
        f.confirm(preview).await,
        Err(ManagementError::References)
    ));
    assert!(f.worker.read(handle).await.unwrap().is_some());
    assert_eq!(f.calls.load(Ordering::SeqCst), 1);
    f.close().await;
}

#[tokio::test]
async fn checkpoint_release_capability_is_session_bound_and_blocks_competing_controls() {
    let a = Fixture::new().await;
    let ah = a.finding().await;
    let b = Fixture::new().await;
    let bh = b.finding().await;
    tokio::task::yield_now().await;
    let session = a.app.current().unwrap().session;
    let checkpoint = session.checkpoint().await.unwrap();
    assert!(matches!(
        b.app
            .current()
            .unwrap()
            .session
            .fence_retirement_at_checkpoint(&checkpoint)
            .await,
        Err(wes_engine::session::SessionError::CheckpointBusy)
    ));
    session
        .fence_retirement_at_checkpoint(&checkpoint)
        .await
        .unwrap();
    assert!(matches!(
        session
            .submit(SourceInput::new("fenced".into(), ":calc { return 1; }".into()).unwrap())
            .await,
        Err(wes_engine::session::SessionError::CheckpointBusy)
    ));
    assert!(matches!(
        session.keep(ah.clone()).await,
        Err(wes_engine::session::SessionError::CheckpointBusy)
    ));
    assert!(matches!(
        session.release(ah.clone()).await,
        Err(wes_engine::session::SessionError::CheckpointBusy)
    ));
    assert!(matches!(
        b.app
            .current()
            .unwrap()
            .session
            .release_at_checkpoint(bh.clone(), &checkpoint)
            .await,
        Err(wes_engine::session::SessionError::CheckpointBusy)
    ));
    assert!(b.worker.read(bh).await.unwrap().is_some());
    let released = session
        .release_at_checkpoint(ah.clone(), &checkpoint)
        .await
        .unwrap();
    assert!(released.problem.is_none());
    assert!(a.worker.read(ah).await.unwrap().is_none());
    checkpoint.resume().await;
    a.source("after-fence", ":calc { return 1; }").await;
    a.close().await;
    b.close().await;
}

#[tokio::test]
async fn preview_capacity_is_bounded_and_disconnect_does_not_cancel_admitted_confirmation() {
    disconnected_release(false).await;
}
#[tokio::test]
async fn disconnected_work_confirmation_still_joins_physical_cleanup() {
    disconnected_release(true).await;
}
async fn disconnected_release(work: bool) {
    use wes_engine::storage::{LoadedValue, RetentionUsage, StoreError, ValueStore};
    struct GatedRelease {
        values: TieredValues,
        entered: Option<tokio::sync::oneshot::Sender<()>>,
        proceed: std::sync::mpsc::Receiver<()>,
    }
    impl ValueStore for GatedRelease {
        fn retained_persistence(&self) -> wes_engine::history::Persistence {
            self.values.retained_persistence()
        }
        fn store(&mut self, v: &wes_core::Value) -> Result<ValueHandle, StoreError> {
            self.values.store(v)
        }
        fn read(&self, h: &ValueHandle) -> Result<Option<LoadedValue>, StoreError> {
            self.values.read(h)
        }
        fn encoded(&self, h: &ValueHandle) -> Result<Option<Vec<u8>>, StoreError> {
            self.values.encoded(h)
        }
        fn size(&self, h: &ValueHandle) -> Result<Option<u64>, StoreError> {
            self.values.size(h)
        }
        fn keep(&mut self, h: &ValueHandle) -> Result<bool, StoreError> {
            self.values.keep(h)
        }
        fn is_kept(&self, h: &ValueHandle) -> Result<bool, StoreError> {
            self.values.is_kept(h)
        }
        fn keep_with_reason(&mut self, h: &ValueHandle, r: Retention) -> Result<bool, StoreError> {
            self.values.keep_with_reason(h, r)
        }
        fn retention(&self, h: &ValueHandle) -> Result<Retention, StoreError> {
            self.values.retention(h)
        }
        fn retention_usage(&self) -> Result<RetentionUsage, StoreError> {
            self.values.retention_usage()
        }
        fn release(&mut self, h: &ValueHandle) -> Result<bool, StoreError> {
            if let Some(entered) = self.entered.take() {
                let _ = entered.send(());
                self.proceed
                    .recv_timeout(Duration::from_secs(5))
                    .map_err(|e| StoreError::backend("test release gate", e))?;
            }
            self.values.release(h)
        }
    }
    let (entered, entry) = tokio::sync::oneshot::channel();
    let (proceed, gate) = std::sync::mpsc::channel();
    let f = Fixture::with_store(|values| {
        spawn_store(
            GatedRelease {
                values,
                entered: Some(entered),
                proceed: gate,
            },
            StoreWorkerLimits::default(),
        )
        .unwrap()
    })
    .await;
    let handle = f.finding().await;
    let generation = f.app.current().unwrap().generation;
    for index in 0..16 {
        f.app
            .preview_release(
                generation.clone(),
                format!("client-{index}"),
                handle.clone(),
            )
            .await
            .unwrap();
        tokio::task::yield_now().await;
    }
    assert!(matches!(
        f.app
            .preview_release(generation.clone(), "overflow".into(), handle.clone())
            .await,
        Err(ManagementError::Busy)
    ));
    let preview = f
        .app
        .preview_release(generation.clone(), "client-0".into(), handle.clone())
        .await
        .unwrap();
    // Once the app actor receives this request it owns the physical operation.
    let work_preview = if work {
        Some(f.work_preview("finding").await)
    } else {
        None
    };
    let app = f.app.clone();
    let task = tokio::spawn(async move {
        if let Some(work) = work_preview {
            app.delete_work(generation, "alice".into(), work.token, false, false)
                .await
        } else {
            app.confirm_release(generation, "client-0".into(), preview.token)
                .await
        }
    });
    tokio::time::timeout(Duration::from_secs(5), entry)
        .await
        .unwrap()
        .unwrap();
    if work {
        let session = f.app.current().unwrap().session;
        assert!(matches!(
            session.check_retirement_access().await,
            Err(wes_engine::session::SessionError::CheckpointBusy)
        ));
        assert!(matches!(
            session
                .submit(
                    SourceInput::new("during-delete".into(), ":calc { return 9; }".into()).unwrap()
                )
                .await,
            Err(wes_engine::session::SessionError::CheckpointBusy)
        ));
    }
    // Disconnect while physical deletion is entered but blocked, not after completion.
    task.abort();
    let _ = task.await;
    assert!(
        f.root
            .path()
            .join("archive")
            .join(format!("{handle}.json"))
            .exists()
    );
    proceed.send(()).unwrap();
    assert!(f.worker.read(handle).await.unwrap().is_none());
    if work {
        assert!(
            f.app
                .current()
                .unwrap()
                .session
                .observe()
                .await
                .unwrap()
                .cells
                .is_empty()
        );
    }
    assert_eq!(f.calls.load(Ordering::SeqCst), 1);
    f.close().await;
}

#[tokio::test]
async fn old_run_protection_keeps_exact_evidence_through_revision_reopen_and_work_deletion() {
    let f = Fixture::new().await;
    let old_handle = f.finding().await;
    let generation = f.app.current().unwrap().generation;
    let history = f
        .app
        .work_history(generation.clone(), "alice".into(), "finding".into(), None)
        .await
        .unwrap();
    let old_run = history["runs"][0]["run"].as_str().unwrap().to_owned();
    let revision = SourceInput::new("revision".into(), "catalog echo value:new > finding".into())
        .unwrap()
        .with_revision("finding".into())
        .unwrap();
    f.app.submit(revision).await.unwrap();
    idle(&f.app).await;
    let protected = f
        .app
        .protect_run(
            generation.clone(),
            "alice".into(),
            "revision".into(),
            old_run.clone(),
        )
        .await
        .unwrap();
    assert_eq!(protected["run"]["handle"], old_handle.to_string());
    assert_eq!(protected["run"]["protected"], true);
    assert_eq!(
        protected["definition"]["text"],
        "catalog echo value:original > finding"
    );
    assert!(protected["trace"].is_null());
    assert!(
        protected["traceNote"]
            .as_str()
            .unwrap()
            .contains("No recorded trace")
    );
    assert_eq!(
        f.worker.retention(old_handle.clone()).await.unwrap(),
        Retention::Protected
    );
    let duplicate = f
        .app
        .protect_run(
            generation,
            "alice".into(),
            "finding".into(),
            old_run.clone(),
        )
        .await
        .unwrap();
    assert_eq!(duplicate, protected);
    assert_eq!(f.calls.load(Ordering::SeqCst), 2);
    f.source("save-evidence", ":workspace save \"evidence\"")
        .await;
    f.source("load-evidence", ":workspace load \"evidence\"")
        .await;
    let generation = f.app.current().unwrap().generation;
    let read = f
        .app
        .work_history(generation, "alice".into(), "finding".into(), Some(old_run))
        .await
        .unwrap();
    assert_eq!(read, protected);
    assert_eq!(f.calls.load(Ordering::SeqCst), 2);
    let observation = f.app.current().unwrap().session.observe().await.unwrap();
    let revision = observation
        .cells
        .iter()
        .find(|c| c.input.cell() == "revision")
        .unwrap()
        .reply
        .as_ref()
        .unwrap()
        .as_ref()
        .unwrap();
    let output = observation.values.as_ref().unwrap().outputs[&revision.nodes[0]]
        .handle()
        .unwrap();
    assert_ne!(output, &old_handle);
    let preview = f.work_preview("revision").await;
    assert!(
        preview.cells.contains(&"finding".into()) && preview.cells.contains(&"revision".into())
    );
    assert!(preview.dependents.is_empty());
    // Another saved workspace still references the protected result, so it is retained there.
    f.delete_work(preview, true, true).await.unwrap();
    assert!(f.worker.read(old_handle).await.unwrap().is_some());
    f.close().await;
}

#[tokio::test]
async fn failed_protection_acknowledgement_never_claims_complete_run_evidence() {
    use wes_engine::storage::{LoadedValue, RetentionUsage, StoreError, ValueStore};
    struct FailProtection(TieredValues);
    impl ValueStore for FailProtection {
        fn retained_persistence(&self) -> wes_engine::history::Persistence {
            self.0.retained_persistence()
        }
        fn store(&mut self, v: &wes_core::Value) -> Result<ValueHandle, StoreError> {
            self.0.store(v)
        }
        fn read(&self, h: &ValueHandle) -> Result<Option<LoadedValue>, StoreError> {
            self.0.read(h)
        }
        fn encoded(&self, h: &ValueHandle) -> Result<Option<Vec<u8>>, StoreError> {
            self.0.encoded(h)
        }
        fn size(&self, h: &ValueHandle) -> Result<Option<u64>, StoreError> {
            self.0.size(h)
        }
        fn release(&mut self, h: &ValueHandle) -> Result<bool, StoreError> {
            self.0.release(h)
        }
        fn keep(&mut self, h: &ValueHandle) -> Result<bool, StoreError> {
            self.0.keep(h)
        }
        fn is_kept(&self, h: &ValueHandle) -> Result<bool, StoreError> {
            self.0.is_kept(h)
        }
        fn retention(&self, h: &ValueHandle) -> Result<Retention, StoreError> {
            self.0.retention(h)
        }
        fn retention_usage(&self) -> Result<RetentionUsage, StoreError> {
            self.0.retention_usage()
        }
        fn keep_with_reason(&mut self, h: &ValueHandle, r: Retention) -> Result<bool, StoreError> {
            let kept = self.0.keep_with_reason(h, r)?;
            if r == Retention::Protected {
                Err(StoreError::Limit(
                    "synthetic lost protection acknowledgement",
                ))
            } else {
                Ok(kept)
            }
        }
    }
    let f = Fixture::with_store(|values| {
        spawn_store(FailProtection(values), StoreWorkerLimits::default()).unwrap()
    })
    .await;
    let handle = f.finding().await;
    let generation = f.app.current().unwrap().generation;
    let history = f
        .app
        .work_history(generation.clone(), "alice".into(), "finding".into(), None)
        .await
        .unwrap();
    let run = history["runs"][0]["run"].as_str().unwrap().to_owned();
    assert!(matches!(
        f.app
            .protect_run(
                generation.clone(),
                "alice".into(),
                "finding".into(),
                run.clone()
            )
            .await,
        Err(ManagementError::ProtectionUnconfirmed)
    ));
    assert_eq!(
        f.worker.retention(handle).await.unwrap(),
        Retention::Protected
    );
    let evidence = f
        .app
        .work_history(generation, "alice".into(), "finding".into(), Some(run))
        .await
        .unwrap();
    assert_eq!(evidence["run"]["protected"], false);
    assert_eq!(f.calls.load(Ordering::SeqCst), 1);
    f.close().await;
}

#[tokio::test]
async fn retirement_reports_every_incomplete_downstream_and_requires_physical_completion() {
    use wes_engine::session::retirement::{RetirementError, RetirementPlan};
    let f = Fixture::new().await;
    f.finding().await;
    f.source("consumer", ":calc { return $finding; } > report")
        .await;
    let current = f.app.current().unwrap();
    let checkpoint = current.session.checkpoint().await.unwrap();
    let observed = current.session.observe().await.unwrap();
    let root = observed.state.names["finding"].node.clone();
    let child = observed.state.names["report"].node.clone();
    for state in [NodeState::Pending, NodeState::Running, NodeState::Stale] {
        for blocked in [&root, &child] {
            let mut snapshot = observed.clone();
            snapshot
                .state
                .execution
                .graph
                .set_state(blocked, state)
                .unwrap();
            let Err(RetirementError::Blocked(blockers)) =
                RetirementPlan::prepare(checkpoint.history(), &snapshot, "finding")
            else {
                panic!("{state:?} at {blocked} must refuse the entire closure");
            };
            assert_eq!(blockers.len(), 1);
            assert_eq!(blockers[0].node.as_ref(), Some(blocked));
            assert_eq!(blockers[0].state, format!("{state:?}"));
            assert_eq!(
                blockers[0].cells,
                [if blocked == &root {
                    "finding"
                } else {
                    "consumer"
                }]
            );
        }
    }
    for terminal in [
        NodeState::Ready,
        NodeState::Failed,
        NodeState::Cancelled,
        NodeState::Skipped,
    ] {
        let mut snapshot = observed.clone();
        snapshot
            .state
            .execution
            .graph
            .set_state(&child, terminal)
            .unwrap();
        RetirementPlan::prepare(checkpoint.history(), &snapshot, "finding").unwrap();
        snapshot.state.execution.executing.push(child.clone());
        assert!(matches!(
            RetirementPlan::prepare(checkpoint.history(), &snapshot, "finding"),
            Err(RetirementError::Blocked(_))
        ));
    }
    let mut streaming = observed.clone();
    streaming.state.execution.streaming.push(root.clone());
    assert!(matches!(
        RetirementPlan::prepare(checkpoint.history(), &streaming, "finding"),
        Err(RetirementError::Blocked(_))
    ));
    let mut preparing = observed.clone();
    preparing
        .cells
        .iter_mut()
        .find(|cell| cell.input.cell() == "finding")
        .unwrap()
        .reply = None;
    let Err(RetirementError::Blocked(blockers)) =
        RetirementPlan::prepare(checkpoint.history(), &preparing, "finding")
    else {
        panic!("preparing cell must be blocked")
    };
    assert!(
        blockers
            .iter()
            .any(|b| b.state == "Preparing" && b.cells == ["finding"])
    );
    checkpoint.resume().await;
    // The matrix only inspects cloned observations: live work and history survived.
    assert_eq!(current.session.observe().await.unwrap().cells.len(), 2);
    f.close().await;
}

#[tokio::test]
async fn failed_work_and_its_failed_dependents_can_be_deleted_without_successful_results() {
    let f = Fixture::new().await;
    f.source("failed", ":calc { return 1 / 0; } > failed").await;
    f.source("skipped", ":calc { return $failed; } > skipped")
        .await;
    let current = f.app.current().unwrap();
    let before = current.session.observe().await.unwrap();
    assert_eq!(
        before
            .state
            .execution
            .graph
            .node(&before.state.names["failed"].node)
            .unwrap()
            .state(),
        NodeState::Failed
    );
    assert_eq!(
        before
            .state
            .execution
            .graph
            .node(&before.state.names["skipped"].node)
            .unwrap()
            .state(),
        NodeState::Failed
    );
    let preview = f.work_preview("failed").await;
    assert_eq!(preview.cells, ["failed", "skipped"]);
    assert!(matches!(
        f.delete_work(preview, false, false).await,
        Err(ManagementError::Approval)
    ));
    f.delete_work(f.work_preview("failed").await, true, false)
        .await
        .unwrap();
    let after = current.session.observe().await.unwrap();
    assert!(after.cells.is_empty());
    assert!(after.state.execution.graph.is_empty());
    let checkpoint = current.session.checkpoint().await.unwrap();
    assert!(!checkpoint.history().journal().iter().any(|entry| matches!(
        entry,
        wes_engine::history::JournalEntry::Submitted(_)
            | wes_engine::history::JournalEntry::Command(_)
    )));
    checkpoint.resume().await;
    f.close().await;
}

#[tokio::test]
async fn confirmation_rechecks_lifecycle_and_refuses_stale_downstream_without_partial_deletion() {
    use wes_engine::session::retirement::RetirementError;
    let f = Fixture::new().await;
    f.finding().await;
    f.source("consumer", "catalog echo value:$finding > report")
        .await;
    let preview = f.work_preview("finding").await;
    f.source("edit", ":change $report value:changed").await;
    let before = f.app.current().unwrap().session.observe().await.unwrap();
    let result = f.delete_work(preview, true, false).await;
    let Err(ManagementError::Work(RetirementError::Blocked(blockers))) = result else {
        panic!("changed downstream must invalidate deletion")
    };
    assert!(
        blockers
            .iter()
            .any(|b| b.state == "Stale" && b.cells == ["consumer"])
    );
    let after = f.app.current().unwrap().session.observe().await.unwrap();
    assert_eq!(before.cells.len(), after.cells.len());
    assert_eq!(
        before.state.execution.graph.len(),
        after.state.execution.graph.len()
    );
    f.close().await;
}

#[tokio::test]
async fn cancelled_work_is_deletable_only_after_its_physical_invocation_exits() {
    use tokio::sync::Notify;
    struct Gated {
        entered: Arc<Notify>,
        release: Arc<Notify>,
    }
    impl Invoker for Gated {
        fn invoke(&self, call: Call, _: CancellationToken) -> InvocationFuture {
            let entered = self.entered.clone();
            let release = self.release.clone();
            Box::pin(async move {
                entered.notify_one();
                // Deliberately ignore cancellation to prove logical Cancelled is
                // not evidence of physical exit or permission to erase history.
                release.notified().await;
                Ok(call.arguments["value"].clone())
            })
        }
    }
    let root = temp();
    let calls = Arc::new(AtomicUsize::new(0));
    let mut configuration = config(root.path(), calls, Arc::new(AtomicBool::new(false)), None);
    let entered = Arc::new(Notify::new());
    let release = Arc::new(Notify::new());
    let provider = Arc::new(Gated {
        entered: entered.clone(),
        release: release.clone(),
    });
    configuration.workspace = Arc::new(move || {
        let mut workspace =
            Workspace::local(wes_engine::providers::LocalScope::new("fixture").unwrap());
        let mut capability = Capability::new(["echo"], Shape::Unknown, Safety::Safe);
        capability
            .parameters
            .push(Parameter::new("value", Shape::Unknown, true));
        workspace.register_provider(
            ProviderDescription::new("catalog", [capability], vec![]).unwrap(),
            provider.clone(),
        )?;
        Ok(workspace)
    });
    let (app, task) = wes::open(configuration).await.unwrap();
    accepted(
        submit(&app, "blocked", "catalog echo value:wait > blocked")
            .await
            .as_ref(),
    );
    tokio::time::timeout(Duration::from_secs(5), entered.notified())
        .await
        .unwrap();
    let current = app.current().unwrap();
    let node = current.session.observe().await.unwrap().state.names["blocked"]
        .node
        .clone();
    let preview =
        || app.preview_delete_work(current.generation.clone(), "alice".into(), "blocked".into());
    let Err(ManagementError::Work(wes_engine::session::retirement::RetirementError::Blocked(
        blockers,
    ))) = preview().await
    else {
        panic!("running invocation must block")
    };
    assert_eq!(blockers[0].state, "Running");
    current.session.cancel(node.clone()).await.unwrap();
    assert_eq!(
        current
            .session
            .observe()
            .await
            .unwrap()
            .state
            .execution
            .graph
            .node(&node)
            .unwrap()
            .state(),
        NodeState::Cancelled
    );
    let Err(ManagementError::Work(wes_engine::session::retirement::RetirementError::Blocked(
        blockers,
    ))) = preview().await
    else {
        panic!("cancelled invocation with live lease must block")
    };
    assert_eq!(blockers[0].state, "Cancelled");
    assert!(blockers[0].reason.contains("physically"));
    release.notify_one();
    idle(&app).await;
    let approved = preview().await.unwrap();
    app.delete_work(
        current.generation,
        "alice".into(),
        approved.token,
        false,
        false,
    )
    .await
    .unwrap();
    assert!(current.session.observe().await.unwrap().cells.is_empty());
    app.shutdown().await;
    task.join().await.unwrap();
}

#[tokio::test]
async fn live_workspace_deletion_keeps_shared_payload_until_the_final_live_reference() {
    let f = Fixture::new().await;
    let handle = f.finding().await;
    f.source("copy", ":workspace save \"peer\"").await;
    f.app
        .open_workspace(WorkspaceName::new("peer".into()).unwrap(), false)
        .await
        .unwrap();
    let peer = f.app.bound("peer").unwrap();
    let preview = f.work_preview("finding").await;
    assert_eq!(preview.shared_workspaces, ["peer"]);
    assert!(preview.payloads.is_empty());
    f.delete_work(preview, false, false).await.unwrap();
    assert!(f.worker.read(handle.clone()).await.unwrap().is_some());
    let peer_generation = peer.current().unwrap().generation;
    let preview = peer
        .preview_delete_work(
            peer_generation.clone(),
            "peer-client".into(),
            "finding".into(),
        )
        .await
        .unwrap();
    assert_eq!(preview.payloads, [handle.to_string()]);
    peer.delete_work(
        peer_generation.clone(),
        "peer-client".into(),
        preview.token,
        false,
        false,
    )
    .await
    .unwrap();
    assert!(f.worker.read(handle).await.unwrap().is_none());
    assert_eq!(peer.current().unwrap().generation, peer_generation);
    assert_eq!(f.app.current().unwrap().name.as_str(), "default");
    f.close().await;
}

#[tokio::test]
async fn peer_history_changes_expire_work_preview_and_release_invalidates_both_live_owners() {
    let f = Fixture::new().await;
    let handle = f.finding().await;
    f.source("copy", ":workspace save \"peer\"").await;
    f.app
        .open_workspace(WorkspaceName::new("peer".into()).unwrap(), false)
        .await
        .unwrap();
    let peer = f.app.bound("peer").unwrap();
    let preview = f.work_preview("finding").await;
    accepted(
        submit(
            &peer,
            "new-peer-history",
            "catalog echo value:another > other",
        )
        .await
        .as_ref(),
    );
    idle(&peer).await;
    assert!(matches!(
        f.delete_work(preview, false, false).await,
        Err(ManagementError::Stale)
    ));
    let release = f.preview(&handle).await;
    assert_eq!(release.workspaces, ["default", "peer"]);
    f.confirm(release).await.unwrap();
    assert!(f.worker.read(handle.clone()).await.unwrap().is_none());
    for app in [&f.app, &peer] {
        let observed = app.current().unwrap().session.observe().await.unwrap();
        assert!(
            !observed
                .values
                .unwrap()
                .outputs
                .values()
                .any(|output| output.handle() == Some(&handle))
        );
    }
    f.close().await;
}

#[tokio::test]
async fn whole_workspace_deletion_preserves_shared_protected_payload_until_its_last_owner_approves()
{
    let f = Fixture::new().await;
    let handle = f.finding().await;
    f.app
        .current()
        .unwrap()
        .session
        .keep(handle.clone())
        .await
        .unwrap();
    f.source("copy", ":workspace save \"copy\"").await;
    f.app
        .open_workspace(WorkspaceName::new("copy".into()).unwrap(), false)
        .await
        .unwrap();
    let first = f.app.current().unwrap();
    let preview = f
        .app
        .preview_workspace_deletion(first.generation.clone(), "qa".into())
        .await
        .unwrap();
    assert_eq!(preview.protected_payloads, 0);
    assert!(preview.shared_workspaces.contains(&"copy".into()));
    f.app
        .delete_workspace(first.generation, "qa".into(), preview.token, false, false)
        .await
        .unwrap();
    assert!(f.worker.read(handle.clone()).await.unwrap().is_some());
    let last = f.app.current().unwrap();
    assert_eq!(last.name.as_str(), "copy");
    let p = f
        .app
        .preview_workspace_deletion(last.generation.clone(), "qa".into())
        .await
        .unwrap();
    assert_eq!(p.protected_payloads, 1);
    assert!(matches!(
        f.app
            .delete_workspace(last.generation.clone(), "qa".into(), p.token, false, false)
            .await,
        Err(ManagementError::Approval)
    ));
    let p = f
        .app
        .preview_workspace_deletion(last.generation.clone(), "qa".into())
        .await
        .unwrap();
    f.app
        .delete_workspace(last.generation, "qa".into(), p.token, false, true)
        .await
        .unwrap();
    assert!(f.worker.read(handle).await.unwrap().is_none());
    f.app.shutdown().await;
    f.task.join().await.unwrap();
    f.worker.shutdown().await.unwrap();
    f.writer.join().await.unwrap();
}

#[tokio::test]
async fn typed_workspace_plan_requires_explicit_protected_consent() {
    let f = Fixture::new().await;
    let handle = f.finding().await;
    f.app
        .current()
        .unwrap()
        .session
        .keep(handle.clone())
        .await
        .unwrap();
    let source = |id: &str, text: &str| {
        f.app.submit(
            SourceInput::new(id.into(), text.into())
                .unwrap()
                .with_client("qa".into())
                .unwrap(),
        )
    };
    source("plan", ":workspace plan delete > plan")
        .await
        .unwrap();
    idle(&f.app).await;
    let error = source("refuse", ":workspace delete $plan")
        .await
        .unwrap_err();
    assert!(error.to_string().contains("STO"));
    assert!(f.worker.read(handle.clone()).await.unwrap().is_some());
    source("replan", ":workspace plan delete > plan")
        .await
        .unwrap();
    idle(&f.app).await;
    tokio::time::timeout(
        Duration::from_secs(10),
        source("delete", ":workspace delete $plan protected:true"),
    )
    .await
    .unwrap()
    .unwrap();
    assert!(f.worker.read(handle).await.unwrap().is_none());
    f.app.shutdown().await;
    f.task.join().await.unwrap();
    f.worker.shutdown().await.unwrap();
    f.writer.join().await.unwrap();
}

#[tokio::test]
async fn run_evidence_uses_exact_mutation_targets_across_reopen() {
    let f = Fixture::new().await;
    f.finding().await;
    let generation = f.app.current().unwrap().generation;
    let history = f
        .app
        .work_history(generation.clone(), "alice".into(), "finding".into(), None)
        .await
        .unwrap();
    let old_run = history["runs"][0]["run"].as_str().unwrap().to_owned();
    f.source("literal", "catalog echo value:\":change\" > other")
        .await;
    f.source("unrelated-edit", ":change $other value:changed")
        .await;
    f.source("refresh", ":refresh $finding").await;
    let history = f
        .app
        .work_history(generation.clone(), "alice".into(), "finding".into(), None)
        .await
        .unwrap();
    let unaffected_run = history["runs"]
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["run"] != old_run)
        .unwrap()["run"]
        .as_str()
        .unwrap()
        .to_owned();
    let protected = f
        .app
        .protect_run(
            generation.clone(),
            "alice".into(),
            "finding".into(),
            unaffected_run.clone(),
        )
        .await
        .unwrap();
    assert_eq!(
        protected["definition"]["text"],
        "catalog echo value:original > finding"
    );
    assert_eq!(protected["run"]["protected"], true);
    f.source("actual-edit", ":node change $finding value:edited")
        .await;
    f.source("refresh-edited", ":refresh $finding").await;
    let history = f
        .app
        .work_history(generation.clone(), "alice".into(), "finding".into(), None)
        .await
        .unwrap();
    let edited_run = history["runs"]
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["run"] != old_run && r["run"] != unaffected_run)
        .unwrap()["run"]
        .as_str()
        .unwrap()
        .to_owned();
    for reopen in [false, true] {
        let calls = f.calls.load(Ordering::SeqCst);
        if reopen {
            f.source("save-evidence", ":workspace save \"mutation-evidence\"")
                .await;
            f.source("load-evidence", ":workspace load \"mutation-evidence\"")
                .await;
        }
        let generation = f.app.current().unwrap().generation;
        for run in [&old_run, &unaffected_run] {
            let detail = f
                .app
                .work_history(
                    generation.clone(),
                    "alice".into(),
                    "finding".into(),
                    Some(run.clone()),
                )
                .await
                .unwrap();
            assert_eq!(
                detail["definition"]["text"],
                "catalog echo value:original > finding"
            );
        }
        assert!(matches!(
            f.app
                .work_history(
                    generation.clone(),
                    "alice".into(),
                    "finding".into(),
                    Some(edited_run.clone())
                )
                .await,
            Err(ManagementError::EvidenceMissing)
        ));
        assert!(matches!(
            f.app
                .protect_run(
                    generation,
                    "alice".into(),
                    "finding".into(),
                    edited_run.clone()
                )
                .await,
            Err(ManagementError::EvidenceMissing)
        ));
        assert_eq!(f.calls.load(Ordering::SeqCst), calls);
    }
    f.close().await;
}

#[tokio::test]
async fn pinned_projected_input_recovers_exact_protected_bytes_and_never_replays_missing_data() {
    use wes_engine::{
        session::{PinBinding, RetentionPolicy},
        views::InputBinding,
    };
    for missing in [false, true] {
        let f = Fixture::new().await;
        f.app
            .current()
            .unwrap()
            .session
            .set_retention_policy(RetentionPolicy {
                automatic: false,
                under: 1024,
            })
            .await
            .unwrap();
        f.source("data",":calc { return {body:{view:\"metric\",value:17},metadata:\"not retained by Pin\"}; } > response").await;
        f.source("view", ":view create Metric input:$response.body > chart")
            .await;
        f.source("pin", ":view pin $chart > pinned").await;
        let session = f.app.current().unwrap().session;
        let state = session.snapshot().await.unwrap();
        let view = state.names["chart"].node.clone();
        let node = state.names["pinned"].node.clone();
        let ValuePublication::Complete(published) =
            session.values().await.unwrap().unwrap().outputs[&node].clone()
        else {
            panic!("published pin")
        };
        assert!(published.durably_retained());
        assert!(matches!(published.pin, Some(PinBinding::Bound)));
        let selected = published.stored.unwrap().handle;
        assert_eq!(
            f.worker.retention(selected.clone()).await.unwrap(),
            Retention::Protected
        );
        assert!(matches!(
            f.app
                .preview_release(
                    f.app.current().unwrap().generation,
                    "alice".into(),
                    selected.clone()
                )
                .await,
            Err(ManagementError::PinnedViews(_))
        ));
        f.app.shutdown().await;
        f.task.join().await.unwrap();
        f.worker.shutdown().await.unwrap();
        f.writer.join().await.unwrap();
        let values = TieredValues::open(
            &f.root.path().join("live"),
            &f.root.path().join("archive"),
            Limits::default(),
            Durability::File,
            None,
        )
        .unwrap();
        let (worker, writer) = spawn_store(values, StoreWorkerLimits::default()).unwrap();
        if missing {
            assert!(worker.release(selected.clone()).await.unwrap());
        }
        let (app, task) = wes::open(config(
            f.root.path(),
            f.calls.clone(),
            Arc::new(AtomicBool::new(false)),
            Some(SessionStorage {
                worker: worker.clone(),
                auto_keep: AutoKeep::Never,
            }),
        ))
        .await
        .unwrap();
        let frame = app
            .current()
            .unwrap()
            .session
            .view_frame(view.clone())
            .await
            .unwrap()
            .instances
            .remove(0);
        let input = frame.input.unwrap();
        let InputBinding::Retained(reference) = &input.binding else {
            panic!("retained reference after restore")
        };
        assert_eq!(reference.handle(), &selected);
        assert_eq!(reference.node(), &node);
        assert_eq!(reference.origin().unwrap().fields, vec!["body"]);
        assert!(!frame.observing);
        if missing {
            assert!(input.value().is_none());
            assert!(frame.input_problem.is_some());
        } else {
            let Data::Record(data) = input.value().unwrap().data() else {
                panic!("body")
            };
            assert_eq!(data["value"], Data::Int(17));
            assert!(!data.contains_key("metadata"));
            assert!(frame.input_problem.is_none());
        }
        assert_eq!(f.calls.load(Ordering::SeqCst), 0);
        app.shutdown().await;
        task.join().await.unwrap();
        worker.shutdown().await.unwrap();
        writer.join().await.unwrap();
    }
}

#[tokio::test]
async fn release_refuses_a_saved_or_concurrently_open_peer_pinned_view_until_it_is_rebound() {
    for open_peer in [false, true] {
        let f = Fixture::new().await;
        f.source(
            "data",
            ":calc { return {view:\"metric\",value:23}; } > sample",
        )
        .await;
        f.source("view", ":view create Metric input:$sample > chart")
            .await;
        f.source("pin", ":view pin $chart > pinned").await;
        let state = f.app.current().unwrap().session.snapshot().await.unwrap();
        let selected = f
            .app
            .current()
            .unwrap()
            .session
            .values()
            .await
            .unwrap()
            .unwrap()
            .outputs[&state.names["pinned"].node]
            .handle()
            .unwrap()
            .clone();
        f.source("save", ":workspace save \"copy\"").await;
        f.source("rebind", ":view bind $chart input:$sample").await;
        if open_peer {
            f.app
                .open_workspace(WorkspaceName::new("copy".into()).unwrap(), false)
                .await
                .unwrap();
        }
        let refused = f
            .app
            .preview_release(
                f.app.current().unwrap().generation,
                "alice".into(),
                selected.clone(),
            )
            .await;
        assert!(
            matches!(refused, Err(ManagementError::PinnedViews(_))),
            "{refused:?}"
        );
        assert!(f.worker.read(selected.clone()).await.unwrap().is_some());
        let peer = if open_peer {
            f.app.bound("copy").unwrap()
        } else {
            f.app
                .open_workspace(WorkspaceName::new("copy".into()).unwrap(), false)
                .await
                .unwrap();
            f.app.bound("copy").unwrap()
        };
        accepted(
            submit(&peer, "peer-rebind", ":view bind $chart input:$sample")
                .await
                .as_ref(),
        );
        idle(&peer).await;
        let preview = f.preview(&selected).await;
        f.confirm(preview).await.unwrap();
        assert!(f.worker.read(selected).await.unwrap().is_none());
        assert_eq!(f.calls.load(Ordering::SeqCst), 0);
        f.close().await;
    }
}

#[tokio::test]
async fn a_foreign_receipt_with_identical_bytes_does_not_authorize_retained_input_recovery() {
    use wes_engine::history::{HistoryCapture, HistoryCaptureLimits, JournalEntry, Record};
    let f = Fixture::new().await;
    f.source(
        "data",
        ":calc { return {view:\"metric\",value:31}; } > sample",
    )
    .await;
    f.source("view", ":view create Metric input:$sample > chart")
        .await;
    f.source("pin1", ":view pin $chart > first").await;
    let state = f.app.current().unwrap().session.snapshot().await.unwrap();
    let view = state.names["chart"].node.clone();
    let foreign = f
        .app
        .current()
        .unwrap()
        .session
        .values()
        .await
        .unwrap()
        .unwrap()
        .outputs[&state.names["first"].node]
        .handle()
        .unwrap()
        .clone();
    f.source("pin2", ":view pin $chart > second").await;
    f.app.shutdown().await;
    f.task.join().await.unwrap();
    let mut files = wes_adapters::workspaces::FileWorkspaces::open(
        &f.root.path().join("workspaces"),
        wes_adapters::journal::ReadLimits::default(),
        Durability::File,
    )
    .unwrap();
    let name = WorkspaceName::new("default".into()).unwrap();
    let (writer, history) = files.load(&name).unwrap();
    drop(writer);
    let last = history
        .journal()
        .iter()
        .rposition(|entry| matches!(entry, JournalEntry::Views(_)))
        .unwrap();
    let mut capture = HistoryCapture::new(HistoryCaptureLimits::default());
    for (index, entry) in history.journal().iter().enumerate() {
        let mut entry = entry.clone();
        if index == last {
            let JournalEntry::Views(record) = &mut entry else {
                unreachable!()
            };
            let mut data = record.value.data().clone();
            let Data::List(entries) = &mut data else {
                panic!("view checkpoint")
            };
            let Data::Record(fields) = &mut entries[0] else {
                panic!("view entry")
            };
            let Data::List(input) = fields.get_mut("input").unwrap() else {
                panic!("input")
            };
            let Data::Record(reference) = &mut input[0] else {
                panic!("reference")
            };
            reference.insert("handle".into(), Data::Text(foreign.as_str().into()));
            record.value =
                wes_core::Value::new(Shape::Unknown, data, record.value.provenance().clone())
                    .unwrap();
            record.validate().unwrap(); // Structurally valid; receipt identity is nevertheless wrong.
        }
        capture.push(Record::Journal(entry)).unwrap();
    }
    for entry in history.recovery() {
        capture.push(Record::Recovery(entry.clone())).unwrap();
    }
    files
        .save(&name, &capture.finish(history.checkpoint()))
        .unwrap();
    drop(files);
    let (app, task) = wes::open(config(
        f.root.path(),
        f.calls.clone(),
        Arc::new(AtomicBool::new(false)),
        Some(SessionStorage {
            worker: f.worker.clone(),
            auto_keep: AutoKeep::Never,
        }),
    ))
    .await
    .unwrap();
    let frame = app
        .current()
        .unwrap()
        .session
        .view_frame(view)
        .await
        .unwrap()
        .instances
        .remove(0);
    assert!(frame.input.as_ref().unwrap().value().is_none());
    assert!(frame.input_problem.is_some());
    assert!(f.worker.read(foreign).await.unwrap().is_some());
    assert_eq!(f.calls.load(Ordering::SeqCst), 0);
    app.shutdown().await;
    task.join().await.unwrap();
    f.worker.shutdown().await.unwrap();
    f.writer.join().await.unwrap();
}
