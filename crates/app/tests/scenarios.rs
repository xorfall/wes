#[path = "support/python.rs"]
mod python;
use serde_json::{Value, json};
use wes::{
    runtime::{RuntimeOptions, launch},
    scenarios::{Scenario, run},
};
use wes_engine::{driver::CancellationToken, source::SourceInput};

fn scenario(steps: Value, cleanup: Value) -> Scenario {
    Scenario::parse(
        &json!({"version":1,"name":"synthetic","steps":steps,"cleanup":cleanup}).to_string(),
    )
    .unwrap()
}

#[test]
fn preflight_rejects_invalid_whole_documents_and_impure_or_injected_expectations() {
    for source in [
        "version: 1\nname: x\nsteps: []",
        "version: 2\nname: x\nsteps: []",
        "version: 1\nversion: 1\nname: x\nsteps: []",
        "version: 1\nname: x\nsteps: []\nextra: true",
    ] {
        assert!(Scenario::parse(source).is_err(), "{source}");
    }
    for command in [
        ":calc { return true; }\n:calc { return false; }",
        ":workspace load \"old\"",
        ":env use \"x\"",
        "@interactive sh run cmd:ls",
        ":calc { broken syntax }",
        ":calc { return 1; } | :calc { return input; }",
        ":calc { return 1; } | :env clear",
    ] {
        let source =
            json!({"version":1,"name":"x","steps":[{"name":"bad","run":command}]}).to_string();
        assert!(Scenario::parse(&source).is_err(), "{command}");
    }
    for condition in [
        "call(\"http\", [\"request\"], {url:\"http://example.invalid\"})",
        "true); return (false",
        "(()=> { const f = call; return f(\"http\",[\"request\"],{}); })()",
    ] {
        let source = json!({"version":1,"name":"x","steps":[{"name":"bad","run":":calc { return true; }","expect":[{"name":"bad","condition":condition}]}]}).to_string();
        assert!(Scenario::parse(&source).is_err(), "{condition}");
    }
    let duplicate = json!({"version":1,"name":"x","steps":[{"name":"duplicate","run":":list nodes"},{"name":"duplicate","run":":list nodes"}]}).to_string();
    assert!(Scenario::parse(&duplicate).is_err());
    assert!(Scenario::parse(&"x".repeat(1024 * 1024 + 1)).is_err());
}

#[tokio::test]
async fn ordered_values_expected_errors_and_report_identity_use_existing_session_execution() {
    let root = tempfile::tempdir().unwrap();
    let runtime = launch(RuntimeOptions::new(
        root.path().join("home"),
        root.path().into(),
    ))
    .await
    .unwrap();
    let session = runtime.handle.current().unwrap().session;
    let spec = scenario(
        json!([
            {"name":"create","run":":calc { return {id: 7, total: 42}; } > created","expect":[{"name":"total","condition":"$created.total == 42"}]},
            {"name":"fetch","run":":calc { return $created.id; } > fetched","expect":[{"name":"same id","condition":"$fetched == 7"}]},
            {"name":"negative","run":"http request url:\"not-a-url\" *> problem","expect_error":{"code":"HTTP001"},"expect":[{"name":"typed code","condition":"$problem.code == \"HTTP001\""}]}
        ]),
        json!([]),
    );
    let report = run(session.clone(), &spec, None, CancellationToken::new())
        .await
        .unwrap();
    assert!(report.passed());
    let report = report.export().unwrap();
    assert_eq!(report["steps"][2]["error"]["code"], "HTTP001");
    assert!(report["steps"][0]["evidence"][0]["run"].is_string());
    let repeated = run(session, &spec, None, CancellationToken::new())
        .await
        .unwrap()
        .export()
        .unwrap();
    assert_ne!(report["run"], repeated["run"]);
    runtime.shutdown().await.unwrap();
    let restored = launch(RuntimeOptions::new(
        root.path().join("home"),
        root.path().into(),
    ))
    .await
    .unwrap();
    let resumed = run(
        restored.handle.current().unwrap().session,
        &spec,
        None,
        CancellationToken::new(),
    )
    .await
    .unwrap();
    assert!(resumed.passed());
    restored.shutdown().await.unwrap();
}

