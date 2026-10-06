#[path = "support/python.rs"]
mod python;
#[test]
fn actual_command_system_example_uses_canonical_sources_and_structured_results() {
    let result = python::command()
        .arg(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../examples/command-system/check.py"
        ))
        .args(["--binary", env!("CARGO_BIN_EXE_wes")])
        .output()
        .unwrap();
    assert!(
        result.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&result.stdout),
        String::from_utf8_lossy(&result.stderr)
    );
}

#[tokio::test]
async fn plan_handles_are_client_scoped_and_cannot_be_confused_with_shared_output_names() {
    use wes::runtime::{RuntimeOptions, launch};
    use wes_engine::{session::SessionHandle, source::SourceInput};
    async fn submit(
        session: &SessionHandle,
        cell: &str,
        source: &str,
        actor: &str,
    ) -> Result<
        std::sync::Arc<wes_engine::session::SubmissionResult>,
        wes_engine::session::SessionError,
    > {
        let result = session
            .submit(
                SourceInput::new(cell.into(), source.into())
                    .unwrap()
                    .with_client(actor.into())
                    .unwrap(),
            )
            .await;
        session.wait_idle().await.unwrap();
        result
    }
    let root = tempfile::tempdir().unwrap();
    let runtime = launch(RuntimeOptions::new(
        root.path().join("home"),
        root.path().into(),
    ))
    .await
    .unwrap();
    let session = runtime.handle.current().unwrap().session;
    submit(&session, "value", ":calc {return 42;} > output", "a")
        .await
        .unwrap();
    let wrong = submit(&session, "wrong", ":env apply $output", "a")
        .await
        .unwrap();
    assert!(
        wrong.diagnostics.diagnostics[0]
            .message
            .contains("received an output reference")
    );
    let plan = r#":env plan source:"version: 1\nenvironments: {}\n" > proposed"#;
    let proposed = submit(&session, "plan", plan, "a").await.unwrap();
    assert!(
        !proposed
            .diagnostics
            .diagnostics
            .iter()
            .any(|d| d.severity == wes_language::Severity::Error)
    );
    let foreign = submit(&session, "foreign", ":env apply $proposed", "b")
        .await
        .unwrap();
    assert!(
        foreign.diagnostics.diagnostics[0]
            .message
            .contains("No live environment plan")
    );
    assert!(
        submit(&session, "collision", ":calc {return 7;} > proposed", "b")
            .await
            .is_err()
    );
    let discard = submit(&session, "discard", ":env discard $proposed", "a")
        .await
        .unwrap();
    assert!(
        !discard
            .diagnostics
            .diagnostics
            .iter()
            .any(|d| d.severity == wes_language::Severity::Error)
    );
    let released = submit(&session, "released", ":calc {return 7;} > proposed", "b")
        .await
        .unwrap();
    let collision = submit(&session, "plan-collision", plan, "a").await.unwrap();
    assert!(
        collision.diagnostics.diagnostics[0]
            .message
            .contains("already")
    );
    let selected = submit(&session, "unmarked", r#"env use "default""#, "a")
        .await
        .unwrap();
    assert!(
        !selected
            .diagnostics
            .diagnostics
            .iter()
            .any(|d| d.severity == wes_language::Severity::Error),
        "{selected:?}"
    );
    let next_number: u64 = released.nodes[0]
        .as_str()
        .strip_prefix("id")
        .unwrap()
        .parse::<u64>()
        .unwrap()
        + 1;
    let future_id = format!("id{next_number}");
    let future_plan = plan.replace("> proposed", &format!("> {future_id}"));
    let reserved = submit(&session, "reserve-future", &future_plan, "a")
        .await
        .unwrap();
    assert!(
        !reserved
            .diagnostics
            .diagnostics
            .iter()
            .any(|d| d.severity == wes_language::Severity::Error)
    );
    let next = submit(&session, "allocate-around-plan", ":calc {return 9;}", "b")
        .await
        .unwrap();
    assert_ne!(next.nodes[0].as_str(), future_id);
    let discarded = submit(
        &session,
        "discard-future",
        &format!(":env discard ${future_id}"),
        "a",
    )
    .await
    .unwrap();
    assert!(
        !discarded
            .diagnostics
            .diagnostics
            .iter()
            .any(|d| d.severity == wes_language::Severity::Error)
    );
    runtime.shutdown().await.unwrap();
}
