use super::*;
use wes_engine::{
    runtime::{Effect, ExecutionTraits, Runtime},
    streams::{self, Phase},
};
fn call(product: &ImportProduct) -> Call {
    let mut runtime = Runtime::new();
    runtime
        .add(
            (),
            [],
            ExecutionTraits {
                pure: false,
                repeatable: false,
                bounded: true,
            },
        )
        .unwrap();
    let run = runtime
        .start(Duration::ZERO)
        .into_iter()
        .find_map(|e| match e {
            Effect::Spawn(ticket) => Some(ticket.run),
            _ => None,
        })
        .unwrap();
    Call {
        authority: Default::default(),
        run,
        capability: product.description().capabilities().next().unwrap().clone(),
        arguments: Default::default(),
    }
}
fn document(
    root: &std::path::Path,
    mode: &str,
    streaming: bool,
    result: &str,
) -> serde_json::Value {
    let python = crate::process::serialized_spawn(|| {
        std::process::Command::new("python3")
            .args(["-c", "import sys; print(sys.executable)"])
            .output()
    })
    .unwrap();
    assert!(python.status.success());
    let python = String::from_utf8(python.stdout).unwrap().trim().to_owned();
    let fixture =
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/support/provider_session.py");
    serde_json::json!({"version":1,"provider":"samples","backend":{"kind":"session","bin":python},"types":{},"operations":[{"path":["read"],"argv":[fixture,mode,root],"result":{"format":"json","type":result},"streaming":streaming}]})
}
fn executable(mode: &str, streaming: bool, result: &str) -> (tempfile::TempDir, ImportProduct) {
    let root = tempfile::tempdir().unwrap();
    let doc = document(root.path(), mode, streaming, result);
    let definition = super::super::schema::compile(&doc.to_string(), None).unwrap();
    let product = unbound(definition, ProcessConfig::default()).unwrap();
    (root, product)
}
struct Managed {
    root: tempfile::TempDir,
    product: ImportProduct,
    authority: Authority,
    binding: wes_core::environments::Binding,
    handle: wes_engine::session::SessionHandle,
    task: wes_engine::session::SessionTask,
    recorder: wes_engine::recording::Recorder,
    writer: wes_engine::recording::RecorderTask,
}
impl Managed {
    async fn new(mode: &str, streaming: bool) -> Self {
        Self::new_with_policy(mode, streaming, false).await
    }
    async fn new_with_policy(mode: &str, streaming: bool, private: bool) -> Self {
        use wes_engine::{environments::EnvironmentLoader, session, workspace::Workspace};
        let root = tempfile::tempdir().unwrap();
        let mut doc = document(root.path(), mode, streaming, "Option<Text>");
        doc["backend"]["credentials"] = serde_json::json!(["Token"]);
        if matches!(mode, "control" | "control-eof") {
            doc["operations"][0]["control"] = serde_json::json!({"input":"Int","output":"Bool"});
        }
        if mode == "private-control" {
            doc["operations"][0]["control"] =
                serde_json::json!({"input":"Int","output":"Text","output_policy":"private"});
        }
        std::fs::write(root.path().join("provider.json"), doc.to_string()).unwrap();
        let yaml = r#"version: 1
targets: {local: {kind: local}}
environments:
  lab:
    secretSlots: {token: {required: true}}
    secretRefs: {token: fixtures/session}
    imports:
      samples:
        source: {kind: provider, file: provider.json}
        bind:
          target: local
          endpoint: 'https://127.0.0.1:58443'
          output: public
          credentials: {Token: {secret: token}}
"#;
        let yaml = if private {
            yaml.replace("output: public", "output: private")
        } else {
            yaml.into()
        };
        std::fs::write(root.path().join("env.yaml"), yaml).unwrap();
        let authority = Authority::default();
        let loader = Arc::new(
            crate::environments::LocalEnvironments::new(root.path())
                .unwrap()
                .with_authority(authority.clone()),
        );
        let history = crate::journal::FileHistory::open(
            &root.path().join("history"),
            Default::default(),
            crate::journal::Durability::File,
        )
        .unwrap();
        let (recorder, writer) =
            wes_engine::recording::spawn_recorder(history, Default::default()).unwrap();
        let (handle, task) = session::spawn(
            Workspace::local(wes_engine::providers::LocalScope::new("session-fixture").unwrap())
                .with_environment_loader(loader.clone()),
            session::RecordingMode::Required(wes_engine::calls::CallJournal::new(
                recorder.clone(),
                wes_engine::calls::RequiredPersistence::FileSynced,
            )),
            Arc::new(crate::type_sources::FileTypeSources::new(root.path()).unwrap()),
            2.try_into().unwrap(),
        )
        .unwrap();
        let plan = handle
            .plan_environment_file("env.yaml".into(), false)
            .await
            .unwrap();
        handle.apply_environments(plan).await.unwrap();
        let captured = loader.capture("env.yaml").unwrap();
        let package = wes_core::environments::Package::parse(&captured.yaml).unwrap();
        let binding = Arc::new(
            wes_core::environments::EffectiveEnvironment::resolve(
                &package,
                "lab",
                None,
                &captured.sources,
            )
            .unwrap(),
        )
        .bind("samples")
        .unwrap();
        let product = loader.build("samples", &binding).unwrap();
        assert!(!root.path().join("attempts").exists());
        Self {
            root,
            product,
            authority,
            binding,
            handle,
            task,
            recorder,
            writer,
        }
    }
    fn supply(&self) {
        self.authority
            .supply(
                "fixtures/session".into(),
                Arc::new(wes_engine::credentials::SecretString::from(
                    "synthetic-provider-session-secret",
                )),
            )
            .unwrap();
    }
    fn grant(&self) {
        self.authority
            .grant(&self.binding, "samples", Duration::from_secs(60))
            .unwrap();
    }
    async fn stop(self) {
        self.handle.shutdown().await.unwrap();
        self.task.join().await.unwrap();
        let drain = self.recorder.shutdown().await.unwrap();
        assert_eq!(drain.failed, 0);
        self.writer.join().await.unwrap();
        for file in ["journal.jsonl", "recovery.jsonl"] {
            if let Ok(bytes) = std::fs::read(self.root.path().join("history").join(file)) {
                assert!(
                    !String::from_utf8_lossy(&bytes).contains("synthetic-provider-session-secret")
                );
            }
        }
        #[cfg(unix)]
        if let Ok(pid) = std::fs::read_to_string(self.root.path().join("pid")) {
            assert_eq!(
                rustix::process::test_kill_process_group(
                    rustix::process::Pid::from_raw(pid.parse().unwrap()).unwrap()
                ),
                Err(rustix::io::Errno::SRCH)
            );
        }
    }
    async fn submit(
        &self,
        text: &str,
        cooperative: bool,
    ) -> Arc<wes_engine::session::SubmissionResult> {
        let input =
            wes_engine::source::SourceInput::new(uuid::Uuid::new_v4().to_string(), text.into())
                .unwrap()
                .with_client("native".into())
                .unwrap();
        let input = if cooperative {
            input.cooperative()
        } else {
            input
        };
        let reply = self.handle.submit(input).await.unwrap();
        assert!(
            !reply
                .diagnostics
                .diagnostics
                .iter()
                .any(|d| d.severity == wes_language::Severity::Error),
            "{text}: {:?}",
            reply.diagnostics
        );
        self.handle.wait_idle().await.unwrap();
        reply
    }
    async fn value(&self, name: &str) -> Value {
        let snapshot = self.handle.snapshot().await.unwrap();
        snapshot
            .execution
            .values
            .get(&snapshot.names[name].node)
            .unwrap_or_else(|| panic!("{name}: {:?}", snapshot.execution.errors))
            .clone()
    }
    async fn failed(&self, name: &str) -> wes_core::ErrorValue {
        let snapshot = self.handle.snapshot().await.unwrap();
        snapshot.execution.errors[&snapshot.names[name].node].clone()
    }
}

