#![cfg(unix)]
use std::sync::Arc;
use wes_adapters::{
    codec::{self, Limits},
    environments::LocalEnvironments,
    journal::{Durability, FileHistory, ReadLimits},
    storage::TieredValues,
    type_sources::FileTypeSources,
};
use wes_core::{Data, Provenance, Shape, Value, flow::FlowPolicy};
use wes_engine::{
    calls::{CallJournal, RequiredPersistence},
    driver::CancellationToken,
    recording::{Recorder, RecorderLimits, RecorderTask, spawn_recorder},
    session::{
        self, EnvironmentAuthorityCommand, RecordingMode, SessionHandle, SessionStorage,
        SessionTask,
    },
    source::SourceInput,
    storage::{AutoKeep, StoreWorker, StoreWorkerLimits, StoreWorkerTask, spawn_store},
    workspace::Workspace,
};

struct Fixture {
    root: tempfile::TempDir,
    handle: SessionHandle,
    task: SessionTask,
    recorder: Recorder,
    writer: RecorderTask,
    store: StoreWorker,
    storage: StoreWorkerTask,
}
impl Fixture {
    fn new(yaml: &str) -> Self {
        use std::os::unix::fs::PermissionsExt;
        let root = tempfile::Builder::new()
            .permissions(std::fs::Permissions::from_mode(0o700))
            .tempdir()
            .unwrap();
        std::fs::write(root.path().join("env.yaml"), yaml).unwrap();
        let loader = Arc::new(LocalEnvironments::new(root.path()).unwrap());
        let values = TieredValues::open(
            &root.path().join("live"),
            &root.path().join("archive"),
            Limits::default(),
            Durability::File,
            None,
        )
        .unwrap();
        let (store, storage) = spawn_store(values, StoreWorkerLimits::default()).unwrap();
        let history = FileHistory::open(
            &root.path().join("history"),
            ReadLimits::default(),
            Durability::File,
        )
        .unwrap();
        let (recorder, writer) = spawn_recorder(history, RecorderLimits::default()).unwrap();
        let (handle, task) = session::spawn_with_storage(
            Workspace::local(wes_engine::providers::LocalScope::new("fixture").unwrap())
                .with_environment_loader(loader)
                .with_calculation_services(Arc::new(codec::CalculationServices)),
            RecordingMode::Required(CallJournal::new(
                recorder.clone(),
                RequiredPersistence::FileSynced,
            )),
            Arc::new(FileTypeSources::new(root.path()).unwrap()),
            2.try_into().unwrap(),
            SessionStorage {
                worker: store.clone(),
                auto_keep: AutoKeep::default(),
            },
        )
        .unwrap();
        Self {
            root,
            handle,
            task,
            recorder,
            writer,
            store,
            storage,
        }
    }
    async fn install(&self) {
        let plan = self
            .handle
            .plan_environment_file("env.yaml".into(), false)
            .await
            .unwrap();
        self.handle.apply_environments(plan).await.unwrap();
    }
    async fn submit(&self, client: &str, text: &str) -> Arc<session::SubmissionResult> {
        let reply = self
            .handle
            .submit(
                SourceInput::new(uuid::Uuid::new_v4().to_string(), text.into())
                    .unwrap()
                    .with_client(client.into())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert!(
            reply
                .diagnostics
                .diagnostics
                .iter()
                .all(|d| d.severity != wes_language::Severity::Error),
            "{text}: {:?}",
            reply.diagnostics
        );
        self.handle.wait_idle().await.unwrap();
        reply
    }
    async fn value(&self, name: &str) -> Value {
        let snapshot = self.handle.snapshot().await.unwrap();
        let node = &snapshot.names[name].node;
        snapshot
            .execution
            .values
            .get(node)
            .unwrap_or_else(|| panic!("{name}: {:?}", snapshot.execution.errors))
            .clone()
    }
    async fn stop(self) {
        self.handle.shutdown().await.unwrap();
        self.task.join().await.unwrap();
        self.recorder.shutdown().await.unwrap();
        self.writer.join().await.unwrap();
        self.store.shutdown().await.unwrap();
        self.storage.join().await.unwrap();
    }
}
fn simple() -> &'static str {
    "version: 1\ntargets: {local: {kind: local, cwd: /, env: {QA_ONLY: declared}}}\nenvironments:\n  base: {imports: {echo: {source: {kind: process, bin: /bin/echo}, bind: {target: local}}}}\n  dev: {extends: {env: base, track: latest}}\n  prod: {extends: {env: base, track: latest}}\n"
}
fn stdout(value: &Value) -> &[u8] {
    let Data::Record(fields) = value.data() else {
        panic!("record")
    };
    let Data::Bytes(bytes) = &fields["stdout"] else {
        panic!("bytes")
    };
    bytes
}

#[tokio::test]
async fn retirement_preserves_old_work_delete_checks_references_and_never_restores_fallback() {
    let f = Fixture::new(
        "version: 1\ntargets: {local: {kind: local}}\nenvironments: {dev: {imports: {echo: {source: {kind: process, bin: /bin/echo}, bind: {target: local}}}}}",
    );
    f.install().await;
    f.submit("one", ":env use \"dev\"").await;
    f.submit("one", "echo run args:retained > old").await;
    f.submit("one", ":env retire \"dev\"").await;
    let failed = f
        .handle
        .submit(
            SourceInput::new("new-retired".into(), "echo run args:new".into())
                .unwrap()
                .with_client("one".into())
                .unwrap(),
        )
        .await
        .unwrap();
    assert!(failed.nodes.is_empty());
    f.submit("one", ":refresh $old").await;
    assert_eq!(stdout(&f.value("old").await), b"retained\n");
    let failed = f
        .handle
        .submit(
            SourceInput::new("delete-referenced".into(), ":env delete \"dev\"".into())
                .unwrap()
                .with_client("one".into())
                .unwrap(),
        )
        .await
        .unwrap_err();
    assert!(failed.to_string().contains("ENV025"));
    f.submit("one", ":node remove $old scope:downstream").await;
    f.submit("one", ":env delete \"dev\"").await;
    assert!(f.handle.environment_revisions().await.unwrap().is_empty());
    let failed = f
        .handle
        .submit(
            SourceInput::new(
                "no-fallback".into(),
                ":import process bin:/bin/echo as:global".into(),
            )
            .unwrap()
            .with_client("new-pane".into())
            .unwrap(),
        )
        .await
        .unwrap_err();
    assert!(failed.to_string().contains("ENV011"));
    f.stop().await;
}

