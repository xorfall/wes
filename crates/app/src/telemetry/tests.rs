use super::*;
use schema::{Operation, Outcome};
fn controller() -> (tempfile::TempDir, Arc<Controller>) {
    let root = tempfile::tempdir().unwrap();
    let c = Controller::open(root.path().join("diagnostics"), None).unwrap();
    (root, c)
}
fn wait_stopped(c: &Controller) {
    for _ in 0..200 {
        if !c.status()["writer_stopping"].as_bool().unwrap() {
            return;
        }
        thread::sleep(Duration::from_millis(5));
    }
    panic!("writer did not stop");
}
fn operation(c: &Arc<Controller>, outcome: &'static str) {
    tracing::dispatcher::with_default(&c.dispatch(), || {
        let op = wes_engine::diagnostics::Operation::start("execution");
        op.finish(outcome);
    });
}
#[test]
fn modes_persist_off_clear_memory_and_do_not_change_history_files() {
    let (root, c) = controller();
    operation(&c, "ok");
    assert_eq!(c.status()["metrics"]["operations"][1]["outcomes"][0][1], 1);
    std::fs::write(c.root.join("unrelated.txt"), "keep").unwrap();
    c.set_mode(Mode::Off).unwrap();
    wait_stopped(&c);
    operation(&c, "error");
    assert_eq!(c.status()["recent_count"], 0);
    assert_eq!(c.status()["metrics"]["operations"][1]["outcomes"][1][1], 0);
    c.clear().unwrap();
    assert!(c.root.join("unrelated.txt").exists());
    c.shutdown();
    drop(c);
    let c = Controller::open(root.path().join("diagnostics"), None).unwrap();
    assert_eq!(c.mode(), Mode::Off);
    assert!(c.state.lock().unwrap().worker.is_none());
    c.set_mode(Mode::Basic).unwrap();
    operation(&c, "ok");
    assert_eq!(c.status()["metrics"]["operations"][1]["outcomes"][0][1], 1);
    c.shutdown();
}
#[test]
fn expiry_and_manual_stop_restore_the_previous_mode_without_restart_persistence() {
    let (root, c) = controller();
    c.start_capture().unwrap();
    let deadline = c.until.load(Acquire);
    c.expire_at(deadline - 1);
    assert_eq!(c.mode(), Mode::Diagnostic);
    c.expire_at(deadline);
    assert_eq!(c.mode(), Mode::Basic);
    c.set_mode(Mode::Off).unwrap();
    wait_stopped(&c);
    c.start_capture().unwrap();
    c.expire_at(c.until.load(Acquire));
    assert_eq!(c.status()["mode"], "off");
    wait_stopped(&c);
    c.start_capture().unwrap();
    assert!(c.start_capture().is_err());
    c.stop_capture();
    wait_stopped(&c);
    c.start_capture().unwrap();
    c.shutdown();
    drop(c);
    let reopened = Controller::open(root.path().join("diagnostics"), None).unwrap();
    assert_eq!(reopened.mode(), Mode::Off);
    reopened.shutdown();
}
#[test]
fn arbitrary_fields_and_debug_values_never_reach_logs_or_export() {
    let (_root, c) = controller();
    c.start_capture().unwrap();
    struct Secret;
    impl std::fmt::Debug for Secret {
        fn fmt(&self, _: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            panic!("must never format a payload")
        }
    }
    tracing::dispatcher::with_default(&c.dispatch(), || {
        let span = tracing::info_span!(target:"wes.telemetry", "operation", operation="http", outcome="error", url="SYNTHETIC_SECRET", payload=?Secret);
        tracing::error!(target:"wes.telemetry", notice="ui_error", error="SYNTHETIC_SECRET");
        tracing::error!(target:"untrusted.library", message="SYNTHETIC_SECRET");
        drop(span);
        let unknown =
            tracing::info_span!(target:"wes.telemetry", "operation", operation="SYNTHETIC_SECRET");
        drop(unknown);
    });
    let export = c.export().unwrap().to_string();
    assert!(!export.contains("SYNTHETIC_SECRET"));
    assert!(export.contains("http"));
    assert!(export.contains("ui_error"));
    c.shutdown();
}
#[test]
fn disabled_interval_cannot_leak_old_span_completion_into_a_new_collection() {
    let (_root, c) = controller();
    let span = tracing::dispatcher::with_default(&c.dispatch(), || {
        wes_engine::diagnostics::Operation::start("http")
    });
    c.set_mode(Mode::Off).unwrap();
    wait_stopped(&c);
    c.set_mode(Mode::Basic).unwrap();
    span.finish("error");
    assert_eq!(c.status()["metrics"]["operations"][3]["outcomes"][1][1], 0);
    c.shutdown();
}
#[test]
fn slow_writer_keeps_metrics_and_memory_bounded_and_off_discards_waiting_writes() {
    let (_root, c) = controller();
    c.start_capture().unwrap();
    let file_guard = c.files.lock().unwrap();
    for i in 0..10_000 {
        c.record(
            c.epoch.load(Acquire),
            Record::Operation {
                at_ms: i,
                id: i,
                parent: None,
                kind: Operation::Execution,
                outcome: Outcome::Error,
                elapsed_us: 1000,
            },
        );
    }
    assert_eq!(c.status()["recent_count"], 512);
    assert_eq!(
        c.status()["metrics"]["operations"][1]["outcomes"][1][1],
        10_000
    );
    assert!(c.status()["suppressed"].as_u64().unwrap() > 0);
    c.set_mode(Mode::Off).unwrap();
    drop(file_guard);
    wait_stopped(&c);
    assert_eq!(
        std::fs::metadata(c.root.join("capture.jsonl"))
            .unwrap()
            .len(),
        0
    );
    c.shutdown();
}
#[test]
fn disk_errors_are_visible_and_capture_ceiling_returns_to_basic() {
    let (_root, c) = controller();
    // The writer may already have opened this synthetic file. Hold its file lock,
    // reset the cached handle, then install the failure before it can write again.
    {
        let mut files = c.files.lock().unwrap();
        match std::fs::remove_file(c.root.join("basic-0.jsonl")) {
            Ok(()) => (),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => (),
            Err(error) => panic!("synthetic log fixture: {error}"),
        }
        *files = files::Files::new(c.root.clone()).unwrap();
        std::fs::create_dir(c.root.join("basic-0.jsonl")).unwrap();
    }
    c.notice(Notice::UiError);
    for _ in 0..200 {
        if c.write_errors.load(Relaxed) > 0 {
            break;
        }
        thread::sleep(Duration::from_millis(5));
    }
    assert!(c.write_errors.load(Relaxed) > 0);
    c.start_capture().unwrap();
    {
        let mut f = c.files.lock().unwrap();
        f.test_fill_capture();
    }
    c.notice(Notice::UiError);
    for _ in 0..200 {
        if c.mode() == Mode::Basic {
            break;
        }
        thread::sleep(Duration::from_millis(5));
    }
    assert_eq!(c.mode(), Mode::Basic);
    c.shutdown();
}
#[test]
fn invalid_preferences_and_duplicate_owners_fail_closed() {
    let (root, c) = controller();
    assert!(Controller::open(c.root.clone(), None).is_err());
    c.shutdown();
    drop(c);
    std::fs::write(root.path().join("diagnostics/settings.json"), "{broken").unwrap();
    assert!(Controller::open(root.path().join("diagnostics"), None).is_err());
}
#[cfg(unix)]
#[test]
fn symlinks_and_edited_logs_are_not_exported_or_deleted() {
    use std::os::unix::fs::{PermissionsExt, symlink};
    let (root, c) = controller();
    c.set_mode(Mode::Off).unwrap();
    wait_stopped(&c);
    let secret = root.path().join("secret");
    std::fs::write(&secret, "SYNTHETIC_SECRET").unwrap();
    symlink(&secret, c.root.join("basic-4.jsonl")).unwrap();
    assert!(c.export().is_err());
    assert!(c.clear().is_err());
    assert_eq!(std::fs::read_to_string(secret).unwrap(), "SYNTHETIC_SECRET");
    assert_eq!(
        std::fs::metadata(&c.root).unwrap().permissions().mode() & 0o077,
        0
    );
    std::fs::write(
        c.root.join("capture.jsonl"),
        "{\"event\":\"SYNTHETIC_SECRET\"}\n",
    )
    .unwrap();
    assert!(c.export().is_err());
    c.shutdown();
}
#[test]
fn forced_mode_is_not_persisted_and_does_not_allow_capture() {
    let root = tempfile::tempdir().unwrap();
    let c = Controller::open(root.path().join("d"), Some(Mode::Off)).unwrap();
    assert!(c.set_mode(Mode::Basic).is_err());
    assert!(c.start_capture().is_err());
    assert!(!c.root.join("settings.json").exists());
    c.shutdown();
}