#[tokio::test]
async fn view_prepares_typed_control_without_dispatch_and_preserves_native_ownership() {
    let fixture = Managed::new("control", true).await;
    fixture.supply();
    fixture.grant();
    fixture.submit(":env use \"lab\"", false).await;
    fixture.submit("samples read > events", false).await;
    fixture
        .submit(":source bind $events > controls", false)
        .await;
    std::fs::write(
        fixture.root.path().join("action-types.yaml"),
        "version: 1\ntypes:\n  OnlySeven: {base: Int, min: 7, max: 7}\n",
    )
    .unwrap();
    fixture
        .submit(":package load path:\"action-types.yaml\"", false)
        .await;
    fixture
        .submit(
            ":def send(action:OnlySeven) as :source send $controls action:?action budget:PT5S",
            false,
        )
        .await;
    fixture
        .submit(":calc pure { return 6; } > wrong", false)
        .await;
    fixture.submit("send action:$wrong > rejected", false).await;
    assert!(
        fixture
            .failed("rejected")
            .await
            .message()
            .contains("declared contract")
    );
    fixture.submit("send action:7 > actor", true).await;
    assert!(fixture.failed("actor").await.message().contains("native"));
    let reply = fixture.submit(":view create Metric > card", false).await;
    let node = reply.nodes[0].clone();
    let frame = fixture.handle.view_frame(node.clone()).await.unwrap();
    let entry = &frame.instances[0];
    let request = wes_engine::views::commands::CommandRequest {
        root: node.to_string(),
        instance: entry.identity.to_string(),
        member: node.to_string(),
        revision: entry.revision,
        input_revision: entry.input_revision,
        environments: None,
        template: "send".into(),
        arguments: [("action".into(), serde_json::json!(7))].into(),
    };
    let mut bad = request.clone();
    bad.arguments.insert("action".into(), serde_json::json!(6));
    assert!(fixture.handle.prepare_view_command(bad).await.is_err());
    let draft = fixture.handle.prepare_view_command(request).await.unwrap();
    assert!(draft.source.contains("@view"));
    assert!(!fixture.root.path().join("actions").exists());
    fixture
        .submit(&format!("{} > reviewed", draft.source), false)
        .await;
    assert_eq!(fixture.value("reviewed").await.data(), &Data::Bool(true));
    assert_eq!(
        std::fs::read_to_string(fixture.root.path().join("actions")).unwrap(),
        "7\n"
    );
    fixture.submit(":cancel $events", false).await;
    one_joined_attempt(fixture.root.path());
    fixture.stop().await;
}

