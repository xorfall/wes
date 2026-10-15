//! Actual external offline decoder, native input ownership and Dataset retention; synthetic only.
use std::{io::Write, path::Path, sync::Arc, time::Duration};
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
        .unwrap_or_else(|e| panic!("{text}: {e:?}"));
    assert!(
        !result
            .diagnostics
            .diagnostics
            .iter()
            .any(|d| d.severity == wes_language::Severity::Error),
        "{result:?}"
    );
    tokio::time::timeout(Duration::from_secs(30), session.wait_idle())
        .await
        .unwrap_or_else(|error| panic!("Idle deadline for {text}: {error:?}"))
        .unwrap();
    result
}
fn capture(path: &Path) -> u64 {
    let mut file = std::fs::File::create(path).unwrap();
    file.write_all(&[0xd4, 0xc3, 0xb2, 0xa1]).unwrap();
    file.write_all(&2u16.to_le_bytes()).unwrap();
    file.write_all(&4u16.to_le_bytes()).unwrap();
    for n in [0u32, 0, 65535, 1] {
        file.write_all(&n.to_le_bytes()).unwrap();
    }
    let mut packet = vec![0x91; 65535];
    packet[..14].copy_from_slice(&[2, 0, 0, 0, 0, 2, 2, 0, 0, 0, 0, 1, 8, 0]);
    packet[14..34].copy_from_slice(&[
        0x45, 0, 0xff, 0xf1, 0, 7, 0, 0, 64, 6, 0, 0, 192, 0, 2, 10, 198, 51, 100, 20,
    ]);
    packet[34..54].copy_from_slice(&[
        0xd4, 0x31, 0x1f, 0x90, 0, 0, 0, 100, 0, 0, 0, 0, 0x50, 2, 0x10, 0, 0, 0, 0, 0,
    ]);
    // Cross both the finite output limit and the 512-entry index ceiling without
    // adding unrelated encrypted input work to each decoder and retention check.
    let record_bytes = packet.len() as u64 + 16;
    let count = (wes_budgets::get("process.output.bytes") / record_bytes + 1).max(513);
    for n in 0..u32::try_from(count).unwrap() {
        for value in [n, 0, 65535, 65535] {
            file.write_all(&value.to_le_bytes()).unwrap();
        }
        file.write_all(&packet).unwrap();
    }
    file.metadata().unwrap().len()
}
fn field<'a>(data: &'a Data, name: &str) -> &'a Data {
    let Data::Record(fields) = data else {
        panic!("Expected record: {data:?}")
    };
    &fields[name]
}

