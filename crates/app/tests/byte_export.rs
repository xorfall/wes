//! Actual declarative writer with synthetic bytes and isolated storage homes.
#![cfg(unix)]
use std::{os::unix::fs::PermissionsExt, path::Path, time::Duration};
use wes::runtime::{RuntimeOptions, launch};
use wes_core::Data;
use wes_engine::{session::SessionHandle, source::SourceInput, storage::AutoKeep};

async fn submit(session: &SessionHandle, text: &str) {
    let result = session
        .submit(SourceInput::new(uuid::Uuid::new_v4().to_string(), text.into()).unwrap())
        .await
        .unwrap();
    assert!(
        !result
            .diagnostics
            .diagnostics
            .iter()
            .any(|d| d.severity == wes_language::Severity::Error),
        "{text}: {result:?}"
    );
    tokio::time::timeout(Duration::from_secs(20), session.wait_idle())
        .await
        .unwrap()
        .unwrap();
}

#[tokio::test]
async fn file_export_has_separate_literal_egress_permission_without_source_replay() {
    for policy in [
        "public",
        "confidential",
        "confidential-temporary",
        "private",
    ] {
        let root = tempfile::tempdir().unwrap();
        let base = root.path().canonicalize().unwrap();
        let example = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../examples/byte-export");
        let helper = example.join("export_bytes.py").canonicalize().unwrap();
        let provider = std::fs::read_to_string(example.join("provider.yaml"))
            .unwrap()
            .replace(
                "bin: export_bytes.py",
                &format!("bin: {}", serde_json::to_string(&helper).unwrap()),
            );
        std::fs::write(base.join("provider.yaml"), provider).unwrap();
        let key = base.join("key");
        std::fs::write(&key, [93; 32]).unwrap();
        std::fs::set_permissions(&key, std::fs::Permissions::from_mode(0o600)).unwrap();
        let payload = b"synthetic-byte-export-sentinel\x00\xff";
        std::fs::write(base.join("payload.bin"), payload).unwrap();
        std::fs::write(base.join("env.yaml"),format!("version: 1\ntargets:\n  local: {{kind: local, cwd: {}}}\nenvironments:\n  lab:\n    imports:\n      sh:\n        source: {{kind: builtin, name: sh}}\n        bind: {{target: local, output: {policy}}}\n      bytes:\n        source: {{kind: provider, file: provider.yaml}}\n        bind: {{target: local, output: public}}\n",serde_json::to_string(&base).unwrap())).unwrap();
        let mut options = RuntimeOptions::new(base.join("home"), base.clone());
        options.auto_keep = AutoKeep::Never;
        options.storage_key_file = Some(key);
        options.docker_candidates = Some(vec![base.join("unavailable.sock")]);
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
            "sh run cmd:\"printf x >> attempts; cat payload.bin\" > original",
        )
        .await;
        let file = base.join("export.bin");
        let file_arg = serde_json::to_string(&file).unwrap();
        submit(
            &session,
            &format!("bytes export source:$original.stdout file:{file_arg} > unapproved"),
        )
        .await;
        let state = session.snapshot().await.unwrap();
        assert_eq!(
            state
                .execution
                .errors
                .contains_key(&state.names["unapproved"].node),
            policy != "public",
            "{:?}",
            state.execution.errors
        );
        assert_eq!(file.exists(), policy == "public");
        drop(state);
        if file.exists() {
            std::fs::remove_file(&file).unwrap();
        }
        submit(&session,&format!("bytes export source:$original.stdout file:{file_arg} allow_confidential:true > approved")).await;
        if policy != "private" && file.exists() {
            submit(&session, ":calc pure { return $approved; } > receipt").await;
        }
        let state = session.snapshot().await.unwrap();
        assert_eq!(
            state
                .execution
                .errors
                .contains_key(&state.names["approved"].node),
            policy == "private",
            "{:?}",
            state.execution.errors
        );
        assert_eq!(file.exists(), policy != "private");
        if policy != "private" {
            assert_eq!(std::fs::read(&file).unwrap(), payload);
            let receipt = &state.execution.values[&state.names["receipt"].node];
            assert_eq!(
                receipt.provenance().policy().is_confidential(),
                policy != "public"
            );
            let Data::Record(fields) = receipt.data() else {
                panic!("{receipt:?}")
            };
            assert_eq!(
                fields["bytes"],
                Data::Text(payload.len().to_string().into())
            );
            assert_eq!(fields["published"], Data::Bool(true));
        }
        drop(state);
        submit(
            &session,
            &format!(":calc pure {{ return {file_arg}; }} > destination"),
        )
        .await;
        submit(
            &session,
            &format!(":calc pure {{ return {{file:{file_arg},approve:true}}; }} > arguments"),
        )
        .await;
        let absent = base.join("not-exported.bin");
        for text in [
            "bytes export source:$original.stdout file:$destination allow_confidential:true".into(),
            "bytes export source:$original.stdout file:$arguments.file allow_confidential:true"
                .into(),
            format!(
                "bytes export source:$original.stdout file:{} allow_confidential:$arguments.approve",
                serde_json::to_string(&absent).unwrap()
            ),
        ] {
            let result = session
                .submit(SourceInput::new(uuid::Uuid::new_v4().to_string(), text).unwrap())
                .await
                .unwrap();
            session.wait_idle().await.unwrap();
            assert!(
                result
                    .diagnostics
                    .diagnostics
                    .iter()
                    .any(|d| d.severity == wes_language::Severity::Error),
                "{result:?}"
            );
            assert!(!absent.exists());
        }
        if policy != "private" {
            submit(&session,&format!("bytes export source:$original.stdout file:{file_arg} allow_confidential:true > collision")).await;
            let state = session.snapshot().await.unwrap();
            assert!(
                state
                    .execution
                    .errors
                    .contains_key(&state.names["collision"].node)
            );
            assert_eq!(std::fs::read(&file).unwrap(), payload);
        }
        submit(
            &session,
            ":calc pure { return $original.stdout; } > stillReadable",
        )
        .await;
        let state = session.snapshot().await.unwrap();
        assert_eq!(
            state.execution.values[&state.names["stillReadable"].node].data(),
            &Data::Bytes(payload.to_vec().into())
        );
        assert_eq!(std::fs::read(base.join("attempts")).unwrap(), b"x");
        assert!(!std::fs::read_dir(&base).unwrap().any(|entry| {
            entry
                .unwrap()
                .file_name()
                .to_string_lossy()
                .starts_with(".wes-export-")
        }));
        drop(state);
        drop(session);
        runtime.shutdown().await.unwrap();
        if policy != "public" {
            fn scan(path: &Path, needle: &[u8]) {
                for entry in std::fs::read_dir(path).unwrap() {
                    let p = entry.unwrap().path();
                    if p.is_dir() {
                        scan(&p, needle)
                    } else {
                        assert!(
                            !std::fs::read(&p)
                                .unwrap()
                                .windows(needle.len())
                                .any(|w| w == needle),
                            "{}",
                            p.display()
                        )
                    }
                }
            }
            scan(&base.join("home"), payload);
        }
    }
}
