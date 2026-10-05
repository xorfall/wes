use super::*;
use wes_engine::workspace::{Preparation, Workspace, WorkspaceError};

fn workspace(recording: &Recording) -> Workspace {
    let mut workspace =
        Workspace::local(wes_engine::providers::LocalScope::new("fixture").unwrap());
    let mut capability = Capability::new(["echo"], Shape::Unknown, Safety::Safe);
    capability.parameters = vec![Parameter::new("value", Shape::Unknown, true)];
    workspace
        .register_provider(
            ProviderDescription::new("catalog", [capability], vec![]).unwrap(),
            Arc::new(Echo {
                timeline: recording.timeline.clone(),
                result: None,
                panic: false,
            }),
        )
        .unwrap();
    workspace
}

async fn batch(workspace: &Workspace) -> (wes_engine::source::PreparedSource, CommandRecord) {
    use wes_engine::source::{SourceInput, SourcePreparation, prepare_declarations};
    let text = ":def named as catalog echo value:?value\nnamed value:hello > first\ninvalid provider call\ncatalog echo value:$first > second";
    let prepared = prepare_declarations(
        SourceInput::new("batch-cell".into(), text.into()).unwrap(),
        workspace.draft().unwrap(),
        wes_engine::type_sources::TypeSourceCapture::replay(IndexMap::new()).unwrap(),
        CancellationToken::new(),
    )
    .await
    .unwrap();
    let SourcePreparation::Declarations(prepared) = prepared else {
        panic!("declarations")
    };
    let record = prepared.record().unwrap().clone();
    assert_eq!(record.nodes.len(), 2);
    assert!(!record.replay.contains("invalid"));
    (prepared, record)
}

#[tokio::test]
async fn blocked_admission_does_not_install_nodes_or_prevent_live_cancellation() {
    let (entered, entered_rx) = oneshot::channel();
    let (release, released) = mpsc::channel();
    let mut sink = sink();
    sink.gate = Some(("command", entered, released));
    let recording = Recording::new(sink);
    let mut workspace = workspace(&recording);
    let parsed = parse(&SourceText::new("test", "catalog echo value:old > old"));
    let Preparation::Change(root) = workspace.prepare(&parsed.script.statements[0]).unwrap() else {
        panic!()
    };
    let root = workspace.commit(root).unwrap().node.unwrap();
    let running = workspace
        .start(Duration::ZERO)
        .into_iter()
        .find_map(|effect| {
            if let Effect::Spawn(ticket) = effect {
                Some(ticket)
            } else {
                None
            }
        })
        .unwrap();
    assert!(workspace.enter(&running.run));
    let (batch, record) = batch(&workspace).await;
    let expected = record.clone();
    let journal = recording.journal.clone();
    let pending = tokio::spawn(async move { journal.admit(record).await });
    tokio::time::timeout(Duration::from_secs(5), entered_rx)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(workspace.runtime().graph().len(), 1);
    assert!(workspace.resolve("first").is_none());
    assert!(!pending.is_finished());
    workspace.cancel(&root, Duration::from_secs(1));
    assert_eq!(
        workspace.runtime().graph().node(&root).unwrap().state(),
        NodeState::Cancelled
    );
    assert!(workspace.runtime().is_executing(&root));
    release.send(()).unwrap();
    let admitted = pending.await.unwrap().unwrap();
    batch
        .with_admission(admitted)
        .unwrap()
        .commit(&mut workspace, Duration::ZERO)
        .unwrap();
    assert_eq!(workspace.runtime().graph().len(), 3);
    assert!(workspace.runtime().is_executing(&root));
    let reason = workspace.runtime().error_of(&root).unwrap().clone();
    workspace.complete(
        &running.run,
        Outcome::Cancelled(reason),
        Duration::from_secs(2),
    );
    let work = workspace
        .start(Duration::from_secs(3))
        .into_iter()
        .find_map(|effect| {
            if let Effect::Spawn(ticket) = effect {
                Some(ticket)
            } else {
                None
            }
        })
        .unwrap();
    let run = work.run.clone();
    assert!(workspace.enter(&run));
    let report = wes_engine::tasks::TaskExecutor::recorded(recording.journal.clone())
        .execute(work, CancellationToken::new())
        .await;
    assert!(matches!(report.outcome, Outcome::Produced(_)));
    workspace.complete(&run, report.outcome, Duration::from_secs(4));
    assert_eq!(
        *recording.timeline.lock().unwrap(),
        ["command", "accepted", "calling", "invoke", "called"]
    );
    assert_eq!(
        recording.records.lock().unwrap()[0],
        Record::Journal(JournalEntry::Command(expected))
    );
    recording.finish().await;
}

#[tokio::test]
async fn source_receipt_for_another_cell_cannot_authorize_identical_node_ids() {
    let recording = Recording::new(sink());
    let workspace = workspace(&recording);
    let (batch, mut record) = batch(&workspace).await;
    record.cell = "another-cell".into();
    let receipt = recording.journal.admit(record).await.unwrap();
    assert!(matches!(
        batch.with_admission(receipt),
        Err(WorkspaceError::AdmissionMismatch)
    ));
    assert!(workspace.runtime().graph().is_empty());
    recording.finish().await;
}

#[tokio::test]
async fn failed_admission_or_incomplete_receipt_leaves_the_whole_batch_uninstalled() {
    for phase in ["command", "accepted"] {
        let mut sink = sink();
        sink.fail = Some(phase);
        let recording = Recording::new(sink);
        let workspace = workspace(&recording);
        let (batch, record) = batch(&workspace).await;
        assert!(recording.journal.admit(record).await.is_err());
        drop(batch);
        assert!(workspace.runtime().graph().is_empty());
        assert!(!workspace.templates().contains("named"));
        assert!(!recording.timeline.lock().unwrap().contains(&"invoke"));
        recording.finish().await;
    }
    let recording = Recording::new(sink());
    let workspace = workspace(&recording);
    let (batch, mut record) = batch(&workspace).await;
    record.nodes.pop();
    let incomplete = recording.journal.admit(record).await.unwrap();
    assert!(matches!(
        batch.with_admission(incomplete),
        Err(WorkspaceError::AdmissionMismatch)
    ));
    assert!(workspace.runtime().graph().is_empty());
    recording.finish().await;
}
