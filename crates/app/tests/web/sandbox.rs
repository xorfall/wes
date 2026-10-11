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

#[tokio::test]
async fn transient_dataset_reads_require_the_live_scope_and_keep_preserves_the_exact_snapshot() {
    let fixture = Fixture::datasets().await;
    let url = format!("http://{}", fixture.server.address());
    let current = fixture.app.current().unwrap();
    let generation = &current.generation;
    assert_eq!(fixture.source(generation,"scan-types",r#"
:package load source:"types: {IntStep: {base: Record, fields: {state: Int, outputs: 'List<Int>'}}}"
:def step(state:Int, context:Int, item:Int) -> IntStep as :calc pure { return {state:state+item,outputs:[item]}; }
"#).await,202);
    let made=fixture.post(generation,json!({"request":"submit","client":"web-fixture","cell":"transient-dataset","text":":transient {\n:calc pure { return [1,2,3]; } > raw\n:scan source:$raw transition:step initial:0 context:0 profile:TypedRecords sink:dataset > analysis\n} > study"})).await;
    assert_eq!(made.status(), 200, "{}", made.text().await.unwrap());
    let output = current
        .session
        .read_sandbox_export("study.analysis.outputs", false)
        .await
        .unwrap();
    assert!(matches!(output.data, wes_core::Data::Dataset(_)));
    let response = fixture
        .client
        .get(format!("{}/sandbox?reference=study.analysis.outputs", url))
        .header("X-Wes-Workspace", "default")
        .header("X-Wes-Session", generation)
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 200);
    let body: Value = serde_json::from_slice(&response.bytes().await.unwrap()).unwrap();
    assert_eq!(body["sandbox"]["kind"], "transient");
    assert_eq!(body["sandbox"]["value"]["type"]["kind"], "dataset");
    let source = &body["sandbox"]["sources"][""];
    let handle = source["handle"].as_str().unwrap();
    let scope = source["scope"].as_str().unwrap();
    assert_eq!(source["select"], "/outputs");
    let path = format!("{}/datasets/{handle}?select=%2Foutputs&from=1&limit=2", url);
    let page = fixture
        .client
        .get(&path)
        .header("X-Wes-Workspace", "default")
        .header("X-Wes-Session", generation)
        .header("X-Wes-Scope", scope)
        .send()
        .await
        .unwrap();
    assert_eq!(page.status(), 200, "{}", page.text().await.unwrap());
    let unrelated = fixture
        .client
        .get(&path)
        .header("X-Wes-Workspace", "default")
        .header("X-Wes-Session", generation)
        .send()
        .await
        .unwrap();
    assert_eq!(unrelated.status(), 410);
    let kept=fixture.post(generation,json!({"request":"submit","client":"web-fixture","cell":"keep-study","text":":keep $study > saved_study"})).await;
    assert_eq!(kept.status(), 200, "{}", kept.text().await.unwrap());
    let receipt: Value = serde_json::from_slice(&kept.bytes().await.unwrap()).unwrap();
    assert_eq!(receipt["sandbox"]["kind"], "saved");
    assert_eq!(receipt["sandbox"]["state"], "saved");
    let stopped=fixture.post(generation,json!({"request":"submit","client":"web-fixture","cell":"stop-study","text":":cancel $study"})).await;
    assert_eq!(stopped.status(), 200);
    // A raw old UUID cannot renew a stopped scope, even if all selected files were kept.
    let expired = fixture
        .client
        .get(&path)
        .header("X-Wes-Workspace", "default")
        .header("X-Wes-Session", generation)
        .header("X-Wes-Scope", scope)
        .send()
        .await
        .unwrap();
    assert_eq!(expired.status(), 410);
    let expired_value = fixture
        .client
        .get(format!("{url}/values/{handle}"))
        .header("X-Wes-Workspace", "default")
        .header("X-Wes-Session", generation)
        .header("X-Wes-Scope", scope)
        .send()
        .await
        .unwrap();
    assert_eq!(expired_value.status(), 403);
    let problem: Value = serde_json::from_slice(&expired_value.bytes().await.unwrap()).unwrap();
    assert_eq!(problem["error"]["code"], "VALUE_ACCESS_REFUSED");
    assert_eq!(problem["error"]["context"]["handle"], handle);
    assert_eq!(problem["error"]["retryable"], false);
    let saved = current
        .session
        .read_sandbox_export("saved_study.analysis.outputs", false)
        .await
        .unwrap();
    let source = &saved.sources[""];
    let saved_path = format!(
        "{}/datasets/{}?select=%2Foutputs&from=1&limit=2",
        url,
        source.handle.as_ref().unwrap()
    );
    let page = fixture
        .client
        .get(saved_path)
        .header("X-Wes-Workspace", "default")
        .header("X-Wes-Session", generation)
        .send()
        .await
        .unwrap();
    assert_eq!(page.status(), 200, "{}", page.text().await.unwrap());
    assert_eq!(fixture.calls.load(Ordering::SeqCst), 0);
    fixture.close().await;
}

#[tokio::test]
async fn finite_views_use_the_analysis_scope_without_restoring_live_controls() {
    let fixture = Fixture::datasets().await;
    let current = fixture.app.current().unwrap();
    let generation = &current.generation;
    assert_eq!(
        fixture
            .source(
                generation,
                "view-input",
                r#":calc pure { return {view:"metric",value:1250}; } > input"#
            )
            .await,
        202
    );
    current.session.wait_idle().await.unwrap();
    let source = r#":transient {
:view create Metric input:$input > chart
:calc pure { return $input; } > evidence
} > study"#;
    let response = fixture
        .post(
            generation,
            json!({"request":"submit","client":"web-fixture","cell":"scoped-view","text":source}),
        )
        .await;
    assert_eq!(response.status(), 200, "{}", response.text().await.unwrap());
    let state = current
        .session
        .read_sandbox_export("study", true)
        .await
        .unwrap();
    let chart = current
        .session
        .read_sandbox_export("study.chart", false)
        .await
        .unwrap_or_else(|e| panic!("{e}: {:?}", state.data));
    let wes_core::Data::Record(fields) = &chart.data else {
        panic!("view instance")
    };
    let wes_core::Data::Text(node) = &fields["id"] else {
        panic!("node")
    };
    let wes_core::Data::Text(identity) = &fields["instance"] else {
        panic!("identity")
    };
    let scope = chart.scope.as_ref().unwrap();
    let mount_url = fixture.url(&format!("/view-mounts/{node}/{identity}"));
    let opened = fixture
        .client
        .post(&mount_url)
        .header("X-Wes-Session", generation)
        .header("X-Wes-Scope", scope)
        .header("Content-Type", "application/json")
        .body(json!({"action":"open"}).to_string())
        .send()
        .await
        .unwrap();
    assert_eq!(opened.status(), 200, "{}", opened.text().await.unwrap());
    let opened: Value = serde_json::from_slice(&opened.bytes().await.unwrap()).unwrap();
    let token = opened["token"].as_str().unwrap();
    let frame_url = fixture.url(&format!("/view-instances/{node}/{identity}"));
    let frame = fixture
        .client
        .get(&frame_url)
        .header("X-Wes-Session", generation)
        .header("X-Wes-Scope", scope)
        .header("X-Wes-View-Mount", token)
        .send()
        .await
        .unwrap();
    assert_eq!(frame.status(), 200, "{}", frame.text().await.unwrap());
    let frame: Value = serde_json::from_slice(&frame.bytes().await.unwrap()).unwrap();
    assert_eq!(frame["instances"][0]["input"]["data"]["value"], 1250);
    let unrelated = fixture
        .client
        .get(&frame_url)
        .header("X-Wes-Session", generation)
        .header("X-Wes-View-Mount", token)
        .send()
        .await
        .unwrap();
    assert_eq!(unrelated.status(), 403);
    let saved=fixture.post(generation,json!({"request":"submit","client":"web-fixture","cell":"save-scoped-view","text":":keep $study > saved_study"})).await;
    assert_eq!(saved.status(), 200, "{}", saved.text().await.unwrap());
    let control = current
        .session
        .read_sandbox("saved_study.chart", true)
        .await
        .unwrap();
    assert!(format!("{:?}", control.data).contains("ViewInstance"));
    assert!(
        current
            .session
            .read_sandbox("saved_study.chart", false)
            .await
            .is_err(),
        "a saved control's state is not a new live view"
    );
    assert!(
        current
            .session
            .read_sandbox_export("saved_study.evidence", false)
            .await
            .is_ok(),
        "explicit native capture is retained data"
    );
    let stopped=fixture.post(generation,json!({"request":"submit","client":"web-fixture","cell":"stop-scoped-view","text":":cancel $study"})).await;
    assert_eq!(stopped.status(), 200);
    let expired = fixture
        .client
        .get(&frame_url)
        .header("X-Wes-Session", generation)
        .header("X-Wes-Scope", scope)
        .header("X-Wes-View-Mount", token)
        .send()
        .await
        .unwrap();
    assert_eq!(expired.status(), 410);
    assert_eq!(fixture.calls.load(Ordering::SeqCst), 0);
    fixture.close().await;
}