#[tokio::test]
async fn managed_http_uses_bound_endpoint_and_scoped_secret_without_import_io() {
    use tokio::{
        io::{AsyncReadExt, AsyncWriteExt},
        net::TcpListener,
    };
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let endpoint = format!("http://{}", listener.local_addr().unwrap());
    let yaml = format!(
        "version: 1\ntargets: {{local: {{kind: local}}}}\nenvironments: {{dev: {{parameters: {{endpoint: {{type: Text}}}}, config: {{endpoint: '{endpoint}'}}, secretSlots: {{token: {{required: true}}}}, secretRefs: {{token: qa/http}}, imports: {{api: {{source: {{kind: spec, file: api.json}}, bind: {{target: local, endpoint: {{config: endpoint}}, output: private, credentials: {{token: {{secret: token}}}}}}}}}}}}}}"
    );
    let f = Fixture::new(&yaml);
    std::fs::write(f.root.path().join("api.json"), serde_json::json!({"version":1,"provider":"api","types":{},"operations":[{"path":["get"],"method":"GET","route":"/fixture","auth":[{"secret":"token"}],"parameters":[],"responses":{"200":"Text"}}]}).to_string()).unwrap();
    f.install().await;
    f.submit("one", ":env use \"dev\"").await;
    f.submit("one", "api get > denied_http").await;
    assert!(
        f.handle
            .snapshot()
            .await
            .unwrap()
            .execution
            .errors
            .values()
            .any(|e| e.policy().is_private())
    );
    let revision = f.handle.environment_revisions().await.unwrap()["dev"];
    f.handle
        .environment_authority(EnvironmentAuthorityCommand::Supply {
            reference: "qa/http".into(),
            value: Arc::new(wes_engine::credentials::SecretString::from(
                "qa-scoped-http-material",
            )),
        })
        .await
        .unwrap();
    f.handle
        .environment_authority(EnvironmentAuthorityCommand::Grant {
            environment: "dev".into(),
            revision,
            provider: "api".into(),
            seconds: 60,
        })
        .await
        .unwrap();
    let serving = tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.unwrap();
        let mut request = vec![];
        while !request.ends_with(b"\r\n\r\n") {
            let mut byte = [0];
            stream.read_exact(&mut byte).await.unwrap();
            request.push(byte[0]);
            assert!(request.len() < 65536);
        }
        let request = String::from_utf8(request).unwrap();
        assert!(request.starts_with("GET /fixture "));
        assert!(
            request
                .to_lowercase()
                .contains("authorization: bearer qa-scoped-http-material")
        );
        stream.write_all(b"HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: 4\r\nConnection: close\r\n\r\n\"ok\"").await.unwrap();
    });
    f.submit("one", "api get > received_http").await;
    let response = f.value("received_http").await;
    let Data::Record(fields) = response.data() else {
        panic!()
    };
    assert_eq!(fields["body"], Data::Text("ok".into()));
    assert!(
        f.value("received_http")
            .await
            .provenance()
            .policy()
            .is_private()
    );
    tokio::time::timeout(std::time::Duration::from_secs(10), serving)
        .await
        .unwrap()
        .unwrap();
    f.stop().await;
}

#[tokio::test]
async fn process_secret_delivery_is_scoped_rotatable_and_absent_from_disk_artifacts() {
    let f = Fixture::new(
        "version: 1\ntargets: {local: {kind: local, cwd: /, env: {QA_ONLY: declared}}}\nenvironments:\n  dev: {secretSlots: {token: {required: true}}, secretRefs: {token: qa/key}, imports: {vars: {source: {kind: process, bin: /usr/bin/printenv}, bind: {target: local, output: private, credentials: {QA_SECRET: {secret: token}}}}}}\n",
    );
    f.install().await;
    f.submit("one", ":env use \"dev\"").await;
    f.handle
        .environment_authority(EnvironmentAuthorityCommand::Supply {
            reference: "qa/key".into(),
            value: Arc::new(wes_engine::credentials::SecretString::from(
                "qa-engine-supplied-secret-one",
            )),
        })
        .await
        .unwrap();
    f.submit("one", "vars run args:QA_SECRET > denied").await;
    let snapshot = f.handle.snapshot().await.unwrap();
    assert!(
        snapshot
            .execution
            .errors
            .contains_key(&snapshot.names["denied"].node)
    );
    let revision = f.handle.environment_revisions().await.unwrap()["dev"];
    f.handle
        .environment_authority(EnvironmentAuthorityCommand::Grant {
            environment: "dev".into(),
            revision,
            provider: "vars".into(),
            seconds: 60,
        })
        .await
        .unwrap();
    f.submit("one", "vars run args:QA_SECRET > first").await;
    assert_eq!(
        stdout(&f.value("first").await),
        b"qa-engine-supplied-secret-one\n"
    );
    f.handle
        .environment_authority(EnvironmentAuthorityCommand::Supply {
            reference: "qa/key".into(),
            value: Arc::new(wes_engine::credentials::SecretString::from(
                "qa-engine-supplied-secret-two",
            )),
        })
        .await
        .unwrap();
    f.submit("one", "vars run args:QA_SECRET > second").await;
    assert_eq!(
        stdout(&f.value("second").await),
        b"qa-engine-supplied-secret-two\n"
    );
    f.submit("one", "vars run args:HOME > ambient").await;
    assert!(stdout(&f.value("ambient").await).is_empty());
    f.handle
        .environment_authority(EnvironmentAuthorityCommand::Revoke {
            environment: "dev".into(),
            revision,
            provider: "vars".into(),
        })
        .await
        .unwrap();
    f.submit("one", ":refresh $second").await;
    let snapshot = f.handle.snapshot().await.unwrap();
    assert!(
        snapshot
            .execution
            .errors
            .contains_key(&snapshot.names["second"].node)
    );
    let checkpoint = f.handle.checkpoint().await.unwrap();
    drop(checkpoint);
    fn scan(path: &std::path::Path) {
        for entry in std::fs::read_dir(path).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                scan(&path);
            } else {
                let bytes = std::fs::read(path).unwrap();
                let text = String::from_utf8_lossy(&bytes);
                assert!(!text.contains("qa-engine-supplied-secret-one"));
                assert!(!text.contains("qa-engine-supplied-secret-two"));
            }
        }
    }
    scan(f.root.path());
    f.stop().await;
}

