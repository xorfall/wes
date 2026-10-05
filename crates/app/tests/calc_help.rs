//! Operation help and executable examples share the language/runtime boundary.
use serde_json::{Value, json};
use std::time::Duration;
use wes::runtime::{RuntimeOptions, launch};
use wes_adapters::codec::{Limits, encode_json};
use wes_engine::source::SourceInput;

#[tokio::test]
async fn function_help_and_its_self_contained_examples_are_usable() {
    let root = tempfile::tempdir().unwrap();
    let runtime = launch(RuntimeOptions::new(
        root.path().join("home"),
        root.path().into(),
    ))
    .await
    .unwrap();
    let session = runtime.handle.current().unwrap().session;
    let mut count = 0;
    for (name, spec) in wes_language::calc::Package::standard().operations() {
        let help = spec.operation.help();
        let reply = session
            .submit(SourceInput::new(format!("help-{name}"), format!(":help calc {name}")).unwrap())
            .await
            .unwrap();
        assert!(
            reply.diagnostics.diagnostics.is_empty(),
            "{name}: {reply:?}"
        );
        tokio::time::timeout(Duration::from_secs(10), session.wait_idle())
            .await
            .unwrap()
            .unwrap();
        let snapshot = session.snapshot().await.unwrap();
        let value = snapshot
            .execution
            .values
            .get(&reply.nodes[0])
            .expect("help value");
        let data: Value =
            serde_json::from_slice(&encode_json(value.data(), Limits::default()).unwrap()).unwrap();
        assert_eq!(data["path"], format!("calc {name}"));
        assert_eq!(data["invocation"]["returns"], help.returns);
        assert_eq!(
            data["invocation"]["parameters"].as_array().unwrap().len(),
            usize::from(spec.max)
        );
        for (i, parameter) in data["invocation"]["parameters"]
            .as_array()
            .unwrap()
            .iter()
            .enumerate()
        {
            assert_eq!(parameter["required"], i < usize::from(spec.min), "{name}");
        }
        let source = data["invocation"]["examples"][0].as_str().unwrap();
        if !help.prerequisites.is_empty() {
            continue;
        } // Dependencies are stated, never silently provisioned.
        let reply = session
            .submit(SourceInput::new(format!("example-{name}"), source.into()).unwrap())
            .await
            .unwrap();
        assert!(
            reply.diagnostics.diagnostics.is_empty(),
            "{name}: {reply:?}"
        );
        tokio::time::timeout(Duration::from_secs(10), session.wait_idle())
            .await
            .unwrap()
            .unwrap();
        let snapshot = session.snapshot().await.unwrap();
        let value = snapshot
            .execution
            .values
            .get(&reply.nodes[0])
            .unwrap_or_else(|| panic!("{name}: {:?}", snapshot.execution.errors));
        if name == "iter.matches" {
            let data: Value =
                serde_json::from_slice(&encode_json(value.data(), Limits::default()).unwrap())
                    .unwrap();
            assert_eq!(data, json!(["503", "200"]));
        }
        if name == "iter.captures" {
            let data: Value =
                serde_json::from_slice(&encode_json(value.data(), Limits::default()).unwrap())
                    .unwrap();
            assert_eq!(
                data,
                json!([{ "match":"a=12", "groups":[{"kind":"some","value":"a"},{"kind":"some","value":"12"}]},{"match":"b=7", "groups":[{"kind":"some","value":"b"},{"kind":"some","value":"7"}]}])
            );
        }
        count += 1;
    }
    assert!(count >= 55);
    // An optional unmatched group remains none; the full match is a separate field.
    let reply = session
        .submit(
            SourceInput::new(
                "optional-captures".into(),
                ":calc { return collect(iter.captures('a b7','([a-z])([0-9]+)?')); }".into(),
            )
            .unwrap(),
        )
        .await
        .unwrap();
    session.wait_idle().await.unwrap();
    let snapshot = session.snapshot().await.unwrap();
    let data: Value = serde_json::from_slice(
        &encode_json(
            snapshot.execution.values[&reply.nodes[0]].data(),
            Limits::default(),
        )
        .unwrap(),
    )
    .unwrap();
    assert_eq!(
        data[0]["groups"],
        json!([{"kind":"some","value":"a"},{"kind":"none"}])
    );
}

