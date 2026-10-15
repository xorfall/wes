//! Synthetic byte producers/consumers through the real declarative and protected-store boundary.
use std::{path::Path, sync::Arc, time::Duration};
use wes::runtime::{RuntimeOptions, launch};
use wes_core::Data;
use wes_engine::{
    session::{SessionHandle, SubmissionResult},
    source::SourceInput,
};

async fn submit(session: &SessionHandle, text: &str) -> Arc<SubmissionResult> {
    let result = session
        .submit(SourceInput::new(uuid::Uuid::new_v4().to_string(), text.into()).unwrap())
        .await
        .unwrap_or_else(|error| panic!("Submission {text} failed: {error:?}"));
    assert!(
        !result
            .diagnostics
            .diagnostics
            .iter()
            .any(|d| d.severity == wes_language::Severity::Error),
        "{result:?}"
    );
    if tokio::time::timeout(Duration::from_secs(30), session.wait_idle())
        .await
        .is_err()
    {
        let snapshot = session.snapshot().await.unwrap();
        panic!(
            "Timed out awaiting {text}; executing: {:?}; errors: {:?}",
            snapshot.execution.executing, snapshot.execution.errors
        );
    }
    result
}
const TOOL: &str = r#"import sys, json, struct, hashlib, pathlib
mode=sys.argv[1]
root=pathlib.Path(sys.argv[2])
with (root/'calls').open('a') as f: f.write(mode+'\n')
if mode=='producer-failed': sys.exit(9)
if mode=='produce':
    header=json.dumps({'raw':'','label':'synthetic original bytes'}).encode()
    sys.stdout.buffer.write(struct.pack('>I', len(header))+header)
    for n in range(640): sys.stdout.buffer.write(bytes([n%251])*65536)
else:
    if mode=='early': sys.exit(0)
    h=hashlib.sha256(); count=0
    while True:
        data=sys.stdin.buffer.read(65536)
        if not data: break
        count+=len(data); h.update(data)
    if mode=='fail': sys.exit(7)
    print(json.dumps({'bytes':str(count),'sha256':h.hexdigest(),'rows':[count]}))
"#;