#[tokio::test]
async fn separate_packages_preserve_each_others_definitions_and_reject_namespace_theft() {
    let f = Fixture::new(&format!("package: first\n{}", simple()));
    f.install().await;
    std::fs::write(f.root.path().join("second.yaml"), "version: 1\npackage: second\nenvironments: {staging: {extends: {env: base, track: latest}}}").unwrap();
    let plan = f
        .handle
        .plan_environment_file("second.yaml".into(), false)
        .await
        .unwrap();
    f.handle.apply_environments(plan).await.unwrap();
    assert_eq!(f.handle.environment_revisions().await.unwrap().len(), 4);
    std::fs::write(
        f.root.path().join("second.yaml"),
        "version: 1\npackage: second\nenvironments: {dev: {abstract: true}}",
    )
    .unwrap();
    assert!(
        f.handle
            .plan_environment_file("second.yaml".into(), false)
            .await
            .is_err()
    );
    assert_eq!(f.handle.environment_revisions().await.unwrap().len(), 4);
    f.stop().await;
}

#[tokio::test]
async fn panes_explicit_selectors_updates_and_refresh_keep_captured_bindings() {
    let f = Fixture::new(simple());
    f.install().await;
    f.submit("one", ":env use \"dev\"").await;
    f.submit("two", ":env use \"prod\"").await;
    let (a, b) = tokio::join!(
        f.submit("one", "echo run args:dev > a"),
        f.submit("two", "echo run args:prod > b")
    );
    assert_eq!(a.nodes.len(), 1);
    assert_eq!(b.nodes.len(), 1);
    assert_eq!(stdout(&f.value("a").await), b"dev\n");
    assert_eq!(stdout(&f.value("b").await), b"prod\n");
    let before = f.value("a").await.provenance().policy().clone();
    f.submit("one", "@env{prod} echo run args:explicit > explicit")
        .await;
    assert_ne!(
        f.value("explicit").await.provenance().policy().origins(),
        before.origins()
    );
    std::fs::write(
        f.root.path().join("env.yaml"),
        simple().replace("declared", "changed"),
    )
    .unwrap();
    f.install().await;
    let rejected = f
        .handle
        .submit(
            SourceInput::new("stale".into(), "echo run args:no".into())
                .unwrap()
                .with_client("one".into())
                .unwrap(),
        )
        .await
        .unwrap();
    assert!(rejected.nodes.is_empty());
    assert!(
        rejected
            .diagnostics
            .diagnostics
            .iter()
            .any(|d| d.code == "ENV008")
    );
    let refused = f
        .handle
        .submit(
            SourceInput::new("changed-refresh".into(), ":refresh $a".into())
                .unwrap()
                .with_client("one".into())
                .unwrap(),
        )
        .await
        .unwrap();
    assert!(
        refused
            .diagnostics
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.code == "ENV039")
    );
    assert_eq!(f.value("a").await.provenance().policy(), &before);
    f.submit("one", ":env clear").await;
    f.submit("one", ":calc { return 1+2; } > pure").await;
    assert_eq!(f.value("pure").await.data(), &Data::Int(3));
    f.stop().await;
}

#[tokio::test]
async fn inherited_ad_hoc_imports_are_atomic_and_file_drift_requires_reconciliation() {
    let f = Fixture::new(simple());
    f.install().await;
    f.submit("one", ":env use \"base\"").await;
    f.submit("one", ":import process bin:/bin/echo as:second")
        .await;
    let state = f.handle.observe().await.unwrap();
    assert!(
        state.environment_catalogues["dev"]
            .provider("second")
            .is_some()
    );
    assert!(
        f.handle
            .plan_environment_file("env.yaml".into(), false)
            .await
            .is_err()
    );
    let duplicate = f
        .handle
        .submit(
            SourceInput::new(
                "duplicate".into(),
                ":import process bin:/bin/echo as:second env:base".into(),
            )
            .unwrap(),
        )
        .await;
    assert!(duplicate.is_err());
    let plan = f
        .handle
        .plan_environment_file("env.yaml".into(), true)
        .await
        .unwrap();
    f.handle.apply_environments(plan).await.unwrap();
    assert!(
        f.handle.observe().await.unwrap().environment_catalogues["dev"]
            .provider("second")
            .is_none()
    );
    f.stop().await;
}