#[tokio::test]
async fn declared_control_policy_protects_private_body_without_classifying_public_source_metadata()
{
    let fixture = Managed::new("private-control", true).await;
    fixture.supply();
    fixture.grant();
    fixture.submit(":env use \"lab\"", false).await;
    fixture.submit("samples read > events", false).await;
    fixture
        .submit(":source bind $events > controls", false)
        .await;
    assert!(
        !fixture
            .value("controls")
            .await
            .provenance()
            .policy()
            .is_private()
    );
    fixture
        .submit(":source send $controls action:7 > inspected", false)
        .await;
    let body = fixture.value("inspected").await;
    assert_eq!(
        body.data(),
        &Data::Text("synthetic-private-control-body".into())
    );
    assert!(body.provenance().policy().is_private());
    assert_eq!(
        body.provenance().policy().residence(),
        wes_core::flow::Residence::Memory
    );
    assert!(
        !fixture
            .value("controls")
            .await
            .provenance()
            .policy()
            .is_private()
    );
    fixture.submit(":cancel $events", false).await;
    one_joined_attempt(fixture.root.path());
    for file in ["journal.jsonl", "recovery.jsonl"] {
        if let Ok(bytes) = std::fs::read(fixture.root.path().join("history").join(file)) {
            assert!(!String::from_utf8_lossy(&bytes).contains("synthetic-private-control-body"));
        }
    }
    fixture.stop().await;
}

