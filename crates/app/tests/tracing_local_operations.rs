//! Public local trace workflow through actual YAML helpers, synthetic PCAP and isolated homes.
#![cfg(unix)]
use std::{os::unix::fs::PermissionsExt, path::Path, time::Duration};
use wes::runtime::{RuntimeOptions, launch};
use wes_core::Data;
use wes_engine::{
    session::{SessionHandle, SubmissionResult},
    source::SourceInput,
    storage::AutoKeep,
};

async fn submit(session: &SessionHandle, text: &str) -> std::sync::Arc<SubmissionResult> {
    let result = session
        .submit(SourceInput::new(uuid::Uuid::new_v4().to_string(), text.into()).unwrap())
        .await
        .unwrap();
    tokio::time::timeout(Duration::from_secs(20), session.wait_idle())
        .await
        .unwrap()
        .unwrap();
    result
}
async fn accepted(session: &SessionHandle, text: &str) {
    let result = submit(session, text).await;
    assert!(
        !result
            .diagnostics
            .diagnostics
            .iter()
            .any(|d| d.severity == wes_language::Severity::Error),
        "{text}: {result:?}"
    );
}
fn capture() -> Vec<u8> {
    let mut bytes = vec![0xd4, 0xc3, 0xb2, 0xa1, 2, 0, 4, 0];
    for n in [0u32, 0, 65535, 1, 1, 0, 54, 54] {
        bytes.extend(n.to_le_bytes());
    }
    bytes.extend([2, 0, 0, 0, 0, 2, 2, 0, 0, 0, 0, 1, 8, 0]);
    bytes.extend([
        0x45, 0, 0, 40, 0, 7, 0, 0, 64, 6, 0, 0, 192, 0, 2, 10, 198, 51, 100, 20,
    ]);
    bytes.extend([
        0xd4, 0x31, 0x1f, 0x90, 0, 0, 0, 100, 0, 0, 0, 0, 0x50, 2, 0x10, 0, 0, 0, 0, 0,
    ]);
    bytes
}

#[tokio::test]
async fn local_trace_import_analyze_export_and_retain_keep_exact_original_dependency_after_reopen()
{
    let temp = tempfile::tempdir().unwrap();
    let base = temp.path().canonicalize().unwrap();
    let examples = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../examples");
    for (directory, helper, file) in [
        ("tracing-offline", "capture_adapter.py", "files.yaml"),
        ("tracing-analysis", "analyze_capture.py", "decoder.yaml"),
        ("byte-export", "export_bytes.py", "writer.yaml"),
    ] {
        let bin = examples
            .join(directory)
            .join(helper)
            .canonicalize()
            .unwrap();
        let source =
            std::fs::read_to_string(examples.join(directory).join("provider.yaml")).unwrap();
        let source = if directory == "tracing-analysis" {
            let mut doc: serde_json::Value = serde_json::from_str(&source).unwrap();
            doc["backend"]["bin"] = serde_json::json!(bin);
            doc.to_string()
        } else {
            source.replace(
                &format!("bin: {helper}"),
                &format!("bin: {}", serde_json::to_string(&bin).unwrap()),
            )
        };
        std::fs::write(base.join(file), source).unwrap();
    }
    std::fs::write(base.join("env.yaml"),"version: 1\ntargets:\n  host: {kind: local}\nenvironments:\n  lab:\n    imports:\n      files:\n        source: {kind: provider, file: files.yaml}\n        bind: {target: host, output: confidential}\n      decoder:\n        source: {kind: provider, file: decoder.yaml}\n        bind: {target: host, output: public}\n      writer:\n        source: {kind: provider, file: writer.yaml}\n        bind: {target: host, output: public}\n").unwrap();
    let key = base.join("key");
    std::fs::write(&key, [71; 32]).unwrap();
    std::fs::set_permissions(&key, std::fs::Permissions::from_mode(0o600)).unwrap();
    let options = || {
        let mut options = RuntimeOptions::new(base.join("home"), base.clone());
        options.storage_key_file = Some(key.clone());
        options.auto_keep = AutoKeep::Never;
        options.docker_candidates = Some(vec![base.join("unavailable.sock")]);
        options
    };
    let original = capture();
    let input = base.join("synthetic.pcap");
    std::fs::write(&input, &original).unwrap();
    let runtime = launch(options()).await.unwrap();
    let session = runtime.handle.current().unwrap().session;
    let plan = session
        .plan_environment_file("env.yaml".into(), false)
        .await
        .unwrap();
    session.apply_environments(plan).await.unwrap();
    accepted(&session, ":env use \"lab\"").await;
    accepted(
        &session,
        &format!(
            ":trace import using:\"pcap\" file:{} > imported",
            serde_json::to_string(&input).unwrap()
        ),
    )
    .await;
    let state = session.snapshot().await.unwrap();
    assert!(
        state.execution.errors.is_empty(),
        "{:?}",
        state.execution.errors
    );
    let source = state.names["imported"].node.clone();
    let run = state.execution.runs[&source].clone();
    drop(state);
    std::fs::remove_file(&input).unwrap();
    for source in [
        ":trace analyze $imported.raw using:\"missing\" options:{packets:1,headerBytes:128}",
        ":trace analyze $imported.raw using:\"headers\" options:{packets:0,headerBytes:128}",
        ":trace analyze $imported.raw using:\"headers\" options:{packets:1,headerBytes:128,source:'copied'}",
        ":trace analyze $imported.raw using:\"headers\" options:{packets:1,unknown:1}",
        ":trace analyze $imported.raw using:\"headers\" options:{packets:$imported,headerBytes:128}",
        ":trace import file:\"missing.pcap\" using:\"headers\"",
        ":trace export $imported.raw using:\"pcap\" file:$imported",
        ":trace fetch $imported",
    ] {
        let result = submit(&session, source).await;
        assert!(
            result
                .diagnostics
                .diagnostics
                .iter()
                .any(|d| d.severity == wes_language::Severity::Error),
            "{source}: {result:?}"
        );
    }
    accepted(&session,":trace analyze $imported.raw using:\"headers\" options:{packets:1,headerBytes:128} > decoded").await;
    accepted(&session, ":calc pure { return $decoded; } > facts").await;
    let state = session.snapshot().await.unwrap();
    assert!(
        state.execution.errors.is_empty(),
        "{:?}",
        state.execution.errors
    );
    assert_eq!(state.execution.runs[&source], run);
    let value = &state.execution.values[&state.names["facts"].node];
    assert!(value.provenance().policy().is_confidential());
    let Data::Record(fields) = value.data() else {
        panic!("{value:?}")
    };
    let Data::List(packets) = &fields["packets"] else {
        panic!()
    };
    let Data::Record(packet) = &packets[0] else {
        panic!()
    };
    assert_eq!(packet["protocol"], Data::Text("TCP".into()));
    drop(state);
    let output = base.join("explicit.pcap");
    let path = serde_json::to_string(&output).unwrap();
    accepted(
        &session,
        &format!(":trace export $imported.raw using:\"pcap\" file:{path} > noApproval"),
    )
    .await;
    assert!(!output.exists());
    let state = session.snapshot().await.unwrap();
    assert!(
        state
            .execution
            .errors
            .contains_key(&state.names["noApproval"].node)
    );
    drop(state);
    accepted(&session,&format!(":trace export $imported.raw using:\"pcap\" file:{path} allow_confidential:true > receipt")).await;
    assert_eq!(std::fs::read(&output).unwrap(), original);
    accepted(&session, ":trace retain $decoded > retained").await;
    let state = session.snapshot().await.unwrap();
    assert!(
        !state
            .execution
            .errors
            .contains_key(&state.names["retained"].node),
        "{:?}",
        state.execution.errors
    );
    let retained = state.names["retained"].node.clone();
    let retained_run = state.execution.runs[&retained].clone();
    drop(state);
    drop(session);
    runtime.shutdown().await.unwrap();
    let runtime = launch(options()).await.unwrap();
    let session = runtime.handle.current().unwrap().session;
    let state = session.snapshot().await.unwrap();
    assert_eq!(state.execution.runs[&retained], retained_run);
    drop(state);
    accepted(
        &session,
        ":read bytes:$retained dependency:\"input\" from:\"0\" limit:94 > raw",
    )
    .await;
    let state = session.snapshot().await.unwrap();
    let value = state
        .execution
        .values
        .get(&state.names["raw"].node)
        .unwrap_or_else(|| panic!("{:?}", state.execution.errors));
    let Data::Record(fields) = value.data() else {
        panic!("{value:?}")
    };
    assert_eq!(fields["data"], Data::Bytes(original.into()));
    assert!(value.provenance().policy().is_confidential());
    drop(state);
    drop(session);
    runtime.shutdown().await.unwrap();
}

