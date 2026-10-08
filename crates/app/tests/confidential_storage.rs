//! Isolated runtime acceptance: loopback acquisition, derivation, encrypted checkpoints and reopen.
use std::{
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use wes::runtime::{RuntimeOptions, launch};
use wes_core::Data;
use wes_engine::{session::SessionHandle, source::SourceInput};

async fn submit(session: &SessionHandle, source: &str) {
    let result = session
        .submit(SourceInput::new(uuid::Uuid::new_v4().to_string(), source.into()).unwrap())
        .await
        .unwrap();
    assert!(!result.accepted.is_empty(), "{result:?}");
    assert!(
        !result
            .diagnostics
            .diagnostics
            .iter()
            .any(|d| d.severity == wes_language::Severity::Error),
        "{result:?}"
    );
    tokio::time::timeout(Duration::from_secs(15), session.wait_idle())
        .await
        .unwrap()
        .unwrap();
}
async fn keep(session: &SessionHandle, name: &str) {
    let observed = session.observe().await.unwrap();
    let node = &observed.state.names[name].node;
    let handle = observed.values.as_ref().unwrap().outputs[node]
        .handle()
        .unwrap()
        .clone();
    let receipt = session.keep(handle).await.unwrap();
    assert!(receipt.problem.is_none(), "{receipt:?}");
    assert!(receipt.affected);
}
#[tokio::test]
async fn confidential_http_derivation_and_dataset_analysis_survive_without_replay() {
    let root = tempfile::tempdir().unwrap();
    let home = root.path().join("data");
    let key = root.path().join("key");
    std::fs::write(&key, [7; 32]).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&key, std::fs::Permissions::from_mode(0o600)).unwrap();
    }
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let endpoint = format!("http://{}", listener.local_addr().unwrap());
    let calls = Arc::new(AtomicUsize::new(0));
    let count = calls.clone();
    let server = tokio::spawn(async move {
        loop {
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut input = [0; 8192];
            stream.read(&mut input).await.unwrap();
            count.fetch_add(1, Ordering::SeqCst);
            let body = r#"["first","second","confidential-runtime-sentinel"]"#;
            let response = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            );
            stream.write_all(response.as_bytes()).await.unwrap();
        }
    });
    std::fs::write(root.path().join("api.json"),r#"{"version":1,"provider":"evidence","types":{},"operations":[{"path":["read"],"method":"GET","route":"/samples","auth":[],"parameters":[],"responses":{"200":"List<Text>"}}]}"#).unwrap();
    std::fs::write(root.path().join("env.yaml"),format!("version: 1\ntargets: {{local: {{kind: local}}}}\nenvironments: {{lab: {{imports: {{evidence: {{source: {{kind: spec, file: api.json}}, bind: {{target: local, endpoint: '{endpoint}', output: confidential}}}}}}}}}}\n")).unwrap();
    let options = || {
        let mut options = RuntimeOptions::new(home.clone(), root.path().into());
        options.storage_key_file = Some(key.clone());
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
    submit(&session,r#":package load source:"types: {EvidenceStep: {base: Record, fields: {state: Text, outputs: 'List<Text>'}}}""#).await;
    submit(&session,":def capture(state:Text, context:Int, item:Text) -> EvidenceStep as :calc pure { return {state:item,outputs:[item]}; }").await;
    submit(&session, "evidence read > response").await;
    let snapshot = session.snapshot().await.unwrap();
    let response = &snapshot.names["response"].node;
    assert!(
        snapshot.execution.values[response]
            .provenance()
            .policy()
            .is_confidential(),
        "{:?}",
        snapshot.execution.errors
    );
    submit(&session, ":calc pure { return $response.body; } > raw").await;
    submit(&session,r#":scan source:$raw transition:capture initial:"" context:0 profile:TypedRecords sink:dataset > analysis"#).await;
    let snapshot = session.snapshot().await.unwrap();
    let raw = snapshot.names["raw"].node.clone();
    let analysis = snapshot.names["analysis"].node.clone();
    assert!(
        snapshot.execution.errors.is_empty(),
        "{:?}",
        snapshot.execution.errors
    );
    let result = snapshot
        .execution
        .values
        .get(&analysis)
        .unwrap_or_else(|| panic!("{:?}", snapshot.execution))
        .clone();
    assert!(result.provenance().policy().is_confidential());
    let Data::Record(fields) = result.data() else {
        panic!("scan result")
    };
    // The confidential payload reaches both output segments and the retained
    // inline state checkpoint; the disk scan below must cover both channels.
    assert_eq!(
        fields["state"],
        Data::Text("confidential-runtime-sentinel".into())
    );
    keep(&session, "raw").await;
    keep(&session, "analysis").await;
    let runs = snapshot.execution.runs.clone();
    drop(snapshot);
    drop(session);
    runtime.shutdown().await.unwrap();
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    server.abort();
    let _ = server.await;
    fn no_plaintext(path: &std::path::Path) {
        for entry in std::fs::read_dir(path).unwrap() {
            let p = entry.unwrap().path();
            if p.is_dir() {
                no_plaintext(&p);
            } else {
                assert!(
                    !std::fs::read(&p)
                        .unwrap()
                        .windows(29)
                        .any(|w| w == b"confidential-runtime-sentinel"),
                    "{}",
                    p.display()
                );
            }
        }
    }
    no_plaintext(&home);
    assert!(
        launch(RuntimeOptions::new(home.clone(), root.path().into()))
            .await
            .is_err()
    );
    let reopened = launch(options()).await.unwrap();
    let session = reopened.handle.current().unwrap().session;
    let snapshot = session.snapshot().await.unwrap();
    assert_eq!(
        snapshot.execution.values[&raw].data(),
        &Data::List(vec![
            Data::Text("first".into()),
            Data::Text("second".into()),
            Data::Text("confidential-runtime-sentinel".into())
        ])
    );
    assert_eq!(snapshot.execution.values[&analysis].data(), result.data());
    assert_eq!(snapshot.execution.runs[&raw], runs[&raw]);
    assert_eq!(snapshot.execution.runs[&analysis], runs[&analysis]);
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    reopened.shutdown().await.unwrap();
}
