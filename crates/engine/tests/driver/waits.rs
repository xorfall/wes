use super::*;
fn selection(node: &NodeId, port: OutputPort) -> OutputSelection {
    OutputSelection::new([OutputRef {
        node: node.clone(),
        port,
    }])
    .unwrap()
}

#[tokio::test]
async fn cancellation_output_can_be_waited_without_waiting_for_physical_exit() {
    let (handle, task, mut started) = controlled(Runtime::new(), 1);
    let node = add(&handle, "work", vec![]).await;
    handle.command(Command::Start).await.unwrap();
    let work = started.recv().await.unwrap();
    handle.command(Command::Cancel(node.clone())).await.unwrap();
    assert!(
        handle
            .wait_outputs(selection(&node, OutputPort::Cancel), Duration::ZERO)
            .await
            .unwrap()
    );
    assert!(
        !handle
            .wait_outputs(selection(&node, OutputPort::Data), Duration::ZERO)
            .await
            .unwrap()
    );
    assert!(
        !handle
            .wait_outputs(selection(&node, OutputPort::Error), Duration::ZERO)
            .await
            .unwrap()
    );
    assert!(
        tokio::time::timeout(Duration::from_millis(5), handle.wait_idle())
            .await
            .is_err()
    );
    work.finish.send(Outcome::Produced(value(3))).unwrap();
    handle.wait_idle().await.unwrap();
    stop(handle, task).await;
}

#[tokio::test]
async fn waiting_neither_starts_pending_work_nor_cancels_it_on_wait_expiry() {
    let (handle, task, mut started) = controlled(Runtime::new(), 1);
    let node = add(&handle, "pending", vec![]).await;
    assert!(
        !handle
            .wait_outputs(selection(&node, OutputPort::Data), Duration::from_millis(5))
            .await
            .unwrap()
    );
    assert!(started.try_recv().is_err());
    let Reply::Snapshot(snapshot) = handle.command(Command::Snapshot).await.unwrap() else {
        panic!("snapshot")
    };
    assert_eq!(
        snapshot.graph.node(&node).unwrap().state(),
        NodeState::Pending
    );
    assert!(!snapshot.errors.contains_key(&node));
    stop(handle, task).await;
}

#[tokio::test]
async fn selected_error_wait_completes_after_failure_while_data_wait_reports_unavailable() {
    let (handle, task, mut started) = controlled(Runtime::new(), 1);
    let node = add(&handle, "work", vec![]).await;
    handle.command(Command::Start).await.unwrap();
    let work = started.recv().await.unwrap();
    let waiting_handle = handle.clone();
    let selected = selection(&node, OutputPort::Error);
    let waiting = tokio::spawn(async move {
        waiting_handle
            .wait_outputs(selected, Duration::from_secs(60))
            .await
    });
    work.finish
        .send(Outcome::Failed(
            RuntimeCode::ExecutionFailed.error("failed", None),
        ))
        .unwrap();
    assert!(waiting.await.unwrap().unwrap());
    assert!(
        !handle
            .wait_outputs(selection(&node, OutputPort::Data), Duration::ZERO)
            .await
            .unwrap()
    );
    stop(handle, task).await;
}

#[tokio::test]
async fn empty_missing_stale_and_oversized_selections_are_explicit() {
    let mut runtime = Runtime::new();
    let stale = NodeId::new("stale").unwrap();
    runtime
        .restore(
            stale.clone(),
            "restored",
            [],
            SAFE,
            RestoredState::Stale,
            None,
        )
        .unwrap();
    let (handle, task, _) = controlled(runtime, 1);
    assert!(
        handle
            .wait_outputs(OutputSelection::default(), Duration::ZERO)
            .await
            .unwrap()
    );
    assert!(
        !handle
            .wait_outputs(selection(&stale, OutputPort::Data), Duration::ZERO)
            .await
            .unwrap()
    );
    assert!(
        !handle
            .wait_outputs(
                selection(&NodeId::new("missing").unwrap(), OutputPort::Error),
                Duration::ZERO
            )
            .await
            .unwrap()
    );
    let huge = OutputSelection::new(
        (0..10_001).map(|n| OutputRef::data(NodeId::new(format!("node{n}")).unwrap())),
    )
    .unwrap();
    assert_eq!(
        handle.wait_outputs(huge, Duration::ZERO).await,
        Err(DriverError::InvalidWait)
    );
    stop(handle, task).await;
}

#[tokio::test]
async fn shutdown_ends_output_waits_but_still_joins_running_work() {
    let (handle, task, mut started) = controlled(Runtime::new(), 1);
    let node = add(&handle, "work", vec![]).await;
    handle.command(Command::Start).await.unwrap();
    let work = started.recv().await.unwrap();
    let waiting_handle = handle.clone();
    let waiting = tokio::spawn(async move {
        waiting_handle
            .wait_outputs(selection(&node, OutputPort::Data), Duration::from_secs(60))
            .await
    });
    handle.command(Command::Shutdown).await.unwrap();
    assert_eq!(waiting.await.unwrap(), Err(DriverError::Stopped));
    assert!(work.token.is_cancelled());
    work.finish.send(Outcome::Produced(value(1))).unwrap();
    task.join().await.unwrap();
}

#[test]
fn output_selection_deduplicates_exact_channels_but_rejects_mutually_exclusive_ones() {
    let node = NodeId::new("one").unwrap();
    let selected =
        OutputSelection::new([OutputRef::data(node.clone()), OutputRef::data(node.clone())])
            .unwrap();
    assert_eq!(selected.len(), 1);
    assert!(
        OutputSelection::new([
            OutputRef::data(node.clone()),
            OutputRef {
                node,
                port: OutputPort::Cancel
            }
        ])
        .is_err()
    );
}