#[tokio::test]
async fn native_control_binding_is_typed_owned_and_does_not_reacquire_its_source() {
    let fixture = Managed::new("control", true).await;
    fixture.supply();
    fixture.grant();
    fixture.submit(":env use \"lab\"", false).await;
    fixture.submit("samples read > events", false).await;
    fixture
        .submit(":source bind $events > controls", false)
        .await;
    let projection = fixture.value("controls").await;
    assert_eq!(
        projection.shape(),
        &Shape::Meta(wes_core::MetaType::SourceControls)
    );
    let wire = codec::encode_value(&projection, Default::default()).unwrap();
    assert!(!String::from_utf8_lossy(&wire).contains(projection.management_authority().unwrap()));
    let restored = codec::decode_value(&wire, Default::default())
        .unwrap()
        .value;
    assert_eq!(restored.shape(), projection.shape());
    assert_eq!(restored.data(), projection.data());
    assert!(restored.management_authority().is_none());
    fixture
        .submit(
            ":source send $controls action:7 budget:PT5S > receipt",
            false,
        )
        .await;
    assert_eq!(fixture.value("receipt").await.data(), &Data::Bool(true));
    fixture
        .submit(":source send $controls action:bad > invalid", false)
        .await;
    assert!(
        fixture
            .failed("invalid")
            .await
            .message()
            .contains("captured contract")
    );
    fixture
        .submit(":source send $controls action:7 > foreign", true)
        .await;
    assert!(
        fixture
            .failed("foreign")
            .await
            .message()
            .contains("native user")
    );
    fixture
        .authority
        .revoke(&fixture.binding, "samples")
        .unwrap();
    fixture
        .submit(":source send $controls action:7 > revoked", false)
        .await;
    assert!(
        fixture
            .failed("revoked")
            .await
            .message()
            .contains("authority refused")
    );
    fixture.grant();
    fixture
        .submit(":source send $controls action:8 > refused", false)
        .await;
    assert!(
        fixture
            .failed("refused")
            .await
            .message()
            .contains("authority refused")
    );
    fixture
        .submit(":source send $controls action:9 > unknown", false)
        .await;
    assert_eq!(
        fixture.failed("unknown").await.code(),
        wes_core::ErrorValue::REMOTE_OUTCOME_UNKNOWN
    );
    assert_ne!(
        fixture.failed("refused").await.code(),
        wes_core::ErrorValue::REMOTE_OUTCOME_UNKNOWN
    );
    assert!(
        fixture
            .failed("unknown")
            .await
            .message()
            .contains("unknown")
    );
    assert_eq!(
        std::fs::read_to_string(fixture.root.path().join("actions")).unwrap(),
        "7\n8\n9\n"
    );
    fixture.submit(":cancel $events", false).await;
    fixture.submit(":refresh $events", false).await;
    fixture
        .submit(":source send $controls action:7 > stale", false)
        .await;
    assert!(
        fixture
            .failed("stale")
            .await
            .message()
            .contains("no live authority")
    );
    assert_eq!(
        std::fs::read_to_string(fixture.root.path().join("actions")).unwrap(),
        "7\n8\n9\n"
    );
    fixture
        .submit(":source bind $events > renewed", false)
        .await;
    fixture
        .submit(":source send $renewed action:7 > again", false)
        .await;
    assert_eq!(fixture.value("again").await.data(), &Data::Bool(true));
    fixture.stop().await;
}

#[tokio::test]
async fn managed_credentials_are_separate_scoped_and_checked_before_launch() {
    let fixture = Managed::new("credentials", false).await;
    for supplied in [false, true] {
        if supplied {
            fixture.supply();
        }
        let Err(InvocationError::Failed(error)) = fixture
            .product
            .invoker()
            .invoke(call(&fixture.product), CancellationToken::new())
            .await
        else {
            panic!("missing credentials must be a known refusal");
        };
        assert_ne!(error.code(), wes_core::ErrorValue::REMOTE_OUTCOME_UNKNOWN);
        assert!(!fixture.root.path().join("attempts").exists());
    }
    fixture.grant();
    let result = fixture
        .product
        .invoker()
        .invoke(call(&fixture.product), CancellationToken::new())
        .await
        .unwrap();
    assert_eq!(result.data(), &Data::Option(None));
    one_joined_attempt(fixture.root.path());
    for path in ["provider.json", "env.yaml", "attempts"] {
        assert!(
            !std::fs::read_to_string(fixture.root.path().join(path))
                .unwrap()
                .contains("synthetic-provider-session-secret")
        );
    }
    fixture.stop().await;
}

#[tokio::test]
async fn dispatched_native_control_timeout_is_unknown_owned_and_never_replayed() {
    let fixture = Managed::new("control", true).await;
    fixture.supply();
    fixture.grant();
    fixture.submit(":env use \"lab\"", false).await;
    fixture.submit("samples read > events", false).await;
    fixture
        .submit(":source bind $events > controls", false)
        .await;
    let input = wes_engine::source::SourceInput::new(
        "timeout".into(),
        ":source send $controls action:10 budget:PT1S > timed".into(),
    )
    .unwrap()
    .with_client("native".into())
    .unwrap();
    let reply = fixture.handle.submit(input).await.unwrap();
    assert!(reply.diagnostics.diagnostics.is_empty());
    tokio::time::timeout(Duration::from_secs(5), async {
        while std::fs::read_to_string(fixture.root.path().join("actions"))
            .ok()
            .as_deref()
            != Some("10\n")
        {
            tokio::time::sleep(Duration::from_millis(1)).await;
        }
    })
    .await
    .unwrap();
    fixture.handle.wait_idle().await.unwrap();
    let error = fixture.failed("timed").await;
    assert_eq!(error.code(), wes_core::ErrorValue::REMOTE_OUTCOME_UNKNOWN);
    assert!(error.message().contains("unknown"));
    assert!(fixture.handle.remote_outcome_uncertain());
    assert_eq!(
        std::fs::read_to_string(fixture.root.path().join("attempts")).unwrap(),
        "started\n"
    );
    fixture.submit(":cancel $events", false).await;
    one_joined_attempt(fixture.root.path());
    assert_eq!(
        std::fs::read_to_string(fixture.root.path().join("actions")).unwrap(),
        "10\n"
    );
    fixture.stop().await;
}