#[tokio::test]
async fn managed_spec_url_import_and_package_capture_preserve_offline_lock_evidence() {
    use tokio::{
        io::{AsyncReadExt, AsyncWriteExt},
        net::TcpListener,
    };
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}/spec.json", listener.local_addr().unwrap());
    let body = include_str!("../../../tests/fixtures/catalog.provider.json");
    let serving = tokio::spawn(async move {
        // One command import and one independent environment package capture.
        for _ in 0..2 {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut request = Vec::new();
            while !request.ends_with(b"\r\n\r\n") {
                request.push(socket.read_u8().await.unwrap());
            }
            socket
                .write_all(
                    format!(
                        "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                        body.len()
                    )
                    .as_bytes(),
                )
                .await
                .unwrap();
        }
    });
    let f = Fixture::new("version: 1\ntargets: {local: {kind: local}}\nenvironments: {dev: {}}\n");
    f.install().await;
    f.submit("one", ":env use \"dev\"").await;
    f.submit(
        "one",
        &format!(":import spec url:{url:?} as:remote endpoint:https://example.invalid/api"),
    )
    .await;
    assert!(
        f.handle.observe().await.unwrap().environment_catalogues["dev"]
            .provider("remote")
            .is_some()
    );
    for command in [
        format!(":import spec url:{url:?} file:missing.json as:bad env:dev"),
        format!(":import process url:{url:?} bin:/bin/echo as:bad env:dev"),
        ":import spec url:'file:///missing.json' as:bad env:dev".into(),
    ] {
        assert!(
            f.handle
                .submit(SourceInput::new(uuid::Uuid::new_v4().to_string(), command).unwrap())
                .await
                .is_err()
        );
    }
    let root = tempfile::tempdir().unwrap();
    let yaml = format!(
        "version: 1\ntargets: {{local: {{kind: local}}}}\nenvironments: {{dev: {{imports: {{api: {{source: {{kind: spec, url: '{url}'}}, bind: {{target: local, endpoint: 'https://example.invalid/api'}}}}}}}}}}\n"
    );
    std::fs::write(root.path().join("env.yaml"), yaml).unwrap();
    let loaded = tokio::task::spawn_blocking(move || {
        use wes_engine::environments::EnvironmentLoader;
        LocalEnvironments::new(root.path())
            .unwrap()
            .capture("env.yaml")
            .unwrap()
    })
    .await
    .unwrap();
    let package = wes_core::environments::Package::parse(&loaded.yaml).unwrap();
    let key = package.required_sources().into_iter().next().unwrap();
    assert_eq!(key.source_field(), "url");
    assert_eq!(key.location(), url);
    assert_eq!(loaded.sources.get(&key).unwrap().bytes(), body);
    serving.await.unwrap(); // Export and reconstruction cannot download again.
    f.submit("one", ":env export file:captured.lock.json").await;
    let loader = LocalEnvironments::new(f.root.path()).unwrap();
    let records = loader.read_lock("captured.lock.json").unwrap();
    assert_eq!(records.len(), 2); // Initial environment and imported revision.
    let imported = records.last().unwrap();
    let source = imported.sources().get(&key).unwrap();
    assert_eq!(source.bytes(), body);
    assert!(imported.yaml().contains("\"url\""));
    f.stop().await;
}

#[tokio::test]
async fn protected_descendants_block_short_edits_and_rename_keeps_runtime_identity() {
    let f = Fixture::new(&simple().replace("prod: {extends", "prod: {protected: true, extends"));
    f.install().await;
    let old = f.handle.environment_revisions().await.unwrap();
    let denied = f
        .handle
        .submit(
            SourceInput::new(
                "guard".into(),
                ":import process bin:/bin/echo as:second env:base".into(),
            )
            .unwrap(),
        )
        .await;
    assert!(denied.is_err());
    assert_eq!(f.handle.environment_revisions().await.unwrap(), old);
    f.submit("one", ":env use \"dev\"").await;
    f.submit("one", "echo run args:old > existing").await;
    f.submit("one", ":env disable \"dev\"").await;
    f.submit("one", ":env rename \"dev\" to:development").await;
    assert!(!f.handle.observe().await.unwrap().environment_enabled["development"]);
    f.submit("one", ":refresh $existing").await;
    let state = f.handle.snapshot().await.unwrap();
    assert!(
        state
            .execution
            .errors
            .contains_key(&state.names["existing"].node)
    );
    f.submit("one", ":env enable \"development\"").await;
    f.submit("one", ":refresh $existing").await;
    assert_eq!(stdout(&f.value("existing").await), b"old\n");
    f.stop().await;
}

#[tokio::test]
async fn private_outputs_derived_values_and_errors_use_memory_not_retained_files() {
    let f = Fixture::new(&simple().replace(
        "bind: {target: local}",
        "bind: {target: local, output: private}",
    ));
    f.install().await;
    f.submit("one", ":env use \"dev\"").await;
    f.submit("one", "echo run args:qa-private-sentinel > secret_output")
        .await;
    f.submit("one", ":calc { return $secret_output; } > derived")
        .await;
    let value = f.value("derived").await;
    assert!(value.provenance().policy().is_private());
    assert!(codec::encode_value(&value, Limits::default()).is_err());
    let observation = f.handle.observe().await.unwrap();
    let node = &observation.state.names["derived"].node;
    let handle = observation.values.as_ref().unwrap().outputs[node]
        .handle()
        .unwrap()
        .clone();
    assert!(f.store.encoded(handle.clone()).await.is_err());
    assert!(f.store.retain(handle.clone()).await.is_err());
    assert_eq!(
        f.store.read(handle.clone()).await.unwrap().unwrap().value,
        value
    );
    let checkpoint = f.handle.checkpoint().await.unwrap();
    assert!(
        !checkpoint.history().journal().iter().any(
            |j| matches!(j, wes_engine::history::JournalEntry::Result(r) if r.handle == handle)
        )
    );
    drop(checkpoint);
    f.handle.shutdown().await.unwrap();
    f.task.join().await.unwrap();
    assert!(f.store.read(handle).await.unwrap().is_none());
    f.recorder.shutdown().await.unwrap();
    f.writer.join().await.unwrap();
    f.store.shutdown().await.unwrap();
    f.storage.join().await.unwrap();
}

