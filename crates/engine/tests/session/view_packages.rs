use super::*;

#[tokio::test]
async fn mounted_current_views_wait_through_refresh_without_rebinding_or_replaying() {
    use wes_engine::views::MountAction;
    let compiled = artifact("CountBadge");
    let gate = Arc::new(Notify::new());
    let (entered, entered_rx) = oneshot::channel();
    let (base, calls) = workspace(Some(gate.clone()), Some(entered));
    let (handle, task) = session::spawn(
        base,
        RecordingMode::Ephemeral,
        reader(move |_, _| Ok(compiled.clone())),
        NonZeroUsize::new(2).unwrap(),
    )
    .unwrap();
    gate.notify_one();
    let reply = submit(
        &handle,
        "build",
        r#":package load path:badge.json
:def badgeData(input:Text) -> CountBadge as :calc { return {count:1}; }
catalog echo value:blocked > raw | badgeData > mapped | :view create CountBadge > first"#,
    )
    .await;
    assert_eq!(reply.accepted.len(), 3, "{:?}", reply.diagnostics);
    entered_rx.await.unwrap();
    handle.wait_idle().await.unwrap();
    submit(&handle, "siblings", ":view create CountBadge input:$mapped > second\n:view create CountBadge input:$mapped > third").await;
    handle.wait_idle().await.unwrap();
    let before = handle.snapshot().await.unwrap();
    let mapped = before.names["mapped"].node.clone();
    let old_run = before.execution.runs[&mapped].clone();
    let mut mounts = vec![];
    for name in ["first", "second", "third"] {
        let node = before.names[name].node.clone();
        let frame = handle
            .view_frame(node.clone())
            .await
            .unwrap_or_else(|e| panic!("{name}: {e}; {:?}", before.execution.errors));
        let identity = frame.instances[0].identity.to_string();
        let token = handle
            .view_mount(node.clone(), identity.clone(), MountAction::Open)
            .await
            .unwrap()
            .unwrap();
        handle
            .view_mount(node.clone(), identity.clone(), MountAction::Start)
            .await
            .unwrap();
        let initial = handle
            .observed_view_frame(node.clone(), identity.clone(), token.clone())
            .await
            .unwrap();
        assert!(initial.instances[0].observing);
        mounts.push((node, identity, token));
    }
    // A fresh attempt is blocked in the synthetic provider, leaving the adapter stale.
    submit(&handle, "refresh", ":refresh $raw scope:downstream").await;
    for (node, identity, token) in &mounts {
        let pending = handle
            .observed_view_frame(node.clone(), identity.clone(), token.clone())
            .await
            .unwrap();
        let view = &pending.instances[0];
        assert!(view.observing, "{:?}", view.input_problem);
        assert_eq!(
            view.input_problem.as_deref(),
            Some("Waiting for the current source result")
        );
        assert_eq!(
            view.input.as_ref().unwrap().source().unwrap().run(),
            Some(&old_run)
        );
    }
    assert!(
        handle
            .display_value(mapped.clone())
            .await
            .unwrap()
            .value
            .is_none()
    );
    gate.notify_one();
    handle.wait_idle().await.unwrap();
    let after = handle.snapshot().await.unwrap();
    let new_run = &after.execution.runs[&mapped];
    assert_ne!(new_run, &old_run);
    for (node, identity, token) in mounts {
        let current = handle
            .observed_view_frame(node.clone(), identity.clone(), token.clone())
            .await
            .unwrap();
        let view = &current.instances[0];
        assert!(view.observing);
        assert!(view.input_problem.is_none());
        assert_eq!(view.identity.as_ref(), identity);
        assert_eq!(
            view.input.as_ref().unwrap().source().unwrap().run(),
            Some(new_run)
        );
        assert_eq!(after.execution.runs[&node], before.execution.runs[&node]);
        handle
            .view_mount(node, identity, MountAction::Close(token))
            .await
            .unwrap();
    }
    assert_eq!(calls.load(Ordering::SeqCst), 2);
    stop(handle, task).await;
}

#[tokio::test]
async fn presentation_pipeline_observes_new_adapter_data_without_recreating_the_view() {
    use wes_engine::views::MountAction;
    let compiled = artifact("CountBadge");
    let (base, calls) = workspace(None, None);
    let (handle, task) = session::spawn(
        base,
        RecordingMode::Ephemeral,
        reader(move |_, _| Ok(compiled.clone())),
        NonZeroUsize::new(2).unwrap(),
    )
    .unwrap();
    let source = r#":package load path:badge.json
:def badgeData(input:Int) -> CountBadge as :calc { return {count:input}; }
:calc { return 7; } > raw | badgeData > mapped | :view create CountBadge > shown"#;
    let reply = submit(&handle, "build", source).await;
    assert_eq!(reply.accepted.len(), 3, "{:?}", reply.diagnostics);
    handle.wait_idle().await.unwrap();
    let before = handle.snapshot().await.unwrap();
    let shown = before.names["shown"].node.clone();
    let mapped = before.names["mapped"].node.clone();
    let creation_run = before.execution.runs[&shown].clone();
    let original_input_run = before.execution.runs[&mapped].clone();
    let frame = handle.view_frame(shown.clone()).await.unwrap();
    let identity = frame.instances[0].identity.to_string();
    let token = handle
        .view_mount(shown.clone(), identity.clone(), MountAction::Open)
        .await
        .unwrap()
        .unwrap();
    handle
        .view_mount(shown.clone(), identity.clone(), MountAction::Start)
        .await
        .unwrap();
    submit(&handle, "refresh", ":refresh $raw").await;
    handle.wait_idle().await.unwrap();
    let after = handle.snapshot().await.unwrap();
    assert_eq!(after.execution.runs[&shown], creation_run);
    assert_ne!(after.execution.runs[&mapped], original_input_run);
    assert!(after.execution.creation_inputs[&shown]);
    let sampled = handle
        .observed_view_frame(shown.clone(), identity.clone(), token.clone())
        .await
        .unwrap();
    assert_eq!(sampled.instances[0].identity.as_ref(), identity);
    let source = sampled.instances[0]
        .input
        .as_ref()
        .unwrap()
        .source()
        .unwrap();
    assert_eq!(source.run(), Some(&after.execution.runs[&mapped]));
    assert_eq!(calls.load(Ordering::SeqCst), 0);
    handle
        .view_mount(shown.clone(), identity.clone(), MountAction::Close(token))
        .await
        .unwrap();
    assert_eq!(
        handle.view_frame(shown).await.unwrap().instances[0]
            .identity
            .as_ref(),
        identity
    );
    assert_eq!(calls.load(Ordering::SeqCst), 0);
    stop(handle, task).await;
}

