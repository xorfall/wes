#[tokio::test(flavor = "multi_thread")]
async fn render_reads_are_scoped_revalidated_and_never_mount_or_rerun_sources() {
    let root = tempfile::tempdir().unwrap();
    let home = root.path().join("home");
    let runtime = launch(RuntimeOptions::new(home.clone(), root.path().into()))
        .await
        .unwrap();
    let manager = Manager::default();
    let terminal = test_terminal(
        &manager,
        &runtime,
        &home,
        root.path(),
        &uuid::Uuid::new_v4().to_string(),
    )
    .await;
    let _stop = terminal.stopped.clone().drop_guard();
    let app = &runtime.handle;
    let context = invoke(&terminal, app, "workspace_context", json!({})).await;
    invoke(&terminal,app,"execute",json!({"source":":calc { return {view:\"choice\",title:\"Synthetic\",options:[]}; } > input\n:view create Choice input:$input > chart","request_id":"setup","context":context["context"],"sequential":true,"wait_ms":1000})).await;
    runtime
        .handle
        .current()
        .unwrap()
        .session
        .wait_idle()
        .await
        .unwrap();
    let before = invoke(&terminal, app, "workspace_snapshot", json!({})).await;
    // The real UI bridge request is answered by an isolated synthetic UI. No browser
    // input data or source code is included in this receipt, and the helper never mounts.
    let reader = terminal.clone();
    let app_clone = app.clone();
    let request = tokio::spawn(async move {
        invoke(
            &reader,
            &app_clone,
            "view_render_status",
            json!({"name":"chart"}),
        )
        .await
    });
    let pending = tokio::time::timeout(Duration::from_secs(3), async {
        loop {
            if let Some(pending) = terminal.ui.peek() {
                break pending;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    let crate::terminal::ui::Operation::ViewRenderStatus { scope } = pending.operation else {
        panic!("render operation");
    };
    let node = wes_engine::graph::NodeId::new(scope.node.clone()).unwrap();
    let frame = runtime
        .handle
        .current()
        .unwrap()
        .session
        .view_frame(node)
        .await
        .unwrap();
    let entry = &frame.instances[0];
    let receipt = json!({"host":uuid::Uuid::new_v4().to_string(),"digest":entry.definition.digest,"mode":"preview","status":"drawn","requestedInputRevision":entry.input_revision.to_string(),"drawnInputRevision":entry.input_revision.to_string(),"sentSequence":1,"ackSequence":1,"error":null});
    terminal.ui.finish(&pending.id,json!({"ok":true,"workspace":scope.workspace,"generation":scope.generation,"node":scope.node,"instance":scope.instance,"hosts":[receipt]}));
    let result = request.await.unwrap();
    assert_eq!(result["deliveryVerified"], true, "{result}");
    assert_eq!(result["visualVerified"], false);
    assert_eq!(
        invoke(&terminal, app, "workspace_snapshot", json!({})).await,
        before
    );
    assert!(result.get("input").is_none());
    for (kind, expected) in [
        ("unmounted", "not_mounted"),
        ("stale_sequence", "awaiting_draw"),
        ("stale_revision", "awaiting_draw"),
        ("failed", "failed"),
        ("foreign_scope", "invalid_ui_receipt"),
        ("private_error", "invalid_ui_receipt"),
    ] {
        let reader = terminal.clone();
        let app_clone = app.clone();
        let request = tokio::spawn(async move {
            invoke(
                &reader,
                &app_clone,
                "view_render_status",
                json!({"name":"chart"}),
            )
            .await
        });
        let pending = tokio::time::timeout(Duration::from_secs(3), async {
            loop {
                if let Some(pending) = terminal.ui.peek() {
                    break pending;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        let crate::terminal::ui::Operation::ViewRenderStatus { scope } = pending.operation else {
            panic!("render operation");
        };
        let mut packet = json!({"ok":true,"workspace":scope.workspace,"generation":scope.generation,"node":scope.node,"instance":scope.instance,
          "hosts":[{"host":uuid::Uuid::new_v4().to_string(),"digest":entry.definition.digest,"mode":"preview","status":"drawn","requestedInputRevision":entry.input_revision.to_string(),"drawnInputRevision":entry.input_revision.to_string(),"sentSequence":2,"ackSequence":2,"error":null}]});
        match kind {
            "unmounted" => packet["hosts"] = json!([]),
            "stale_sequence" => packet["hosts"][0]["ackSequence"] = json!(1),
            "stale_revision" => {
                packet["hosts"][0]["drawnInputRevision"] =
                    json!((entry.input_revision + 1).to_string())
            }
            "failed" => {
                packet["hosts"][0]["status"] = json!("failed");
                packet["hosts"][0]["error"] = json!("renderer_failed");
            }
            "foreign_scope" => packet["generation"] = json!("other-generation"),
            "private_error" => {
                packet["hosts"][0]["error"] = json!("synthetic private renderer text")
            }
            _ => unreachable!(),
        }
        terminal.ui.finish(&pending.id, packet);
        let result = request.await.unwrap();
        assert_eq!(result["status"], expected, "{kind}: {result}");
        assert_eq!(result["deliveryVerified"], false, "{result}");
        assert_eq!(result["visualVerified"], false, "{result}");
        assert!(
            !result
                .to_string()
                .contains("synthetic private renderer text")
        );
        assert_eq!(
            invoke(&terminal, app, "workspace_snapshot", json!({})).await,
            before
        );
    }
    // A valid earlier receipt cannot acknowledge a view whose binding changed during the UI read.
    let reader = terminal.clone();
    let app_clone = app.clone();
    let request = tokio::spawn(async move {
        invoke(
            &reader,
            &app_clone,
            "view_render_status",
            json!({"name":"chart"}),
        )
        .await
    });
    let pending = tokio::time::timeout(Duration::from_secs(3), async {
        loop {
            if let Some(pending) = terminal.ui.peek() {
                break pending;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    let crate::terminal::ui::Operation::ViewRenderStatus { scope } = pending.operation else {
        panic!("render operation");
    };
    let context = invoke(&terminal, app, "workspace_context", json!({})).await;
    let changed=invoke(&terminal,app,"execute",json!({"source":format!(":view bind $chart input:$input revision:{}",entry.revision),"request_id":"rebind","context":context["context"],"sequential":true,"wait_ms":1000})).await;
    runtime
        .handle
        .current()
        .unwrap()
        .session
        .wait_idle()
        .await
        .unwrap();
    assert!(
        changed["execution"]["steps"][0]["accepted"]
            .as_array()
            .is_some_and(|accepted| !accepted.is_empty()),
        "{changed}"
    );
    terminal.ui.finish(&pending.id,json!({"ok":true,"workspace":scope.workspace,"generation":scope.generation,"node":scope.node,"instance":scope.instance,
        "hosts":[{"host":uuid::Uuid::new_v4().to_string(),"digest":entry.definition.digest,"mode":"preview","status":"drawn","requestedInputRevision":entry.input_revision.to_string(),"drawnInputRevision":entry.input_revision.to_string(),"sentSequence":1,"ackSequence":1,"error":null}]}));
    let result = request.await.unwrap();
    assert_eq!(result["status"], "reference_changed", "{result}");
    assert_eq!(result["deliveryVerified"], false);

    terminal.stopped.cancel();
    drop(terminal);
    manager.shutdown().await;
    runtime.shutdown().await.unwrap();
}
