//! Offline synthetic files through real declarative providers and native encrypted result readers.
use std::{
    io::{Read, Seek, SeekFrom, Write},
    path::Path,
    sync::Arc,
    time::Duration,
};
use wes::runtime::{RuntimeOptions, launch};
use wes_core::Data;
use wes_engine::{
    session::{SessionHandle, SubmissionResult},
    source::SourceInput,
};
const SENTINEL: &[u8] = b"synthetic-offline-capture-sentinel!";

async fn submit(session: &SessionHandle, source: &str) -> Arc<SubmissionResult> {
    let result = session
        .submit(SourceInput::new(uuid::Uuid::new_v4().to_string(), source.into()).unwrap())
        .await
        .unwrap();
    assert!(
        !result
            .diagnostics
            .diagnostics
            .iter()
            .any(|d| d.severity == wes_language::Severity::Error),
        "{result:?}"
    );
    tokio::time::timeout(Duration::from_secs(20), session.wait_idle())
        .await
        .unwrap()
        .unwrap();
    result
}
fn synthetic_capture(path: &Path) -> u64 {
    let mut file = std::fs::File::create(path).unwrap();
    file.write_all(&[0xd4, 0xc3, 0xb2, 0xa1]).unwrap();
    file.write_all(&2u16.to_le_bytes()).unwrap();
    file.write_all(&4u16.to_le_bytes()).unwrap();
    for n in [0u32, 0, 65535, 1] {
        file.write_all(&n.to_le_bytes()).unwrap();
    }
    let mut payload = vec![0x93; 65535];
    payload[..SENTINEL.len()].copy_from_slice(SENTINEL);
    for n in 0u32..640 {
        for number in [n, 0, 65535, 65535] {
            file.write_all(&number.to_le_bytes()).unwrap();
        }
        file.write_all(&payload).unwrap();
    }
    file.metadata().unwrap().len()
}
fn no_plaintext(path: &Path) {
    for entry in std::fs::read_dir(path).unwrap() {
        let p = entry.unwrap().path();
        if p.is_dir() {
            no_plaintext(&p);
        } else {
            assert!(
                !std::fs::read(&p)
                    .unwrap()
                    .windows(SENTINEL.len())
                    .any(|w| w == SENTINEL),
                "{}",
                p.display()
            );
        }
    }
}
#[tokio::test]
async fn offline_import_reads_exact_large_encrypted_raw_and_index_after_input_removal() {
    for policy in ["confidential", "confidential-temporary"] {
        let root = tempfile::tempdir().unwrap();
        let package = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../examples/tracing-offline");
        let capture = root.path().join("synthetic.pcap");
        let size = synthetic_capture(&capture);
        assert!(size > wes_budgets::get("process.output.bytes"));
        let mut file = std::fs::File::open(&capture).unwrap();
        file.seek(SeekFrom::Start(65574)).unwrap();
        let mut expected = [0; 10];
        file.read_exact(&mut expected).unwrap();
        drop(file);
        let provider = std::fs::read_to_string(package.join("provider.yaml")).unwrap();
        let adapter = package.join("capture_adapter.py");
        // Pin the test interpreter explicitly, including hosts without executable shebangs.
        let python = wes_adapters::process::serialized_spawn(|| {
            std::process::Command::new("python3")
                .args(["-c", "import sys; print(sys.executable)"])
                .output()
        })
        .unwrap();
        assert!(python.status.success());
        let python = String::from_utf8(python.stdout).unwrap();
        let provider = provider
            .replace(
                "bin: capture_adapter.py",
                &format!("bin: {}", serde_json::to_string(python.trim()).unwrap()),
            )
            .replace(
                "argv: [import,",
                &format!(
                    "argv: [{}, import,",
                    serde_json::to_string(adapter.to_str().unwrap()).unwrap()
                ),
            );
        std::fs::write(root.path().join("provider.yaml"), provider).unwrap();
        std::fs::write(root.path().join("env.yaml"), format!("version: 1\ntargets:\n  local: {{kind: local}}\nenvironments:\n  lab:\n    imports:\n      capture:\n        source: {{kind: provider, file: provider.yaml}}\n        bind: {{target: local, output: {policy}}}\n")).unwrap();
        let key = root.path().join("key");
        std::fs::write(&key, [47; 32]).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&key, std::fs::Permissions::from_mode(0o600)).unwrap();
        }
        let home = root.path().join("home");
        let mut options = RuntimeOptions::new(home.clone(), root.path().into());
        options.storage_key_file = Some(key);
        options.docker_candidates = Some(vec![root.path().join("unavailable.sock")]);
        let runtime = launch(options).await.unwrap();
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
                ":transient {{ capture import file:{} > evidence }} > offline",
                serde_json::to_string(capture.to_str().unwrap()).unwrap()
            ),
        )
        .await;
        let export = session.read_sandbox_export("offline", false).await.unwrap();
        let scope = export.scope.clone().unwrap();
        drop(export);
        let (child, _) = session.resolve_read_scope(&scope).await.unwrap();
        child.wait_idle().await.unwrap();
        let before = child.snapshot().await.unwrap();
        assert!(
            before.execution.errors.is_empty(),
            "{:?}",
            before.execution.errors
        );
        let node = before.names["evidence"].node.clone();
        let run = before.execution.runs[&node].clone();
        assert!(before.execution.stored_values.contains_key(&node));
        drop(before);
        std::fs::remove_file(capture).unwrap();
        let indexed = submit(&child, ":calc pure { return $evidence.index; } > index").await;
        let ranged = submit(
            &child,
            ":read bytes:$evidence from:\"65574\" limit:10 select:\"raw\" > bytes",
        )
        .await;
        let after = child.snapshot().await.unwrap();
        assert_eq!(after.execution.runs[&node], run);
        assert!(
            after.execution.errors.is_empty(),
            "{:?}",
            after.execution.errors
        );
        let index = &after.execution.values[indexed.nodes.last().unwrap()];
        assert!(index.provenance().policy().is_confidential());
        let Data::Record(fields) = index.data() else {
            panic!("index");
        };
        assert_eq!(fields["rawBytes"], Data::Text(size.to_string().into()));
        assert_eq!(fields["packetCount"], Data::Text("640".into()));
        assert_eq!(fields["indexComplete"], Data::Bool(false));
        assert!(matches!(&fields["packets"], Data::List(rows) if rows.len() == 512));
        let range = &after.execution.values[ranged.nodes.last().unwrap()];
        assert!(range.provenance().policy().is_confidential());
        let Data::Record(fields) = range.data() else {
            panic!("range");
        };
        assert_eq!(fields["extent"], Data::Text(size.to_string().into()));
        assert_eq!(fields["data"], Data::Bytes(expected.to_vec().into()));
        drop(after);
        let observed = child.observe().await.unwrap();
        let handle = observed.values.as_ref().unwrap().outputs[&node]
            .handle()
            .unwrap()
            .clone();
        drop(observed);
        let receipt = child.keep(handle).await.unwrap();
        if policy == "confidential" {
            assert!(receipt.affected && receipt.problem.is_none(), "{receipt:?}");
        } else {
            assert!(
                !receipt.affected && receipt.problem.is_some(),
                "{receipt:?}"
            );
        }
        drop(child);
        drop(session);
        runtime.shutdown().await.unwrap();
        no_plaintext(&home);
    }
}