#[tokio::test]
async fn shadowing_example_executes_with_independent_workspace_names() {
    let root = tempfile::tempdir().unwrap();
    let runtime = launch(RuntimeOptions::new(
        root.path().join("home"),
        root.path().into(),
    ))
    .await
    .unwrap();
    let session = runtime.handle.current().unwrap().session;
    let reply = session
        .submit(
            SourceInput::new(
                "shadowing-example".into(),
                include_str!("../../../examples/lexical-shadowing/program.wes").into(),
            )
            .unwrap(),
        )
        .await
        .unwrap();
    assert!(reply.diagnostics.diagnostics.is_empty(), "{reply:?}");
    session.wait_idle().await.unwrap();
    let snapshot = session.snapshot().await.unwrap();
    let node = &snapshot.names["report"].node;
    let value = &snapshot.execution.values[node];
    let data: Value =
        serde_json::from_slice(&encode_json(value.data(), Limits::default()).unwrap()).unwrap();
    assert_eq!(
        data,
        json!({"duration":5,"count":3,"helper":7,"values":[6,7],"method":2,"localCall":9,"workspace":20})
    );
}

#[cfg(unix)]
#[tokio::test]
async fn reconstruction_separates_pure_staleness_from_provider_execution_warnings() {
    use std::process::Command;
    let root = tempfile::tempdir().unwrap();
    let home = root.path().join("home");
    std::fs::write(root.path().join("env.yaml"),"version: 1\ntargets: {local: {kind: local}}\nenvironments: {dev: {imports: {echo: {source: {kind: process, bin: /bin/echo}, bind: {target: local}}}}}\n").unwrap();
    let run = |flags: &[&str], source: &str| {
        Command::new(env!("CARGO_BIN_EXE_wes"))
            .current_dir(root.path())
            .arg("--home")
            .arg(&home)
            .args(flags)
            .args(["--command", source])
            .output()
            .unwrap()
    };
    let first = run(
        &["--env-file", "env.yaml", "--env", "dev", "--sequential"],
        "echo run args:first > raw\n:calc { return $raw; } > one\n:calc { return $one; } > two\n:calc { return $two; } > three",
    );
    assert!(
        first.status.success(),
        "{}",
        String::from_utf8_lossy(&first.stderr)
    );
    let changed = run(
        &["--env-file", "env.yaml", "--env", "dev", "--activate-env"],
        ":change $raw args:changed",
    );
    assert!(
        changed.status.success(),
        "{}",
        String::from_utf8_lossy(&changed.stderr)
    );
    for command in [":list environments", ":inspect $three"] {
        let output = run(&[], command);
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(!String::from_utf8_lossy(&output.stderr).contains("changed declaration"));
    }
    let runtime = launch(RuntimeOptions::new(home, root.path().into()))
        .await
        .unwrap();
    let snapshot = runtime
        .handle
        .current()
        .unwrap()
        .session
        .snapshot()
        .await
        .unwrap();
    let report = snapshot.restoration.as_ref().unwrap();
    assert_eq!(report.unconfirmed_changes.len(), 4);
    assert_eq!(report.unconfirmed_external_changes.len(), 1);
    let warning = wes::startup::warnings(report)
        .into_iter()
        .find(|w| w.id == "unconfirmed-changes")
        .unwrap();
    assert!(
        warning
            .message
            .contains("1 provider-capable declaration remains")
    );
    assert_eq!(
        report.unconfirmed_external_changes[0],
        snapshot.names["raw"].node
    );
    assert_eq!(
        snapshot
            .execution
            .graph
            .node(&snapshot.names["three"].node)
            .unwrap()
            .state(),
        wes_engine::graph::NodeState::Stale
    );
    runtime.handle.shutdown().await;
    runtime.task.join().await.unwrap();
    runtime.worker.shutdown().await.unwrap();
    runtime.worker_task.join().await.unwrap();
}