#[tokio::test]
async fn disabling_then_enabling_never_revives_an_owned_session() {
    let fixture = Managed::new("hold", true).await;
    fixture.supply();
    fixture.grant();
    let (handle, task) = streams::spawn(
        call(&fixture.product),
        fixture.product.streams().unwrap().clone(),
        Default::default(),
        Default::default(),
        CancellationToken::new(),
    )
    .unwrap();
    let mut changed = handle.subscribe();
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if changed.borrow_and_update().phase == Phase::Open {
                break;
            }
            changed.changed().await.unwrap();
        }
    })
    .await
    .unwrap();
    fixture.authority.disable("lab").unwrap();
    fixture.authority.enable("lab").unwrap();
    tokio::time::timeout(Duration::from_secs(5), task.join())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(handle.snapshot().phase, Phase::Cancelled);
    one_joined_attempt(fixture.root.path());
    fixture.stop().await;
}
fn one_joined_attempt(root: &std::path::Path) {
    assert_eq!(
        std::fs::read_to_string(root.join("attempts")).unwrap(),
        "started\n"
    );
    #[cfg(unix)]
    {
        let pid = std::fs::read_to_string(root.join("pid"))
            .unwrap()
            .parse::<i32>()
            .unwrap();
        assert_eq!(
            rustix::process::test_kill_process_group(rustix::process::Pid::from_raw(pid).unwrap()),
            Err(rustix::io::Errno::SRCH)
        );
    }
}
#[tokio::test]
async fn executable_finite_and_nullable_results_keep_exact_contracts_and_join_before_return() {
    for (mode, contract, expected) in [
        ("finite", "Int", Data::Int(9007199254740993)),
        ("nullable", "Option<Text>", Data::Option(None)),
    ] {
        let (root, product) = executable(mode, false, contract);
        let result = product
            .invoker()
            .invoke(call(&product), CancellationToken::new())
            .await
            .unwrap();
        assert_eq!(result.data(), &expected);
        assert!(result.metadata().is_some());
        one_joined_attempt(root.path());
    }
}
#[tokio::test]
async fn executable_stream_requires_actual_ready_preserves_first_delivery_and_honors_credit() {
    let (root, product) = executable("stream", true, "Int");
    let (handle, task) = streams::spawn(
        call(&product),
        product.streams().unwrap().clone(),
        Default::default(),
        Default::default(),
        CancellationToken::new(),
    )
    .unwrap();
    tokio::time::timeout(Duration::from_secs(5), task.join())
        .await
        .unwrap()
        .unwrap();
    let snapshot = handle.snapshot();
    assert_eq!(snapshot.phase, Phase::Ended);
    assert_eq!(
        snapshot.window.data(),
        &Data::List(vec![Data::Int(7), Data::Int(8)])
    );
    one_joined_attempt(root.path());
}
#[tokio::test]
async fn malformed_peers_fail_without_retry_payload_disclosure_or_unjoined_subprocesses() {
    for mode in [
        "before-ready",
        "foreign",
        "oversized",
        "bad-sequence",
        "wrong-type",
        "stderr",
    ] {
        let (root, product) = executable(mode, true, "Int");
        let (handle, task) = streams::spawn(
            call(&product),
            product.streams().unwrap().clone(),
            Default::default(),
            Default::default(),
            CancellationToken::new(),
        )
        .unwrap();
        tokio::time::timeout(Duration::from_secs(5), task.join())
            .await
            .unwrap()
            .unwrap();
        let snapshot = handle.snapshot();
        assert!(
            matches!(snapshot.phase, Phase::Failed(_)),
            "{mode}: {snapshot:?}"
        );
        assert_eq!(snapshot.window.data(), &Data::List(vec![]));
        assert!(!format!("{:?}", snapshot.phase).contains("synthetic-session-private-diagnostic"));
        one_joined_attempt(root.path());
    }
}
#[tokio::test]
async fn cancellation_after_real_ready_kills_reaps_and_joins_the_exact_session() {
    let (root, product) = executable("hold", true, "Int");
    let (handle, task) = streams::spawn(
        call(&product),
        product.streams().unwrap().clone(),
        Default::default(),
        Default::default(),
        CancellationToken::new(),
    )
    .unwrap();
    let mut changed = handle.subscribe();
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if changed.borrow_and_update().phase == Phase::Open {
                break;
            }
            changed.changed().await.unwrap();
        }
    })
    .await
    .unwrap();
    tokio::time::timeout(Duration::from_secs(5), task.shutdown())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(handle.snapshot().phase, Phase::Cancelled);
    one_joined_attempt(root.path());
}