#[tokio::test]
async fn false_and_non_bool_expectations_fail_skip_forward_effects_and_still_run_cleanup() {
    let root = tempfile::tempdir().unwrap();
    let runtime = launch(RuntimeOptions::new(
        root.path().join("home"),
        root.path().into(),
    ))
    .await
    .unwrap();
    let spec = scenario(
        json!([
            {"name":"check","run":":calc { return 42; } > result","expect":[
                {"name":"wrong","condition":"$result == 999"}, {"name":"also checked","condition":"true"}, {"name":"must be Bool","condition":"42"}]},
            {"name":"skipped","run":"http request url:\"http://example.invalid\""}
        ]),
        json!([{"name":"cleanup","run":":calc { return true; } > cleaned"}]),
    );
    let report = run(
        runtime.handle.current().unwrap().session,
        &spec,
        None,
        CancellationToken::new(),
    )
    .await
    .unwrap();
    assert!(!report.passed());
    let report = report.export().unwrap();
    assert_eq!(report["steps"][0]["checks"][0]["codes"][0], "TEST_FALSE");
    assert_eq!(report["steps"][0]["checks"][1]["status"], "passed");
    assert_eq!(
        report["steps"][0]["checks"][2]["codes"][0],
        "TEST_EXPECTED_BOOL"
    );
    assert_eq!(report["steps"][1]["status"], "skipped");
    assert_eq!(report["cleanup"][0]["status"], "passed");
    runtime.shutdown().await.unwrap();
}

#[tokio::test]
async fn cleanup_cannot_reuse_a_same_named_value_left_by_a_previous_run() {
    let root = tempfile::tempdir().unwrap();
    let runtime = launch(RuntimeOptions::new(
        root.path().join("home"),
        root.path().into(),
    ))
    .await
    .unwrap();
    let session = runtime.handle.current().unwrap().session;
    session
        .submit(SourceInput::new("old".into(), ":calc { return 99; } > created".into()).unwrap())
        .await
        .unwrap();
    session.wait_idle().await.unwrap();
    let spec = scenario(
        json!([{"name":"failed","run":"not_imported create > created"}]),
        json!([{"name":"cleanup","run":":calc { return $created; }"}]),
    );
    let report = run(session, &spec, None, CancellationToken::new())
        .await
        .unwrap()
        .export()
        .unwrap();
    assert_eq!(report["cleanup"][0]["codes"][0], "TEST_FOREIGN_INPUT");
    assert_eq!(report["status"], "failed");
    runtime.shutdown().await.unwrap();
}

#[tokio::test]
async fn error_mismatch_unexpected_success_and_cleanup_failures_never_pass() {
    let root = tempfile::tempdir().unwrap();
    let runtime = launch(RuntimeOptions::new(
        root.path().join("home"),
        root.path().into(),
    ))
    .await
    .unwrap();
    let session = runtime.handle.current().unwrap().session;
    for step in [
        json!({"name":"mismatch","run":"http request url:bad","expect_error":{"code":"HTTP008","issue":"HTTP_STATUS_404"}}),
        json!({"name":"wrong issue","run":"http request url:bad","expect_error":{"code":"HTTP001","issue":"HTTP_STATUS_404"}}),
        json!({"name":"success","run":":calc { return true; }","expect_error":{"code":"HTTP001"}}),
        json!({"name":"admission","run":"missing provider","expect_error":{"code":"RES004"}}),
    ] {
        let report = run(
            session.clone(),
            &scenario(json!([step]), json!([])),
            None,
            CancellationToken::new(),
        )
        .await
        .unwrap();
        assert!(!report.passed());
    }
    let report = run(
        session,
        &scenario(
            json!([{"name":"ok","run":":calc { return true; }"}]),
            json!([{"name":"cleanup","run":"http request url:bad"}]),
        ),
        None,
        CancellationToken::new(),
    )
    .await
    .unwrap();
    assert!(!report.passed());
    runtime.shutdown().await.unwrap();
}

