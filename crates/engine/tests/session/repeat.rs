use super::*;

fn repeat(attempt: &str, source: &str, confirmed: bool) -> SourceInput {
    input(attempt, source)
        .with_repeat("original".into(), confirmed)
        .unwrap()
}

#[tokio::test]
async fn pipeline_repeat_runs_unsafe_waiting_stages_once_and_suffix_preserves_upstream() {
    let (mut base, calls) = workspace(None, None);
    let mut capability = Capability::new(["write"], Shape::Unknown, Safety::Unsafe);
    capability.parameters = vec![Parameter::new("value", Shape::Unknown, true)];
    base.register_provider(
        ProviderDescription::new("effectful", [capability], vec![]).unwrap(),
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
        NonZeroUsize::new(1).unwrap(),
    )
    .unwrap();
    let source =
        "effectful write value:hello | effectful write value:input | effectful write value:input";
    let first = submit(&handle, "original", source).await;
    assert_eq!(first.nodes.len(), 3);
    handle.wait_idle().await.unwrap();
    let before = handle.snapshot().await.unwrap().execution.runs;
    assert!(matches!(
        handle.submit(repeat("refused", source, false)).await,
        Err(SessionError::RepeatRefused(_))
    ));
    assert_eq!(calls.load(Ordering::SeqCst), 3);
    let request = repeat("whole", source, true);
    let accepted = handle.submit(request.clone()).await.unwrap();
    assert!(Arc::ptr_eq(
        &accepted,
        &handle.submit(request).await.unwrap()
    ));
    handle.wait_idle().await.unwrap();
    assert_eq!(calls.load(Ordering::SeqCst), 6);
    let whole = handle.snapshot().await.unwrap().execution.runs;
    for node in &first.nodes {
        assert_ne!(before[node], whole[node]);
    }
    let suffix = repeat("suffix", source, true)
        .with_repeat_from(Some(first.nodes[1].clone()))
        .unwrap();
    let accepted = handle.submit(suffix.clone()).await.unwrap();
    assert_eq!(accepted.nodes, first.nodes);
    assert!(Arc::ptr_eq(
        &accepted,
        &handle.submit(suffix.clone()).await.unwrap()
    ));
    assert!(matches!(
        handle.submit(repeat("suffix", source, true)).await,
        Err(SessionError::Conflict)
    ));
    handle.wait_idle().await.unwrap();
    let after = handle.snapshot().await.unwrap().execution.runs;
    assert_eq!(after[&first.nodes[0]], whole[&first.nodes[0]]);
    for node in &first.nodes[1..] {
        assert_ne!(after[node], whole[node]);
    }
    assert_eq!(calls.load(Ordering::SeqCst), 8);
    assert_eq!(handle.snapshot().await.unwrap().execution.graph.len(), 3);
    stop(handle, task).await;
}

#[tokio::test]
async fn cancelling_pipeline_closes_waiting_stages_without_entering_them() {
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
    let source = "catalog echo value:blocked | catalog echo value:input | catalog echo value:input";
    let first = submit(&handle, "original", source).await;
    tokio::time::timeout(Duration::from_secs(5), receive)
        .await
        .unwrap()
        .unwrap();
    handle.cancel_work("original".into()).await.unwrap();
    gate.notify_one();
    tokio::time::timeout(Duration::from_secs(5), handle.wait_idle())
        .await
        .unwrap()
        .unwrap();
    let snapshot = handle.snapshot().await.unwrap();
    for node in &first.nodes {
        assert_eq!(
            snapshot.execution.graph.node(node).unwrap().state(),
            NodeState::Cancelled
        );
    }
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    handle.cancel_work("original".into()).await.unwrap();
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    stop(handle, task).await;
}

#[tokio::test]
async fn effectful_repeat_needs_confirmation_and_retry_cannot_upgrade_intent() {
    let (mut workspace, calls) = workspace(None, None);
    let mut capability = Capability::new(["write"], Shape::Unknown, Safety::Unsafe);
    capability.parameters = vec![Parameter::new("value", Shape::Unknown, true)];
    workspace
        .register_provider(
            ProviderDescription::new("effectful", [capability], vec![]).unwrap(),
            Arc::new(Echo {
                calls: calls.clone(),
                gate: None,
                entered: Mutex::new(None),
            }),
        )
        .unwrap();
    let (handle, task) = session::spawn(
        workspace,
        RecordingMode::Ephemeral,
        no_files(),
        NonZeroUsize::new(1).unwrap(),
    )
    .unwrap();
    let source = "effectful write value:hello";
    submit(&handle, "original", source).await;
    handle.wait_idle().await.unwrap();
    assert!(matches!(
        handle.submit(repeat("unconfirmed", source, false)).await,
        Err(SessionError::RepeatRefused(_))
    ));
    assert!(matches!(
        handle.submit(repeat("unconfirmed", source, true)).await,
        Err(SessionError::Conflict)
    ));
    let request = repeat("confirmed", source, true);
    let accepted = handle.submit(request.clone()).await.unwrap();
    assert!(Arc::ptr_eq(
        &accepted,
        &handle.submit(request).await.unwrap()
    ));
    handle.wait_idle().await.unwrap();
    assert_eq!(calls.load(Ordering::SeqCst), 2);
    stop(handle, task).await;
}