#[tokio::test]
async fn large_original_input_is_bounded_and_observational_and_failures_preserve_raw() {
    let root = tempfile::tempdir().unwrap();
    let python = wes_adapters::process::serialized_spawn(|| {
        std::process::Command::new("python3")
            .args(["-c", "import sys; print(sys.executable)"])
            .output()
    })
    .unwrap();
    assert!(python.status.success());
    let python = String::from_utf8(python.stdout).unwrap().trim().to_owned();
    let tool = root.path().join("tool.py");
    std::fs::write(&tool, TOOL).unwrap();
    let types = serde_json::json!({"Blob":{"base":"Record","fields":{"raw":"Bytes","label":"Text"}},"Summary":{"base":"Record","fields":{"bytes":"Text","sha256":"Text","rows":"List<Int>"}}});
    let mut operations = vec![
        serde_json::json!({"path":["produce"],"argv":[tool.to_str().unwrap(),"produce",root.path().to_str().unwrap()],"result":{"format":"record-bytes","type":"Blob","byte_field":["raw"]}}),
    ];
    operations.push(serde_json::json!({"path":["refuse"],"argv":[tool.to_str().unwrap(),"producer-failed",root.path().to_str().unwrap()],"result":{"format":"json","type":"Blob"}}));
    for mode in ["inspect", "fail", "early", "small"] {
        operations.push(serde_json::json!({"path":[mode],"parameters":[{"name":"source","type":"Bytes"}],"byte_input":{"parameter":"source","max_bytes":if mode=="small" {1_000} else {50_000_000}},"argv":[tool.to_str().unwrap(),mode,root.path().to_str().unwrap()],"result":{"format":"json","type":"Summary"}}));
    }
    let provider = serde_json::json!({"version":1,"provider":"bytes","types":types,"backend":{"kind":"process","bin":python},"operations":operations});
    std::fs::write(root.path().join("provider.yaml"), provider.to_string()).unwrap();
    std::fs::write(root.path().join("env.yaml"), "version: 1\ntargets:\n  local: {kind: local}\nenvironments:\n  lab:\n    imports:\n      bytes:\n        source: {kind: provider, file: provider.yaml}\n        bind: {target: local, output: confidential}\n").unwrap();
    let key = root.path().join("key");
    std::fs::write(&key, [42; 32]).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&key, std::fs::Permissions::from_mode(0o600)).unwrap();
    }
    let mut options = RuntimeOptions::new(root.path().join("home"), root.path().into());
    options.storage_key_file = Some(key);
    options.docker_candidates = Some(vec![root.path().join("missing.sock")]);
    let runtime = launch(options).await.unwrap();
    let session = runtime.handle.current().unwrap().session;
    let plan = session
        .plan_environment_file("env.yaml".into(), false)
        .await
        .unwrap();
    session.apply_environments(plan).await.unwrap();
    submit(&session, ":env use \"lab\"").await;
    submit(&session, ":transient { bytes produce > blob } > scope").await;
    let export = session.read_sandbox_export("scope", false).await.unwrap();
    let scope = export.scope.clone().unwrap();
    drop(export);
    let (child, worker) = session.resolve_read_scope(&scope).await.unwrap();
    child.wait_idle().await.unwrap();
    let before = child.snapshot().await.unwrap();
    let original = before.names["blob"].node.clone();
    let run = before.execution.runs[&original].clone();
    assert!(before.execution.stored_values.contains_key(&original));
    drop(before);
    let inspected = submit(&child, "bytes inspect source:$blob.raw > summary").await;
    let rendered = submit(&child, ":calc pure { return $summary; } > rendered").await;
    let snapshot = child.snapshot().await.unwrap();
    assert!(
        snapshot.execution.errors.is_empty(),
        "{:?}",
        snapshot.execution.errors
    );
    assert!(
        snapshot
            .execution
            .stored_values
            .contains_key(inspected.nodes.last().unwrap())
    );
    let value = &snapshot.execution.values[rendered.nodes.last().unwrap()];
    assert!(value.provenance().policy().is_confidential());
    let Data::Record(fields) = value.data() else {
        panic!("summary");
    };
    assert_eq!(
        fields["bytes"],
        Data::Text((640u64 * 65536).to_string().into())
    );
    let mut hash = sha2::Sha256::new();
    use sha2::Digest;
    for n in 0..640 {
        hash.update(vec![(n % 251) as u8; 65536]);
    }
    let expected_hash = format!("{:x}", hash.finalize());
    assert_eq!(fields["sha256"], Data::Text(expected_hash.clone().into()));
    drop(snapshot);
    for mode in ["small", "fail", "early"] {
        submit(&child, &format!("bytes {mode} source:$blob.raw > {mode}")).await;
    }
    let snapshot = child.snapshot().await.unwrap();
    assert_eq!(snapshot.execution.runs[&original], run);
    for mode in ["small", "fail", "early"] {
        assert!(
            snapshot
                .execution
                .errors
                .contains_key(&snapshot.names[mode].node)
        );
    }
    drop(snapshot);
    submit(
        &child,
        ":read bytes:$blob from:\"65536\" limit:4 select:\"raw\" > originalBytes",
    )
    .await;
    let snapshot = child.snapshot().await.unwrap();
    let Data::Record(fields) =
        snapshot.execution.values[&snapshot.names["originalBytes"].node].data()
    else {
        panic!()
    };
    assert_eq!(fields["data"], Data::Bytes(vec![1; 4].into()));
    drop(snapshot);
    let dependency = submit(
        &child,
        ":read bytes:$summary dependency:\"input\" from:\"65536\" limit:4 > inputBytes",
    )
    .await;
    let snapshot = child.snapshot().await.unwrap();
    assert!(
        !snapshot
            .execution
            .errors
            .contains_key(dependency.nodes.last().unwrap()),
        "dependency read: {:?}",
        snapshot
            .execution
            .errors
            .get(dependency.nodes.last().unwrap())
    );
    let Data::Record(fields) = snapshot.execution.values[dependency.nodes.last().unwrap()].data()
    else {
        panic!()
    };
    assert_eq!(fields["data"], Data::Bytes(vec![1; 4].into()));
    drop(snapshot);
    let copy = submit(
        &child,
        ":read bytes:$rendered dependency:\"input\" from:\"0\" limit:4 > copiedInput",
    )
    .await;
    let snapshot = child.snapshot().await.unwrap();
    assert!(
        snapshot
            .execution
            .errors
            .contains_key(copy.nodes.last().unwrap())
    );
    drop(snapshot);
    let observed = child.observe().await.unwrap();
    let outputs = &observed.values.as_ref().unwrap().outputs;
    let raw = outputs[&original].handle().unwrap().clone();
    let summary = outputs[inspected.nodes.last().unwrap()]
        .handle()
        .unwrap()
        .clone();
    drop(observed);
    // The source was materialized above. A scan must capture that exact owner,
    // not recapture its selected List as a new independent checkpoint value.
    submit(&child, ":package load source:\"types: {IntegerStep: {base: Record, fields: {state: Int, outputs: 'List<Int>'}}}\"").await;
    submit(&child, ":def collect(state:Int, context:Int, item:Int) -> IntegerStep as :calc pure { return {state:state+1,outputs:[item]}; }").await;
    let scan = submit(&child, ":scan source:$summary.rows transition:collect initial:0 context:0 profile:TypedRecords sink:dataset > inputScan").await;
    let state = child.snapshot().await.unwrap();
    assert!(
        !state
            .execution
            .errors
            .contains_key(scan.nodes.last().unwrap()),
        "{:?}",
        state.execution.errors
    );
    let Data::Record(fields) = state.execution.values[scan.nodes.last().unwrap()].data() else {
        panic!("scan result")
    };
    let Data::Dataset(output) = &fields["outputs"] else {
        panic!("Dataset output")
    };
    let checkpoint = worker
        .dataset_checkpoint(output.as_ref().clone())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(checkpoint.source.handle, summary.to_string());
    assert_eq!(checkpoint.source.selection, ["rows"]);
    let output = output.as_ref().clone();
    let cost = worker
        .dataset_retention_preview(output.clone())
        .await
        .unwrap();
    assert!(cost.captured_source_bytes >= checkpoint.source.bytes + 640 * 65536);
    assert_eq!(cost.total_bytes, cost.shared_bytes + cost.exclusive_bytes);
    drop(state);
    assert!(!worker.is_kept(raw.clone()).await.unwrap());
    assert!(!worker.is_kept(summary.clone()).await.unwrap());
    submit(&session, ":keep $scope.inputScan > savedInputScan").await;
    assert!(worker.is_kept(raw.clone()).await.unwrap());
    assert!(worker.is_kept(summary.clone()).await.unwrap());
    let cost = worker.dataset_retention_preview(output).await.unwrap();
    assert!(cost.shared_bytes >= cost.captured_source_bytes);
    let observed = child.observe().await.unwrap();
    assert_eq!(
        observed.values.as_ref().unwrap().outputs[&original].handle(),
        Some(&raw)
    );
    drop(observed);
    assert_eq!(
        std::fs::read_to_string(root.path().join("calls")).unwrap(),
        "produce\ninspect\nfail\nearly\n"
    );
    let actor = child
        .submit(
            SourceInput::new(
                uuid::Uuid::new_v4().to_string(),
                "bytes inspect source:$blob.raw > actorAttempt".into(),
            )
            .unwrap()
            .with_client("synthetic-actor".into())
            .unwrap()
            .cooperative(),
        )
        .await;
    child.wait_idle().await.unwrap();
    let state = child.snapshot().await.unwrap();
    assert!(
        actor.is_err()
            || actor
                .as_ref()
                .unwrap()
                .diagnostics
                .diagnostics
                .iter()
                .any(|d| d.severity == wes_language::Severity::Error)
            || actor
                .as_ref()
                .unwrap()
                .nodes
                .iter()
                .any(|node| state.execution.errors.contains_key(node))
    );
    drop(state);
    assert_eq!(
        std::fs::read_to_string(root.path().join("calls")).unwrap(),
        "produce\ninspect\nfail\nearly\n"
    );
    // Failed typed output has no accessible payload. Inspection refuses without
    // waiting on, refreshing or replaying its producer.
    submit(&session, "bytes refuse > refused").await;
    let unavailable = submit(&session, "bytes inspect source:$refused.raw > unavailable").await;
    let state = session.snapshot().await.unwrap();
    assert!(
        state
            .execution
            .errors
            .contains_key(unavailable.nodes.last().unwrap())
    );
    assert!(
        !state
            .execution
            .waiting_inputs
            .contains_key(unavailable.nodes.last().unwrap())
    );
    assert_eq!(
        std::fs::read_to_string(root.path().join("calls")).unwrap(),
        "produce\ninspect\nfail\nearly\nproducer-failed\n"
    );
    submit(&session, ":keep $scope.summary > savedSummary").await;
    let range = submit(
        &session,
        ":read bytes:$savedSummary dependency:\"input\" from:\"65536\" limit:4 > savedBytes",
    )
    .await;
    let state = session.snapshot().await.unwrap();
    assert!(
        !state
            .execution
            .errors
            .contains_key(range.nodes.last().unwrap()),
        "{:?}",
        state.execution.errors
    );
    let Data::Record(fields) = state.execution.values[range.nodes.last().unwrap()].data() else {
        panic!("saved range")
    };
    assert_eq!(fields["data"], Data::Bytes(vec![1; 4].into()));
    drop(state);
    submit(&session, ":keep $scope.blob.raw > savedRaw").await;
    submit(&session, "bytes inspect source:$savedRaw > recheck").await;
    let state = session.snapshot().await.unwrap();
    assert!(
        state
            .execution
            .stored_values
            .contains_key(&state.names["recheck"].node),
        "{:?}",
        state.execution.errors
    );
    drop(state);
    let rechecked = submit(&session, ":calc pure { return $recheck; } > rechecked").await;
    let state = session.snapshot().await.unwrap();
    let Data::Record(fields) = state.execution.values[rechecked.nodes.last().unwrap()].data()
    else {
        panic!("rechecked summary")
    };
    assert_eq!(
        fields["bytes"],
        Data::Text((640u64 * 65536).to_string().into())
    );
    assert_eq!(fields["sha256"], Data::Text(expected_hash.into()));
    assert_eq!(
        std::fs::read_to_string(root.path().join("calls")).unwrap(),
        "produce\ninspect\nfail\nearly\nproducer-failed\ninspect\n"
    );
    drop(state);
    drop(worker);
    drop(child);
    drop(session);
    runtime.shutdown().await.unwrap();
    assert!(Path::new(&tool).is_file());
}