#[test]
fn queue_full_is_counted_without_losing_metrics() {
    let (_root, c) = controller();
    c.start_capture().unwrap();
    let files = c.files.lock().unwrap();
    let record = Record::Notice {
        at_ms: 0,
        kind: Notice::UiError,
    };
    {
        let state = c.state.lock().unwrap();
        let worker = state.worker.as_ref().unwrap();
        for _ in 0..QUEUE + 2 {
            let _ = worker.sender.try_send(Some(Message {
                epoch: c.epoch.load(Acquire),
                capture: c.capture.load(Acquire),
                record,
            }));
        }
    }
    let before = c.dropped.load(Relaxed);
    c.notice(Notice::UiError);
    assert!(c.dropped.load(Relaxed) > before);
    assert!(c.status()["metrics"]["notices"][3].as_u64().unwrap() > 0);
    c.set_mode(Mode::Off).unwrap();
    drop(files);
    wait_stopped(&c);
    c.shutdown();
}
#[test]
fn basic_rotation_has_five_segments_and_keeps_unrelated_files() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("basic-0.jsonl");
    std::fs::File::create(&path)
        .unwrap()
        .set_len(files::segment_bytes())
        .unwrap();
    std::fs::write(root.path().join("other.log"), "keep").unwrap();
    let mut files = files::Files::new(root.path().into()).unwrap();
    files
        .write(
            Record::Notice {
                at_ms: 1,
                kind: Notice::Started,
            },
            false,
        )
        .unwrap();
    assert_eq!(
        std::fs::metadata(path).unwrap().len(),
        files::segment_bytes()
    );
    assert!(
        std::fs::metadata(root.path().join("basic-1.jsonl"))
            .unwrap()
            .len()
            < 2048
    );
    files.clear().unwrap();
    assert_eq!(
        std::fs::read_to_string(root.path().join("other.log")).unwrap(),
        "keep"
    );
}

