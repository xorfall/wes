use super::*;
use std::os::unix::fs::PermissionsExt;

#[tokio::test]
async fn ssh_execution_edit_refresh_and_restoration_use_captured_target_without_launch_on_reopen() {
    let files = tempfile::tempdir().unwrap();
    let client = files.path().join("client");
    let count = files.path().join("calls");
    std::fs::write(&client, format!("#!/bin/sh\nprintf 'call\\n' >> '{}'\nfor last; do :; done\nexec /bin/sh -c \"$last\"\n", count.display())).unwrap();
    std::fs::set_permissions(&client, std::fs::Permissions::from_mode(0o700)).unwrap();
    std::fs::write(files.path().join("key"), b"synthetic key").unwrap();
    std::fs::write(files.path().join("hosts"), b"synthetic hosts").unwrap();
    let recipe = serde_json::json!({"version":1,"targets":{"remote":{
        "kind":"ssh","host":"fixture.invalid","user":"qa","client":client,
        "identity_file":files.path().join("key"),"known_hosts":files.path().join("hosts"),
        "shell":"posix","inherit":"remote","cwd":"/","env":{"QA_REMOTE":"captured"}
    }},"environments":{"qa":{"imports":{"echo":{"source":{"kind":"process","bin":"/bin/echo"},"bind":{"target":"remote"}}}}}});
    let f = Fixture::new(&recipe.to_string());
    f.install().await;
    assert!(!count.exists());
    f.submit("one", ":env use \"qa\"").await;
    f.submit("one", "echo run args:original > first").await;
    f.submit("one", ":env rename \"qa\" to:renamed").await;
    f.submit("one", ":env use \"renamed\"").await;
    // Definition editing must preserve SSH fields and the target namespace.
    f.submit(
        "one",
        ":import process bin:/usr/bin/printenv as:vars target:remote",
    )
    .await;
    f.submit("one", ":env use \"renamed\"").await;
    f.submit("one", "vars run args:QA_REMOTE > variable").await;
    assert_eq!(stdout(&f.value("variable").await), b"captured\n");
    f.submit("one", ":refresh $first").await;
    assert_eq!(stdout(&f.value("first").await), b"original\n");
    assert_eq!(std::fs::read_to_string(&count).unwrap().lines().count(), 3);
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
    // Rebuild after the live launch inputs disappear; no client/file/network probing.
    std::fs::remove_file(&client).unwrap();
    std::fs::remove_file(f.root.path().join("env.yaml")).unwrap();
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
    assert!(!handle.observe().await.unwrap().environment_enabled["renamed"]);
    assert_eq!(std::fs::read_to_string(&count).unwrap().lines().count(), 3);
    handle.shutdown().await.unwrap();
    task.join().await.unwrap();
    f.stop().await;
}