#[tokio::test]
async fn cancellation_of_a_blocked_initial_write_joins_physical_ownership() {
    let root = tempfile::tempdir().unwrap();
    let mut doc = document(root.path(), "no-read", false, "Int");
    doc["operations"][0]["parameters"] = serde_json::json!([{"name":"text","type":"Text"}]);
    let definition = super::super::schema::compile(&doc.to_string(), None).unwrap();
    let product = unbound(definition, ProcessConfig::default()).unwrap();
    let mut call = call(&product);
    call.arguments.insert(
        "text".into(),
        Value::new(
            Shape::Unknown,
            Data::Text("x".repeat(300_000).into()),
            Default::default(),
        )
        .unwrap(),
    );
    let token = CancellationToken::new();
    let work = tokio::spawn(product.invoker().invoke(call, token.clone()));
    tokio::time::timeout(Duration::from_secs(5), async {
        while !root.path().join("pid").exists() {
            tokio::time::sleep(Duration::from_millis(1)).await;
        }
    })
    .await
    .unwrap();
    token.cancel();
    assert!(matches!(
        tokio::time::timeout(Duration::from_secs(5), work)
            .await
            .unwrap()
            .unwrap(),
        Err(InvocationError::Cancelled)
    ));
    one_joined_attempt(root.path());
}

#[test]
fn session_import_and_reconstruction_are_inert_and_use_captured_definitions() {
    use wes_engine::imports::{ImportMode, ImportRequest, ImportSnapshot, Importer};
    let root = tempfile::tempdir().unwrap();
    let doc = document(root.path(), "stream", true, "Int");
    std::fs::write(root.path().join("provider.json"), doc.to_string()).unwrap();
    let importer = super::super::ProviderImporter::new(root.path(), Default::default()).unwrap();
    let request = ImportRequest::new(
        "provider".into(),
        Some("captured".into()),
        [(
            "file".into(),
            Value::new(
                Shape::Unknown,
                Data::Text("provider.json".into()),
                Default::default(),
            )
            .unwrap(),
        )]
        .into_iter()
        .collect(),
    )
    .unwrap();
    let recipe = importer.capture(&request, 256 * 1024).unwrap();
    std::fs::write(
        root.path().join("provider.json"),
        "malformed changed definition",
    )
    .unwrap();
    let snapshot = ImportSnapshot::new(request, recipe);
    for mode in [ImportMode::Live, ImportMode::Replay] {
        let product = importer.build(&snapshot, mode).unwrap();
        assert!(
            product
                .description()
                .capabilities()
                .next()
                .unwrap()
                .streaming
        );
        assert!(product.streams().is_some());
        assert_eq!(product.description().name(), "captured");
        assert!(!root.path().join("attempts").exists());
    }
}

#[test]
fn outgoing_controls_preserve_exact_native_scalar_tokens() {
    let value = Value::new(
        Shape::Unknown,
        Data::Int(9007199254740993),
        Default::default(),
    )
    .unwrap();
    let encoded = protocol::action("session", "action", &value).unwrap();
    let json: serde_json::Value = serde_json::from_slice(&encoded).unwrap();
    assert_eq!(json["message"]["data"].as_i64(), Some(9007199254740993));
}

#[tokio::test]
async fn oversized_and_truncated_frames_fail_before_payload_allocation_or_dispatch() {
    for wire in [
        ((protocol::max_frame() + 1) as u32).to_be_bytes().to_vec(),
        vec![0, 0, 0, 0],
        vec![0, 0, 0, 3, b'{'],
    ] {
        assert!(protocol::read(&mut wire.as_slice()).await.is_err());
    }
}

#[tokio::test]
async fn frames_preserve_exact_native_tokens_and_reject_unknown_envelope_fields() {
    let nonce = uuid::Uuid::new_v4();
    let text = format!(
        r#"{{"version":1,"session":"{nonce}","message":{{"kind":"event","sequence":1,"data":9007199254740993}}}}"#
    );
    let wire = [
        (text.len() as u32).to_be_bytes().as_slice(),
        text.as_bytes(),
    ]
    .concat();
    let frame = protocol::read(&mut wire.as_slice()).await.unwrap();
    let protocol::Message::Event { data, .. } = frame.message else {
        panic!("event")
    };
    let int = wes_core::contracts::ContractRegistry::new()
        .resolve("Int")
        .unwrap();
    let value = protocol::value(data.get().as_bytes(), &int, &CancellationToken::new()).unwrap();
    assert_eq!(value.data(), &Data::Int(9007199254740993));
    let bad = text.replace("\"version\":1", "\"unexpected\":true,\"version\":1");
    let wire = [(bad.len() as u32).to_be_bytes().as_slice(), bad.as_bytes()].concat();
    assert!(protocol::read(&mut wire.as_slice()).await.is_err());
}