#[test]
fn live_spans_are_counted_and_parent_links_do_not_include_user_identities() {
    let (_root, c) = controller();
    tracing::dispatcher::with_default(&c.dispatch(), || {
        let parent = wes_engine::diagnostics::Operation::start("execution");
        let child = parent.child("queue");
        assert_eq!(c.status()["metrics"]["operations"][1]["in_flight"], 1);
        child.finish("ok");
        parent.finish("ok");
    });
    let status = c.status();
    assert_eq!(status["metrics"]["operations"][1]["in_flight"], 0);
    let records = c.export().unwrap()["recent"].as_array().unwrap().clone();
    let parent = records.iter().find(|r| r["kind"] == "execution").unwrap();
    let child = records.iter().find(|r| r["kind"] == "queue").unwrap();
    assert_eq!(child["parent"], parent["id"]);
    c.shutdown();
}

#[test]
fn finishing_an_old_capture_cannot_stop_a_new_capture() {
    let (_root, c) = controller();
    c.start_capture().unwrap();
    let old = c.capture.load(Acquire);
    c.stop_capture();
    c.start_capture().unwrap();
    c.stop_capture_id(Some(old));
    assert_eq!(c.mode(), Mode::Diagnostic);
    c.shutdown();
}

#[test]
fn refused_writer_restart_does_not_change_the_saved_off_preference() {
    let (_root, c) = controller();
    c.set_mode(Mode::Off).unwrap();
    wait_stopped(&c);
    let (release, blocked) = mpsc::channel();
    let (done_tx, done) = mpsc::channel();
    let worker = thread::spawn(move || {
        let _ = blocked.recv();
        let _ = done_tx.send(());
    });
    let (sender, _receiver) = mpsc::sync_channel(QUEUE);
    c.state.lock().unwrap().worker = Some(Worker {
        sender,
        stopped: Arc::new(AtomicBool::new(true)),
        done,
        thread: Some(worker),
    });
    assert!(c.set_mode(Mode::Basic).is_err());
    assert_eq!(c.mode(), Mode::Off);
    let preference: Preference = crate::data_home::metadata(&c.root.join("settings.json")).unwrap();
    assert_eq!(preference.mode, Mode::Off);
    assert_eq!(c.status()["saved_mode"], "off");
    release.send(()).unwrap();
    c.shutdown();
}

#[test]
fn failed_capture_creation_from_off_stops_its_new_writer() {
    let (_root, c) = controller();
    c.set_mode(Mode::Off).unwrap();
    wait_stopped(&c);
    std::fs::create_dir(c.root.join("capture.jsonl")).unwrap();
    assert!(c.start_capture().is_err());
    wait_stopped(&c);
    assert_eq!(c.mode(), Mode::Off);
    assert_eq!(c.status()["recent_count"], 0);
    c.shutdown();
}

#[test]
fn the_writer_expires_a_capture_without_status_reads() {
    let (_root, c) = controller();
    c.capture_for(Duration::from_millis(20)).unwrap();
    let deadline = Instant::now() + Duration::from_secs(2);
    while c.mode() == Mode::Diagnostic {
        assert!(Instant::now() < deadline, "capture writer did not expire");
        thread::sleep(Duration::from_millis(5));
    }
    assert_eq!(c.mode(), Mode::Basic);
    c.shutdown();
}