#[tokio::test]
async fn transfer_gate_cannot_be_bypassed_by_calc_literal_arguments() {
    let f = Fixture::new(simple());
    f.install().await;
    f.submit("one", ":env use \"dev\"").await;
    f.submit(
        "one",
        ":calc { return call('echo',['run'],{args:'from-dev'}); } > source",
    )
    .await;
    f.submit("one", ":env use \"prod\"").await;
    f.submit("one", ":calc { let x=$source; return call('echo',['run'],{args:'even-control-dependence'}); } > refused").await;
    let snapshot = f.handle.snapshot().await.unwrap();
    assert!(
        snapshot
            .execution
            .errors
            .contains_key(&snapshot.names["refused"].node)
    );
    let revision = f.handle.environment_revisions().await.unwrap()["prod"];
    f.handle
        .environment_authority(EnvironmentAuthorityCommand::Transfer {
            origin: "dev".into(),
            destination: "prod".into(),
            revision,
            seconds: 60,
        })
        .await
        .unwrap();
    f.submit(
        "one",
        ":calc { let x=$source; return call('echo',['run'],{args:'allowed'}); } > allowed",
    )
    .await;
    assert_eq!(stdout(&f.value("allowed").await), b"allowed\n");
    assert_eq!(
        f.value("allowed")
            .await
            .provenance()
            .policy()
            .origins()
            .len(),
        2
    );
    f.stop().await;
}

#[tokio::test]
async fn export_is_captured_portable_evidence_and_replay_is_held_with_no_authority() {
    let f = Fixture::new(simple());
    f.install().await;
    f.submit("one", ":env use \"dev\"").await;
    f.submit("one", "echo run args:held > original").await;
    f.submit("one", ":env export file:captured.lock.json").await;
    let loader = LocalEnvironments::new(f.root.path()).unwrap();
    let records = loader.read_lock("captured.lock.json").unwrap();
    assert_eq!(records.len(), 1);
    let captured = f.handle.checkpoint().await.unwrap();
    let mut copy = wes_engine::history::HistoryCapture::new(Default::default());
    for entry in captured.history().journal() {
        copy.push(wes_engine::history::Record::Journal(entry.clone()))
            .unwrap();
    }
    for entry in captured.history().recovery() {
        copy.push(wes_engine::history::Record::Recovery(entry.clone()))
            .unwrap();
    }
    let image = copy.finish(captured.history().checkpoint());
    drop(captured);
    let restored = session::restore(
        Workspace::local(wes_engine::providers::LocalScope::new("fixture").unwrap())
            .with_environment_loader(Arc::new(LocalEnvironments::new(f.root.path()).unwrap())),
        RecordingMode::Ephemeral,
        None,
        image,
        CancellationToken::new(),
    )
    .await
    .unwrap();
    assert_eq!(
        restored.workspace().environments().revisions(),
        f.handle.environment_revisions().await.unwrap()
    );
    let (handle, task) = restored
        .spawn(
            Arc::new(FileTypeSources::new(f.root.path()).unwrap()),
            1.try_into().unwrap(),
        )
        .unwrap();
    handle.wait_idle().await.unwrap();
    assert!(
        handle.snapshot().await.unwrap().execution.runs.is_empty()
            || handle.snapshot().await.unwrap().execution.values.is_empty()
    );
    assert!(!handle.observe().await.unwrap().environment_enabled["dev"]);
    handle.shutdown().await.unwrap();
    task.join().await.unwrap();
    f.stop().await;
}

#[tokio::test]
async fn private_store_capacity_never_spills_and_retains_origin_metadata() {
    let f = Fixture::new(simple());
    let value = Value::new(
        Shape::Unknown,
        Data::Text("qa-memory-only".into()),
        Provenance::default().with_policy(&FlowPolicy::default().private()),
    )
    .unwrap();
    let mut handles = vec![];
    for _ in 0..1024 {
        handles.push(
            f.store
                .publish(value.clone(), AutoKeep::default().into())
                .await
                .unwrap()
                .handle,
        );
    }
    assert!(
        f.store
            .publish(value, AutoKeep::Never.into())
            .await
            .is_err()
    );
    for handle in handles {
        f.store.release(handle).await.unwrap();
    }
    let public = Value::new(
        Shape::Unknown,
        Data::Text("public".into()),
        Provenance::default().with_policy(&FlowPolicy::default().from_origin("qa-workspace/dev")),
    )
    .unwrap();
    let encoded = codec::encode_value(&public, Limits::default()).unwrap();
    assert_eq!(
        codec::decode_value(&encoded, Limits::default())
            .unwrap()
            .value,
        public
    );
    f.stop().await;
}