#[test]
fn session_declarations_are_strict_and_do_not_change_finite_process_semantics() {
    let doc = serde_json::json!({"version":1,"provider":"samples","backend":{"kind":"session","bin":"/synthetic/samples"},"types":{},"operations":[{"path":["watch"],"parameters":[{"name":"count","type":"Int"}],"argv":[],"result":{"format":"json","type":"Int"},"streaming":true,"control":{"input":"Int","output":"Bool"}}]});
    let definition = super::super::schema::compile(&doc.to_string(), None).unwrap();
    assert!(
        definition
            .description
            .capabilities()
            .next()
            .unwrap()
            .streaming
    );
    assert!(definition.operations[0].control.is_some());
    for (key, value) in [
        ("streaming", serde_json::json!(false)),
        ("argv", serde_json::json!([{"parameter":"count"}])),
    ] {
        let mut bad = doc.clone();
        bad["operations"][0][key] = value;
        assert!(super::super::schema::compile(&bad.to_string(), None).is_err());
    }
    let mut bad = doc;
    bad["backend"]["kind"] = serde_json::json!("process");
    assert!(super::super::schema::compile(&bad.to_string(), None).is_err());
}

async fn wait_for_fixture_file(root: &std::path::Path, name: &str, expected: &str) {
    tokio::time::timeout(Duration::from_secs(5), async {
        while std::fs::read_to_string(root.join(name)).ok().as_deref() != Some(expected) {
            tokio::time::sleep(Duration::from_millis(1)).await;
        }
    })
    .await
    .unwrap();
}

#[tokio::test]
async fn native_control_uncertainty_survives_private_policy_history_and_interruption() {
    for private in [false, true] {
        for interruption in ["receipt", "timeout", "eof", "cancel", "revoke"] {
            let mode = if interruption == "eof" {
                "control-eof"
            } else {
                "control"
            };
            let fixture = Managed::new_with_policy(mode, true, private).await;
            fixture.supply();
            fixture.grant();
            fixture.submit(":env use \"lab\"", false).await;
            fixture.submit("samples read > events", false).await;
            fixture
                .submit(":source bind $events > controls", false)
                .await;
            let command = if interruption == "receipt" {
                ":source send $controls action:9 budget:PT5S > uncertain"
            } else if interruption == "timeout" {
                ":source send $controls action:10 budget:PT1S > uncertain"
            } else {
                ":source send $controls action:10 budget:PT5S > uncertain"
            };
            let input = wes_engine::source::SourceInput::new(
                uuid::Uuid::new_v4().to_string(),
                command.into(),
            )
            .unwrap()
            .with_client("native".into())
            .unwrap();
            fixture.handle.submit(input).await.unwrap();
            wait_for_fixture_file(
                fixture.root.path(),
                "actions",
                if interruption == "receipt" {
                    "9\n"
                } else {
                    "10\n"
                },
            )
            .await;
            let cancelled = interruption == "cancel";
            if cancelled {
                fixture.submit(":cancel $uncertain", false).await;
            } else if interruption == "revoke" {
                fixture.authority.disable("lab").unwrap();
            }
            fixture.handle.wait_idle().await.unwrap();
            assert!(
                fixture.handle.remote_outcome_uncertain(),
                "{private}/{interruption}"
            );
            if !cancelled {
                let error = fixture.failed("uncertain").await;
                assert_eq!(
                    error.code(),
                    wes_core::ErrorValue::REMOTE_OUTCOME_UNKNOWN,
                    "{private}/{interruption}"
                );
                assert!(error.message().contains("unknown"));
                assert_eq!(error.policy().is_private(), private);
                if private {
                    assert!(!error.message().contains("source retains"));
                }
            }
            fixture.submit(":cancel $events", false).await;
            one_joined_attempt(fixture.root.path());
            assert_eq!(
                std::fs::read_to_string(fixture.root.path().join("actions")).unwrap(),
                if interruption == "receipt" {
                    "9\n"
                } else {
                    "10\n"
                }
            );
            let history =
                std::fs::read_to_string(fixture.root.path().join("history/journal.jsonl")).unwrap();
            assert!(history.contains(wes_core::ErrorValue::REMOTE_OUTCOME_UNKNOWN));
            assert!(!history.contains("synthetic-provider-session-secret"));
            fixture.stop().await;
        }
    }
}

