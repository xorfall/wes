use super::*;
use crate::runtime::{RuntimeOptions, launch};
use wes_engine::source::SourceInput;

// A failed assertion must not leave blocking PTY workers alive during runtime teardown.
struct Cleanup(Manager);
impl Drop for Cleanup {
    fn drop(&mut self) {
        for terminal in self.0.sessions.lock().unwrap().values() {
            terminal.stopped.cancel();
        }
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn desktop_survives_suspended_reads_and_workspace_focus_but_close_and_shutdown_revoke() {
    let root = tempfile::tempdir().unwrap();
    let runtime = launch(RuntimeOptions::new(
        root.path().join("home"),
        root.path().into(),
    ))
    .await
    .unwrap();
    let manager = Manager::default();
    let _cleanup = Cleanup(manager.clone());
    let current = runtime.handle.current().unwrap();
    let client = "synthetic-resume-client";
    let browser_config =
        crate::runtime::browser_services(root.path().join("home"), root.path().into())
            .unwrap()
            .terminal
            .unwrap();
    assert_eq!(browser_config.idle_timeout, Some(Duration::from_secs(120)));
    let desktop_config = Config {
        idle_timeout: None,
        ..browser_config.clone()
    };
    let desktop = manager
        .start(
            desktop_config.clone(),
            current.clone(),
            client.into(),
            0,
            runtime.handle.clone(),
        )
        .await
        .unwrap();
    let browser = manager
        .start(
            browser_config,
            current.clone(),
            client.into(),
            0,
            runtime.handle.clone(),
        )
        .await
        .unwrap();
    let resumed = manager
        .owned(&desktop, &current.generation, client)
        .unwrap();
    let expired = manager
        .owned(&browser, &current.generation, client)
        .unwrap();
    // Work retirement is an in-place edit, not a terminal lifetime boundary.
    runtime
        .handle
        .submit(SourceInput::new("disposable".into(), ":calc { return 1; }".into()).unwrap())
        .await
        .unwrap();
    current.session.wait_idle().await.unwrap();
    let preview = runtime
        .handle
        .preview_delete_work(
            current.generation.clone(),
            client.into(),
            "disposable".into(),
        )
        .await
        .unwrap();
    runtime
        .handle
        .delete_work(
            current.generation.clone(),
            client.into(),
            preview.token,
            false,
            false,
        )
        .await
        .unwrap();
    assert_eq!(
        runtime.handle.current().unwrap().generation,
        current.generation
    );
    assert!(Arc::ptr_eq(
        &resumed,
        &manager
            .owned(&desktop, &current.generation, client)
            .unwrap()
    ));
    assert!(resumed.check(&runtime.handle).is_ok());
    assert!(!resumed.stopped.is_cancelled());
    // The input/output assertion below must use this same process after deletion.
    // Simulate WebView suspension without sleeping for two minutes or reading user data.
    let old = std::time::Instant::now() - Duration::from_secs(121);
    *resumed.heartbeat.lock().unwrap() = old;
    *expired.heartbeat.lock().unwrap() = old;
    tokio::time::timeout(Duration::from_secs(5), async {
        while manager.owned(&browser, &current.generation, client).is_ok() {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap();
    assert!(expired.stopped.is_cancelled());
    assert!(!resumed.stopped.is_cancelled());
    assert!(
        !manager
            .poll(&desktop, &current.generation, client, 0)
            .unwrap()
            .closed
    );
    manager
        .write(
            &desktop,
            &current.generation,
            client,
            "printf 'RESUMED_%s\\n' 'OK'\r".into(),
        )
        .unwrap();
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let bytes: Vec<u8> = resumed
                .output
                .lock()
                .unwrap()
                .bytes
                .iter()
                .copied()
                .collect();
            if bytes
                .windows(b"RESUMED_OK".len())
                .any(|part| part == b"RESUMED_OK")
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap();
    manager
        .close(&desktop, &current.generation, client)
        .unwrap();
    assert!(resumed.stopped.is_cancelled());
    assert!(
        manager
            .owned(&desktop, &current.generation, client)
            .is_err()
    );
    drop(resumed);
    drop(expired);

    let replaced = manager
        .start(
            desktop_config.clone(),
            current.clone(),
            client.into(),
            0,
            runtime.handle.clone(),
        )
        .await
        .unwrap();
    let old_workspace = manager
        .owned(&replaced, &current.generation, client)
        .unwrap();
    for (id, text) in [
        ("save", ":workspace save \"resume_copy\""),
        ("load", ":workspace load \"resume_copy\""),
    ] {
        runtime
            .handle
            .submit(SourceInput::new(id.into(), text.into()).unwrap())
            .await
            .unwrap();
    }
    let fresh = runtime.handle.current().unwrap();
    assert_ne!(fresh.generation, current.generation);
    assert!(old_workspace.check(&runtime.handle).is_ok());
    assert!(!old_workspace.stopped.is_cancelled());
    assert!(manager.owned(&replaced, &fresh.generation, client).is_err());
    let reply = manager
        .bridge(
            &old_workspace.token,
            BridgeRequest {
                tool: "wes-agent".into(),
                args: vec![
                    serde_json::json!({"name":"workspace_context","arguments":{}}).to_string(),
                ],
            },
            runtime.handle.clone(),
        )
        .await;
    assert_eq!(reply.code, 0, "{}", reply.stderr);
    let context: serde_json::Value = serde_json::from_str(&reply.stdout).unwrap();
    assert_eq!(context["workspace"], current.name.as_str());
    drop(old_workspace);

    let last = manager
        .start(
            desktop_config,
            fresh.clone(),
            client.into(),
            0,
            runtime.handle.clone(),
        )
        .await
        .unwrap();
    let stopped = manager.owned(&last, &fresh.generation, client).unwrap();
    manager.shutdown().await;
    assert!(stopped.stopped.is_cancelled());
    assert!(
        stopped.output.lock().unwrap().exit.is_some(),
        "shutdown joined the PTY worker"
    );
    assert!(manager.owned(&last, &fresh.generation, client).is_err());
    runtime.shutdown().await.unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn history_shutdown_serializes_start_and_forget_is_final() {
    let root = tempfile::tempdir().unwrap();
    let runtime = launch(RuntimeOptions::new(
        root.path().join("home"),
        root.path().into(),
    ))
    .await
    .unwrap();
    let manager = Manager::default();
    let _cleanup = Cleanup(manager.clone());
    let mut config = crate::runtime::browser_services(root.path().join("home"), root.path().into())
        .unwrap()
        .terminal
        .unwrap();
    let key = uuid::Uuid::new_v4().to_string();
    config.history = Some(key.clone());
    let current = runtime.handle.current().unwrap();
    let id = manager
        .start(
            config.clone(),
            current.clone(),
            "history-client".into(),
            0,
            runtime.handle.clone(),
        )
        .await
        .unwrap();
    assert!(
        manager
            .start(
                config.clone(),
                current.clone(),
                "other-client".into(),
                0,
                runtime.handle.clone()
            )
            .await
            .is_err()
    );
    assert!(
        manager
            .forget(&root.path().join("home"), &key, "other-client")
            .await
            .is_err()
    );
    let terminal = manager
        .owned(&id, &current.generation, "history-client")
        .unwrap();
    manager
        .forget(&root.path().join("home"), &key, "history-client")
        .await
        .unwrap();
    assert!(terminal.finished.is_cancelled());
    assert!(
        !root
            .path()
            .join("home/terminal-history")
            .join(&key)
            .exists()
    );
    assert!(
        manager
            .start(
                config.clone(),
                current.clone(),
                "history-client".into(),
                0,
                runtime.handle.clone()
            )
            .await
            .is_err()
    );
    manager
        .forget(&root.path().join("home"), &key, "history-client")
        .await
        .unwrap();

    // Hold the exact lifecycle gate to queue shutdown before another start. A start
    // cannot escape the shutdown scan even when it was queued before host teardown.
    let gate = manager.histories.lock().await;
    let stopping = manager.clone();
    let mut shutdown = Box::pin(stopping.shutdown());
    assert!(matches!(
        futures_util::poll!(shutdown.as_mut()),
        std::task::Poll::Pending
    ));
    let starting = manager.clone();
    let app = runtime.handle.clone();
    config.history = Some(uuid::Uuid::new_v4().to_string());
    let start = tokio::spawn(async move {
        starting
            .start(config, current, "history-client".into(), 0, app)
            .await
    });
    drop(gate);
    tokio::time::timeout(Duration::from_secs(5), shutdown)
        .await
        .unwrap();
    assert!(
        tokio::time::timeout(Duration::from_secs(5), start)
            .await
            .unwrap()
            .unwrap()
            .is_err()
    );
    runtime.shutdown().await.unwrap();
}

#[test]
fn generic_transport_loop_preserves_partial_input_resize_output_and_authority_stop() {
    use std::io::{Read, Write};
    use wes_engine::execution::TerminalExit;
    #[derive(Default)]
    struct Synthetic {
        received: Vec<u8>,
        sizes: Vec<(u16, u16)>,
        reads: usize,
        writes: usize,
    }
    impl Read for Synthetic {
        fn read(&mut self, bytes: &mut [u8]) -> io::Result<usize> {
            if self.reads == 0 {
                self.reads += 1;
                bytes[..3].copy_from_slice(b"out");
                Ok(3)
            } else {
                Err(io::ErrorKind::WouldBlock.into())
            }
        }
    }
    impl Write for Synthetic {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
            self.writes += 1;
            if self.writes == 1 {
                return Err(io::ErrorKind::WouldBlock.into());
            }
            let n = bytes.len().min(2);
            self.received.extend_from_slice(&bytes[..n]);
            Ok(n)
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }
    impl TerminalIo for Synthetic {
        fn resize(&mut self, size: TerminalSize) -> io::Result<()> {
            self.sizes.push((size.cols, size.rows));
            Ok(())
        }
        fn wait_ready(&mut self, _: bool, _: Duration) -> io::Result<()> {
            Ok(())
        }
        fn try_exit(&mut self) -> io::Result<Option<TerminalExit>> {
            Ok((self.received.len() == 5).then_some(TerminalExit {
                code: 0,
                uncertain: false,
            }))
        }
        fn shutdown(&mut self) -> io::Result<TerminalExit> {
            Ok(TerminalExit {
                code: 0,
                uncertain: false,
            })
        }
    }
    let (send, receive) = mpsc::sync_channel(4);
    send.send(Input::Resize(99, 33)).unwrap();
    send.send(Input::Write(b"hello".to_vec())).unwrap();
    let mut endpoint = Synthetic::default();
    let mut output = vec![];
    pump(
        &CancellationToken::new(),
        &mut endpoint,
        &receive,
        &CancellationToken::new(),
        |bytes| output.extend_from_slice(bytes),
    )
    .unwrap();
    assert_eq!(endpoint.received, b"hello");
    assert_eq!(endpoint.sizes, [(99, 33)]);
    assert_eq!(output, b"out");
    let cancelled = CancellationToken::new();
    cancelled.cancel();
    let mut endpoint = Synthetic::default();
    pump(
        &CancellationToken::new(),
        &mut endpoint,
        &receive,
        &cancelled,
        |_| panic!("revoked output"),
    )
    .unwrap();
    assert_eq!(endpoint.writes, 0);
    assert_eq!(endpoint.reads, 0);
}