#[tokio::test]
async fn remote_import_profile_refuses_before_any_client_entry() {
    let temp = tempfile::tempdir().unwrap();
    let base = temp.path().canonicalize().unwrap();
    let client = base.join("synthetic-client");
    std::fs::write(
        &client,
        format!(
            "#!/bin/sh\nprintf x >> '{}'\nexit 98\n",
            base.join("entered").display()
        ),
    )
    .unwrap();
    std::fs::set_permissions(&client, std::fs::Permissions::from_mode(0o700)).unwrap();
    let provider =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../examples/tracing-offline/provider.yaml");
    let source = std::fs::read_to_string(provider)
        .unwrap()
        .replace("bin: capture_adapter.py", "bin: /synthetic/capture-adapter");
    std::fs::write(base.join("provider.yaml"), source).unwrap();
    std::fs::write(base.join("key"), "synthetic identity").unwrap();
    std::fs::write(base.join("hosts"), "").unwrap();
    std::fs::write(base.join("env.yaml"),format!("version: 1\ntargets:\n  host: {{kind: ssh, host: 192.0.2.18, user: fixture, port: 2222, client: {}, identity_file: {}, known_hosts: {}, shell: posix, inherit: remote}}\nenvironments:\n  lab:\n    imports:\n      files:\n        source: {{kind: provider, file: provider.yaml}}\n        bind: {{target: host, output: public}}\n",serde_json::to_string(&client).unwrap(),serde_json::to_string(&base.join("key")).unwrap(),serde_json::to_string(&base.join("hosts")).unwrap())).unwrap();
    let mut options = RuntimeOptions::new(base.join("home"), base.clone());
    options.docker_candidates = Some(vec![base.join("unavailable.sock")]);
    let runtime = launch(options).await.unwrap();
    let session = runtime.handle.current().unwrap().session;
    let plan = session
        .plan_environment_file("env.yaml".into(), false)
        .await
        .unwrap();
    session.apply_environments(plan).await.unwrap();
    accepted(&session, ":env use \"lab\"").await;
    let result = submit(
        &session,
        ":trace import file:\"synthetic.pcap\" using:\"pcap\"",
    )
    .await;
    assert!(
        result
            .diagnostics
            .diagnostics
            .iter()
            .any(|d| d.severity == wes_language::Severity::Error && d.code.as_ref() == "TRC003"),
        "{result:?}"
    );
    assert!(!base.join("entered").exists());
    drop(session);
    runtime.shutdown().await.unwrap();
}