#[tokio::test]
async fn finite_session_lost_response_is_unknown_but_confirmed_outcomes_stay_known() {
    for (mode, expected) in [
        ("finite-eof", wes_core::ErrorValue::REMOTE_OUTCOME_UNKNOWN),
        ("finite-failed", "RUN001"),
        ("finite-result-no-end", "RUN001"),
    ] {
        let (root, product) = executable(mode, false, "Option<Text>");
        let Err(InvocationError::Failed(error)) = product
            .invoker()
            .invoke(call(&product), CancellationToken::new())
            .await
        else {
            panic!("expected failure for {mode}");
        };
        assert_eq!(error.code(), expected, "{mode}");
        let private = error.with_policy(&wes_core::flow::FlowPolicy::default().private());
        assert_eq!(
            private.code(),
            if expected == wes_core::ErrorValue::REMOTE_OUTCOME_UNKNOWN {
                expected
            } else {
                "ENV021"
            }
        );
        one_joined_attempt(root.path());
    }
}

#[tokio::test]
async fn finite_session_cancel_and_authority_withdrawal_after_handoff_are_unknown() {
    for revoke in [false, true] {
        let fixture = Managed::new("hold", false).await;
        fixture.supply();
        fixture.grant();
        let token = CancellationToken::new();
        let job = tokio::spawn(
            fixture
                .product
                .invoker()
                .invoke(call(&fixture.product), token.clone()),
        );
        wait_for_fixture_file(fixture.root.path(), "handoff", "received\n").await;
        if revoke {
            fixture.authority.disable("lab").unwrap();
        } else {
            token.cancel();
        }
        let Err(InvocationError::Failed(error)) = job.await.unwrap() else {
            panic!("lost finite receipt must be unknown");
        };
        assert_eq!(error.code(), wes_core::ErrorValue::REMOTE_OUTCOME_UNKNOWN);
        one_joined_attempt(fixture.root.path());
        fixture.stop().await;
    }
    let (root, product) = executable("finite", false, "Int");
    let token = CancellationToken::new();
    token.cancel();
    assert!(matches!(
        product.invoker().invoke(call(&product), token).await,
        Err(InvocationError::Cancelled)
    ));
    assert!(!root.path().join("attempts").exists());
}

#[tokio::test]
async fn finite_session_deadline_after_handoff_is_unknown_without_replay() {
    let root = tempfile::tempdir().unwrap();
    let doc = document(root.path(), "hold", false, "Option<Text>");
    let definition = super::super::schema::compile(&doc.to_string(), None).unwrap();
    let product = unbound(
        definition,
        ProcessConfig {
            timeout: Duration::from_secs(1),
            ..Default::default()
        },
    )
    .unwrap();
    let Err(InvocationError::Failed(error)) = product
        .invoker()
        .invoke(call(&product), CancellationToken::new())
        .await
    else {
        panic!("deadline must be unknown");
    };
    assert_eq!(error.code(), wes_core::ErrorValue::REMOTE_OUTCOME_UNKNOWN);
    assert!(root.path().join("handoff").exists());
    one_joined_attempt(root.path());
}

#[tokio::test]
async fn private_finite_session_unknown_reaches_recorded_diagnostics() {
    for (mode, expected) in [
        ("finite-eof", wes_core::ErrorValue::REMOTE_OUTCOME_UNKNOWN),
        ("finite-failed", "ENV021"),
    ] {
        let fixture = Managed::new_with_policy(mode, false, true).await;
        fixture.supply();
        fixture.grant();
        fixture.submit(":env use \"lab\"", false).await;
        fixture.submit("samples read > result", false).await;
        let error = fixture.failed("result").await;
        assert_eq!(error.code(), expected);
        assert!(error.policy().is_private());
        assert_eq!(
            fixture.handle.remote_outcome_uncertain(),
            expected == wes_core::ErrorValue::REMOTE_OUTCOME_UNKNOWN
        );
        let history =
            std::fs::read_to_string(fixture.root.path().join("history/journal.jsonl")).unwrap();
        assert!(history.contains(expected));
        assert!(!history.contains("synthetic-provider-session-secret"));
        one_joined_attempt(fixture.root.path());
        fixture.stop().await;
    }
}
