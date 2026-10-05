use super::*;

#[tokio::test]
async fn downstream_refresh_repeats_finite_effects_once_and_preserves_unrelated_work() {
    let (mut base, calls) = workspace(None, None);
    let mut capability = Capability::new(["write"], Shape::Unknown, Safety::Unsafe);
    capability.parameters = vec![Parameter::new("value", Shape::Unknown, true)];
    base.register_provider(
        ProviderDescription::new("effects", [capability], vec![]).unwrap(),
        Arc::new(Echo {
            calls: calls.clone(),
            gate: None,
            entered: Mutex::new(None),
        }),
    )
    .unwrap();
    let (handle, task) = session::spawn(
        base,
        RecordingMode::Ephemeral,
        no_files(),
        NonZeroUsize::new(2).unwrap(),
    )
    .unwrap();
    for (cell, source) in [
        ("root", "effects write value:hello > root"),
        ("local", ":calc { return $root; } > local"),
        ("child", "effects write value:$local > child"),
        ("other", "catalog echo value:outside > other"),
    ] {
        let reply = submit(&handle, cell, source).await;
        assert!(
            reply.diagnostics.diagnostics.is_empty(),
            "{source}: {:?}",
            reply.diagnostics
        );
        handle.wait_idle().await.unwrap();
    }
    let before = handle.snapshot().await.unwrap();
    let request = input("refresh", ":refresh $root scope:downstream");
    let accepted = handle.submit(request.clone()).await.unwrap();
    assert!(!accepted.recorded);
    assert!(
        accepted.nodes.is_empty(),
        "refresh must not claim declarations"
    );
    assert_eq!(accepted.refreshed.len(), 3);
    for name in ["root", "local", "child"] {
        assert!(accepted.refreshed.contains(&before.names[name].node));
    }
    assert_eq!(accepted.accepted.len(), 1);
    assert!(Arc::ptr_eq(
        &accepted,
        &handle.submit(request).await.unwrap()
    ));
    handle.wait_idle().await.unwrap();
    assert_eq!(calls.load(Ordering::SeqCst), 5);
    let after = handle.snapshot().await.unwrap();
    for name in ["root", "local", "child"] {
        let node = &before.names[name].node;
        assert_ne!(before.execution.runs[node], after.execution.runs[node]);
        assert_eq!(
            after.execution.graph.node(node).unwrap().state(),
            NodeState::Ready
        );
    }
    let other = &before.names["other"].node;
    assert_eq!(before.execution.runs[other], after.execution.runs[other]);
    assert_eq!(after.execution.graph.len(), before.execution.graph.len());
    stop(handle, task).await;
}

#[tokio::test]
async fn downstream_refresh_unavailable_external_input_rejects_before_root_starts() {
    let (base, calls) = workspace(None, None);
    let (handle, task) = session::spawn(
        base,
        RecordingMode::Ephemeral,
        no_files(),
        NonZeroUsize::new(2).unwrap(),
    )
    .unwrap();
    for (cell, source) in [
        ("root", "catalog echo value:root > root"),
        ("outside", ":calc { return 1 / 0; } > outside"),
        ("join", ":calc { return [$root, $outside]; } > joined"),
    ] {
        let reply = submit(&handle, cell, source).await;
        assert!(reply.diagnostics.diagnostics.is_empty());
        handle.wait_idle().await.unwrap();
    }
    let before = handle.snapshot().await.unwrap();
    let reply = submit(&handle, "refresh", ":refresh $root scope:downstream").await;
    assert!(reply.accepted.is_empty());
    assert!(reply.refreshed.is_empty());
    assert!(!reply.diagnostics.diagnostics.is_empty());
    handle.wait_idle().await.unwrap();
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    let after = handle.snapshot().await.unwrap();
    assert_eq!(before.execution.runs, after.execution.runs);
    for name in ["root", "joined"] {
        let node = &before.names[name].node;
        assert_eq!(
            before.execution.graph.node(node).unwrap().state(),
            after.execution.graph.node(node).unwrap().state()
        );
    }
    stop(handle, task).await;
}

#[tokio::test]
async fn downstream_refresh_busy_closure_rejects_without_cancelling_existing_work() {
    let gate = Arc::new(Notify::new());
    let (entered, receive) = oneshot::channel();
    let (base, calls) = workspace(Some(gate.clone()), Some(entered));
    let (handle, task) = session::spawn(
        base,
        RecordingMode::Ephemeral,
        no_files(),
        NonZeroUsize::new(1).unwrap(),
    )
    .unwrap();
    submit(&handle, "root", "catalog echo value:blocked > root").await;
    tokio::time::timeout(Duration::from_secs(5), receive)
        .await
        .unwrap()
        .unwrap();
    let before = handle.snapshot().await.unwrap();
    let reply = submit(&handle, "refresh", ":refresh $root scope:downstream").await;
    assert!(reply.accepted.is_empty());
    assert!(reply.refreshed.is_empty());
    assert!(!reply.diagnostics.diagnostics.is_empty());
    let after = handle.snapshot().await.unwrap();
    let node = &before.names["root"].node;
    assert_eq!(
        after.execution.graph.node(node).unwrap().state(),
        NodeState::Running
    );
    assert_eq!(before.execution.runs, after.execution.runs);
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    gate.notify_one();
    handle.wait_idle().await.unwrap();
    stop(handle, task).await;
}

#[tokio::test]
async fn refresh_downstream_example_preserves_plain_refresh_and_runs_actual_recipe() {
    let (base, _) = workspace(None, None);
    let (handle, task) = session::spawn(
        base,
        RecordingMode::Ephemeral,
        no_files(),
        NonZeroUsize::new(2).unwrap(),
    )
    .unwrap();
    let lines: Vec<_> = include_str!("../../../../examples/refresh-downstream/work.wes")
        .lines()
        .filter(|line| !line.trim().is_empty())
        .collect();
    for (index, line) in lines[..lines.len() - 1].iter().enumerate() {
        let reply = submit(&handle, &format!("recipe-{index}"), line).await;
        assert!(
            reply.diagnostics.diagnostics.is_empty(),
            "{line}: {:?}",
            reply.diagnostics
        );
        handle.wait_idle().await.unwrap();
    }
    let before = handle.snapshot().await.unwrap();
    submit(&handle, "plain-refresh", ":refresh $source").await;
    handle.wait_idle().await.unwrap();
    let plain = handle.snapshot().await.unwrap();
    assert_eq!(
        plain
            .execution
            .graph
            .node(&plain.names["tripled"].node)
            .unwrap()
            .state(),
        NodeState::Stale
    );
    let reply = submit(&handle, "downstream", lines.last().unwrap()).await;
    assert_eq!(reply.accepted.len(), 1);
    handle.wait_idle().await.unwrap();
    let after = handle.snapshot().await.unwrap();
    for name in ["source", "tripled", "total"] {
        let node = &before.names[name].node;
        assert_eq!(
            after.execution.graph.node(node).unwrap().state(),
            NodeState::Ready
        );
        assert_ne!(before.execution.runs[node], after.execution.runs[node]);
    }
    assert_eq!(
        after.execution.values[&after.names["total"].node].data(),
        &Data::Int(7)
    );
    let other = &before.names["unrelated"].node;
    assert_eq!(before.execution.runs[other], after.execution.runs[other]);
    stop(handle, task).await;
}