#[tokio::test]
async fn developer_iteration_80_runs_keeps_one_node_and_retries_do_not_execute() {
    let (workspace, calls) = workspace(None, None);
    let (handle, task) = session::spawn(
        workspace,
        RecordingMode::Ephemeral,
        no_files(),
        NonZeroUsize::new(1).unwrap(),
    )
    .unwrap();
    let source = "catalog echo value:hello";
    let first = submit(&handle, "original", source).await;
    let node = &first.nodes[0];
    handle.wait_idle().await.unwrap();
    let mut runs = std::collections::BTreeSet::new();
    runs.insert(handle.snapshot().await.unwrap().execution.runs[node].to_string());
    for n in 1..80 {
        let request = repeat(&format!("repeat-{n}"), source, false);
        let accepted = handle.submit(request.clone()).await.unwrap();
        assert_eq!(accepted.nodes, first.nodes);
        assert!(runs.insert(accepted.repeated_run.as_ref().unwrap().to_string()));
        assert!(Arc::ptr_eq(
            &accepted,
            &handle.submit(request).await.unwrap()
        ));
        handle.wait_idle().await.unwrap();
        assert_eq!(handle.snapshot().await.unwrap().execution.graph.len(), 1);
    }
    assert_eq!(calls.load(Ordering::SeqCst), 80);
    assert_eq!(runs.len(), 80);
    let fork = submit(&handle, "fork", source).await;
    assert_ne!(fork.nodes, first.nodes);
    handle.wait_idle().await.unwrap();
    assert_eq!(handle.snapshot().await.unwrap().execution.graph.len(), 2);
    assert_eq!(calls.load(Ordering::SeqCst), 81);
    stop(handle, task).await;
}

#[tokio::test]
async fn repeat_refuses_definition_source_environment_drift_and_multi_command_origins() {
    let (workspace, calls) = workspace(None, None);
    let (handle, task) = session::spawn(
        workspace,
        RecordingMode::Ephemeral,
        no_files(),
        NonZeroUsize::new(1).unwrap(),
    )
    .unwrap();
    let source = "catalog echo value:hello > answer";
    submit(&handle, "original", source).await;
    handle.wait_idle().await.unwrap();
    assert!(matches!(
        handle
            .submit(repeat("text", "catalog echo value:other", false))
            .await,
        Err(SessionError::RepeatRefused(_))
    ));
    let changed_context = repeat("environment", source, false)
        .with_environments(wes_core::environments::EnvironmentContext {
            selected: None,
            revisions: Default::default(),
        })
        .unwrap();
    assert!(matches!(
        handle.submit(changed_context).await,
        Err(SessionError::RepeatRefused(_))
    ));
    submit(&handle, "change", ":change $answer value:changed").await;
    handle.wait_idle().await.unwrap();
    let before = calls.load(Ordering::SeqCst);
    assert!(matches!(
        handle.submit(repeat("definition", source, true)).await,
        Err(SessionError::RepeatRefused(_))
    ));
    assert_eq!(calls.load(Ordering::SeqCst), before);
    let multi = "catalog echo value:a; catalog echo value:b";
    submit(&handle, "multi", multi).await;
    handle.wait_idle().await.unwrap();
    assert!(matches!(
        handle
            .submit(
                input("multi-repeat", multi)
                    .with_repeat("multi".into(), true)
                    .unwrap()
            )
            .await,
        Err(SessionError::RepeatRefused(_))
    ));
    stop(handle, task).await;
}

#[tokio::test]
async fn outstanding_work_is_not_a_successful_no_op_and_refused_attempt_stays_refused() {
    let gate = Arc::new(Notify::new());
    let (entered, receive) = oneshot::channel();
    let (workspace, calls) = workspace(Some(gate.clone()), Some(entered));
    let (handle, task) = session::spawn(
        workspace,
        RecordingMode::Ephemeral,
        no_files(),
        NonZeroUsize::new(1).unwrap(),
    )
    .unwrap();
    let source = "catalog echo value:blocked";
    submit(&handle, "original", source).await;
    tokio::time::timeout(Duration::from_secs(5), receive)
        .await
        .unwrap()
        .unwrap();
    let request = repeat("busy", source, true);
    assert!(matches!(
        handle.submit(request.clone()).await,
        Err(SessionError::RepeatRefused(_))
    ));
    gate.notify_one();
    tokio::time::timeout(Duration::from_secs(5), handle.wait_idle())
        .await
        .unwrap()
        .unwrap();
    assert!(matches!(
        handle.submit(request).await,
        Err(SessionError::RepeatRefused(_))
    ));
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    stop(handle, task).await;
}

