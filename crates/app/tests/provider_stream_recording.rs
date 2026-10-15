//! Synthetic owned peers exercise real provider declarations, storage and native result admission.
use std::{path::Path, sync::Arc, time::Duration};
use wes::runtime::{LaunchedRuntime, RuntimeOptions, launch};
use wes_core::Data;
use wes_engine::{
    session::{SessionHandle, SubmissionResult},
    source::SourceInput,
};

struct Fixture {
    root: tempfile::TempDir,
    runtime: LaunchedRuntime,
    session: SessionHandle,
}
impl Fixture {
    async fn new(mode: &str, policy: &str, maximum: u64, lifetime: u64) -> Self {
        let root = tempfile::tempdir().unwrap();
        let python = wes_adapters::process::serialized_spawn(|| {
            std::process::Command::new("python3")
                .args(["-c", "import sys; print(sys.executable)"])
                .output()
        })
        .unwrap();
        assert!(python.status.success());
        let python = String::from_utf8(python.stdout).unwrap().trim().to_owned();
        let peer = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../adapters/tests/support/provider_session.py");
        let document = serde_json::json!({"version":1,"provider":"samples","backend":{"kind":"session","bin":python},
            "types":{"Blob":{"base":"Record","fields":{"label":"Text","raw":"Bytes"}}},
            "operations":[{"path":["watch"],"streaming":true,"argv":[peer,mode,root.path()],
                "result":{"format":"json","type":"Int"},"control":{"input":"Int","output":"Bool"},
                "recording":{"type":"Blob","byte_field":["raw"],"max_bytes":maximum,"max_duration_ms":lifetime}}]});
        std::fs::write(root.path().join("provider.json"), document.to_string()).unwrap();
        std::fs::write(root.path().join("env.yaml"), format!("version: 1\ntargets: {{local: {{kind: local}}}}\nenvironments:\n  lab:\n    imports:\n      samples:\n        source: {{kind: provider, file: provider.json}}\n        bind: {{target: local, output: {policy}}}\n")).unwrap();
        let key = root.path().join("key");
        std::fs::write(&key, [73; 32]).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&key, std::fs::Permissions::from_mode(0o600)).unwrap();
        }
        let mut options = RuntimeOptions::new(root.path().join("home"), root.path().into());
        options.storage_key_file = Some(key);
        options.docker_candidates = Some(vec![root.path().join("unavailable.sock")]);
        let runtime = launch(options).await.unwrap();
        let session = runtime.handle.current().unwrap().session;
        let plan = session
            .plan_environment_file("env.yaml".into(), false)
            .await
            .unwrap();
        session.apply_environments(plan).await.unwrap();
        let fixture = Self {
            root,
            runtime,
            session,
        };
        fixture.submit(":env use \"lab\"").await;
        fixture
    }
    async fn submit(&self, text: &str) -> Arc<SubmissionResult> {
        let result = self
            .session
            .submit(SourceInput::new(uuid::Uuid::new_v4().to_string(), text.into()).unwrap())
            .await
            .unwrap();
        assert!(
            !result
                .diagnostics
                .diagnostics
                .iter()
                .any(|d| d.severity == wes_language::Severity::Error),
            "{text}: {:?}",
            result.diagnostics
        );
        tokio::time::timeout(Duration::from_secs(30), self.session.wait_idle())
            .await
            .unwrap()
            .unwrap();
        result
    }
    async fn joined(&self, name: &str) {
        tokio::time::timeout(Duration::from_secs(30), async {
            loop {
                let snapshot = self.session.snapshot().await.unwrap();
                if !snapshot
                    .execution
                    .executing
                    .contains(&snapshot.names[name].node)
                {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();
    }
    async fn field(&self, selection: &str, name: &str) -> Data {
        let reply = self
            .submit(&format!(
                ":calc pure {{ return $evidence.{selection}; }} > {name}"
            ))
            .await;
        let snapshot = self.session.snapshot().await.unwrap();
        snapshot
            .execution
            .values
            .get(&reply.nodes[0])
            .unwrap_or_else(|| panic!("{:?}", snapshot.execution.errors))
            .data()
            .clone()
    }
    async fn raw(&self, from: u64, length: usize, name: &str) -> Data {
        let reply = self
            .submit(&format!(
                ":read bytes:$evidence from:\"{from}\" limit:{length} select:\"value.raw\" > {name}"
            ))
            .await;
        let snapshot = self.session.snapshot().await.unwrap();
        let value = snapshot
            .execution
            .values
            .get(&reply.nodes[0])
            .unwrap_or_else(|| panic!("{:?}", snapshot.execution.errors));
        assert!(value.provenance().policy().is_confidential());
        let Data::Record(fields) = value.data() else {
            panic!("range")
        };
        fields["data"].clone()
    }
    fn attempts(&self) -> usize {
        std::fs::read_to_string(self.root.path().join("attempts"))
            .map(|s| s.lines().count())
            .unwrap_or(0)
    }
    async fn stop(self) {
        drop(self.session);
        self.runtime.shutdown().await.unwrap();
        fn scan(path: &Path) {
            for entry in std::fs::read_dir(path).unwrap() {
                let path = entry.unwrap().path();
                if path.is_dir() {
                    scan(&path);
                } else {
                    assert!(
                        !std::fs::read(&path)
                            .unwrap()
                            .windows(28)
                            .any(|w| w == b"synthetic-owned-raw-sentinel"),
                        "{}",
                        path.display()
                    );
                }
            }
        }
        scan(&self.root.path().join("home"));
    }
}

#[tokio::test]
async fn large_protected_recording_is_a_separate_exact_result_without_source_replay() {
    let f = Fixture::new("raw-large", "confidential", 40 * 1024 * 1024, 30_000).await;
    f.submit("samples watch > live").await;
    f.joined("live").await;
    let before = f.session.snapshot().await.unwrap();
    let source = before.names["live"].node.clone();
    let run = before.execution.runs[&source].clone();
    assert!(
        !before.execution.errors.contains_key(&source),
        "{:?}",
        before.execution.errors
    );
    f.submit(":source result $live > evidence").await;
    let expected = 28 + 520 * 65536u64;
    assert!(expected > wes_budgets::get("process.output.bytes"));
    assert_eq!(
        f.field("acceptedBytes", "extent").await,
        Data::Text(expected.to_string().into())
    );
    assert_eq!(f.field("complete", "complete").await, Data::Bool(true));
    assert_eq!(
        f.field("value.label", "label").await,
        Data::Text("final".into())
    );
    assert_eq!(
        f.raw(24, 10, "range").await,
        Data::Bytes([b"inel".as_slice(), &[147; 6]].concat().into())
    );
    assert_eq!(f.attempts(), 1);
    assert_eq!(
        f.session.snapshot().await.unwrap().execution.runs[&source],
        run
    );
    let observed = f.session.observe().await.unwrap();
    let node = observed.state.names["evidence"].node.clone();
    let handle = observed.values.as_ref().unwrap().outputs[&node]
        .handle()
        .unwrap()
        .clone();
    drop(observed);
    let kept = f.session.keep(handle).await.unwrap();
    assert!(kept.affected && kept.problem.is_none(), "{kept:?}");
    f.stop().await;
}

#[tokio::test]
async fn a_closed_credit_pipe_requires_a_valid_original_terminal_receipt_without_replay() {
    for (mode, complete, label) in [
        ("raw-final-credit-end", true, "final"),
        ("raw-final-credit-eof", false, "initial"),
        ("raw-final-credit-no-shell", false, "initial"),
        ("raw-final-credit-foreign", false, "initial"),
        ("raw-final-credit-extra-event", false, "initial"),
    ] {
        let f = Fixture::new(mode, "confidential-temporary", 65536, 30_000).await;
        f.submit("samples watch > live").await;
        f.joined("live").await;
        f.submit(":source result $live > evidence").await;
        assert_eq!(
            f.field("complete", "complete").await,
            Data::Bool(complete),
            "{mode}"
        );
        assert_eq!(
            f.field("value.label", "label").await,
            Data::Text(label.into()),
            "{mode}"
        );
        assert_eq!(
            f.raw(0, 28, "raw").await,
            Data::Bytes(b"synthetic-owned-raw-sentinel".to_vec().into())
        );
        assert_eq!(f.attempts(), 1);
        f.stop().await;
    }
}

#[tokio::test]
async fn active_read_refuses_and_graceful_control_stop_joins_before_exposing_recording() {
    let f = Fixture::new("raw-hold", "confidential-temporary", 65536, 30_000).await;
    f.submit("samples watch > live").await;
    f.submit(":source result $live > early").await;
    let before = f.session.snapshot().await.unwrap();
    let source = before.names["live"].node.clone();
    assert!(before.execution.streaming.contains(&source));
    assert_eq!(
        before.execution.errors[&before.names["early"].node].code(),
        "ENV021"
    );
    f.submit(":source bind $live > controls").await;
    f.submit(":source send $controls action:1 > stopped").await;
    f.joined("live").await;
    f.submit(":source result $live > evidence").await;
    assert_eq!(f.field("complete", "complete").await, Data::Bool(true));
    assert_eq!(
        f.raw(0, 28, "raw").await,
        Data::Bytes(b"synthetic-owned-raw-sentinel".to_vec().into())
    );
    let observed = f.session.observe().await.unwrap();
    let node = observed.state.names["evidence"].node.clone();
    let handle = observed.values.as_ref().unwrap().outputs[&node]
        .handle()
        .unwrap()
        .clone();
    drop(observed);
    let kept = f.session.keep(handle).await.unwrap();
    assert!(!kept.affected && kept.problem.is_some());
    assert_eq!(f.attempts(), 1);
    f.submit(":source send $controls action:1 > late").await;
    assert_eq!(
        std::fs::read_to_string(f.root.path().join("actions")).unwrap(),
        "1\n"
    );
    f.stop().await;
}

#[tokio::test]
async fn errors_and_watchdog_publish_only_the_acknowledged_incomplete_prefix() {
    for (mode, maximum, lifetime, end) in [
        ("raw-fail", 65536, 30_000, "failed"),
        ("raw-gap", 65536, 30_000, "failed"),
        ("raw-quota", 32, 30_000, "limit"),
        ("raw-watchdog", 65536, 1000, "limit"),
    ] {
        let f = Fixture::new(mode, "confidential", maximum, lifetime).await;
        f.submit("samples watch > live").await;
        f.joined("live").await;
        f.submit(":source result $live > evidence").await;
        assert_eq!(
            f.field("acceptedBytes", "extent").await,
            Data::Text("28".into()),
            "{mode}"
        );
        assert_eq!(
            f.field("complete", "complete").await,
            Data::Bool(false),
            "{mode}"
        );
        assert_eq!(
            f.field("termination", "termination").await,
            Data::Text(end.into()),
            "{mode}"
        );
        assert_eq!(
            f.field("value.label", "label").await,
            Data::Text("initial".into())
        );
        assert_eq!(f.attempts(), 1);
        f.stop().await;
    }
}

#[tokio::test]
async fn private_recording_refuses_before_external_launch() {
    let f = Fixture::new("raw-hold", "private", 65536, 1000).await;
    f.submit("samples watch > live").await;
    f.joined("live").await;
    assert_eq!(f.attempts(), 0);
    f.stop().await;
}

#[tokio::test]
async fn invalid_frames_never_fabricate_protocol_completion_or_restart_the_peer() {
    for (mode, bytes) in [
        ("raw-before-shell", None),
        ("raw-duplicate-shell", Some("0")),
        ("raw-bad-base64", Some("0")),
        ("raw-foreign", Some("28")),
        ("raw-premature-end", Some("28")),
    ] {
        let f = Fixture::new(mode, "confidential", 65536, 1000).await;
        f.submit("samples watch > live").await;
        f.joined("live").await;
        let reply = f.submit(":source result $live > evidence").await;
        if let Some(bytes) = bytes {
            assert_eq!(
                f.field("acceptedBytes", "extent").await,
                Data::Text(bytes.into()),
                "{mode}"
            );
            assert_eq!(f.field("complete", "complete").await, Data::Bool(false));
        } else {
            assert!(
                f.session
                    .snapshot()
                    .await
                    .unwrap()
                    .execution
                    .errors
                    .contains_key(&reply.nodes[0])
            );
        }
        assert_eq!(f.attempts(), 1);
        f.stop().await;
    }
}
#[tokio::test]
async fn unknown_control_keeps_only_raw_prefix_and_copied_windows_cannot_confer_ownership() {
    let f = Fixture::new("raw-unknown", "confidential", 65536, 30_000).await;
    f.submit("samples watch > live").await;
    f.submit(":calc pure { return $live; } > copy").await;
    let copy = f.submit(":source result $copy > copied").await;
    assert!(
        f.session
            .snapshot()
            .await
            .unwrap()
            .execution
            .errors
            .contains_key(&copy.nodes[0])
    );
    f.submit(":source bind $live > controls").await;
    let action = f.submit(":source send $controls action:1 > unknown").await;
    f.joined("live").await;
    assert_eq!(
        f.session.snapshot().await.unwrap().execution.errors[&action.nodes[0]].code(),
        "ENV036"
    );
    f.submit(":source result $live > evidence").await;
    assert_eq!(
        f.field("termination", "termination").await,
        Data::Text("unconfirmed".into())
    );
    assert_eq!(f.field("complete", "complete").await, Data::Bool(false));
    assert_eq!(f.attempts(), 1);
    assert_eq!(
        std::fs::read_to_string(f.root.path().join("actions")).unwrap(),
        "1\n"
    );
    f.stop().await;
}
