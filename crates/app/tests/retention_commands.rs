//! Native retention uses the acknowledged original output, never a producer replay.
#![cfg(unix)]
use std::{os::unix::fs::PermissionsExt, time::Duration};
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
fn no_plaintext(path: &std::path::Path, needle: &[u8]) {
    for entry in std::fs::read_dir(path).unwrap() {
        let path = entry.unwrap().path();
        if path.is_dir() {
            no_plaintext(&path, needle);
        } else {
            assert!(
                !std::fs::read(&path)
                    .unwrap()
                    .windows(needle.len())
                    .any(|w| w == needle),
                "{}",
                path.display()
            );
        }
    }
}

#[tokio::test]
async fn keep_freezes_original_identity_and_confidential_policy_across_refresh_and_reopen() {
    for policy in ["public", "confidential"] {
        let root = tempfile::tempdir().unwrap();
        let home = root.path().join("home");
        let key = root.path().join("key");
        std::fs::write(&key, [43; 32]).unwrap();
        std::fs::set_permissions(&key, std::fs::Permissions::from_mode(0o600)).unwrap();
        let payload = b"synthetic-retention-payload-sentinel";
        std::fs::write(root.path().join("payload.bin"), payload).unwrap();
        std::fs::write(root.path().join("env.yaml"),format!("version: 1\ntargets:\n  local: {{kind: local, cwd: {}}}\nenvironments:\n  lab:\n    imports:\n      sh:\n        source: {{kind: builtin, name: sh}}\n        bind: {{target: local, output: {policy}}}\n", serde_json::to_string(root.path()).unwrap())).unwrap();
        let options = || {
            let mut options = RuntimeOptions::new(home.clone(), root.path().into());
            options.storage_key_file = Some(key.clone());
            options.auto_keep = AutoKeep::Never;
            options.docker_candidates = Some(vec![root.path().join("unavailable.sock")]);
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
            "sh run cmd:\"printf x >> attempts; cat payload.bin\" > source",
        )
        .await;
        let observed = session.observe().await.unwrap();
        assert!(
            observed.state.execution.errors.is_empty(),
            "{:?}",
            observed.state.execution.errors
        );
        let original = observed.values.as_ref().unwrap().outputs
            [&observed.state.names["source"].node]
            .handle()
            .unwrap()
            .clone();
        drop(observed);
        submit(&session, ":keep $source > saved").await;
        let observed = session.observe().await.unwrap();
        assert!(
            observed.state.execution.errors.is_empty(),
            "{:?}",
            observed.state.execution.errors
        );
        let saved = observed.state.names["saved"].node.clone();
        let publication = &observed.values.as_ref().unwrap().outputs[&saved];
        assert_eq!(publication.handle(), Some(&original));
        let wes_engine::session::ValuePublication::Complete(publication) = publication else {
            panic!("{publication:?}")
        };
        assert!(publication.durably_retained(), "{publication:?}");
        let run = observed.state.execution.runs[&saved].clone();
        drop(observed);
        std::fs::write(root.path().join("payload.bin"), b"new synthetic revision").unwrap();
        submit(&session, ":refresh $source").await;
        submit(&session, ":calc pure { return $saved.stdout; } > selected").await;
        let state = session.snapshot().await.unwrap();
        assert!(
            state.execution.errors.is_empty(),
            "{:?}",
            state.execution.errors
        );
        let value = &state.execution.values[&state.names["selected"].node];
        assert_eq!(value.data(), &Data::Bytes(payload.to_vec().into()));
        assert_eq!(
            value.provenance().policy().is_confidential(),
            policy == "confidential"
        );
        drop(state);
        drop(session);
        runtime.shutdown().await.unwrap();
        assert_eq!(std::fs::read(root.path().join("attempts")).unwrap(), b"xx");
        if policy == "confidential" {
            no_plaintext(&home, payload);
        }
        let runtime = launch(options()).await.unwrap();
        let session = runtime.handle.current().unwrap().session;
        let state = session.snapshot().await.unwrap();
        assert_eq!(state.execution.runs[&saved], run);
        drop(state);
        submit(&session, ":calc pure { return $saved.stdout; } > reopened").await;
        let state = session.snapshot().await.unwrap();
        assert!(
            state.execution.errors.is_empty(),
            "{:?}",
            state.execution.errors
        );
        assert_eq!(
            state.execution.values[&state.names["reopened"].node].data(),
            &Data::Bytes(payload.to_vec().into())
        );
        assert_eq!(std::fs::read(root.path().join("attempts")).unwrap(), b"xx");
        drop(state);
        drop(session);
        runtime.shutdown().await.unwrap();
        if policy == "confidential" {
            no_plaintext(&home, payload);
        }
    }
}