#[tokio::test]
async fn cancellation_closes_owned_session_and_does_not_start_cleanup() {
    let root = tempfile::tempdir().unwrap();
    let runtime = launch(RuntimeOptions::new(
        root.path().join("home"),
        root.path().into(),
    ))
    .await
    .unwrap();
    let session = runtime.handle.current().unwrap().session;
    let spec = scenario(
        json!([{"name":"never","run":"http request url:\"http://example.invalid\""}]),
        json!([{"name":"never cleanup","run":":calc { return true; }"}]),
    );
    let token = CancellationToken::new();
    token.cancel();
    let report = run(session, &spec, None, token).await.unwrap();
    assert!(!report.passed());
    assert!(
        report.export().is_none(),
        "incomplete lineage cannot be exported as public"
    );
    runtime.shutdown().await.unwrap();
}

#[tokio::test]
async fn overall_deadline_cancels_a_waiting_http_call_and_joins_shutdown() {
    use tokio::{io::AsyncReadExt, net::TcpListener};
    let root = tempfile::tempdir().unwrap();
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.unwrap();
        let mut bytes = [0; 8192];
        assert!(stream.read(&mut bytes).await.unwrap() > 0);
        let _ = stream.read(&mut bytes).await;
    });
    let runtime = launch(RuntimeOptions::new(
        root.path().join("home"),
        root.path().into(),
    ))
    .await
    .unwrap();
    let spec = Scenario::parse(&json!({"version":1,"name":"timeout","timeout_seconds":1,"steps":[{"name":"wait","run":format!("http request url:\"http://{address}\"") }]}).to_string()).unwrap();
    let report = tokio::time::timeout(
        std::time::Duration::from_secs(5),
        run(
            runtime.handle.current().unwrap().session,
            &spec,
            None,
            CancellationToken::new(),
        ),
    )
    .await
    .unwrap()
    .unwrap();
    assert!(!report.passed());
    runtime.shutdown().await.unwrap();
    tokio::time::timeout(std::time::Duration::from_secs(5), server)
        .await
        .unwrap()
        .unwrap();
}

#[test]
fn checked_in_api_scenario_project_passes_against_isolated_loopback() {
    let checker = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../examples/api-scenarios/check.py");
    let output = python::command()
        .arg(checker)
        .args(["--binary", env!("CARGO_BIN_EXE_wes")])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(String::from_utf8_lossy(&output.stdout).starts_with("PASS actual API scenarios"));
}

#[tokio::test]
async fn nested_scenario_references_cannot_read_a_foreign_workspace_result() {
    let root = tempfile::tempdir().unwrap();
    let runtime = launch(RuntimeOptions::new(
        root.path().join("home"),
        root.path().into(),
    ))
    .await
    .unwrap();
    let session = runtime.handle.current().unwrap().session;
    session
        .submit(
            SourceInput::new(
                "foreign".into(),
                ":calc { return 99; } > foreign_result".into(),
            )
            .unwrap(),
        )
        .await
        .unwrap();
    session.wait_idle().await.unwrap();
    let before = session.snapshot().await.unwrap().execution.graph.len();
    let spec = scenario(
        json!([{"name":"attempt","run":"catalog echo value:{nested:[$foreign_result]} > leaked"}]),
        json!([]),
    );
    let report = run(session.clone(), &spec, None, CancellationToken::new())
        .await
        .unwrap()
        .export()
        .unwrap();
    assert_eq!(report["steps"][0]["codes"][0], "TEST_FOREIGN_INPUT");
    assert_eq!(
        session.snapshot().await.unwrap().execution.graph.len(),
        before
    );
    runtime.shutdown().await.unwrap();
}
