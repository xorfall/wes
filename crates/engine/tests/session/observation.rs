use super::*;
#[tokio::test]
async fn coherent_observation_survives_notification_lag_and_reading_does_not_generate_wakeups() {
    let (base, calls) = workspace(None, None);
    let (handle, task) = session::spawn(
        base,
        RecordingMode::Ephemeral,
        no_files(),
        NonZeroUsize::new(1).unwrap(),
    )
    .unwrap();
    let mut updates = handle.subscribe_updates().unwrap();
    let initial = handle.observe().await.unwrap();
    assert!(initial.cells.is_empty());
    assert!(matches!(
        updates.try_recv(),
        Err(tokio::sync::broadcast::error::TryRecvError::Empty)
    ));
    for n in 0..4 {
        submit(
            &handle,
            &format!("cell{n}"),
            &format!("catalog echo value:v{n} > name{n}"),
        )
        .await;
    }
    handle.wait_idle().await.unwrap();
    assert!(matches!(
        updates.try_recv(),
        Err(tokio::sync::broadcast::error::TryRecvError::Lagged(_))
    ));
    let observed = handle.observe().await.unwrap();
    assert_eq!(observed.cells.len(), 4);
    assert_eq!(observed.state.execution.graph.len(), 4);
    assert_eq!(calls.load(Ordering::SeqCst), 4);
    for cell in &observed.cells {
        let result = cell.reply.as_ref().unwrap().as_ref().unwrap();
        assert_eq!(result.nodes.len(), 1);
        assert!(
            observed
                .state
                .execution
                .values
                .contains_key(&result.nodes[0])
        );
        assert!(observed.log.entries.iter().any(|e|matches!(e.entry(),JournalEntry::Observed(r) if r.node()==&result.nodes[0] && r.state()==NodeState::Ready)));
    }
    while updates.try_recv().is_ok() {}
    for _ in 0..3 {
        handle.observe().await.unwrap();
        handle.snapshot().await.unwrap();
        handle.log().await.unwrap();
        handle.values().await.unwrap();
    }
    assert!(matches!(
        updates.try_recv(),
        Err(tokio::sync::broadcast::error::TryRecvError::Empty)
    ));
    handle.shutdown().await.unwrap();
    task.join().await.unwrap();
    assert!(handle.subscribe_updates().is_err());
}
#[tokio::test]
async fn typed_cancel_never_parses_the_node_id_as_source() {
    let (base, calls) = workspace(None, None);
    let (handle, task) = session::spawn(
        base,
        RecordingMode::Ephemeral,
        no_files(),
        NonZeroUsize::new(1).unwrap(),
    )
    .unwrap();
    let node = wes_engine::graph::NodeId::new("missing\n:help > surprise").unwrap();
    assert!(matches!(
        handle.cancel(node).await,
        Err(SessionError::UnknownNode)
    ));
    assert!(handle.observe().await.unwrap().cells.is_empty());
    assert_eq!(calls.load(Ordering::SeqCst), 0);
    handle.shutdown().await.unwrap();
    task.join().await.unwrap();
}
