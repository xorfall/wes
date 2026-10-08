#[path = "support/python.rs"]
mod python;
use std::process::Command;
#[path = "cli/files.rs"]
mod files;
#[cfg(unix)]
#[path = "cli/interactive.rs"]
mod interactive;
#[path = "cli/workflows.rs"]
mod workflows;

#[test]
fn cli_captures_user_operating_policy_before_opening_an_explicit_data_home() {
    let root = tempfile::tempdir().unwrap();
    let settings = wes::budgets::Store::for_user_home(root.path());
    settings
        .save(wes::budgets::Change {
            revision: 0,
            values: [("scan.work".into(), 1)].into(),
        })
        .unwrap();
    let source = ":package load source:\"types: {Step: {base: Record, fields: {state: Int, outputs: 'List<Int>'}}}\"\n:def fold(state:Int, context:Int, item:Int) -> Step as :calc pure { return {state:state+item,outputs:[item]}; }\n:calc pure { return [1,2]; } > raw\n:scan source:$raw transition:fold initial:0 context:0 profile:TypedRecords sink:memory > analysis";
    let run = |data: &str| {
        Command::new(env!("CARGO_BIN_EXE_wes"))
            .env("HOME", root.path())
            .env("USERPROFILE", root.path())
            .arg("--home")
            .arg(root.path().join(data))
            .args(["--sequential", "--command", source])
            .output()
            .unwrap()
    };
    for data in ["first", "second"] {
        let result = run(data);
        assert!(
            !result.status.success(),
            "{}",
            String::from_utf8_lossy(&result.stdout)
        );
        assert!(
            String::from_utf8_lossy(&result.stderr).contains("CAL006"),
            "{}",
            String::from_utf8_lossy(&result.stderr)
        );
    }
    settings
        .save(wes::budgets::Change {
            revision: 1,
            values: [("scan.work".into(), 64_000_000)].into(),
        })
        .unwrap();
    let result = run("third");
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
}

#[cfg(unix)]
#[test]
fn managed_batch_file_lock_and_explicit_restored_activation() {
    let root = tempfile::tempdir().unwrap();
    std::fs::write(root.path().join("env.yaml"), "version: 1\ntargets: {local: {kind: local}}\nenvironments: {dev: {imports: {echo: {source: {kind: process, bin: /bin/echo}, bind: {target: local}}}}}").unwrap();
    let run = |home: &str, flags: &[&str], source: &str| {
        Command::new(env!("CARGO_BIN_EXE_wes"))
            .current_dir(root.path())
            .arg("--home")
            .arg(root.path().join(home))
            .args(flags)
            .args(["--command", source])
            .output()
            .unwrap()
    };
    let first = run(
        "original",
        &["--env-file", "env.yaml", "--env", "dev"],
        "echo run args:qa > first",
    );
    assert!(
        first.status.success(),
        "{}",
        String::from_utf8_lossy(&first.stderr)
    );
    let denied = run(
        "original",
        &["--env-file", "env.yaml", "--env", "dev"],
        "echo run args:denied",
    );
    assert_eq!(denied.status.code(), Some(1));
    let restored = run(
        "original",
        &["--env-file", "env.yaml", "--env", "dev", "--activate-env"],
        "echo run args:restored",
    );
    assert!(
        restored.status.success(),
        "{}",
        String::from_utf8_lossy(&restored.stderr)
    );
    let export = run("original", &[], ":env export file:captured.lock.json");
    assert!(
        export.status.success(),
        "{}",
        String::from_utf8_lossy(&export.stderr)
    );
    let locked = run(
        "fresh",
        &["--env-lock", "captured.lock.json", "--env", "dev"],
        "echo run args:locked",
    );
    assert!(
        locked.status.success(),
        "{}",
        String::from_utf8_lossy(&locked.stderr)
    );
    let collision = run(
        "fresh",
        &["--env-lock", "captured.lock.json", "--env", "dev"],
        ":help",
    );
    assert!(!collision.status.success());
}

