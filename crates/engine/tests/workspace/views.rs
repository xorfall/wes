use super::*;
#[test]
fn unknown_render_command_never_invokes_a_provider() {
    let (w, calls) = workspace();
    let result = w.prepare(&statement(":render \"hello\" as:text"));
    assert!(
        matches!(result,Err(WorkspaceError::Rejected{ref diagnostics,..}) if diagnostics.iter().any(|d|d.code=="RES002"))
    );
    assert_eq!(calls.load(Ordering::SeqCst), 0);
    assert!(!wes_language::vocabulary::commands::roots().contains(&"render"));
}

#[tokio::test]
async fn historical_render_is_rejected_without_a_compatibility_engine() {
    use wes_engine::{history::CommandRecord, workspace::ReplayWorkspace};
    let mut replay = ReplayWorkspace::new(Workspace::local(
        wes_engine::providers::LocalScope::new("fixture").unwrap(),
    ))
    .unwrap();
    let record = CommandRecord {
        source_name: "fixture.wes".into(),
        source_start: wes_language::Position { line: 1, column: 1 },
        changed_nodes: vec![],
        document: None,
        revision_of: None,
        environments: None,
        cell: "old-render".into(),
        text: ":render 1 as:table > shown".into(),
        replay: ":render 1 as:table > shown".into(),
        nodes: vec![NodeId::new("id1000").unwrap()],
        type_sources: Default::default(),
        calculation_package: None,
        imports: vec![],
    };
    assert!(
        replay
            .prepare(&record, CancellationToken::new())
            .await
            .is_err()
    );
    let restored = replay.finish();
    assert!(restored.runtime().graph().is_empty());
    assert!(restored.resolve("shown").is_none());
}