#[tokio::test]
async fn text_owned_plans_keep_unrelated_packages_and_require_explicit_source_reconciliation() {
    let f = Fixture::new(simple());
    f.install().await;
    f.submit("one", ":env use \"base\"").await;
    f.submit("one", ":import process bin:/bin/echo as:second")
        .await;
    let text = format!(
        ":env plan source:{} > fromText",
        wes_language::quote_text(simple())
    );
    assert!(
        f.handle
            .submit(SourceInput::new("drift".into(), text.clone()).unwrap())
            .await
            .is_err()
    );
    let other = "version: 1\npackage: unrelated\nenvironments: {external: {}}\n";
    f.submit(
        "one",
        &format!(
            ":env plan source:{} > other",
            wes_language::quote_text(other)
        ),
    )
    .await;
    f.submit("one", ":env apply $other").await;
    f.submit(
        "one",
        &format!(
            ":env plan source:{} reconcile:source > reconciled",
            wes_language::quote_text(simple())
        ),
    )
    .await;
    assert!(
        f.handle.observe().await.unwrap().environment_catalogues["dev"]
            .provider("second")
            .is_some()
    );
    f.submit("one", ":env apply $reconciled").await;
    let state = f.handle.observe().await.unwrap();
    assert!(
        state.environment_catalogues["dev"]
            .provider("second")
            .is_none()
    );
    assert!(state.environment_revisions.contains_key("external"));
    f.stop().await;
}

#[path = "managed_environments/docker_observation.rs"]
mod docker_observation;

#[path = "managed_environments/docker_logs.rs"]
mod docker_logs;

#[path = "managed_environments/docker_metrics.rs"]
mod docker_metrics;

#[path = "managed_environments/ssh.rs"]
mod ssh;

#[tokio::test]
async fn compose_targets_survive_definition_edits_and_held_recovery_without_daemon_io() {
    use wes_core::environments::{DockerDestination, TargetKind};
    let f = Fixture::new(
        "version: 1\ntargets: {app: {kind: docker, socket: /not/listening.sock, compose: {project: shop, service: api, replica: 2}, shell: /bin/bash, inherit: container, cwd: /app}}\nenvironments: {qa: {targets: [app]}}",
    );
    f.install().await;
    f.submit("one", ":env use \"qa\"").await;
    f.submit("one", ":import process bin:/bin/echo as:echo target:app")
        .await;
    f.submit("one", ":env rename \"qa\" to:renamed").await;
    let revision = f.handle.environment_revisions().await.unwrap()["renamed"];
    let lease = f
        .handle
        .capture_execution_target("renamed".into(), revision, "app".into())
        .await
        .unwrap();
    assert!(
        matches!(lease.target.kind(), TargetKind::Docker { destination: DockerDestination::Compose { project, service, replica: Some(2) }, shell: Some(shell), .. } if project == "shop" && service == "api" && shell == "/bin/bash")
    );
    let captured = f.handle.checkpoint().await.unwrap();
    let mut copy = wes_engine::history::HistoryCapture::new(Default::default());
    for entry in captured.history().journal() {
        copy.push(wes_engine::history::Record::Journal(entry.clone()))
            .unwrap();
    }
    for entry in captured.history().recovery() {
        copy.push(wes_engine::history::Record::Recovery(entry.clone()))
            .unwrap();
    }
    let image = copy.finish(captured.history().checkpoint());
    drop(captured);
    let restored = session::restore(
        Workspace::local(wes_engine::providers::LocalScope::new("fixture").unwrap())
            .with_environment_loader(Arc::new(LocalEnvironments::new(f.root.path()).unwrap())),
        RecordingMode::Ephemeral,
        None,
        image,
        CancellationToken::new(),
    )
    .await
    .unwrap();
    assert_eq!(
        restored.workspace().environments().revisions()["renamed"],
        revision
    );
    let (handle, task) = restored
        .spawn(
            Arc::new(FileTypeSources::new(f.root.path()).unwrap()),
            1.try_into().unwrap(),
        )
        .unwrap();
    handle.wait_idle().await.unwrap();
    assert!(!handle.observe().await.unwrap().environment_enabled["renamed"]);
    assert!(
        handle
            .capture_execution_target("renamed".into(), revision, "app".into())
            .await
            .is_err()
    );
    handle.shutdown().await.unwrap();
    task.join().await.unwrap();
    drop(lease);
    f.stop().await;
}

#[tokio::test]
async fn http_transport_example_uses_both_environments_and_survives_edit_roundtrip() {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let yaml = include_str!("../../../examples/http-transports/environments.yaml");
    let source = include_str!("../../../examples/http-transports/run.wes");
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let task = tokio::spawn(async move {
        for _ in 0..3 {
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut head = vec![];
            while !head.ends_with(b"\r\n\r\n") {
                head.push(stream.read_u8().await.unwrap());
            }
            assert!(head.starts_with(b"GET /health "));
            stream
                .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\nok")
                .await
                .unwrap();
        }
    });
    let f = Fixture::new(yaml);
    f.install().await;
    f.submit("one", &source.replace(":18947/", &format!(":{port}/")))
        .await;
    let value = f.value("health").await;
    let Data::Record(fields) = value.data() else {
        panic!("record")
    };
    assert_eq!(fields["internal"], Data::Int(200));
    assert_eq!(fields["curl"], Data::Int(200));
    f.submit("one", ":env rename \"external\" to:renamed").await;
    f.submit(
        "one",
        &format!(
            "@env{{renamed}} http request url:\"http://127.0.0.1:{port}/health\" > renamed_health"
        ),
    )
    .await;
    assert!(matches!(
        f.value("renamed_health").await.data(),
        Data::Record(_)
    ));
    task.await.unwrap();
    f.stop().await;
}

