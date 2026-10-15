//! Real declarative capture sessions use isolated homes and a backend with no network access.
#![cfg(unix)]
use std::{os::unix::fs::PermissionsExt, path::Path, sync::Arc, time::Duration};
use wes::runtime::{LaunchedRuntime, RuntimeOptions, launch};
use wes_core::Data;
use wes_engine::{
    session::{SessionHandle, SubmissionResult},
    source::SourceInput,
};

const PAYLOAD: &[u8] = b"synthetic-local-capture-sentinel\0\xff";
struct Fixture {
    root: tempfile::TempDir,
    runtime: LaunchedRuntime,
    session: SessionHandle,
}
impl Fixture {
    async fn new(mode: &str, policy: &str, maximum: u64) -> Self {
        Self::configured(mode, policy, maximum, false).await
    }
    async fn configured(mode: &str, policy: &str, maximum: u64, configured: bool) -> Self {
        Self::configuration(mode, policy, maximum, configured, false).await
    }
    async fn capture(policy: &str) -> Self {
        Self::configuration("synthetic-idle", policy, 1048576, true, true).await
    }
    async fn configuration(
        mode: &str,
        policy: &str,
        maximum: u64,
        configured: bool,
        single: bool,
    ) -> Self {
        Self::timed_configuration(
            mode,
            policy,
            maximum,
            configured,
            single,
            Duration::from_secs(60),
        )
        .await
    }
    async fn timed_configuration(
        mode: &str,
        policy: &str,
        maximum: u64,
        configured: bool,
        single: bool,
        lifetime: Duration,
    ) -> Self {
        let duration_ms = lifetime.as_millis();
        let root = tempfile::tempdir().unwrap();
        let package = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../examples/tracing-live");
        let python = wes_adapters::process::serialized_spawn(|| {
            std::process::Command::new("python3")
                .args(["-c", "import sys; print(sys.executable)"])
                .output()
        })
        .unwrap();
        assert!(python.status.success());
        let python = String::from_utf8(python.stdout).unwrap().trim().to_owned();
        let fixture = std::fs::read_to_string(package.join("fixture_backend.py")).unwrap();
        let backend = root.path().join("synthetic-backend");
        std::fs::write(
            &backend,
            format!("#!{python}\n{}", fixture.split_once('\n').unwrap().1),
        )
        .unwrap();
        std::fs::set_permissions(&backend, std::fs::Permissions::from_mode(0o700)).unwrap();
        let provider = std::fs::read_to_string(package.join(if configured {
            "profile.provider.yaml"
        } else {
            "provider.yaml"
        }))
        .unwrap()
        .replace(
            "namespace: wes.capture.",
            &format!("namespace: synthetic.{}.capture.", uuid::Uuid::new_v4()),
        )
        .replace(
            "bin: capture_session.py",
            &format!("bin: {}", serde_json::to_string(&python).unwrap()),
        )
        .replace(
            "argv: [start]",
            &format!(
                "argv: [{}, start]",
                serde_json::to_string(package.join("capture_session.py").to_str().unwrap())
                    .unwrap()
            ),
        )
        .replace(
            "backend: /usr/bin/dumpcap",
            &format!(
                "backend: {}",
                serde_json::to_string(backend.to_str().unwrap()).unwrap()
            ),
        )
        .replace(
            "interface: lo\n",
            &format!("interface: {}\n", serde_json::to_string(mode).unwrap()),
        )
        .replace("maxBytes: 16777216", &format!("maxBytes: {maximum}"))
        .replace("durationMs: 30000", &format!("durationMs: {duration_ms}"));
        std::fs::write(root.path().join("provider.yaml"), &provider).unwrap();
        std::fs::write(
            root.path().join("other.provider.yaml"),
            provider.replace("name: lab-tcp", "name: lab-other"),
        )
        .unwrap();
        std::fs::write(root.path().join("env.yaml"), format!("version: 1\ntargets:\n  local: {{kind: local, cwd: {}}}\nenvironments:\n  lab:\n    imports:\n      capture:\n        source: {{kind: provider, file: provider.yaml}}\n        bind: {{target: local, output: {policy}}}\n      capture_alias:\n        source: {{kind: provider, file: provider.yaml}}\n        bind: {{target: local, output: {policy}}}\n      capture_other:\n        source: {{kind: provider, file: other.provider.yaml}}\n        bind: {{target: local, output: {policy}}}\n", serde_json::to_string(root.path().to_str().unwrap()).unwrap())).unwrap();
        if single {
            std::fs::write(root.path().join("env.yaml"),format!("version: 1\ntargets:\n  local: {{kind: local, cwd: {}}}\nenvironments:\n  lab:\n    imports:\n      sh:\n        source: {{kind: builtin, name: sh}}\n        bind: {{target: local, output: public}}\n      capture:\n        source: {{kind: provider, file: provider.yaml}}\n        bind: {{target: local, output: {policy}}}\n", serde_json::to_string(root.path().to_str().unwrap()).unwrap())).unwrap();
        }
        let key = root.path().join("key");
        std::fs::write(&key, [87; 32]).unwrap();
        std::fs::set_permissions(&key, std::fs::Permissions::from_mode(0o600)).unwrap();
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
        let f = Self {
            root,
            runtime,
            session,
        };
        f.submit(":env use \"lab\"").await;
        f.submit(&format!(":calc pure {{ return {{name:\"synthetic capture\",backend:{},interface:{},filter:\"tcp port 4242\",durationMs:{duration_ms},maxBytes:{maximum},packets:1000,snapLength:65535,promiscuous:false}}; }} > profile", serde_json::to_string(backend.to_str().unwrap()).unwrap(),serde_json::to_string(mode).unwrap())).await;
        f
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
        tokio::time::timeout(Duration::from_secs(20), self.session.wait_idle())
            .await
            .unwrap()
            .unwrap();
        result
    }
    async fn joined(&self) {
        self.joined_within(Duration::from_secs(20)).await;
    }
    async fn joined_within(&self, deadline: Duration) {
        tokio::time::timeout(deadline, async {
            loop {
                let state = self.session.snapshot().await.unwrap();
                if !state
                    .execution
                    .executing
                    .contains(&state.names["live"].node)
                {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();
    }
    async fn value(&self, text: &str) -> Data {
        let reply = self.submit(text).await;
        let state = self.session.snapshot().await.unwrap();
        let value = state
            .execution
            .values
            .get(&reply.nodes[0])
            .unwrap_or_else(|| panic!("{text}: {:?}", state.execution.errors));
        assert!(value.provenance().policy().is_confidential());
        value.data().clone()
    }
    async fn field(&self, path: &str, name: &str) -> Data {
        self.value(&format!(
            ":calc pure {{ return $evidence.{path}; }} > {name}"
        ))
        .await
    }
    fn attempts(&self) -> usize {
        std::fs::read_to_string(self.root.path().join("attempts"))
            .map(|s| s.lines().count())
            .unwrap_or(0)
    }
    async fn shutdown(self) {
        drop(self.session);
        self.runtime.shutdown().await.unwrap();
        fn scan(path: &Path) {
            for entry in std::fs::read_dir(path).unwrap() {
                let p = entry.unwrap().path();
                if p.is_dir() {
                    scan(&p);
                } else {
                    assert!(
                        !std::fs::read(&p)
                            .unwrap()
                            .windows(PAYLOAD.len())
                            .any(|w| w == PAYLOAD),
                        "{}",
                        p.display()
                    );
                }
            }
        }
        scan(&self.root.path().join("home"));
    }
}

#[tokio::test]
async fn independent_capture_graceful_stop_and_native_original_reads_keep_the_captured_ceiling() {
    let f = Fixture::new("synthetic-hold", "confidential-temporary", 1048576).await;
    f.submit("capture start profile:$profile > live").await;
    let state = f.session.snapshot().await.unwrap();
    let source = state.names["live"].node.clone();
    let run = state.execution.runs[&source].clone();
    assert!(
        state.execution.streaming.contains(&source),
        "{:?}",
        state.execution.errors
    );
    f.submit(":source bind $live > controls").await;
    f.submit(":calc pure { return {operation:\"stop\"}; } > stop_action")
        .await;
    f.submit(":source send $controls action:$stop_action > stopped")
        .await;
    f.joined().await;
    std::fs::remove_file(f.root.path().join("synthetic-backend")).unwrap();
    f.submit(":source result $live > evidence").await;
    let complete = f.field("complete", "complete").await;
    assert_eq!(
        complete,
        Data::Bool(true),
        "termination={:?}; phase={:?}; reason={:?}",
        f.field("termination", "termination").await,
        f.field("value.summary.phase", "final_phase").await,
        f.field("value.reason", "reason").await
    );
    assert_eq!(
        f.field("value.summary.phase", "phase").await,
        Data::Text("ended".into())
    );
    assert_eq!(
        f.field("value.summary.drops", "drops").await,
        Data::Option(None)
    );
    assert_eq!(
        f.field("value.summary.coverageKnown", "coverage").await,
        Data::Bool(false)
    );
    let Data::Option(Some(index)) = f.field("value.index", "index").await else {
        panic!("original index")
    };
    let Data::Record(index) = index.as_ref() else {
        panic!("index record")
    };
    assert_eq!(index["packetCount"], Data::Text("1".into()));
    assert_eq!(
        index["rawBytes"],
        Data::Text((40 + PAYLOAD.len()).to_string().into())
    );
    let raw = f
        .value(&format!(
            ":read bytes:$evidence from:\"40\" limit:{} select:\"value.raw\" > bytes",
            PAYLOAD.len()
        ))
        .await;
    let Data::Record(raw) = raw else {
        panic!("native bytes")
    };
    assert_eq!(raw["data"], Data::Bytes(PAYLOAD.to_vec().into()));
    assert_eq!(
        f.session.snapshot().await.unwrap().execution.runs[&source],
        run
    );
    assert_eq!(f.attempts(), 1);
    let observed = f.session.observe().await.unwrap();
    let node = observed.state.names["evidence"].node.clone();
    let handle = observed.values.as_ref().unwrap().outputs[&node]
        .handle()
        .unwrap()
        .clone();
    drop(observed);
    let kept = f.session.keep(handle).await.unwrap();
    assert!(!kept.affected && kept.problem.is_some(), "{kept:?}");
    f.shutdown().await;
}

#[tokio::test]
async fn configured_profile_is_immutable_and_exclusive_until_joined_stop_then_explicit_restart() {
    let f = Fixture::configured("synthetic-hold", "confidential-temporary", 1048576, true).await;
    let state = f.session.observe().await.unwrap();
    let parameters = &state.environment_catalogues["lab"]
        .provider("capture")
        .unwrap()
        .capability(&["start".into()])
        .unwrap()
        .parameters;
    assert_eq!(
        parameters
            .iter()
            .map(|p| p.name.as_str())
            .collect::<Vec<_>>(),
        [
            "duration_ms",
            "packet_limit",
            "max_bytes",
            "filter_condition"
        ]
    );
    assert!(parameters.iter().all(|p| !p.required));
    let rejected = f
        .session
        .submit(
            SourceInput::new(
                "fixed-override".into(),
                "capture start profile:$profile > overridden".into(),
            )
            .unwrap(),
        )
        .await
        .unwrap();
    assert!(
        rejected
            .diagnostics
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.severity == wes_language::Severity::Error)
    );
    assert_eq!(f.attempts(), 0);
    f.submit("capture start > live").await;
    assert_eq!(f.attempts(), 1);
    for provider in ["capture_alias", "capture_other"] {
        let rejected = f
            .submit(&format!("{provider} start > rejected_{provider}"))
            .await;
        let state = f.session.snapshot().await.unwrap();
        assert_eq!(state.execution.errors[&rejected.nodes[0]].code(), "ENV021");
        assert!(
            state.execution.errors[&rejected.nodes[0]]
                .policy()
                .is_confidential()
        );
        assert_eq!(f.attempts(), 1);
    }
    // Existing imported owners consume frozen declarations, never a later mutable-file version.
    let path = f.root.path().join("provider.yaml");
    let changed = std::fs::read_to_string(&path)
        .unwrap()
        .replace("name: lab-tcp", "name: changed-profile")
        .replace("filter: tcp and host 127.0.0.1", "filter: udp");
    std::fs::write(path, changed).unwrap();
    f.submit(":source bind $live > controls").await;
    f.submit(":calc pure { return {operation:\"stop\"}; } > stop_action")
        .await;
    f.submit(":source send $controls action:$stop_action > stopped")
        .await;
    f.joined().await;
    f.submit(":source result $live > evidence").await;
    assert_eq!(
        f.field("value.profile.name", "profile_name").await,
        Data::Text("lab-tcp".into())
    );
    assert_eq!(
        f.field("value.profile.filter", "profile_filter").await,
        Data::Text("tcp and host 127.0.0.1".into())
    );
    let Data::Record(bytes) = f
        .value(&format!(
            ":read bytes:$evidence from:\"40\" limit:{} select:\"value.raw\" > original",
            PAYLOAD.len()
        ))
        .await
    else {
        panic!("Original bytes")
    };
    assert_eq!(bytes["data"], Data::Bytes(PAYLOAD.to_vec().into()));
    let observed = f.session.observe().await.unwrap();
    let handle = observed.values.as_ref().unwrap().outputs[&observed.state.names["evidence"].node]
        .handle()
        .unwrap()
        .clone();
    drop(observed);
    let keep = f.session.keep(handle).await.unwrap();
    assert!(!keep.affected && keep.problem.is_some());
    f.submit("capture_alias start > later").await;
    assert_eq!(f.attempts(), 2);
    f.submit(":source bind $later > later_controls").await;
    f.submit(":source send $later_controls action:$stop_action > later_stopped")
        .await;
    tokio::time::timeout(Duration::from_secs(20), async {
        loop {
            let state = f.session.snapshot().await.unwrap();
            if !state
                .execution
                .executing
                .contains(&state.names["later"].node)
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    assert_eq!(f.attempts(), 2);
    f.shutdown().await;
}

#[tokio::test]
async fn configured_profile_narrows_before_backend_entry_and_publishes_effective_original_evidence()
{
    let f = Fixture::configured("synthetic-idle", "confidential-temporary", 1048576, true).await;
    for (name, options) in [
        ("duration", "duration_ms:60001"),
        ("packets", "packet_limit:10001"),
        ("bytes", "max_bytes:1048577"),
        ("filter", "filter_condition:\"vlan 7 and tcp\""),
    ] {
        let rejected = f
            .submit(&format!("capture start {options} > rejected_{name}"))
            .await;
        let state = f.session.snapshot().await.unwrap();
        assert!(
            state.execution.errors.contains_key(&rejected.nodes[0]),
            "{name}: {:?}",
            state.execution.errors
        );
        assert_eq!(f.attempts(), 0);
    }
    f.submit("capture start duration_ms:20000 packet_limit:10 max_bytes:4096 filter_condition:\"dst port 4242\" > live").await;
    let state = f.session.snapshot().await.unwrap();
    let node = state.names["live"].node.clone();
    let run = state.execution.runs[&node].clone();
    assert!(
        state.execution.streaming.contains(&node),
        "{:?}",
        state.execution.errors
    );
    assert_eq!(f.attempts(), 1);
    f.submit(":source bind $live > controls").await;
    f.submit(":calc pure { return {operation:\"stop\"}; } > stop_action")
        .await;
    f.submit(":source send $controls action:$stop_action > stopped")
        .await;
    f.joined().await;
    f.submit(":source result $live > evidence").await;
    for (field, expected) in [("durationMs", 20000), ("packets", 10), ("maxBytes", 4096)] {
        assert_eq!(
            f.field(
                &format!("value.profile.{field}"),
                &format!("effective_{field}")
            )
            .await,
            Data::Int(expected)
        );
    }
    let filter = "((tcp and host 127.0.0.1) and dst port 4242)";
    assert_eq!(
        f.field("value.profile.filter", "effective_filter").await,
        Data::Text(filter.into())
    );
    assert_eq!(
        f.field("value.profile.name", "effective_name").await,
        Data::Text("lab-tcp".into())
    );
    assert_eq!(
        f.field("acceptedBytes", "accepted").await,
        Data::Text("24".into())
    );
    assert_eq!(f.field("complete", "complete").await, Data::Bool(true));
    let arguments: Vec<String> = serde_json::from_slice(
        &std::fs::read(f.root.path().join("backend.arguments.json")).unwrap(),
    )
    .unwrap();
    assert_eq!(
        arguments,
        [
            "-i",
            "synthetic-idle",
            "-f",
            filter,
            "-F",
            "pcap",
            "-q",
            "-s",
            "65535",
            "-c",
            "10",
            "-a",
            "duration:20.000",
            "-w",
            "-",
            "-p"
        ]
    );
    let Data::Record(bytes) = f
        .value(":read bytes:$evidence from:\"0\" limit:24 select:\"value.raw\" > original_header")
        .await
    else {
        panic!("native header")
    };
    let Data::Bytes(bytes) = &bytes["data"] else {
        panic!("original bytes")
    };
    assert_eq!(bytes.len(), 24);
    assert_eq!(&bytes[..4], &[0xd4, 0xc3, 0xb2, 0xa1]);
    assert_eq!(
        f.session.snapshot().await.unwrap().execution.runs[&node],
        run
    );
    assert_eq!(f.attempts(), 1);
    f.shutdown().await;
}

#[tokio::test]
async fn configured_private_profile_refuses_before_backend_launch_and_releases_its_resources() {
    let f = Fixture::configured("synthetic-hold", "private", 1048576, true).await;
    for provider in ["capture", "capture_alias"] {
        let submitted = f
            .submit(&format!("{provider} start > private_{provider}"))
            .await;
        assert!(
            f.session
                .snapshot()
                .await
                .unwrap()
                .execution
                .errors
                .contains_key(&submitted.nodes[0])
        );
        assert_eq!(f.attempts(), 0);
    }
    let path = f.root.path().join("env.yaml");
    std::fs::write(
        &path,
        std::fs::read_to_string(&path)
            .unwrap()
            .replace("output: private", "output: confidential-temporary"),
    )
    .unwrap();
    let plan = f
        .session
        .plan_environment_file("env.yaml".into(), false)
        .await
        .unwrap();
    f.session.apply_environments(plan).await.unwrap();
    f.submit(":env use \"lab\"").await;
    f.submit("capture_alias start > live").await;
    assert_eq!(f.attempts(), 1);
    f.submit(":cancel $live").await;
    f.joined().await;
    f.shutdown().await;
}

#[tokio::test]
async fn local_capture_limits_and_malformed_input_preserve_only_original_honest_prefixes() {
    for (mode, maximum, bytes, phase, index) in [
        (
            "synthetic-normal",
            1048576,
            40 + PAYLOAD.len(),
            "ended",
            true,
        ),
        ("synthetic-normal", 43, 43, "incomplete", false),
        (
            "synthetic-partial-packet",
            1048576,
            39 + PAYLOAD.len(),
            "incomplete",
            false,
        ),
        (
            "synthetic-malformed-packet",
            1048576,
            40,
            "incomplete",
            false,
        ),
        ("synthetic-invalid-header", 1048576, 24, "opening", false),
        (
            "synthetic-failed",
            1048576,
            40 + PAYLOAD.len(),
            "incomplete",
            true,
        ),
    ] {
        let f = Fixture::new(mode, "confidential", maximum).await;
        f.submit("capture start profile:$profile > live").await;
        f.joined().await;
        f.submit(":source result $live > evidence").await;
        assert_eq!(
            f.field("acceptedBytes", "extent").await,
            Data::Text(bytes.to_string().into()),
            "{mode}"
        );
        // Protocol completion and the backend/container summary are different facts.
        assert_eq!(
            f.field("complete", "complete").await,
            Data::Bool(mode != "synthetic-invalid-header")
        );
        assert_eq!(
            f.field("value.summary.phase", "phase").await,
            Data::Text(phase.into()),
            "{mode}"
        );
        assert_eq!(
            matches!(f.field("value.index", "index").await, Data::Option(Some(_))),
            index,
            "{mode}"
        );
        assert_eq!(f.attempts(), 1);
        f.shutdown().await;
    }
}

#[tokio::test]
async fn private_capture_refuses_before_even_launching_the_backend() {
    let f = Fixture::new("synthetic-normal", "private", 1048576).await;
    f.submit("capture start profile:$profile > live").await;
    f.joined().await;
    let state = f.session.snapshot().await.unwrap();
    assert!(
        state
            .execution
            .errors
            .contains_key(&state.names["live"].node)
    );
    assert_eq!(f.attempts(), 0);
    f.shutdown().await;
}

#[tokio::test]
async fn unclosed_backend_child_makes_stop_unknown_and_native_owner_joins_without_replay() {
    let f = Fixture::new("synthetic-child-pipe", "confidential", 1048576).await;
    f.submit("capture start profile:$profile > live").await;
    tokio::time::timeout(Duration::from_secs(10), async {
        while !f.root.path().join("backend.child.pid").exists() {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap();
    f.submit(":source bind $live > controls").await;
    f.submit(":calc pure { return {operation:\"stop\"}; } > stop_action")
        .await;
    let stop = f
        .submit(":source send $controls action:$stop_action > stopped")
        .await;
    f.joined().await;
    let state = f.session.snapshot().await.unwrap();
    assert_eq!(state.execution.errors[&stop.nodes[0]].code(), "ENV036");
    f.submit(":source result $live > evidence").await;
    assert_eq!(f.field("complete", "complete").await, Data::Bool(false));
    assert_eq!(
        f.field("termination", "termination").await,
        // The peer explicitly reported an unknown control and a capture failure. The
        // recording termination describes that failure, not a fictitious stop receipt.
        Data::Text("failed".into())
    );
    let child = std::fs::read_to_string(f.root.path().join("backend.child.pid")).unwrap();
    let status = wes_adapters::process::serialized_spawn(|| {
        std::process::Command::new("ps")
            .args(["-p", child.trim(), "-o", "stat="])
            .output()
    })
    .unwrap();
    let stat = String::from_utf8(status.stdout).unwrap();
    assert!(
        stat.trim().is_empty() || stat.trim().starts_with('Z'),
        "still running after native join: {stat}"
    );
    assert_eq!(f.attempts(), 1);
    f.shutdown().await;
}

#[tokio::test]
async fn large_live_raw_exceeds_finite_process_output_while_index_and_reads_stay_bounded() {
    let size = 24 + 513 * (16 + 65535);
    assert!(size > wes_budgets::get("process.output.bytes"));
    let lifetime = Duration::from_secs(120);
    let f = Fixture::timed_configuration(
        "synthetic-large",
        "confidential",
        50 * 1024 * 1024,
        false,
        false,
        lifetime,
    )
    .await;
    f.submit("capture start profile:$profile > live").await;
    // 513 maximum-size packets minimally exceed both the finite output and index caps.
    // This debug-build case writes over 32 MiB through authenticated physical appends.
    // Give this workload the helper's existing 120-second profile ceiling, and bound
    // the join by that actual declared capture lifetime plus physical cleanup.
    // Full size, packet count and protocol success below still reject a partial capture.
    f.joined_within(lifetime + Duration::from_secs(5)).await;
    f.submit(":source result $live > evidence").await;
    assert_eq!(f.field("complete", "complete").await, Data::Bool(true));
    assert_eq!(
        f.field("value.reason", "reason").await,
        Data::Text("ended".into())
    );
    assert_eq!(
        f.field("value.summary.phase", "phase").await,
        Data::Text("ended".into())
    );
    assert_eq!(
        f.field("acceptedBytes", "extent").await,
        Data::Text(size.to_string().into())
    );
    let Data::Option(Some(index)) = f.field("value.index", "index").await else {
        panic!("bounded index")
    };
    let Data::Record(index) = index.as_ref() else {
        panic!("index")
    };
    assert_eq!(index["packetCount"], Data::Text("513".into()));
    assert_eq!(index["indexComplete"], Data::Bool(false));
    assert!(matches!(&index["packets"],Data::List(rows) if rows.len()==512));
    let Data::Record(raw) = f
        .value(&format!(
            ":read bytes:$evidence from:\"40\" limit:{} select:\"value.raw\" > bytes",
            PAYLOAD.len()
        ))
        .await
    else {
        panic!("byte range")
    };
    assert_eq!(raw["data"], Data::Bytes(PAYLOAD.to_vec().into()));
    assert_eq!(f.attempts(), 1);
    f.shutdown().await;
}

#[tokio::test]
async fn public_capture_start_stop_and_read_share_the_original_owner_and_enforce_native_controls() {
    let f = Fixture::capture("confidential-temporary").await;
    f.submit(":trace start capture:\"lab-tcp\" duration:\"20s\" packets:10 bytes:4096 filter:\"dst port 4242\" > live").await;
    let state = f.session.snapshot().await.unwrap();
    let node = state.names["live"].node.clone();
    let run = state.execution.runs[&node].clone();
    assert!(
        state.execution.streaming.contains(&node),
        "{:?}",
        state.execution.errors
    );
    let refused = f.submit(":trace read $live > too_early").await;
    assert!(
        f.session
            .snapshot()
            .await
            .unwrap()
            .execution
            .errors
            .contains_key(&refused.nodes[0])
    );
    let hostile = f
        .session
        .submit(
            SourceInput::new("agent-stop".into(), ":trace stop $live > forbidden".into())
                .unwrap()
                .cooperative(),
        )
        .await
        .unwrap();
    f.session.wait_idle().await.unwrap();
    let state = f.session.snapshot().await.unwrap();
    assert!(
        hostile
            .diagnostics
            .diagnostics
            .iter()
            .any(|d| d.severity == wes_language::Severity::Error)
            || hostile
                .nodes
                .iter()
                .any(|n| state.execution.errors.contains_key(n)),
        "{:?}",
        state.execution.errors
    );
    assert!(state.execution.streaming.contains(&node));
    assert_eq!(f.attempts(), 1);
    f.submit(":trace stop $live > stopped").await;
    assert_eq!(
        f.value(":calc pure { return $stopped.stopped; } > stop_confirmed")
            .await,
        Data::Bool(true)
    );
    f.joined().await;
    f.submit(":trace read $live > evidence").await;
    assert_eq!(f.field("complete", "complete").await, Data::Bool(true));
    assert_eq!(
        f.field("run", "original_run").await,
        Data::Text(run.to_string().into())
    );
    assert_eq!(
        f.field("acceptedBytes", "accepted").await,
        Data::Text("24".into())
    );
    assert_eq!(
        f.field("value.profile.durationMs", "duration").await,
        Data::Int(20000)
    );
    let copy = f.submit(":trace read $evidence > inert_copy").await;
    assert!(
        f.session
            .snapshot()
            .await
            .unwrap()
            .execution
            .errors
            .contains_key(&copy.nodes[0])
    );
    assert_eq!(f.attempts(), 1);
    f.shutdown().await;
}

#[tokio::test]
async fn named_capture_annotation_keeps_the_shell_result_and_raw_evidence_on_one_run() {
    let f = Fixture::capture("confidential-temporary").await;
    let mut notices = f.session.subscribe_notices().unwrap();
    f.submit(
        "@trace(capture:\"lab-tcp\",tail:\"1s\") sh run cmd:\"printf synthetic-business\" > live",
    )
    .await;
    let state = f.session.snapshot().await.unwrap();
    let node = state.names["live"].node.clone();
    let run = state.execution.runs[&node].clone();
    let value = state
        .execution
        .values
        .get(&node)
        .unwrap_or_else(|| panic!("{:?}", state.execution.errors));
    let Data::Record(fields) = value.data() else {
        panic!("ordinary ProcessOutput")
    };
    assert_eq!(
        fields["stdout"],
        Data::Bytes(b"synthetic-business".to_vec().into())
    );
    assert!(!value.provenance().policy().is_confidential());
    assert!(
        state.execution.executing.contains(&node),
        "business result precedes the bounded tail"
    );
    f.joined().await;
    f.submit(":trace read $live > evidence").await;
    let notices: Vec<_> = std::iter::from_fn(|| notices.try_recv().ok()).collect();
    assert!(notices.is_empty(), "{notices:?}");
    assert_eq!(f.field("complete", "complete").await, Data::Bool(true));
    assert_eq!(
        f.field("run", "run").await,
        Data::Text(run.to_string().into())
    );
    assert_eq!(
        f.field("acceptedBytes", "accepted").await,
        Data::Text("24".into())
    );
    assert_eq!(
        f.field("value.reason", "reason").await,
        Data::Text("stopped".into()),
        "{notices:?}"
    );
    let Data::Record(bytes) = f
        .value(":read bytes:$evidence from:\"0\" limit:24 select:\"value.raw\" > original_header")
        .await
    else {
        panic!()
    };
    assert!(matches!(&bytes["data"],Data::Bytes(data) if data.len()==24));
    assert_eq!(f.attempts(), 1);
    f.shutdown().await;
}

#[tokio::test]
async fn default_capture_annotation_is_target_selected_and_invalid_selection_has_zero_entry() {
    let f = Fixture::capture("confidential-temporary").await;
    for text in [
        "@trace(capture:\"missing\") sh run cmd:\"exit 99\"",
        "@trace(capture:\"lab-tcp\",tail:\"6s\") sh run cmd:\"exit 99\"",
        "@trace(http) sh run cmd:\"exit 99\"",
        "@trace(capture:\"lab-tcp\") :calc 1",
    ] {
        let reply = f
            .session
            .submit(SourceInput::new(uuid::Uuid::new_v4().to_string(), text.into()).unwrap())
            .await
            .unwrap();
        assert!(
            reply
                .diagnostics
                .diagnostics
                .iter()
                .any(|d| d.severity == wes_language::Severity::Error),
            "{text}: {:?}",
            reply.diagnostics
        );
        assert_eq!(f.attempts(), 0);
    }
    f.submit("@trace(tcp) sh run cmd:\"printf default-business\" > live")
        .await;
    f.joined().await;
    f.submit(":trace read $live > evidence").await;
    assert_eq!(
        f.field("acceptedBytes", "accepted").await,
        Data::Text("24".into())
    );
    assert_eq!(f.attempts(), 1);
    f.shutdown().await;
    let ambiguous =
        Fixture::configured("synthetic-idle", "confidential-temporary", 1048576, true).await;
    let reply = ambiguous
        .session
        .submit(
            SourceInput::new(
                "ambiguous".into(),
                ":trace start capture:\"lab-tcp\" > ambiguous".into(),
            )
            .unwrap(),
        )
        .await
        .unwrap();
    assert!(
        reply
            .diagnostics
            .diagnostics
            .iter()
            .any(|d| d.severity == wes_language::Severity::Error)
    );
    assert_eq!(ambiguous.attempts(), 0);
    ambiguous.shutdown().await;
}

#[tokio::test]
async fn public_capture_private_raw_refusal_precedes_shell_entry_and_never_downgrades_policy() {
    let f = Fixture::capture("private").await;
    let path = f.root.path().join("business-entered");
    let cmd = format!("printf entered > '{}'", path.display());
    f.submit(&format!(
        "@trace(capture:\"lab-tcp\") sh run cmd:{} > live",
        serde_json::to_string(&cmd).unwrap()
    ))
    .await;
    f.joined().await;
    let state = f.session.snapshot().await.unwrap();
    let node = &state.names["live"].node;
    assert!(state.execution.errors[node].policy().is_private());
    assert!(!path.exists());
    assert_eq!(f.attempts(), 0);
    f.shutdown().await;
}