#[tokio::test]
async fn downstream_invalidation_requires_acknowledgement_and_keeps_dependency_identity() {
    let (workspace, _) = workspace(None, None);
    let (handle, task) = session::spawn(
        workspace,
        RecordingMode::Ephemeral,
        no_files(),
        NonZeroUsize::new(1).unwrap(),
    )
    .unwrap();
    let source = "catalog echo value:hello > answer";
    let original = submit(&handle, "original", source).await;
    handle.wait_idle().await.unwrap();
    let dependent = submit(&handle, "dependent", "catalog echo value:$answer").await;
    handle.wait_idle().await.unwrap();
    assert!(matches!(
        handle.submit(repeat("no-consent", source, false)).await,
        Err(SessionError::RepeatRefused(_))
    ));
    let accepted = handle
        .submit(repeat("confirmed", source, true))
        .await
        .unwrap();
    handle.wait_idle().await.unwrap();
    let snapshot = handle.snapshot().await.unwrap();
    assert_eq!(accepted.nodes, original.nodes);
    assert_eq!(snapshot.execution.graph.len(), 2);
    assert!(
        snapshot
            .execution
            .graph
            .node(&dependent.nodes[0])
            .unwrap()
            .dependencies()
            .contains_key(&original.nodes[0])
    );
    stop(handle, task).await;
}

#[tokio::test]
async fn revisions_preserve_old_definitions_and_edges_and_refuse_stale_edits() {
    let (base, calls) = workspace(None, None);
    let (handle, task) = session::spawn(
        base,
        RecordingMode::Ephemeral,
        no_files(),
        NonZeroUsize::new(1).unwrap(),
    )
    .unwrap();
    let old = submit(&handle, "original", "catalog echo value:old > result").await;
    let dependent = submit(&handle, "dependent", "catalog echo value:$result").await;
    handle.wait_idle().await.unwrap();
    let before = handle.snapshot().await.unwrap();
    let edges = before
        .execution
        .graph
        .node(&dependent.nodes[0])
        .unwrap()
        .dependencies()
        .clone();
    let request = input("revision", "catalog echo value:new > result")
        .with_revision("original".into())
        .unwrap();
    let revised = handle.submit(request.clone()).await.unwrap();
    assert!(Arc::ptr_eq(
        &revised,
        &handle.submit(request).await.unwrap()
    ));
    handle.wait_idle().await.unwrap();
    let after = handle.snapshot().await.unwrap();
    assert_ne!(old.nodes, revised.nodes);
    assert_eq!(
        before.execution.values[&old.nodes[0]],
        after.execution.values[&old.nodes[0]]
    );
    assert_eq!(
        &edges,
        after
            .execution
            .graph
            .node(&dependent.nodes[0])
            .unwrap()
            .dependencies()
    );
    assert_eq!(
        before.execution.runs[&dependent.nodes[0]],
        after.execution.runs[&dependent.nodes[0]]
    );
    assert_eq!(calls.load(Ordering::SeqCst), 3);
    assert!(matches!(
        handle
            .submit(
                input("stale-edit", "catalog echo value:lost")
                    .with_revision("original".into())
                    .unwrap()
            )
            .await,
        Err(SessionError::RepeatRefused(_))
    ));
    let rejected = handle
        .submit(
            input("meta-edit", ":node remove $result scope:downstream")
                .with_revision("revision".into())
                .unwrap(),
        )
        .await
        .unwrap();
    assert!(rejected.nodes.is_empty());
    assert!(!rejected.diagnostics.diagnostics.is_empty());
    let next = submit(&handle, "new-dependent", "catalog echo value:$result").await;
    handle.wait_idle().await.unwrap();
    assert!(
        handle
            .snapshot()
            .await
            .unwrap()
            .execution
            .graph
            .node(&next.nodes[0])
            .unwrap()
            .dependencies()
            .iter()
            .any(|(node, _)| node == &revised.nodes[0])
    );
    stop(handle, task).await;
}

#[tokio::test]
async fn repeat_rejects_document_smuggling_before_refreshing_original_nodes() {
    let (base, calls) = workspace(None, None);
    let (handle, task) = session::spawn(
        base,
        RecordingMode::Ephemeral,
        no_files(),
        NonZeroUsize::new(1).unwrap(),
    )
    .unwrap();
    let source = "catalog echo value:unchanged";
    submit(&handle, "original", source).await;
    handle.wait_idle().await.unwrap();
    let before = calls.load(Ordering::SeqCst);
    let request = repeat("smuggle", source, true)
        .with_document(Some("types: {}".into()))
        .unwrap();
    assert!(matches!(
        handle.submit(request).await,
        Err(SessionError::RepeatRefused(_))
    ));
    assert_eq!(calls.load(Ordering::SeqCst), before);
    handle.shutdown().await.unwrap();
    task.join().await.unwrap();
}
