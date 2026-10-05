use super::*;

fn descriptor(route: &str) -> String {
    json!({"version":1,"provider":"synthetic","types":{},"operations":[{
        "path":["latest"],"method":"GET","route":route,"parameters":[],
        "responses":{"200":"Unknown"},"auth":[]
    }]})
    .to_string()
}
struct SpecFixture {
    app: ApplicationHandle,
    runtime: wes::runtime::LaunchedRuntime,
    server: wes::web::Server,
    client: reqwest::Client,
}
impl SpecFixture {
    fn url(&self, path: &str) -> String {
        format!("http://{}{path}", self.server.address())
    }
    async fn post(&self, generation: &str, command: Value) -> reqwest::Response {
        self.client
            .post(self.url("/submit"))
            .header("Content-Type", "application/json")
            .header("X-Wes-Session", generation)
            .body(command.to_string())
            .send()
            .await
            .unwrap()
    }
    async fn source(&self, generation: &str, cell: &str, text: &str) -> u16 {
        self.post(
            generation,
            json!({"request":"submit","client":"web-fixture","cell":cell,"text":text}),
        )
        .await
        .status()
        .as_u16()
    }
    async fn stream(&self) -> Events {
        Events {
            response: self.client.get(self.url("/events")).send().await.unwrap(),
            pending: String::new(),
        }
    }
    async fn close(self) {
        self.server.shutdown().await.unwrap();
        self.runtime.shutdown().await.unwrap();
    }
}
async fn fixture(base: &std::path::Path) -> SpecFixture {
    let mut options = wes::runtime::RuntimeOptions::new(base.join("home"), base.to_owned());
    options.docker_candidates = Some(vec![base.join("absent-docker.sock")]);
    let runtime = wes::runtime::launch(options).await.unwrap();
    let server = runtime.serve(0, None).await.unwrap();
    let app = runtime.handle.clone();
    let client = reqwest::Client::builder()
        .no_proxy()
        .timeout(Duration::from_secs(8))
        .build()
        .unwrap();
    SpecFixture {
        app,
        runtime,
        server,
        client,
    }
}
async fn list(f: &SpecFixture, workspace: &str, generation: &str) -> Value {
    let url = url::Url::parse(&f.url("/workspace-specs")).unwrap();
    let response = f
        .client
        .get(url)
        .header("X-Wes-Workspace", workspace)
        .header("X-Wes-Session", generation)
        .send()
        .await
        .unwrap();
    assert!(
        response.status() == 200,
        "list {workspace} {generation}: {}",
        response.text().await.unwrap()
    );
    assert_eq!(response.headers()["cache-control"], "no-store");
    decode(response).await
}
async fn read(
    f: &SpecFixture,
    workspace: &str,
    generation: &str,
    spec: &Value,
) -> reqwest::Response {
    let mut url = url::Url::parse(&f.url("/workspace-specs")).unwrap();
    url.query_pairs_mut().extend_pairs([
        ("environment", spec["environment"].as_str().unwrap()),
        ("alias", spec["alias"].as_str().unwrap()),
        ("revision", spec["revision"].as_str().unwrap()),
    ]);
    f.client
        .get(url)
        .header("X-Wes-Workspace", workspace)
        .header("X-Wes-Session", generation)
        .send()
        .await
        .unwrap()
}
async fn submit(f: &SpecFixture, generation: &str, id: &str, text: &str) {
    let response = f
        .post(
            generation,
            json!({"request":"submit","client":"web-fixture","cell":id,"text":text}),
        )
        .await;
    let status = response.status();
    assert_eq!(status, 202, "{}", response.text().await.unwrap());
    f.app.current().unwrap().session.wait_idle().await.unwrap();
}
#[tokio::test]
async fn direct_import_inspection_is_captured_scoped_and_survives_restore() {
    let base = tempfile::tempdir().unwrap();
    let original = descriptor("/latest");
    std::fs::write(base.path().join("spec.json"), &original).unwrap();
    let f = fixture(base.path()).await;
    let generation = f.app.current().unwrap().generation;
    submit(
        &f,
        &generation,
        "import",
        ":import spec file:spec.json as:sensor endpoint:\"https://example.invalid\"",
    )
    .await;
    let initial = list(&f, "default", &generation).await;
    assert_eq!(initial["specs"].as_array().unwrap().len(), 1, "{initial}");
    let spec = &initial["specs"][0];
    assert_eq!(spec["alias"], "sensor");
    assert_eq!(spec["environment"], "default");
    assert_eq!(spec["origin"], "spec.json");
    assert!(spec.get("source").is_none());
    assert_eq!(
        spec["revision"],
        wes_adapters::api_library::digest(original.as_bytes())
    );
    let changed = descriptor("/changed");
    std::fs::write(base.path().join("spec.json"), &changed).unwrap();
    assert_eq!(
        decode(read(&f, "default", &generation, spec).await).await["source"],
        original
    );
    submit(&f, &generation, "save", ":workspace save \"captured\"").await;
    submit(
        &f,
        &generation,
        "replace",
        ":import spec file:spec.json as:sensor endpoint:\"https://example.invalid\" replace:true",
    )
    .await;
    assert_eq!(read(&f, "default", &generation, spec).await.status(), 409);
    let replacement = list(&f, "default", &generation).await;
    assert_eq!(replacement["specs"].as_array().unwrap().len(), 1);
    assert_ne!(replacement["specs"][0]["revision"], spec["revision"]);
    let other = f
        .app
        .open_workspace(WorkspaceName::new("other".into()).unwrap(), true)
        .await
        .unwrap();
    assert_eq!(
        list(&f, "other", &other.generation).await["specs"],
        json!([])
    );
    assert_eq!(
        read(&f, "other", &other.generation, spec).await.status(),
        404
    );
    assert_eq!(read(&f, "default", "outdated", spec).await.status(), 409);
    assert_eq!(
        f.client
            .get(f.url("/workspace-specs?workspace=default"))
            .header("X-Wes-Session", &generation)
            .header("origin", "https://untrusted.invalid")
            .send()
            .await
            .unwrap()
            .status(),
        403
    );
    std::fs::remove_file(base.path().join("spec.json")).unwrap();
    let mut events = f.stream().await;
    events.generation().await;
    assert_eq!(
        f.source(&generation, "load", ":workspace load \"captured\"")
            .await,
        202
    );
    let restored = events.generation().await;
    assert_ne!(restored, generation);
    assert_eq!(
        list(&f, "captured", &restored).await["specs"],
        initial["specs"]
    );
    assert_eq!(
        decode(read(&f, "captured", &restored, spec).await).await["source"],
        original
    );
    assert!(
        f.app
            .current()
            .unwrap()
            .session
            .snapshot()
            .await
            .unwrap()
            .execution
            .graph
            .nodes()
            .next()
            .is_none()
    );
    drop(events);
    f.close().await;
}
#[tokio::test]
async fn environment_recipe_specs_are_inspected_without_reopening_the_source() {
    let base = tempfile::tempdir().unwrap();
    let original = descriptor("/recipe");
    std::fs::write(base.path().join("spec.json"), &original).unwrap();
    std::fs::write(base.path().join("env.yaml"),"version: 1\ntargets: {local: {kind: local}}\nenvironments:\n  lab:\n    imports:\n      prices:\n        source: {kind: spec, file: spec.json}\n        bind: {target: local, endpoint: 'https://example.invalid'}\n  inherited: {extends: {env: lab, track: latest}}\n").unwrap();
    let f = fixture(base.path()).await;
    let current = f.app.current().unwrap();
    let plan = current
        .session
        .plan_environment_file("env.yaml".into(), false)
        .await
        .unwrap();
    current.session.apply_environments(plan).await.unwrap();
    std::fs::remove_file(base.path().join("spec.json")).unwrap();
    let captured = list(&f, "default", &current.generation).await;
    let specs = captured["specs"].as_array().unwrap();
    assert_eq!(specs.len(), 2, "{captured}");
    for spec in specs {
        assert_eq!(spec["alias"], "prices");
        assert_eq!(
            decode(read(&f, "default", &current.generation, spec).await).await["source"],
            original
        );
    }
    f.close().await;
}

async fn decode(response: reqwest::Response) -> Value {
    serde_json::from_slice(&response.bytes().await.unwrap()).unwrap()
}
