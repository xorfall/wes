//! The HTTP package remains local and policy-aware across app composition and restarts.
#[path = "support/python.rs"]
mod python;
use std::time::Duration;
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpListener,
};
use wes::{
    runtime::{RuntimeOptions, launch},
    scenarios::{Scenario, run},
};
use wes_core::Data;
use wes_engine::{driver::CancellationToken, source::SourceInput};

#[test]
fn actual_http_domain_project_runs_managed_public_private_and_offline() {
    let checker = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../examples/http-domain/check.py");
    let result = python::command()
        .arg(checker)
        .arg("--binary")
        .arg(env!("CARGO_BIN_EXE_wes"))
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
async fn private_analysis_and_definitions_are_typed_live_values_but_not_restored() {
    let root = tempfile::tempdir().unwrap();
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.unwrap();
        let mut bytes = [0; 8192];
        assert!(stream.read(&mut bytes).await.unwrap() > 0);
        stream.write_all(b"HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: 12\r\nConnection: close\r\n\r\n{\"reading\":42}").await.unwrap();
    });
    std::fs::write(
        root.path().join("sensor.json"),
        include_str!("../../../examples/http-domain/sensor.json"),
    )
    .unwrap();
    std::fs::write(
        root.path().join("environments.yaml"),
        include_str!("../../../examples/http-domain/environments.yaml")
            .replace("http://127.0.0.1:8767", &format!("http://{address}")),
    )
    .unwrap();
    let home = root.path().join("home");
    let runtime = launch(RuntimeOptions::new(home.clone(), root.path().into()))
        .await
        .unwrap();
    let session = runtime.handle.current().unwrap().session;
    let plan = session
        .plan_environment_file("environments.yaml".into(), false)
        .await
        .unwrap();
    session.apply_environments(plan).await.unwrap();
    let selected = session
        .submit(SourceInput::new("select".into(), ":env use \"private\"".into()).unwrap())
        .await
        .unwrap();
    assert!(
        !selected
            .diagnostics
            .diagnostics
            .iter()
            .any(|d| d.severity == wes_language::Severity::Error)
    );
    let scenario =
        Scenario::parse(include_str!("../../../examples/http-domain/private.yaml")).unwrap();
    let report = tokio::time::timeout(
        Duration::from_secs(10),
        run(session.clone(), &scenario, None, CancellationToken::new()),
    )
    .await
    .unwrap()
    .unwrap();
    assert!(report.passed());
    assert!(report.export().is_none());
    server.await.unwrap();
    let snapshot = session.snapshot().await.unwrap();
    for name in ["sample", "trace", "analysis", "status", "definition"] {
        let value = &snapshot.execution.values[&snapshot.names[name].node];
        assert!(value.provenance().policy().is_private(), "{name}");
    }
    let Data::Record(analysis) = snapshot.execution.values[&snapshot.names["analysis"].node].data()
    else {
        panic!("typed analysis")
    };
    assert_eq!(analysis["partial"], Data::Bool(false));
    let Data::Record(definition) =
        snapshot.execution.values[&snapshot.names["definition"].node].data()
    else {
        panic!("typed status")
    };
    assert_eq!(definition["class"], Data::Text("success".into()));
    drop(snapshot);
    runtime.shutdown().await.unwrap();
    let restored = launch(RuntimeOptions::new(home, root.path().into()))
        .await
        .unwrap();
    let session = restored.handle.current().unwrap().session;
    let snapshot = session.snapshot().await.unwrap();
    for name in ["sample", "trace", "analysis", "status", "definition"] {
        assert!(
            !snapshot
                .execution
                .values
                .contains_key(&snapshot.names[name].node),
            "{name}"
        );
    }
    drop(snapshot);
    // Local functions remain available while the restored managed environment is disabled.
    let reply = session
        .submit(
            SourceInput::new(
                "discover".into(),
                ":calc pure { return httpCatalogue(\"statuses\"); }".into(),
            )
            .unwrap(),
        )
        .await
        .unwrap();
    session.wait_idle().await.unwrap();
    let snapshot = session.snapshot().await.unwrap();
    let names = wes_adapters::codec::encode_json(
        snapshot.execution.values[&reply.nodes[0]].data(),
        Default::default(),
    )
    .unwrap();
    assert!(
        String::from_utf8(names)
            .unwrap()
            .contains("catalogueVersion")
    );
    restored.shutdown().await.unwrap();
}
