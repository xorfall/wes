use super::*;
use wes::runtime::{RuntimeOptions, launch};

#[tokio::test]
async fn sandbox_admission_refusal_reports_no_execution_without_replaying_effects() {
    let fixture = Fixture::new().await;
    let current = fixture.app.current().unwrap();
    let generation = &current.generation;
    assert_eq!(
        fixture
            .source(generation, "parent-input", ":calc { return 9; } > upstream")
            .await,
        202
    );
    current.session.wait_idle().await.unwrap();
    let request = json!({
        "request":"submit", "client":"web-fixture", "cell":"sandbox-refused",
        "text":":sandbox {\ncatalog echo value:hello > probe\n:calc { return $upstream + 4; } > total\n} > check"
    });
    for _ in 0..2 {
        let response = fixture.post(generation, request.clone()).await;
        assert_eq!(response.status(), 400);
        assert_eq!(
            response.headers()["x-wes-submission-outcome"],
            "not-started"
        );
        let reason = response.text().await.unwrap();
        assert!(reason.contains("CAL010"), "{reason}");
        assert!(reason.contains("$upstream"), "{reason}");
        assert!(reason.contains("own declared members"), "{reason}");
        assert_eq!(fixture.calls.load(Ordering::SeqCst), 0);
    }
    assert!(current.session.read_sandbox("check", true).await.is_err());
    let conflict = fixture
        .post(
            generation,
            json!({
                "request":"submit", "client":"web-fixture", "cell":"sandbox-refused",
                "text":":calc { return 1; }"
            }),
        )
        .await;
    assert_eq!(conflict.status(), 409);
    assert!(conflict.headers().get("x-wes-submission-outcome").is_none());
    fixture.close().await;
}

#[tokio::test]
async fn sandbox_http_observations_are_transient_generation_bound_and_restore_definitions_only() {
    let root = tempfile::tempdir().unwrap();
    let options = || {
        let mut o = RuntimeOptions::new(root.path().join("home"), root.path().to_owned());
        o.docker_candidates = Some(vec![root.path().join("missing.sock")]);
        o
    };
    let runtime = launch(options()).await.unwrap();
    let server = runtime.serve(0, None).await.unwrap();
    let current = runtime.handle.current().unwrap();
    current.session.wait_idle().await.unwrap();
    let client = reqwest::Client::builder().no_proxy().build().unwrap();
    let url = format!("http://{}", server.address());
    let response=client.post(format!("{url}/submit")).header("Content-Type","application/json").header("X-Wes-Workspace","default").header("X-Wes-Session",&current.generation)
        .body(json!({"request":"submit","client":"web-fixture","cell":"sandbox-create","text":include_str!("../../../../examples/sandbox/metrics.wes")}).to_string()).send().await.unwrap();
    assert_eq!(response.status(), 200, "{}", response.text().await.unwrap());
    let data = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let response = client
                .get(format!("{url}/sandbox?reference=preview.requests"))
                .header("X-Wes-Workspace", "default")
                .header("X-Wes-Session", &current.generation)
                .send()
                .await
                .unwrap();
            if response.status() == 200 {
                assert_eq!(response.headers()["cache-control"], "no-store");
                break serde_json::from_slice::<Value>(&response.bytes().await.unwrap()).unwrap();
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    assert_eq!(data["sandbox"]["value"]["data"], 25);
    assert_eq!(data["sandbox"]["state"], "active");
    assert!(current.session.observe().await.unwrap().cells.is_empty());
    assert!(
        current
            .session
            .snapshot()
            .await
            .unwrap()
            .execution
            .graph
            .is_empty()
    );
    assert_eq!(
        client
            .get(format!("{url}/sandbox?reference=preview"))
            .header("X-Wes-Workspace", "default")
            .header("X-Wes-Session", "old-generation")
            .send()
            .await
            .unwrap()
            .status(),
        409
    );
    server.shutdown().await.unwrap();
    runtime.shutdown().await.unwrap();
    let runtime = launch(options()).await.unwrap();
    let current = runtime.handle.current().unwrap();
    let observation = current.session.read_sandbox("preview", true).await.unwrap();
    assert!(format!("{:?}", observation.data).contains("not run"));
    assert_eq!(observation.state.as_str(), "not run");
    assert!(
        current
            .session
            .read_sandbox("preview.requests", false)
            .await
            .is_err()
    );
    assert!(current.session.observe().await.unwrap().cells.is_empty());
    runtime.shutdown().await.unwrap();
}