#[cfg(unix)]
#[test]
fn managed_batch_credentials_stdin_never_becomes_source_or_stdout() {
    use std::{io::Write, process::Stdio};
    let root = tempfile::tempdir().unwrap();
    std::fs::write(root.path().join("env.yaml"), "version: 1\ntargets: {local: {kind: local}}\nenvironments: {dev: {secretSlots: {key: {required: true}}, secretRefs: {key: qa/batch}, imports: {vars: {source: {kind: process, bin: /usr/bin/printenv}, bind: {target: local, output: private, credentials: {QA_SECRET: {secret: key}}}}}}}").unwrap();
    let mut child = Command::new(env!("CARGO_BIN_EXE_wes"))
        .current_dir(root.path())
        .arg("--home")
        .arg(root.path().join("data"))
        .args([
            "--env-file",
            "env.yaml",
            "--env",
            "dev",
            "--credentials-stdin",
            "--grant-provider",
            "vars",
            "--command",
            "vars run args:QA_SECRET",
        ])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(br#"{"qa/batch":"qa-batch-private-sentinel"}"#)
        .unwrap();
    let output = child.wait_with_output().unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(!String::from_utf8_lossy(&output.stdout).contains("qa-batch-private-sentinel"));
    assert!(!String::from_utf8_lossy(&output.stderr).contains("qa-batch-private-sentinel"));
    assert!(
        String::from_utf8_lossy(&output.stderr)
            .to_lowercase()
            .contains("private")
    );
}
#[test]
fn native_command_runner_saves_reopens_and_reports_errors_without_live_providers() {
    let root = tempfile::tempdir().unwrap();
    let run = |source: &str| {
        Command::new(env!("CARGO_BIN_EXE_wes"))
            .arg("--home")
            .arg(root.path())
            .arg("--command")
            .arg(source)
            .output()
            .unwrap()
    };
    let help = run(":help > guide");
    assert!(
        help.status.success(),
        "{}",
        String::from_utf8_lossy(&help.stderr)
    );
    assert!(String::from_utf8_lossy(&help.stdout).contains("workspace"));
    let refreshed = run(":refresh $guide");
    assert!(
        refreshed.status.success(),
        "{}",
        String::from_utf8_lossy(&refreshed.stderr)
    );
    assert!(String::from_utf8_lossy(&refreshed.stdout).contains("workspace"));
    let saved = run(":workspace save \"kept\"");
    assert!(
        saved.status.success(),
        "{}",
        String::from_utf8_lossy(&saved.stderr)
    );
    assert!(String::from_utf8_lossy(&saved.stdout).contains("saved workspace 'kept'"));
    assert!(!String::from_utf8_lossy(&saved.stderr).contains("MET010"));
    let loaded = run(":workspace load \"kept\"");
    assert!(
        loaded.status.success(),
        "{}",
        String::from_utf8_lossy(&loaded.stderr)
    );
    let invalid = run(":workspace load \"missing\"");
    assert!(!invalid.status.success());
    assert!(String::from_utf8_lossy(&invalid.stderr).contains("MET011"));
    let mixed = run(":workspace save \"never\"\n:help");
    assert!(!mixed.status.success());
    assert!(String::from_utf8_lossy(&mixed.stderr).contains("ENG005"));
}
#[test]
fn usage_errors_do_not_create_a_workspace() {
    let help = Command::new(env!("CARGO_BIN_EXE_wes"))
        .arg("--help")
        .output()
        .unwrap();
    assert!(help.status.success());
    let isolated = tempfile::tempdir().unwrap();
    assert!(String::from_utf8_lossy(&help.stdout).contains("Usage: wes "));
    let missing = Command::new(env!("CARGO_BIN_EXE_wes"))
        .env("HOME", isolated.path())
        .args(["--command"])
        .output()
        .unwrap();
    assert_eq!(missing.status.code(), Some(2));
    assert!(!isolated.path().join(".wes").exists());
    let root = tempfile::tempdir().unwrap();
    let unopened = root.path().join("unopened");
    let invalid = Command::new(env!("CARGO_BIN_EXE_wes"))
        .arg("--home")
        .arg(&unopened)
        .args(["--command", ":help", "--node-timeout", "0"])
        .output()
        .unwrap();
    assert_eq!(invalid.status.code(), Some(2));
    assert!(!unopened.exists());
}

#[test]
fn startup_retention_choices_control_actual_archiving() {
    for (options, expected) in [
        (vec![], 1),
        (vec!["--no-auto-keep"], 0),
        (vec!["--keep-under", "0"], 0),
        (vec!["--keep-under", "1048576"], 1),
    ] {
        let root = tempfile::tempdir().unwrap();
        let result = Command::new(env!("CARGO_BIN_EXE_wes"))
            .arg("--home")
            .arg(root.path())
            .args([
                "--command",
                ":help",
                "--concurrency",
                "1",
                "--live-budget",
                "1048576",
            ])
            .args(options)
            .output()
            .unwrap();
        assert!(
            result.status.success(),
            "{}",
            String::from_utf8_lossy(&result.stderr)
        );
        let count = std::fs::read_dir(root.path().join("values/archive"))
            .unwrap()
            .filter(|entry| {
                entry
                    .as_ref()
                    .unwrap()
                    .path()
                    .extension()
                    .is_some_and(|extension| extension == "json")
            })
            .count();
        assert_eq!(count, expected);
    }
}

#[test]
fn configured_soft_live_budget_evicts_older_results_and_unlimited_preserves_them() {
    for (budget, expected) in [("1", 1), ("unlimited", 2)] {
        let root = tempfile::tempdir().unwrap();
        let result = Command::new(env!("CARGO_BIN_EXE_wes"))
            .arg("--home")
            .arg(root.path())
            .args([
                "--command",
                ":help > first\n:help > second",
                "--concurrency",
                "1",
                "--live-budget",
                budget,
                "--no-auto-keep",
            ])
            .output()
            .unwrap();
        assert!(
            result.status.success(),
            "{}",
            String::from_utf8_lossy(&result.stderr)
        );
        let count = std::fs::read_dir(root.path().join("values/live"))
            .unwrap()
            .filter(|entry| {
                entry
                    .as_ref()
                    .unwrap()
                    .path()
                    .extension()
                    .is_some_and(|extension| extension == "json")
            })
            .count();
        assert_eq!(count, expected);
    }
}

#[cfg(unix)]
#[test]
fn startup_concurrency_serializes_processes_and_default_timeout_cancels_joined_work() {
    let root = tempfile::tempdir().unwrap();
    let source = "sh run cmd:\"mkdir \\\"$WES_TEST_LOCK\\\" || exit 23; sleep 0.1; rmdir \\\"$WES_TEST_LOCK\\\"\" > first\nsh run cmd:\"mkdir \\\"$WES_TEST_LOCK\\\" || exit 23; sleep 0.1; rmdir \\\"$WES_TEST_LOCK\\\"\" > second";
    let run = |source: &str, timeout: &str| {
        Command::new(env!("CARGO_BIN_EXE_wes"))
            .arg("--home")
            .arg(root.path().join("home"))
            .args([
                "--command",
                source,
                "--concurrency",
                "1",
                "--node-timeout",
                timeout,
            ])
            .env("WES_TEST_LOCK", root.path().join("child-lock"))
            .output()
            .unwrap()
    };
    let serial = run(source, "10");
    assert!(
        serial.status.success(),
        "{}",
        String::from_utf8_lossy(&serial.stderr)
    );
    let text = String::from_utf8(serial.stdout).unwrap();
    assert_eq!(text.matches("\"exitCode\":0").count(), 2, "{text}");
    assert!(!root.path().join("child-lock").exists());
    let timed_out = run("sh run cmd:\"exec sleep 10\" > slow", "1");
    assert!(!timed_out.status.success());
    assert!(String::from_utf8_lossy(&timed_out.stderr).contains("RUN002"));
    // Reopening the same home succeeds only after the cancelled child's and history owners' joins.
    assert!(run(":list workspaces", "10").status.success());
}

#[test]
fn native_reopen_reports_missing_retained_value_without_recreating_it() {
    use wes_adapters::{codec::Limits, journal::Durability, storage::TieredValues};
    use wes_engine::storage::{ValueHandle, ValueStore};
    let root = tempfile::tempdir().unwrap();
    let run = |source: &str| {
        Command::new(env!("CARGO_BIN_EXE_wes"))
            .arg("--home")
            .arg(root.path())
            .args(["--command", source])
            .output()
            .unwrap()
    };
    assert!(run(":help > guide").status.success());
    assert!(run(":workspace save \"copy\"").status.success());
    let archive = root.path().join("values/archive");
    let handles: Vec<_> = std::fs::read_dir(&archive)
        .unwrap()
        .map(|entry| entry.unwrap().file_name())
        .filter_map(|name| {
            name.to_str()?
                .strip_suffix(".json")
                .and_then(|name| ValueHandle::new(name).ok())
        })
        .collect();
    assert_eq!(handles.len(), 1);
    {
        let mut store = TieredValues::open(
            &root.path().join("values/live"),
            &archive,
            Limits::default(),
            Durability::File,
            None,
        )
        .unwrap();
        assert!(store.release(&handles[0]).unwrap());
    }
    let reopened = run(":list workspaces");
    assert!(
        reopened.status.success(),
        "{}",
        String::from_utf8_lossy(&reopened.stderr)
    );
    assert!(
        String::from_utf8_lossy(&reopened.stderr)
            .contains("[startup] id1000: The retained value was unavailable")
    );
    let loaded = run(":workspace load \"copy\"");
    assert!(loaded.status.success());
    assert!(String::from_utf8_lossy(&loaded.stderr).contains("The command was not repeated"));
    assert!(!archive.join(format!("{}.json", handles[0])).exists());
}

#[cfg(unix)]
#[test]
fn interrupt_joins_a_finite_child_and_releases_persistent_owners() {
    use std::{
        fs,
        os::unix::fs::PermissionsExt,
        process::Stdio,
        thread,
        time::{Duration, Instant},
    };
    let root = tempfile::tempdir().unwrap();
    let script = root.path().join("blocked.sh");
    let marker = root.path().join("entered");
    fs::write(
        &script,
        b"#!/bin/sh\nprintf started > \"$WES_TEST_MARKER\"\nexec /bin/sleep 30\n",
    )
    .unwrap();
    fs::set_permissions(&script, fs::Permissions::from_mode(0o700)).unwrap();
    let source = format!(
        ":import process bin:\"{}\" as:blocked\nblocked run",
        script.display()
    );
    let mut child = Command::new(env!("CARGO_BIN_EXE_wes"))
        .arg("--home")
        .arg(root.path().join("home"))
        .arg("--command")
        .arg(source)
        .env("WES_TEST_MARKER", &marker)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let start = Instant::now();
    while !marker.exists() && start.elapsed() < Duration::from_secs(5) {
        if let Some(status) = child.try_wait().unwrap() {
            panic!("command exited before entering fixture: {status}");
        }
        thread::sleep(Duration::from_millis(10));
    }
    if !marker.exists() {
        let _ = child.kill();
        let _ = child.wait();
        panic!("fixture was not entered");
    }
    assert!(
        Command::new("/bin/kill")
            .args(["-INT", &child.id().to_string()])
            .status()
            .unwrap()
            .success()
    );
    let start = Instant::now();
    let status = loop {
        if let Some(status) = child.try_wait().unwrap() {
            break status;
        }
        if start.elapsed() > Duration::from_secs(5) {
            let _ = child.kill();
            let _ = child.wait();
            panic!("interrupt did not join promptly");
        }
        thread::sleep(Duration::from_millis(10));
    };
    assert!(!status.success());
    let reopened = Command::new(env!("CARGO_BIN_EXE_wes"))
        .arg("--home")
        .arg(root.path().join("home"))
        .args(["--command", ":help"])
        .output()
        .unwrap();
    assert!(
        reopened.status.success(),
        "{}",
        String::from_utf8_lossy(&reopened.stderr)
    );
}

#[cfg(unix)]
#[test]
fn native_shell_is_registered_and_its_retained_result_reopens_without_execution() {
    let root = tempfile::tempdir().unwrap();
    let marker = root.path().join("marker");
    let run = |source: &str| {
        Command::new(env!("CARGO_BIN_EXE_wes"))
            .arg("--home")
            .arg(root.path().join("home"))
            .arg("--command")
            .arg(source)
            .env("WES_SHELL_MARKER", &marker)
            .output()
            .unwrap()
    };
    let first =
        run(r#"sh run cmd:"printf x >> \"$WES_SHELL_MARKER\"; printf native-shell" > result"#);
    assert!(
        first.status.success(),
        "{}",
        String::from_utf8_lossy(&first.stderr)
    );
    // ProcessOutput.stdout is Bytes; the public JSON codec renders it as base64.
    assert!(String::from_utf8_lossy(&first.stdout).contains("bmF0aXZlLXNoZWxs"));
    assert_eq!(std::fs::read(&marker).unwrap(), b"x");
    let next = run(":list nodes");
    assert!(
        next.status.success(),
        "{}",
        String::from_utf8_lossy(&next.stderr)
    );
    assert_eq!(std::fs::read(&marker).unwrap(), b"x");
}

#[tokio::test]
async fn native_descriptor_stream_opens_and_its_saved_window_reopens_without_live_input() {
    use std::time::Duration;
    use tokio::{
        io::{AsyncReadExt, AsyncWriteExt},
        net::TcpListener,
    };
    let root = tempfile::tempdir().unwrap();
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let descriptor = root.path().join("events.json");
    std::fs::write(
        &descriptor,
        serde_json::to_vec(&serde_json::json!({
            "version":1, "provider":"events", "types":{}, "operations":[{
                "path":["watch"], "stream":true, "method":"GET", "route":"/events",
                "auth":[], "parameters":[], "responses":{"200":"Int"}
            }]
        }))
        .unwrap(),
    )
    .unwrap();
    let server = tokio::spawn(async move {
        tokio::time::timeout(Duration::from_secs(5), async {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut request = vec![];
            while !request.windows(4).any(|part| part == b"\r\n\r\n") {
                let mut bytes = [0; 1024]; let n = socket.read(&mut bytes).await.unwrap();
                assert_ne!(n, 0); request.extend_from_slice(&bytes[..n]); assert!(request.len() < 8192);
            }
            socket.write_all(b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nConnection: close\r\n\r\ndata: 42\n\n").await.unwrap();
            request
        }).await.unwrap()
    });
    let source = format!(
        ":import spec file:{} endpoint:{base:?}\nevents watch > live",
        serde_json::to_string(descriptor.to_str().unwrap()).unwrap()
    );
    let mut command = tokio::process::Command::new(env!("CARGO_BIN_EXE_wes"));
    command
        .kill_on_drop(true)
        .arg("--home")
        .arg(root.path().join("home"))
        .arg("--command")
        .arg(source);
    let first = tokio::time::timeout(Duration::from_secs(8), command.output())
        .await
        .unwrap()
        .unwrap();
    assert!(
        first.status.success(),
        "{}",
        String::from_utf8_lossy(&first.stderr)
    );
    assert!(String::from_utf8_lossy(&first.stdout).contains(": ["));
    let request = String::from_utf8(server.await.unwrap()).unwrap();
    assert!(request.starts_with("GET /events HTTP/1.1"));
    assert!(request.contains("accept: text/event-stream\r\n"));
    // Neither the descriptor nor service remains available. Reopen must use the captured import
    // and retained window, with historical nodes held instead of silently subscribing again.
    std::fs::remove_file(descriptor).unwrap();
    let mut command = tokio::process::Command::new(env!("CARGO_BIN_EXE_wes"));
    command
        .kill_on_drop(true)
        .arg("--home")
        .arg(root.path().join("home"))
        .args(["--command", ":workspace save \"stream-copy\""]);
    let reopened = tokio::time::timeout(Duration::from_secs(8), command.output())
        .await
        .unwrap()
        .unwrap();
    assert!(
        reopened.status.success(),
        "{}",
        String::from_utf8_lossy(&reopened.stderr)
    );
}

#[test]
fn native_calculation_multiline_json_option_and_error_output() {
    let root = tempfile::tempdir().unwrap();
    let run = |source: &str| {
        Command::new(env!("CARGO_BIN_EXE_wes"))
            .arg("--home")
            .arg(root.path())
            .args(["--command", source])
            .output()
            .unwrap()
    };
    let result = run(
        ":calc {\n const rows=parseJson('[1,2,3]');\n return rows.map(x=>x*2).reduce((a,x)=>a+x,0);\n} > sum",
    );
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    assert!(String::from_utf8_lossy(&result.stdout).contains("12"));
    assert!(run(":workspace save \"calc-copy\"").status.success());
    assert!(run(":workspace load \"calc-copy\"").status.success());
    let restored = run(":calc { return $sum; }");
    assert!(
        restored.status.success(),
        "{}",
        String::from_utf8_lossy(&restored.stderr)
    );
    assert!(String::from_utf8_lossy(&restored.stdout).contains("12"));
    let option = run(":calc { return some(none); } > nested");
    assert!(
        option.status.success(),
        "{}",
        String::from_utf8_lossy(&option.stderr)
    );
    let error = run(":calc { return 1/3; } *> problem");
    assert!(!error.status.success());
    assert!(String::from_utf8_lossy(&error.stderr).contains("CAL005"));
    let help = run(":help calc");
    assert!(help.status.success());
    assert!(String::from_utf8_lossy(&help.stdout).contains("parseJson"));
}

#[test]
fn temporal_values_restore_with_exact_types_and_field_references() {
    let root = tempfile::tempdir().unwrap();
    let run = |source: &str| {
        Command::new(env!("CARGO_BIN_EXE_wes"))
            .arg("--home")
            .arg(root.path())
            .args(["--command", source])
            .output()
            .unwrap()
    };
    let first = run(
        ":calc { return interval(instant('2025-01-01T00:00:00.000000001Z'),instant('2025-01-01T00:00:00.000000002Z')); } > window",
    );
    assert!(
        first.status.success(),
        "{}",
        String::from_utf8_lossy(&first.stderr)
    );
    let second = run(":calc { return $window.end - $window.start; }");
    assert!(
        second.status.success(),
        "{}",
        String::from_utf8_lossy(&second.stderr)
    );
    assert!(String::from_utf8_lossy(&second.stdout).contains("PT0.000000001S"));
    let checked = run(":calc { return check('Interval',$window); }");
    assert!(
        checked.status.success(),
        "{}",
        String::from_utf8_lossy(&checked.stderr)
    );
    let invalid = run(":calc { return check('Interval',{start:$window.start,end:$window.end}); }");
    assert!(!invalid.status.success());
}

#[test]
fn calculation_operation_help_is_readable_without_raw_json() {
    let home = tempfile::tempdir().unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_wes"))
        .arg("--home")
        .arg(home.path())
        .args(["--command", ":help calc iter.matches"])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let text = String::from_utf8_lossy(&output.stdout);
    for part in [
        "iter.matches(text: Text, pattern: Text) -> Iter<Text>",
        "Returns",
        "not capture groups",
        "collect(iter.matches",
    ] {
        assert!(text.contains(part), "{text}");
    }
    assert!(!text.contains("\"invocation\""), "{text}");
}

#[test]
fn lexical_shadowing_is_shared_by_cli_execution_and_builtin_help() {
    let home = tempfile::tempdir().unwrap();
    let run = |source: &str| {
        Command::new(env!("CARGO_BIN_EXE_wes"))
            .arg("--home")
            .arg(home.path())
            .args(["--command", source])
            .output()
            .unwrap()
    };
    let result = run(" :calc { const count=8;const call=x=>x+1;return call(count); }");
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    assert!(String::from_utf8_lossy(&result.stdout).contains('9'));
    let help = run(":help calc");
    assert!(help.status.success());
    let text = String::from_utf8_lossy(&help.stdout);
    assert!(text.contains("nearest lexical binding wins"));
    assert!(text.contains("iter namespace remain reserved"));
    let failed = run(":calc { const count=8;return count(1); }");
    assert!(!failed.status.success());
    assert!(String::from_utf8_lossy(&failed.stderr).contains("not callable"));
}

#[test]
fn retrospective_help_and_jsonl_diagnostics_are_actionable() {
    let root = tempfile::tempdir().unwrap();
    let run = |source: &str| {
        Command::new(env!("CARGO_BIN_EXE_wes"))
            .arg("--home")
            .arg(root.path())
            .args(["--command", source])
            .output()
            .unwrap()
    };
    let help = run(":help errors");
    assert!(help.status.success());
    let stdout = String::from_utf8_lossy(&help.stdout);
    assert!(stdout.contains("CMD · Command arguments"));
    assert!(stdout.contains("CAL003 · Internal calculation state invariant"));
    assert!(stdout.lines().count() < 40, "{stdout}");
    let remove = run(":help node remove");
    assert!(remove.status.success());
    assert!(
        String::from_utf8_lossy(&remove.stdout)
            .contains("dependents and their names are removed too")
    );
    let bad = run(":calc { return iter.jsonLines('1\\n \\n2').collect(); }");
    assert!(!bad.status.success());
    let stderr = String::from_utf8_lossy(&bad.stderr);
    assert!(
        stderr.contains("CAL016") && stderr.contains("JSONL line 2 is blank"),
        "{stderr}"
    );
    assert!(stderr.contains("iter.lines(source).filter"));
    assert!(!stderr.contains("byte 2") && !stderr.contains("EOF while parsing"));
    let filtered = run(
        ":calc { const source='1\\n \\n2'; return iter.lines(source).filter(line => iter.words(line).count() > 0).map(line => parseJson(line)).collect(); }",
    );
    assert!(
        filtered.status.success(),
        "{}",
        String::from_utf8_lossy(&filtered.stderr)
    );
    assert!(String::from_utf8_lossy(&filtered.stdout).contains("[1,2]"));
}

#[cfg(unix)]
#[test]
fn bytes_cli_hint_does_not_change_json_or_appear_in_json_mode() {
    let root = tempfile::tempdir().unwrap();
    std::fs::write(root.path().join("env.yaml"),"version: 1\ntargets: {local: {kind: local}}\nenvironments: {dev: {imports: {printer: {source: {kind: process, bin: /usr/bin/printf}, bind: {target: local}}}}}\n").unwrap();
    for json in [false, true] {
        let mut command = Command::new(env!("CARGO_BIN_EXE_wes"));
        command
            .current_dir(root.path())
            .arg("--home")
            .arg(root.path().join(if json { "json" } else { "text" }))
            .args([
                "--env-file",
                "env.yaml",
                "--env",
                "dev",
                "--command",
                "printer run args:3",
            ]);
        if json {
            command.arg("--json");
        }
        let output = command.output().unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(String::from_utf8_lossy(&output.stdout).contains("\"stdout\":\"Mw==\""));
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert_eq!(
            stderr.contains("Bytes are base64-encoded"),
            !json,
            "{stderr}"
        );
        assert!(!String::from_utf8_lossy(&output.stdout).contains("[value]"));
    }
}

#[test]
fn inspect_unknown_collection_elements_explains_the_limit_without_rejecting_values() {
    let root = tempfile::tempdir().unwrap();
    for (body, advisory) in [
        ("[none,some(30),some(1.5)]", true),
        ("[some(1),none,some('x')]", true),
        ("[none,some(30)]", false),
    ] {
        let output = Command::new(env!("CARGO_BIN_EXE_wes"))
            .arg("--home")
            .arg(root.path())
            .args([
                "--sequential",
                "--command",
                &format!(":calc {{ return {body}; }} > items\n:inspect $items"),
            ])
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let text = String::from_utf8_lossy(&output.stdout);
        assert_eq!(
            text.contains("No common element type is known"),
            advisory,
            "{text}"
        );
        if advisory {
            assert!(text.contains("Values are not coerced"));
        }
        let remove = Command::new(env!("CARGO_BIN_EXE_wes"))
            .arg("--home")
            .arg(root.path())
            .args(["--command", ":remove $items scope:downstream"])
            .output()
            .unwrap();
        assert!(
            remove.status.success(),
            "{}",
            String::from_utf8_lossy(&remove.stderr)
        );
    }
}

#[test]
fn imported_contract_help_and_nested_call_errors_show_constraints() {
    let root = tempfile::tempdir().unwrap();
    let spec = serde_json::json!({"version":1,"provider":"inventory","types":{"ApiType3":{"base":"Int","min":1,"max":50},"ApiType4":{"base":"Text","enum":["queued","done"]}},"operations":[{"path":["listItems"],"method":"GET","route":"/items","auth":[],"parameters":[{"name":"limit","wire":"limit","location":"query","type":"ApiType3","required":false,"encoding":"scalar"},{"name":"status","wire":"status","location":"query","type":"ApiType4","required":false,"encoding":"scalar"}],"responses":{"200":"Unknown"}}]});
    std::fs::write(root.path().join("spec.json"), spec.to_string()).unwrap();
    let run = |home: &str, command: &str| {
        Command::new(env!("CARGO_BIN_EXE_wes"))
            .current_dir(root.path())
            .arg("--home")
            .arg(root.path().join(home))
            .args(["--sequential", "--command", command])
            .output()
            .unwrap()
    };
    let help = run(
        "help",
        ":import spec file:spec.json as:inventory endpoint:\"http://127.0.0.1:1\"\n:help inventory listItems",
    );
    assert!(
        help.status.success(),
        "{}",
        String::from_utf8_lossy(&help.stderr)
    );
    let out = String::from_utf8_lossy(&help.stdout);
    assert!(out.contains("1..50 (inclusive)"), "{out}");
    assert!(out.contains("queued") && out.contains("done"), "{out}");
    let failed = run(
        "failure",
        ":import spec file:spec.json as:inventory endpoint:\"http://127.0.0.1:1\"\n:calc { return call('inventory',['listItems'],{limit:99}); }",
    );
    assert!(!failed.status.success());
    let error = String::from_utf8_lossy(&failed.stderr);
    assert!(
        error.contains("CAL008")
            && error.contains("HTTP001")
            && error.contains("1..50 (inclusive)"),
        "{error}"
    );
    assert!(error.contains("/arguments/limit"), "{error}");
    assert!(!error.contains("ApiType3"));
}

#[test]
fn literal_credential_warning_is_once_and_no_value_is_echoed_on_parse_failure() {
    let root = tempfile::tempdir().unwrap();
    for (index, source) in [
        ":calc { return 'Bearer synthetic-credential' ",
        ":calc { return 'Bearer synthetic-credential'; }\n:workspace save 'test'",
    ]
    .into_iter()
    .enumerate()
    {
        let output = Command::new(env!("CARGO_BIN_EXE_wes"))
            .arg("--home")
            .arg(root.path().join(format!("case{index}")))
            .args(["--command", source])
            .output()
            .unwrap();
        assert!(!output.status.success());
        let err = String::from_utf8_lossy(&output.stderr);
        assert_eq!(err.matches("SEC001").count(), 1, "{err}");
        assert!(err.contains("--credentials-stdin"));
        assert!(!err.contains("synthetic-credential"));
    }
}

#[test]
fn credential_access_denial_names_the_provider_even_when_values_are_supplied() {
    use std::{io::Write, process::Stdio};
    let root = tempfile::tempdir().unwrap();
    let spec = serde_json::json!({"version":1,"provider":"inventory","types":{},"operations":[{"path":["listItems"],"method":"GET","route":"/items","auth":[{"header":"Authorization","scheme":"Bearer","secret":"token"}],"parameters":[],"responses":{"200":"Unknown"}}]});
    std::fs::write(root.path().join("spec.json"), spec.to_string()).unwrap();
    std::fs::write(root.path().join("env.yaml"), "version: 1\ntargets: {local: {kind: local}}\nenvironments: {dev: {secretSlots: {key: {required: true}}, secretRefs: {key: synthetic/token}, imports: {inventory: {source: {kind: spec, file: spec.json}, bind: {target: local, endpoint: 'http://127.0.0.1:1', credentials: {token: {secret: key}}}}}}}").unwrap();
    for (index, supplied, grant) in [(0, false, false), (1, true, false), (2, false, true)] {
        let mut command = Command::new(env!("CARGO_BIN_EXE_wes"));
        command
            .current_dir(root.path())
            .arg("--home")
            .arg(root.path().join(format!("data{index}")))
            .args([
                "--env-file",
                "env.yaml",
                "--env",
                "dev",
                "--credentials-stdin",
                "--command",
                "inventory listItems",
            ]);
        if grant {
            command.args(["--grant-provider", "inventory"]);
        }
        let mut child = command
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        child
            .stdin
            .take()
            .unwrap()
            .write_all(if supplied {
                br#"{"synthetic/token":"synthetic-private-material"}"#
            } else {
                b"{}"
            })
            .unwrap();
        let output = child.wait_with_output().unwrap();
        assert!(!output.status.success());
        let error = String::from_utf8_lossy(&output.stderr);
        if grant {
            assert!(error.contains("missing credential 'token'"), "{error}");
            assert!(!error.contains("access has not been granted"), "{error}");
        } else {
            assert!(
                error.contains("access has not been granted for provider 'inventory'"),
                "{error}"
            );
            assert!(error.contains("--grant-provider inventory"), "{error}");
        }
        assert!(!error.contains("synthetic-private-material"));
        assert!(!String::from_utf8_lossy(&output.stdout).contains("synthetic-private-material"));
    }
}

#[cfg(unix)]
#[test]
fn credential_stdin_and_provider_grant_failures_explain_the_missing_input() {
    use std::{io::Write, process::Stdio};
    let root = tempfile::tempdir().unwrap();
    std::fs::write(root.path().join("env.yaml"), "version: 1\ntargets: {local: {kind: local}}\nenvironments: {dev: {secretSlots: {key: {required: true}}, secretRefs: {key: synthetic/batch}, imports: {vars: {source: {kind: process, bin: /usr/bin/printenv}, bind: {target: local, credentials: {SYNTHETIC_SECRET: {secret: key}}}}}}}").unwrap();
    for (provider, material, expected) in [
        ("vars", r#"{"synthetic/batch":""}"#, "nonempty"),
        (
            "missing",
            r#"{"synthetic/batch":"synthetic-private-value"}"#,
            "Credential grant target is unavailable",
        ),
    ] {
        let mut child = Command::new(env!("CARGO_BIN_EXE_wes"))
            .current_dir(root.path())
            .arg("--home")
            .arg(root.path().join(provider))
            .args([
                "--env-file",
                "env.yaml",
                "--env",
                "dev",
                "--credentials-stdin",
                "--grant-provider",
                provider,
                "--command",
                ":help",
            ])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        let _ = child.stdin.take().unwrap().write_all(material.as_bytes());
        let output = child.wait_with_output().unwrap();
        assert!(!output.status.success());
        let message = String::from_utf8_lossy(&output.stderr);
        assert!(message.contains(expected), "{message}");
        assert!(!message.contains("synthetic-private-value"));
        assert!(!String::from_utf8_lossy(&output.stdout).contains("synthetic-private-value"));
    }
}

#[test]
fn human_cli_normalizes_decimal_padding_and_labels_its_error_source() {
    let root = tempfile::tempdir().unwrap();
    let run = |json: bool, command: &str| {
        let mut cli = Command::new(env!("CARGO_BIN_EXE_wes"));
        cli.current_dir(root.path())
            .arg("--home")
            .arg(root.path().join(if json { "json" } else { "human" }));
        if json {
            cli.arg("--json");
        }
        cli.args(["--command", command]).output().unwrap()
    };
    let command =
        ":calc { return [toSeconds(duration('PT1M30S')), toMillis(duration('PT0.0015S'))]; }";
    let human = run(false, command);
    assert!(
        human.status.success(),
        "{}",
        String::from_utf8_lossy(&human.stderr)
    );
    assert!(
        String::from_utf8_lossy(&human.stdout).contains("[90,1.5]"),
        "{}",
        String::from_utf8_lossy(&human.stdout)
    );
    let json = run(true, command);
    assert!(json.status.success());
    assert!(String::from_utf8_lossy(&json.stdout).contains("[90.000000000,1.500000]"));
    let error = run(false, ":calc { return 1 / 0; } > badRatio");
    let stderr = String::from_utf8_lossy(&error.stderr);
    assert!(!error.status.success());
    assert!(stderr.contains("$badRatio"), "{stderr}");
    assert!(
        !stderr.contains("at cell ") && !stderr.contains("called from cell "),
        "{stderr}"
    );
}

#[test]
fn stale_read_has_one_sentence_boundary_and_does_not_rerun_work() {
    let root = tempfile::tempdir().unwrap();
    let run = |command: &str| {
        Command::new(env!("CARGO_BIN_EXE_wes"))
            .current_dir(root.path())
            .arg("--home")
            .arg(root.path().join("home"))
            .args(["--command", command])
            .output()
            .unwrap()
    };
    let first = run(
        ":calc { return 1; } > root\n:calc { return $root + 1; } > head\n:policy $head mode:manual",
    );
    assert!(first.status.success());
    let refreshed = run(":refresh $root");
    assert!(
        refreshed.status.success(),
        "{}",
        String::from_utf8_lossy(&refreshed.stderr)
    );
    let read = run(":read $head");
    let stderr = String::from_utf8_lossy(&read.stderr);
    assert!(!read.status.success());
    assert!(stderr.contains("stale:"));
    assert!(!stderr.contains(".. Use"), "{stderr}");
}

#[test]
fn an_authored_file_named_cell_is_not_confused_with_an_opaque_cell_identity() {
    let root = tempfile::tempdir().unwrap();
    std::fs::write(
        root.path().join("cell fixture.wes"),
        ":calc { return 1 / 0; }",
    )
    .unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_wes"))
        .current_dir(root.path())
        .arg("--home")
        .arg(root.path().join("home"))
        .args(["--file", "cell fixture.wes"])
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(!output.status.success());
    assert!(stderr.contains("at cell fixture.wes: line"), "{stderr}");
}