#[tokio::test]
async fn standalone_target_admission_owns_revision_authority_and_package_membership() {
    let files = tempfile::tempdir().unwrap();
    std::fs::write(files.path().join("key"), b"synthetic key").unwrap();
    std::fs::write(files.path().join("hosts"), b"synthetic hosts").unwrap();
    let yaml = serde_json::json!({"version":1,"package":"owner","targets":{"remote":{"kind":"ssh","client":"/not/read/ssh","host":"fixture.invalid","user":"qa","identity_file":files.path().join("key"),"known_hosts":files.path().join("hosts"),"shell":"posix","inherit":"remote"}},"environments":{"qa":{"targets":["remote"]}}});
    let f = Fixture::new(&yaml.to_string());
    f.install().await;
    let revision = f.handle.environment_revisions().await.unwrap()["qa"];
    let lease = f
        .handle
        .capture_execution_target("qa".into(), revision, "remote".into())
        .await
        .unwrap();
    assert_eq!(lease.target.name(), "remote");
    assert!(
        f.handle
            .capture_execution_target("qa".into(), revision, "absent".into())
            .await
            .is_err()
    );
    f.submit("one", ":env disable \"qa\"").await;
    assert!(lease.cancelled.is_cancelled());
    assert!(
        f.handle
            .capture_execution_target("qa".into(), revision, "remote".into())
            .await
            .is_err()
    );
    f.submit("one", ":env enable \"qa\"").await;
    assert!(lease.cancelled.is_cancelled());
    assert!(
        !f.handle
            .capture_execution_target("qa".into(), revision, "remote".into())
            .await
            .unwrap()
            .cancelled
            .is_cancelled()
    );
    // Another package cannot change a destination attached without any provider imports.
    std::fs::write(f.root.path().join("other.yaml"),"version: 1\npackage: other\ntargets: {remote: {kind: local}}\nenvironments: {other: {targets: [remote]}}").unwrap();
    assert!(
        f.handle
            .plan_environment_file("other.yaml".into(), false)
            .await
            .is_err()
    );
    std::fs::write(
        f.root.path().join("env.yaml"),
        yaml.to_string()
            .replace("fixture.invalid", "changed.invalid"),
    )
    .unwrap();
    f.install().await;
    assert!(
        f.handle
            .capture_execution_target("qa".into(), revision, "remote".into())
            .await
            .is_err()
    );
    f.submit("one", ":env rename \"qa\" to:renamed").await;
    let revision = f.handle.environment_revisions().await.unwrap()["renamed"];
    assert_eq!(
        f.handle
            .capture_execution_target("renamed".into(), revision, "remote".into())
            .await
            .unwrap()
            .target
            .name(),
        "remote"
    );
    f.submit("one", ":env retire \"renamed\"").await;
    assert!(
        f.handle
            .capture_execution_target("renamed".into(), revision, "remote".into())
            .await
            .is_err()
    );
    let refused = f
        .handle
        .submit(
            SourceInput::new("delete-with-lease".into(), ":env delete \"renamed\"".into()).unwrap(),
        )
        .await;
    assert!(
        refused.is_err(),
        "a captured terminal target prevents environment deletion until its owner joins"
    );
    drop(lease);
    f.submit("one", ":env delete \"renamed\"").await;
    f.stop().await;
}

#[tokio::test]
async fn builtin_shell_example_preserves_remote_quoting_and_blocks_private_argv() {
    let files = tempfile::tempdir().unwrap();
    let client = files.path().join("client");
    let count = files.path().join("calls");
    std::fs::write(&client, format!("#!/bin/sh\nprintf 'call\\n' >> '{}'\nfor last; do :; done\nexec /bin/sh -c \"$last\"\n", count.display())).unwrap();
    std::fs::set_permissions(&client, std::fs::Permissions::from_mode(0o700)).unwrap();
    std::fs::write(files.path().join("key"), b"synthetic key").unwrap();
    std::fs::write(files.path().join("hosts"), b"synthetic hosts").unwrap();
    let recipe = include_str!("../../../../examples/target-shell/environments.yaml")
        .replace("/usr/bin/ssh", client.to_str().unwrap())
        .replace(
            "/absolute/path/to/identity",
            files.path().join("key").to_str().unwrap(),
        )
        .replace(
            "/absolute/path/to/known_hosts",
            files.path().join("hosts").to_str().unwrap(),
        )
        .replace("timeout_ms: 10000", "timeout_ms: 10000, output: private");
    let f = Fixture::new(&recipe);
    f.install().await;
    assert!(!count.exists());
    for line in include_str!("../../../../examples/target-shell/run.wes")
        .lines()
        .filter(|line| !line.is_empty())
    {
        f.submit("one", line).await;
    }
    assert_eq!(
        stdout(&f.value("shell_result").await),
        b"declared\nquote's; literal"
    );
    assert_eq!(std::fs::read_to_string(&count).unwrap().lines().count(), 1);
    f.submit("one", "sh run cmd:\"exit 1\" > nonzero").await;
    let Data::Record(output) = f.value("nonzero").await.data().clone() else {
        panic!("record");
    };
    assert_eq!(output["exitCode"], Data::Int(1));
    // A zero/negative/over-budget timeout is refused before starting any SSH client.
    f.submit("one", "sh run cmd:true timeout:PT0S > bad_timeout")
        .await;
    assert_eq!(std::fs::read_to_string(&count).unwrap().lines().count(), 2);
    f.submit(
        "one",
        ":calc { return text($shell_result.stdout); } > private_command",
    )
    .await;
    f.submit("one", "sh run cmd:$private_command > denied")
        .await;
    let state = f.handle.snapshot().await.unwrap();
    assert!(
        state
            .execution
            .errors
            .contains_key(&state.names["denied"].node)
    );
    assert_eq!(std::fs::read_to_string(&count).unwrap().lines().count(), 2);
    f.stop().await;
}