#[tokio::test]
async fn private_and_temporary_roots_refuse_keep_without_replaying_or_promoting_policy() {
    for policy in ["private", "confidential-temporary"] {
        let root = tempfile::tempdir().unwrap();
        let key = root.path().join("key");
        std::fs::write(&key, [44; 32]).unwrap();
        std::fs::set_permissions(&key, std::fs::Permissions::from_mode(0o600)).unwrap();
        std::fs::write(
            root.path().join("payload.bin"),
            b"synthetic-private-root-sentinel",
        )
        .unwrap();
        std::fs::write(root.path().join("env.yaml"),format!("version: 1\ntargets:\n  local: {{kind: local, cwd: {}}}\nenvironments:\n  lab:\n    imports:\n      sh:\n        source: {{kind: builtin, name: sh}}\n        bind: {{target: local, output: {policy}}}\n",serde_json::to_string(root.path()).unwrap())).unwrap();
        let mut options = RuntimeOptions::new(root.path().join("home"), root.path().into());
        options.storage_key_file = Some(key);
        options.auto_keep = AutoKeep::Never;
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
            "sh run cmd:\"printf x >> attempts; cat payload.bin\" > source",
        )
        .await;
        submit(&session, ":keep $source > refused").await;
        let state = session.snapshot().await.unwrap();
        assert!(
            state
                .execution
                .errors
                .contains_key(&state.names["refused"].node),
            "{:?}",
            state.execution
        );
        let original = &state.execution.values[&state.names["source"].node];
        assert_eq!(
            original.provenance().policy().is_private(),
            policy == "private"
        );
        assert!(!original.provenance().policy().allows_retention());
        assert_eq!(std::fs::read(root.path().join("attempts")).unwrap(), b"x");
        drop(state);
        drop(session);
        runtime.shutdown().await.unwrap();
        no_plaintext(
            &root.path().join("home"),
            b"synthetic-private-root-sentinel",
        );
    }
}

#[tokio::test]
async fn cooperative_read_access_does_not_grant_retention() {
    let root = tempfile::tempdir().unwrap();
    let mut options = RuntimeOptions::new(root.path().join("home"), root.path().into());
    options.auto_keep = AutoKeep::Never;
    options.docker_candidates = Some(vec![root.path().join("unavailable.sock")]);
    let runtime = launch(options).await.unwrap();
    let session = runtime.handle.current().unwrap().session;
    submit(
        &session,
        ":calc pure { return 'synthetic public value'; } > source",
    )
    .await;
    let result = session
        .submit(
            SourceInput::new(
                uuid::Uuid::new_v4().to_string(),
                ":keep $source > refused".into(),
            )
            .unwrap()
            .with_client("synthetic-reader".into())
            .unwrap()
            .cooperative(),
        )
        .await;
    session.wait_idle().await.unwrap();
    let state = session.snapshot().await.unwrap();
    assert!(
        result.is_err()
            || result
                .as_ref()
                .unwrap()
                .diagnostics
                .diagnostics
                .iter()
                .any(|d| d.severity == wes_language::Severity::Error)
            || state
                .names
                .get("refused")
                .is_some_and(|node| state.execution.errors.contains_key(&node.node)),
        "{result:?} {state:?}"
    );
    drop(state);
    drop(session);
    runtime.shutdown().await.unwrap();
}