#[tokio::test]
async fn info_reports_contract_auth_options_and_each_environment_choice_without_secrets() {
    let f = Fixture::new(include_str!(
        "../../../examples/http-auth-alternatives/environments.yaml"
    ));
    std::fs::write(
        f.root.path().join("service.json"),
        include_str!("../../../examples/http-auth-alternatives/service.json"),
    )
    .unwrap();
    f.install().await;
    for (i, env) in ["keypair", "basic", "undecided"].iter().enumerate() {
        f.submit("one", &format!(":env use \"{env}\"")).await;
        f.submit("one", &format!(":info demo > authInfo{i}")).await;
        let v = f.value(&format!("authInfo{i}")).await;
        let Shape::Record(root_shape) = v.shape() else {
            panic!("typed info");
        };
        let Some(Shape::Record(info_shape)) = root_shape.field("information") else {
            panic!("typed information");
        };
        let Some(Shape::List(auth_shape)) = info_shape.field("authentication") else {
            panic!("typed auth list");
        };
        let Shape::Record(auth_shape) = auth_shape.as_ref() else {
            panic!("typed auth record");
        };
        let texts = Shape::List(Box::new(Shape::Primitive(wes_core::Primitive::Text)));
        assert_eq!(auth_shape.field("operation"), Some(&texts));
        assert_eq!(
            auth_shape.field("selected"),
            Some(&Shape::Option(Box::new(texts.clone())))
        );
        let Some(Shape::List(option_shape)) = auth_shape.field("options") else {
            panic!("typed auth options");
        };
        let Shape::Record(option_shape) = option_shape.as_ref() else {
            panic!("typed auth option");
        };
        for field in ["schemes", "credentialSlots", "methods"] {
            assert_eq!(option_shape.field(field), Some(&texts));
        }

        let Data::Record(root) = v.data() else {
            panic!("info record")
        };
        let Data::Record(info) = &root["information"] else {
            panic!("information")
        };
        let Data::List(operations) = &info["authentication"] else {
            panic!("auth list")
        };
        let item = operations
            .iter()
            .find_map(|v| match v {
                Data::Record(r)
                    if r["operation"] == Data::List(vec![Data::Text("listItems".into())]) =>
                {
                    Some(r)
                }
                _ => None,
            })
            .unwrap();
        assert_eq!(
            item["state"],
            Data::Text(
                if *env == "undecided" {
                    "selection-required"
                } else {
                    "selected"
                }
                .into()
            )
        );
        let expected = match *env {
            "keypair" => Some(vec!["apiKey", "apiSecret"]),
            "basic" => Some(vec!["basic"]),
            _ => None,
        };
        if let Some(names) = expected {
            assert_eq!(
                item["selected"],
                Data::Option(Some(Box::new(Data::List(
                    names.iter().map(|n| Data::Text((*n).into())).collect()
                ))))
            );
        } else {
            assert_eq!(item["selected"], Data::Option(None));
        }
        assert!(format!("{v:?}").contains("basic.username"));
        assert!(
            !format!("{v:?}").contains("demo-password"),
            "credential references leaked into info"
        );
    }
    f.submit("one", ":env export file:auth.lock.json").await;
    std::fs::rename(
        f.root.path().join("service.json"),
        f.root.path().join("moved.json"),
    )
    .unwrap();
    let records = LocalEnvironments::new(f.root.path())
        .unwrap()
        .read_lock("auth.lock.json")
        .unwrap();
    assert_eq!(records.len(), 1);
    let reopened = wes_core::environments::Package::parse(records[0].yaml()).unwrap();
    assert_eq!(
        reopened.definitions()["keypair"].imports["demo"].auth["listItems"],
        vec!["apiKey", "apiSecret"]
    );
    assert_eq!(
        reopened.definitions()["basic"].imports["demo"].auth["listItems"],
        vec!["basic"]
    );
    f.stop().await;
}

#[path = "managed_environments/terminal_review.rs"]
mod terminal_review;

