//! The actual example runs in a synthetic home; its display envelope is also the GUI fixture.
use serde_json::{Value, json};
use std::{collections::BTreeSet, time::Duration};
use wes::runtime::{RuntimeOptions, launch};
use wes_adapters::codec::{Limits, encode_display_value};
use wes_engine::source::SourceInput;

#[tokio::test]
async fn example_help_is_complete_specific_and_matches_the_surface_fixture() {
    let root = tempfile::tempdir().unwrap();
    let runtime = launch(RuntimeOptions::new(
        root.path().join("home"),
        root.path().into(),
    ))
    .await
    .unwrap();
    let session = runtime.handle.current().unwrap().session;
    let mut fixture = serde_json::Map::new();
    let mut visited = BTreeSet::new();
    for (i, source) in include_str!("../../../examples/readable-help/help.wes")
        .lines()
        .enumerate()
    {
        let reply = session
            .submit(SourceInput::new(format!("help-{i}"), source.into()).unwrap())
            .await
            .unwrap();
        assert!(!reply.nodes.is_empty(), "{source}: {reply:?}");
        assert!(
            !reply
                .diagnostics
                .diagnostics
                .iter()
                .any(|d| d.severity == wes_language::Severity::Error),
            "{source}: {reply:?}"
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
            .expect("help produced a value");
        let display: Value =
            serde_json::from_slice(&encode_display_value(value, Limits::default()).unwrap())
                .unwrap();
        assert_eq!(display["type"]["name"], "wes.Help");
        let data = &display["data"];
        let path = source.strip_prefix(":help").unwrap().trim();
        visited.insert(path.to_string());
        if let Some(command) = data["invocation"]["command"].as_str() {
            assert!(
                command == format!(":{path}") || command == path,
                "{source}: {command}"
            );
            assert!(
                data["invocation"]["usage"]
                    .as_str()
                    .unwrap()
                    .starts_with(command),
                "{source}: {}",
                data["invocation"]["usage"]
            );
        }
        if let Some(registry) = path.strip_prefix("list ") {
            let listing = &data["invocation"]["listing"]["registries"];
            assert_eq!(listing.as_array().unwrap().len(), 1);
            assert_eq!(listing[0]["name"], registry);
            assert_eq!(data["invocation"]["takes"], json!([]));
            assert!(
                serde_json::to_vec(data).unwrap().len() < 1800,
                "leaf repeats irrelevant metadata: {path}"
            );
            if registry == "views" {
                assert_eq!(listing[0]["scope"], "workspace");
                let summary = listing[0]["summary"].as_str().unwrap();
                assert!(summary.contains("Built-in and installed"));
                assert!(summary.contains("state/event outputs"));
                assert!(summary.contains("shared"));
            }
        }
        if let Some(action) = path.strip_prefix("env ") {
            let declared = wes_language::vocabulary::EnvironmentCommand::lookup(action).unwrap();
            assert_eq!(data["invocation"]["summary"], declared.summary);
        }
        for (command, argument) in [
            ("node policy", "mode:automatic|manual|reactive"),
            ("node remove", "scope:downstream"),
            ("node timeout", "after:<Duration>"),
        ] {
            if path == command {
                assert!(
                    data["invocation"]["shortForm"]
                        .as_str()
                        .unwrap()
                        .contains(argument)
                );
            }
        }
        if matches!(
            path,
            "dataset record" | "dataset recording-status" | "dataset stop" | "dataset discard"
        ) {
            let help = serde_json::to_string(data).unwrap();
            assert!(help.contains("RecordingSetup"));
            assert!(help.contains("@hold"));
            assert!(help.contains("one-submit launch"));
            assert!(help.contains("budget:Capture"));
            assert!(!help.contains("launch recording are not available"));
        }
        if path == "dataset retention" {
            let help = serde_json::to_string(data).unwrap();
            assert!(help.contains("totalBytes"));
            assert!(help.contains("exclusiveBytes"));
            assert!(help.contains("not future reclaimable disk"));
        }
        if path == "dataset snapshot" {
            let help = serde_json::to_string(data).unwrap();
            assert!(help.contains("basis:"));
            assert!(help.contains("anchor's own"));
            assert!(help.contains("Keep or Pin is a separate action"));
        }
        if matches!(path, "scan reconcile" | "dataset reconcile") {
            let help = serde_json::to_string(data).unwrap();
            assert!(help.contains("local Dataset mutation"));
            assert!(help.contains("never retries a producer"));
            assert!(help.contains("unknown"));
        }
        if path == "calc" {
            assert!(data["invocation"].get("example").is_none());
            assert_eq!(data["invocation"]["examples"].as_array().unwrap().len(), 5);
        }
        if path == "workspace save" {
            assert!(
                data["invocation"]["usage"]
                    .as_str()
                    .unwrap()
                    .contains("\"workspace name\"")
            );
        }
        if [
            "",
            "workspace",
            "node",
            "env",
            "env delete",
            "list templates",
            "calc",
            "calc iter.matches",
            "calc iter.captures",
            "stream",
            "fork",
            "inspect",
            "http request",
        ]
        .contains(&path)
        {
            fixture.insert(path.into(), display);
        }
    }
    // The checked example covers every published family and child, not a hand-picked smoke test.
    let vocabulary = &wes_language::vocabulary::commands::COMMAND_PATHS;
    for path in vocabulary.iter().map(|p| p.path.join(" ")).chain(
        wes_language::vocabulary::commands::roots()
            .into_iter()
            .map(String::from),
    ) {
        assert!(visited.contains(&path), "missing help example: {path}");
    }
    for registry in wes_language::vocabulary::ListRegistry::ALL.iter() {
        assert!(visited.contains(&format!("list {}", registry.name())));
    }
    for action in wes_language::vocabulary::ENVIRONMENT_COMMANDS {
        assert!(visited.contains(&format!("env {}", action.name)));
    }
    assert_eq!(
        fixture["workspace"]["data"]["children"]
            .as_array()
            .unwrap()
            .len(),
        5
    );
    let fixture = Value::Object(fixture);
    let file = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../examples/readable-help/display.json");
    if std::env::var_os("WES_UPDATE_HELP_FIXTURE").is_some() {
        std::fs::write(
            &file,
            serde_json::to_string_pretty(&fixture).unwrap() + "\n",
        )
        .unwrap();
    }
    assert_eq!(
        fixture,
        serde_json::from_str::<Value>(&std::fs::read_to_string(file).unwrap()).unwrap()
    );
    runtime.shutdown().await.unwrap();
}
