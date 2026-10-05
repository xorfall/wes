//! Acceptance for the shared runtime composition used by the CLI and the desktop shell.
use serde_json::json;
use std::time::Duration;
use wes::runtime::{RuntimeOptions, launch};

#[tokio::test]
async fn desktop_work_retirement_preserves_default_environment_and_surviving_execution() {
    use wes_engine::source::SourceInput;
    let root = tempfile::tempdir().unwrap();
    let runtime = launch(RuntimeOptions::new(
        root.path().join("home"),
        root.path().into(),
    ))
    .await
    .unwrap();
    let session = runtime.handle.current().unwrap().session;
    for (id, source) in [
        ("removed", ":calc { return [1, 2, 3, 4]; } > testData"),
        ("kept", ":calc { return 42; } > untouched"),
    ] {
        session
            .submit(SourceInput::new(id.into(), source.into()).unwrap())
            .await
            .unwrap();
    }
    let checkpoint = session.checkpoint().await.unwrap();
    let before = session.observe().await.unwrap();
    checkpoint.resume().await;
    assert!(before.environment_enabled["default"]);
    let preview = runtime
        .handle
        .preview_delete_work(
            runtime.handle.current().unwrap().generation,
            "fixture".into(),
            "removed".into(),
        )
        .await
        .unwrap();
    runtime
        .handle
        .delete_work(
            runtime.handle.current().unwrap().generation,
            "fixture".into(),
            preview.token,
            false,
            false,
        )
        .await
        .unwrap();
    let after = session.observe().await.unwrap();
    assert_eq!(before.environment_enabled, after.environment_enabled);
    assert_eq!(before.environment_clients, after.environment_clients);
    assert_eq!(before.environment_revisions, after.environment_revisions);
    assert_eq!(after.cells.len(), 1);
    assert_eq!(
        after.state.execution.values[&after.state.names["untouched"].node].data(),
        &wes_core::Data::Int(42)
    );
    runtime.shutdown().await.unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn desktop_scrollback_and_calc_payload_survive_two_restarts_and_new_appends() {
    use wes_engine::source::SourceInput;
    let root = tempfile::tempdir().unwrap();
    let home = root.path().join("desktop-history");
    let texts = [
        ":env use \"default\"",
        ":calc { return [1, 2, 3, 4] } > testData",
        ":inspect env:\"not-installed\"",
        ":calc { return 42; } > afterReopen",
    ];
    let mut originals = Vec::new();
    let mut held_ports = Vec::new();
    for cycle in 0..3 {
        let runtime = launch(RuntimeOptions::new(home.clone(), root.path().into()))
            .await
            .unwrap();
        let server = runtime.serve_desktop(None).await.unwrap();
        let address = server.address();
        let session = runtime.handle.current().unwrap().session;
        if cycle > 0 {
            let observed = session.observe().await.unwrap();
            let expected = if cycle == 1 { 3 } else { 4 };
            assert_eq!(observed.cells.len(), expected);
            for (index, cell) in observed.cells.iter().enumerate() {
                assert_eq!(cell.input.text(), texts[index]);
                let reply = cell.reply.as_ref().unwrap().as_ref().unwrap();
                assert!(reply.restored);
                assert_eq!(reply.diagnostics.diagnostics, originals[index]);
            }
            let calc = &observed.cells[1];
            let node = &calc.reply.as_ref().unwrap().as_ref().unwrap().nodes[0];
            assert_eq!(
                observed.state.execution.graph.node(node).unwrap().state(),
                wes_engine::graph::NodeState::Ready
            );
            let values = observed.values.as_ref().unwrap();
            let handle = values.outputs.get(node).unwrap().handle().unwrap();
            let loaded = runtime.worker.read(handle.clone()).await.unwrap().unwrap();
            let wes_core::Data::List(items) = loaded.value.data() else {
                panic!("expected list");
            };
            assert_eq!(
                items.clone(),
                vec![
                    wes_core::Data::Int(1),
                    wes_core::Data::Int(2),
                    wes_core::Data::Int(3),
                    wes_core::Data::Int(4)
                ]
            );
            assert!(observed.state.execution.idle);
            assert!(observed.state.execution.streaming.is_empty());
        }
        let range = match cycle {
            0 => 0..3,
            1 => 3..4,
            _ => 4..4,
        };
        for index in range {
            let reply = session
                .submit(SourceInput::new(format!("attempt-{index}"), texts[index].into()).unwrap())
                .await
                .unwrap();
            if index == 1 || index == 3 {
                assert_eq!(reply.nodes.len(), 1, "{:?}", reply.diagnostics);
                assert!(
                    !reply
                        .diagnostics
                        .diagnostics
                        .iter()
                        .any(|d| d.severity == wes_language::Severity::Error),
                    "{:?}",
                    reply.diagnostics
                );
            }
            originals.push(reply.diagnostics.diagnostics.clone());
        }
        let checkpoint = tokio::time::timeout(Duration::from_secs(10), session.checkpoint())
            .await
            .unwrap()
            .unwrap();
        checkpoint.resume().await;
        // Read the actual loopback projection too, not just the reconstructed engine graph.
        let client = reqwest::Client::builder()
            .no_proxy()
            .timeout(Duration::from_secs(8))
            .build()
            .unwrap();
        let mut response = client
            .get(format!("http://{address}/events"))
            .send()
            .await
            .unwrap();
        let mut frames = String::new();
        while !frames.contains("\"event\":\"reported\"") || !frames.contains("testData") {
            let bytes = response.chunk().await.unwrap().expect("live SSE greeting");
            frames.push_str(std::str::from_utf8(&bytes).unwrap());
        }
        assert!(!frames.contains("original reply is unavailable"));
        drop(response);
        server.shutdown().await.unwrap();
        runtime.shutdown().await.unwrap();
        held_ports.push(tokio::net::TcpListener::bind(address).await.unwrap());
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn desktop_preferences_survive_a_real_restart_with_a_different_origin() {
    let root = tempfile::tempdir().unwrap();
    let home = root.path().join("desktop");
    let settings = json!({"surfacePalette":"white","surfaceFace":"PT Mono","surfaceDensity":"dense","direction":"TB","stayAtNewest":false,"previewChars":8000,"requestTimeoutMs":30000,"aliases":{"echo":"sh run cmd:\"echo _\""},
        "dashboards": (0..32).map(|index| json!({"workspace":"layout-fixture", "board":{"version":1,"id":format!("board-{index}"),"name":format!("board{index}"),"title":"Synthetic layout", "revision":1, "members":(0..16).map(|member| json!({"id":format!("member-{member}"),"node":format!("node-{member}"),"generation":"layout-generation","label":"界".repeat(512)})).collect::<Vec<_>>(), "layout":{"kind":"column","id":format!("root-{index}"),"weight":1,"children":(0..16).map(|member| json!({"kind":"member","id":format!("leaf-{member}"),"member":format!("member-{member}"),"width":"auto","align":"start","weight":1})).collect::<Vec<_>>()}}})).collect::<Vec<_>>()});
    let client = reqwest::Client::builder()
        .no_proxy()
        .timeout(Duration::from_secs(8))
        .build()
        .unwrap();
    let runtime = launch(RuntimeOptions::new(home.clone(), root.path().into()))
        .await
        .unwrap();
    let server = runtime.serve_desktop(None).await.unwrap();
    let first = server.address();
    let response = client
        .put(format!("http://{first}/client-preferences"))
        .header("Content-Type", "application/json")
        .body(settings.to_string())
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 200);
    server.shutdown().await.unwrap();
    runtime.shutdown().await.unwrap();
    // Occupy the former port so the next desktop launch must use another origin.
    let _reserved = tokio::net::TcpListener::bind(first).await.unwrap();
    let runtime = launch(RuntimeOptions::new(home, root.path().into()))
        .await
        .unwrap();
    let server = runtime.serve_desktop(None).await.unwrap();
    assert_ne!(first, server.address());
    let response = client
        .get(format!("http://{}/client-preferences", server.address()))
        .send()
        .await
        .unwrap();
    let restored: serde_json::Value =
        serde_json::from_slice(&response.bytes().await.unwrap()).unwrap();
    assert_eq!(restored["settings"], settings);
    server.shutdown().await.unwrap();
    runtime.shutdown().await.unwrap();

    let runtime = launch(RuntimeOptions::new(
        root.path().join("other"),
        root.path().into(),
    ))
    .await
    .unwrap();
    let browser = runtime.serve(0, None).await.unwrap();
    assert_eq!(
        client
            .get(format!("http://{}/client-preferences", browser.address()))
            .send()
            .await
            .unwrap()
            .status(),
        404
    );
    browser.shutdown().await.unwrap();
    let desktop = runtime.serve_desktop(None).await.unwrap();
    let response = client
        .get(format!("http://{}/client-preferences", desktop.address()))
        .send()
        .await
        .unwrap();
    let empty: serde_json::Value =
        serde_json::from_slice(&response.bytes().await.unwrap()).unwrap();
    assert!(empty["settings"].is_null());
    desktop.shutdown().await.unwrap();
    runtime.shutdown().await.unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn shared_runtime_serves_client_on_ephemeral_port_and_shuts_down_cleanly() {
    // Arrange: a private home, a base directory and a one-file site stand in for gui/dist.
    let root = tempfile::tempdir().unwrap();
    let home = root.path().join("home");
    let base = root.path().join("base");
    let site = root.path().join("site");
    std::fs::create_dir_all(&home).unwrap();
    std::fs::create_dir_all(&base).unwrap();
    std::fs::create_dir(&site).unwrap();
    std::fs::write(site.join("index.html"), "<title>desktop fixture</title>").unwrap();

    // Act: launch the shared runtime and serve the browser client on an ephemeral port.
    let runtime = launch(RuntimeOptions::new(home, base)).await.unwrap();
    let server = runtime.serve(0, Some(site)).await.unwrap();
    let address = server.address();
    let client = reqwest::Client::builder()
        .no_proxy()
        .timeout(Duration::from_secs(8))
        .build()
        .unwrap();
    let index = client
        .get(format!("http://{address}/"))
        .send()
        .await
        .unwrap();
    let generation = runtime.handle.current().unwrap().generation;
    let submitted = client
        .post(format!("http://{address}/submit"))
        .header("Content-Type", "application/json")
        .header("X-Wes-Session", &generation)
        .body(
            json!({"request":"submit","client":"web-fixture","cell":"c1","text":":help"})
                .to_string(),
        )
        .send()
        .await
        .unwrap();

    // Assert: the ephemeral port is real, the site and protocol answer, shutdown joins cleanly.
    assert_ne!(address.port(), 0);
    assert_eq!(index.status().as_u16(), 200);
    assert!(index.text().await.unwrap().contains("desktop fixture"));
    assert_eq!(submitted.status().as_u16(), 202);
    tokio::time::timeout(Duration::from_secs(5), server.shutdown())
        .await
        .unwrap()
        .unwrap();
    tokio::time::timeout(Duration::from_secs(10), runtime.shutdown())
        .await
        .unwrap()
        .unwrap();
}

#[tokio::test]
async fn custom_stream_capacity_reaches_shared_application_owner() {
    let root = tempfile::tempdir().unwrap();
    let mut options = RuntimeOptions::new(root.path().join("home"), root.path().into());
    options.max_streams = std::num::NonZeroUsize::new(7).unwrap();
    let runtime = launch(options).await.unwrap();
    assert_eq!(
        runtime.handle.subscribe_capacity().borrow().streams.limit,
        7
    );
    runtime.shutdown().await.unwrap();
}