#[tokio::test]
async fn changed_http_binding_refuses_refresh_repeat_and_downstream_before_any_request() {
    use std::sync::atomic::{AtomicUsize, Ordering};
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    async fn server() -> (String, Arc<AtomicUsize>, tokio::task::JoinHandle<()>) {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let endpoint = format!("http://{}", listener.local_addr().unwrap());
        let count = Arc::new(AtomicUsize::new(0));
        let requests = count.clone();
        let task = tokio::spawn(async move {
            loop {
                let (mut socket, _) = listener.accept().await.unwrap();
                let mut head = vec![];
                while !head.ends_with(b"\r\n\r\n") {
                    head.push(socket.read_u8().await.unwrap());
                }
                requests.fetch_add(1, Ordering::SeqCst);
                socket
                    .write_all(
                        b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\nok",
                    )
                    .await
                    .unwrap();
            }
        });
        (endpoint, count, task)
    }
    let (old_endpoint, old_calls, old_server) = server().await;
    let (new_endpoint, new_calls, new_server) = server().await;
    let yaml = |endpoint: &str, extra: bool| {
        format!(
            "version: 1\ntargets: {{local: {{kind: local}}}}\nenvironments: {{dev: {{imports: {{api: {{source: {{kind: spec, file: api.json}}, bind: {{target: local, endpoint: '{endpoint}'}}}}{}}}}}}}",
            if extra {
                ", echo: {source: {kind: process, bin: /bin/echo}, bind: {target: local}}"
            } else {
                ""
            }
        )
    };
    let f = Fixture::new(&yaml(&old_endpoint, false));
    std::fs::write(f.root.path().join("api.json"), r#"{"version":1,"provider":"api","types":{},"operations":[{"path":["health"],"method":"GET","route":"/health","auth":[],"parameters":[{"name":"tag","wire":"tag","location":"query","type":"Text","required":false,"encoding":"scalar"}],"responses":{"200":null}}]}"#).unwrap();
    f.install().await;
    f.submit("one", ":env use \"dev\"").await;
    f.submit("one", ":calc { return 'synthetic'; } > tag").await;
    let source = "api health tag:$tag > original";
    let original = f.submit("one", source).await;
    f.submit(
        "one",
        ":calc { return call('api', ['health'], {tag:$tag}); } > calculated",
    )
    .await;
    assert_eq!(old_calls.load(Ordering::SeqCst), 2);
    // An unrelated addition changes the revision, not the captured API contract.
    std::fs::write(f.root.path().join("env.yaml"), yaml(&old_endpoint, true)).unwrap();
    f.install().await;
    f.submit("one", ":env use \"dev\"").await;
    f.submit("one", ":refresh $original").await;
    assert_eq!(old_calls.load(Ordering::SeqCst), 3);
    // Changing the destination does not rewrite the old definition.
    std::fs::write(f.root.path().join("env.yaml"), yaml(&new_endpoint, true)).unwrap();
    f.install().await;
    f.submit("one", ":env use \"dev\"").await;
    let before = f.handle.snapshot().await.unwrap().execution.runs;
    for text in [
        ":refresh $original",
        ":refresh $calculated",
        ":refresh $tag scope:downstream",
    ] {
        let reply = f
            .handle
            .submit(
                SourceInput::new(uuid::Uuid::new_v4().to_string(), text.into())
                    .unwrap()
                    .with_client("one".into())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert!(
            reply
                .diagnostics
                .diagnostics
                .iter()
                .any(|diagnostic| diagnostic.code == "ENV039"),
            "{:?}",
            reply.diagnostics
        );
    }
    let repeated = SourceInput::new("repeat-changed".into(), source.into())
        .unwrap()
        .with_client("one".into())
        .unwrap()
        .with_repeat(original.cell.clone(), true)
        .unwrap();
    assert!(
        f.handle
            .submit(repeated)
            .await
            .unwrap_err()
            .to_string()
            .contains("ENV039")
    );
    assert_eq!(f.handle.snapshot().await.unwrap().execution.runs, before);
    assert_eq!(old_calls.load(Ordering::SeqCst), 3);
    assert_eq!(new_calls.load(Ordering::SeqCst), 0);
    f.submit("one", ":inspect $original > bindingInfo").await;
    let displayed =
        codec::encode_json(f.value("bindingInfo").await.data(), Limits::default()).unwrap();
    let displayed = String::from_utf8(displayed).unwrap();
    assert!(displayed.contains(&old_endpoint));
    assert!(displayed.contains("binding changed"));
    f.submit("one", "api health > current").await;
    assert_eq!(old_calls.load(Ordering::SeqCst), 3);
    assert_eq!(new_calls.load(Ordering::SeqCst), 1);
    // A reactive definition reaches physical admission after an argument change.
    f.submit("one", ":node policy $original mode:reactive")
        .await;
    f.submit("one", ":change $original tag:modified").await;
    let snapshot = f.handle.snapshot().await.unwrap();
    assert_eq!(
        snapshot.execution.errors[&original.nodes[0]].code(),
        "ENV039"
    );
    assert_eq!(old_calls.load(Ordering::SeqCst), 3);
    assert_eq!(new_calls.load(Ordering::SeqCst), 1);
    f.stop().await;
    old_server.abort();
    new_server.abort();
}

#[tokio::test]
async fn credential_admission_rejects_empty_material_and_unavailable_grant_with_actionable_errors()
{
    let f = Fixture::new(simple());
    f.install().await;
    let error = f
        .handle
        .environment_authority(EnvironmentAuthorityCommand::Supply {
            reference: "synthetic/key".into(),
            value: Arc::new(wes_engine::credentials::SecretString::from("")),
        })
        .await
        .unwrap_err();
    assert!(error.to_string().contains("nonempty"));
    let revision = f.handle.environment_revisions().await.unwrap()["dev"];
    let error = f
        .handle
        .environment_authority(EnvironmentAuthorityCommand::Grant {
            environment: "dev".into(),
            revision,
            provider: "missing".into(),
            seconds: 30,
        })
        .await
        .unwrap_err();
    assert!(
        error
            .to_string()
            .contains("Credential grant target is unavailable")
    );
    f.stop().await;
}

#[test]
fn bound_credentials_without_an_auth_choice_explain_the_missing_selection() {
    use wes_core::environments::{EffectiveEnvironment, Package};
    use wes_engine::environments::EnvironmentLoader;
    let root = tempfile::tempdir().unwrap();
    std::fs::write(
        root.path().join("service.json"),
        include_bytes!("../../../examples/http-auth-alternatives/service.json"),
    )
    .unwrap();
    let yaml = include_str!("../../../examples/http-auth-alternatives/environments.yaml")
        .replace("          auth: {listItems: [apiKey, apiSecret]}\n", "");
    let loader = LocalEnvironments::new(root.path()).unwrap();
    let loaded = loader
        .capture_text(&yaml, None, Some(root.path().to_str().unwrap()))
        .unwrap();
    let package = Package::parse(&loaded.yaml).unwrap();
    let environment = Arc::new(
        EffectiveEnvironment::resolve(&package, "keypair", None, &loaded.sources).unwrap(),
    );
    let error = loader
        .build("demo", &environment.bind("demo").unwrap())
        .unwrap_err();
    assert!(
        error.message.contains("Authentication choice is missing"),
        "{}",
        error.message
    );
    assert!(error.message.contains("listItems") && error.message.contains("bind.auth"));
    assert!(!error.message.contains("slots must exactly match"));
}