#[tokio::test]
async fn owned_decoder_rows_feed_scan_and_only_keeping_the_scan_retains_the_original_capture() {
    let root = tempfile::tempdir().unwrap();
    let examples = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../examples");
    let file = root.path().join("synthetic.pcap");
    let extent = capture(&file);
    assert!(extent > wes_budgets::get("process.output.bytes"));
    let python = wes_adapters::process::serialized_spawn(|| {
        std::process::Command::new("python3")
            .args(["-c", "import sys; print(sys.executable)"])
            .output()
    })
    .unwrap();
    assert!(python.status.success());
    let python = String::from_utf8(python.stdout).unwrap().trim().to_owned();
    let tool = root.path().join("count.py");
    std::fs::write(&tool, "import sys,runpy,pathlib\nroot=pathlib.Path(sys.argv.pop(1))\nmode=sys.argv.pop(1)\nwith (root/'calls').open('a') as f: f.write(mode+'\\n')\nsys.argv=sys.argv[1:]\nrunpy.run_path(sys.argv[0],run_name='__main__')\n").unwrap();
    let prefix = |mode: &str, helper: &Path| {
        vec![
            serde_json::json!(tool),
            serde_json::json!(root.path()),
            serde_json::json!(mode),
            serde_json::json!(helper),
        ]
    };
    let capture_helper = examples.join("tracing-offline/capture_adapter.py");
    let importer = std::fs::read_to_string(examples.join("tracing-offline/provider.yaml"))
        .unwrap()
        .replace(
            "bin: capture_adapter.py",
            &format!("bin: {}", serde_json::to_string(&python).unwrap()),
        )
        .replace(
            "argv: [import,",
            &format!(
                "argv: [{}, {}, {}, {}, import,",
                serde_json::to_string(&tool).unwrap(),
                serde_json::to_string(root.path()).unwrap(),
                serde_json::to_string("import").unwrap(),
                serde_json::to_string(&capture_helper).unwrap()
            ),
        );
    std::fs::write(root.path().join("capture.yaml"), importer).unwrap();
    let mut provider: serde_json::Value = serde_json::from_slice(
        &std::fs::read(examples.join("tracing-analysis/provider.yaml")).unwrap(),
    )
    .unwrap();
    provider["backend"]["bin"] = serde_json::json!(python);
    let mut bad = provider["operations"][0].clone();
    bad["path"] = serde_json::json!(["changed"]);
    bad.as_object_mut().unwrap().remove("finite_profile");
    bad["argv"][2] = serde_json::json!(format!("sha256:{}", "0".repeat(64)));
    provider["operations"].as_array_mut().unwrap().push(bad);
    let helper = examples.join("tracing-analysis/analyze_capture.py");
    for (i, op) in provider["operations"]
        .as_array_mut()
        .unwrap()
        .iter_mut()
        .enumerate()
    {
        let mut argv = prefix(if i == 0 { "decode" } else { "changed" }, &helper);
        argv.extend(op["argv"].as_array().unwrap().iter().cloned());
        op["argv"] = serde_json::json!(argv);
    }
    std::fs::write(root.path().join("analyze.yaml"), provider.to_string()).unwrap();
    std::fs::write(root.path().join("env.yaml"), "version: 1\ntargets:\n  local: {kind: local}\nenvironments:\n  lab:\n    imports:\n      capture:\n        source: {kind: provider, file: capture.yaml}\n        bind: {target: local, output: confidential}\n      analyze:\n        source: {kind: provider, file: analyze.yaml}\n        bind: {target: local, output: confidential}\n").unwrap();
    let key = root.path().join("key");
    std::fs::write(&key, [56; 32]).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&key, std::fs::Permissions::from_mode(0o600)).unwrap();
    }
    let options = || {
        let mut options = RuntimeOptions::new(root.path().join("home"), root.path().into());
        options.storage_key_file = Some(key.clone());
        options.docker_candidates = Some(vec![root.path().join("missing.sock")]);
        options
    };
    let runtime = launch(options()).await.unwrap();
    let session = runtime.handle.current().unwrap().session;
    let plan = session
        .plan_environment_file("env.yaml".into(), false)
        .await
        .unwrap();
    session.apply_environments(plan).await.unwrap();
    submit(&session, ":env use \"lab\"").await;
    submit(
        &session,
        &format!(
            ":transient {{ :trace import using:\"pcap\" file:{} > raw }} > trace",
            serde_json::to_string(&file).unwrap()
        ),
    )
    .await;
    let export = session.read_sandbox_export("trace", false).await.unwrap();
    let (child, worker) = session
        .resolve_read_scope(export.scope.as_ref().unwrap())
        .await
        .unwrap();
    drop(export);
    let before = child.snapshot().await.unwrap();
    let raw_node = before.names["raw"].node.clone();
    let run = before.execution.runs[&raw_node].clone();
    drop(before);
    std::fs::remove_file(&file).unwrap();
    submit(
        &child,
        ":trace analyze $raw.raw using:\"headers\" options:{packets:2,headerBytes:256} > decoded",
    )
    .await;
    submit(
        &child,
        "analyze decode source:$raw.raw packets:1 headerBytes:64 > smaller",
    )
    .await;
    submit(
        &child,
        "analyze changed source:$raw.raw packets:1 headerBytes:64 > refused",
    )
    .await;
    submit(&child, ":calc pure { return $decoded; } > facts").await;
    let state = child.snapshot().await.unwrap();
    assert_eq!(state.execution.runs[&raw_node], run);
    assert!(
        state
            .execution
            .errors
            .contains_key(&state.names["refused"].node)
    );
    let facts = state.execution.values[&state.names["facts"].node].clone();
    assert!(facts.provenance().policy().is_confidential());
    let Data::Record(fields) = facts.data() else {
        panic!("Analysis facts")
    };
    assert_eq!(fields["sampleComplete"], Data::Bool(false));
    assert_eq!(
        field(&fields["index"], "rawBytes"),
        &Data::Text(extent.to_string().into())
    );
    let Data::List(rows) = &fields["packets"] else {
        panic!()
    };
    assert_eq!(rows.len(), 2);
    assert_eq!(field(&rows[0], "protocol"), &Data::Text("TCP".into()));
    assert_eq!(field(&rows[0], "status"), &Data::Text("decoded".into()));
    drop(state);
    let adapters = std::fs::read_to_string(examples.join("tracing-analysis/scan-adapters.wes"))
        .unwrap()
        .replace(
            ":package load path:types.yaml",
            &format!(
                ":package load path:{}",
                serde_json::to_string(&examples.join("tracing-analysis/types.yaml")).unwrap()
            ),
        );
    submit(&child, &adapters).await;
    submit(&child,":scan source:$decoded.packets transition:packetRow initial:0 context:0 profile:TypedRecords sink:dataset > packetScan").await;
    let state = child.snapshot().await.unwrap();
    assert_eq!(
        state.execution.errors.len(),
        1,
        "{:?}",
        state.execution.errors
    );
    let scan = state.execution.values[&state.names["packetScan"].node].clone();
    assert_eq!(field(scan.data(), "state"), &Data::Int(2));
    assert_eq!(
        field(field(scan.data(), "receipt"), "status"),
        &Data::Text("complete".into())
    );
    let decoded_node = state.names["decoded"].node.clone();
    let observed = child.observe().await.unwrap();
    let outputs = &observed.values.as_ref().unwrap().outputs;
    let raw = outputs[&raw_node].handle().unwrap().clone();
    let derived = outputs[&decoded_node].handle().unwrap().clone();
    drop(observed);
    drop(state);
    let captured = worker
        .describe(derived.clone(), vec![])
        .await
        .unwrap()
        .unwrap();
    assert!(!worker.is_kept(raw.clone()).await.unwrap());
    assert!(!worker.is_kept(derived.clone()).await.unwrap());
    submit(&session, ":trace retain $trace.packetScan > savedScan").await;
    assert!(worker.is_kept(raw.clone()).await.unwrap());
    assert!(worker.is_kept(derived.clone()).await.unwrap());
    let range = worker
        .source_range_dependency(captured.clone(), "input".into(), 76, 2)
        .await
        .unwrap();
    assert_eq!(range.bytes, 8080u16.to_be_bytes());
    assert_eq!(range.extent, extent);
    // Retain the typed result for native display without retaining the whole transient scope.
    submit(&session, ":trace retain $trace.decoded > savedDecoded").await;
    assert_eq!(
        std::fs::read_to_string(root.path().join("calls")).unwrap(),
        "import\ndecode\ndecode\nchanged\n"
    );
    drop(worker);
    drop(child);
    drop(session);
    runtime.shutdown().await.unwrap();
    let runtime = launch(options()).await.unwrap();
    let session = runtime.handle.current().unwrap().session;
    assert_eq!(
        session
            .read_sandbox("savedDecoded", false)
            .await
            .unwrap()
            .data,
        *facts.data()
    );
    assert_eq!(
        session
            .read_sandbox("savedScan.state", false)
            .await
            .unwrap()
            .data,
        Data::Int(2)
    );
    let read = submit(
        &session,
        ":read bytes:$savedDecoded dependency:\"input\" from:\"76\" limit:2 > originalPort",
    )
    .await;
    let state = session.snapshot().await.unwrap();
    assert!(
        !state
            .execution
            .errors
            .contains_key(read.nodes.last().unwrap()),
        "{:?}",
        state.execution.errors
    );
    assert_eq!(
        field(
            state.execution.values[read.nodes.last().unwrap()].data(),
            "data"
        ),
        &Data::Bytes(8080u16.to_be_bytes().to_vec().into())
    );
    drop(state);
    submit(
        &session,
        ":transient { :calc pure { return 1; } > opened } > readCheck",
    )
    .await;
    let export = session
        .read_sandbox_export("readCheck", false)
        .await
        .unwrap();
    let (reader, worker) = session
        .resolve_read_scope(export.scope.as_ref().unwrap())
        .await
        .unwrap();
    drop(export);
    assert!(worker.is_kept(raw).await.unwrap());
    let reopened = worker.describe(derived, vec![]).await.unwrap().unwrap();
    assert_eq!(reopened.digest, captured.digest);
    assert_eq!(
        worker
            .source_range_dependency(reopened, "input".into(), 76, 2)
            .await
            .unwrap()
            .bytes,
        8080u16.to_be_bytes()
    );
    assert_eq!(
        std::fs::read_to_string(root.path().join("calls")).unwrap(),
        "import\ndecode\ndecode\nchanged\n"
    );
    drop(worker);
    drop(reader);
    drop(session);
    runtime.shutdown().await.unwrap();
}