fn artifact(name: &str) -> String {
    let manifest = serde_json::json!({
        "name": name, "id": "count-badge", "summary": "Synthetic count badge",
        "renderer": "View.tsx", "input": "CountBadge", "outputs": {}, "interaction": null
    })
    .to_string();
    let types = "types: {CountBadge: {base: Record, fields: {count: Int}}}";
    let package = wes_views::Package::parse(&manifest, types).unwrap();
    serde_json::json!({
        "format": 1, "sdk": wes_views::sdk_version(), "manifest": manifest,
        "types": types, "definition": package.digest,
        "javascript": "throw new Error('renderer must not execute during installation');", "css": ""
    })
    .to_string()
}

#[tokio::test]
async fn package_success_notices_are_reserved_independently_of_input_path_length() {
    for name in ["CountBadge".to_owned(), format!("Badge{}", "a".repeat(123))] {
        for path in [
            "v",
            "badge.wes-view.json",
            "/synthetic/packages/badge.wes-view.json",
        ] {
            let compiled = artifact(&name);
            let reads = Arc::new(AtomicUsize::new(0));
            let observed = reads.clone();
            let expected_path = path.to_owned();
            let (base, calls) = workspace(None, None);
            let (handle, task) = session::spawn(
                base,
                RecordingMode::Ephemeral,
                reader(move |path, _| {
                    assert_eq!(path, expected_path);
                    observed.fetch_add(1, Ordering::SeqCst);
                    Ok(compiled.clone())
                }),
                NonZeroUsize::new(1).unwrap(),
            )
            .unwrap();
            let text = format!(":package load path:{path:?}");
            let reply = submit(&handle, "install", &text).await;
            assert_eq!(reply.accepted.len(), 1, "{:?}", reply.diagnostics);
            let notice = reply
                .diagnostics
                .diagnostics
                .iter()
                .find(|d| d.code == "VIE000")
                .unwrap();
            assert!(
                notice
                    .message
                    .starts_with(&format!("installed view {name} · "))
            );
            assert!(Arc::ptr_eq(
                &reply,
                &submit(&handle, "install", &text).await
            ));
            assert_eq!(reads.load(Ordering::SeqCst), 1);
            let create = submit(&handle, "create", &format!(":view create {name} > badge")).await;
            assert_eq!(create.accepted.len(), 1, "{:?}", create.diagnostics);
            assert_eq!(calls.load(Ordering::SeqCst), 0);
            stop(handle, task).await;
        }
    }
}

#[tokio::test]
async fn repeated_package_loads_in_a_batch_retain_every_notice_and_reject_malformed_input() {
    let compiled = artifact("CountBadge");
    let inline = format!(":package load source:{compiled:?}");
    let (base, calls) = workspace(None, None);
    let (handle, task) = session::spawn(
        base,
        RecordingMode::Ephemeral,
        reader(move |path, _| {
            Ok(if path == "bad" {
                "{}".into()
            } else {
                compiled.clone()
            })
        }),
        NonZeroUsize::new(1).unwrap(),
    )
    .unwrap();
    let reply = submit(
        &handle,
        "batch",
        ":package load path:\"v\"\n:package load path:\"v\"\n:package load path:\"bad\"",
    )
    .await;
    assert_eq!(reply.accepted.len(), 2, "{:?}", reply.diagnostics);
    assert_eq!(
        reply
            .diagnostics
            .diagnostics
            .iter()
            .filter(|d| d.code == "VIE000")
            .count(),
        2
    );
    assert!(
        reply
            .diagnostics
            .diagnostics
            .iter()
            .any(|d| d.severity == wes_language::Severity::Error)
    );
    let inline_reply = submit(&handle, "inline", &inline).await;
    assert_eq!(
        inline_reply.accepted.len(),
        1,
        "{:?}",
        inline_reply.diagnostics
    );
    assert!(
        inline_reply
            .diagnostics
            .diagnostics
            .iter()
            .any(|d| d.code == "VIE000")
    );
    assert_eq!(
        submit(&handle, "create", ":view create CountBadge > badge")
            .await
            .accepted
            .len(),
        1
    );
    assert_eq!(calls.load(Ordering::SeqCst), 0);
    stop(handle, task).await;
}
