#[path = "web/budgets.rs"]
mod budgets;
#[path = "web/calculation.rs"]
mod calculation;
#[path = "web/documents.rs"]
mod documents;
#[path = "web/environment_authentication.rs"]
mod environment_authentication;
#[path = "web/language.rs"]
mod language;
#[path = "web/multi_workspaces.rs"]
mod multi_workspaces;
#[path = "web/presentations.rs"]
mod presentations;
#[path = "web/publication.rs"]
mod publication;
#[path = "web/read_admission.rs"]
mod read_admission;
#[path = "web/repeat.rs"]
mod repeat;
#[path = "web/retention.rs"]
mod retention;
#[path = "web/sandbox.rs"]
mod sandbox;
#[path = "web/telemetry.rs"]
mod telemetry;
#[path = "web/view_instances.rs"]
mod view_instances;
#[path = "web/view_packages.rs"]
mod view_packages;
#[path = "web/views.rs"]
mod views;
#[path = "web/workspace_specs.rs"]
mod workspace_specs;
use serde_json::{Value, json};
use std::{
    num::NonZeroUsize,
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};
use tokio::io::AsyncWriteExt;
use wes::{ApplicationHandle, ApplicationTask};
use wes_adapters::{
    codec::Limits,
    credentials::{CredentialLimits, MemoryCredentials},
    journal::Durability,
    storage::TieredValues,
};
use wes_core::{
    Shape,
    capability::{Capability, Parameter, ProviderDescription, Safety},
};
use wes_engine::{
    driver::CancellationToken,
    providers::{Call, InvocationFuture, Invoker},
    session::SessionStorage,
    storage::{AutoKeep, StoreWorker, StoreWorkerLimits, StoreWorkerTask, spawn_store},
    type_sources::{TypeSourceError, TypeSourceReader},
    workspace::{Workspace, WorkspaceName},
};
struct NoFiles;
impl TypeSourceReader for NoFiles {
    fn read(&self, _: &str, _: usize) -> Result<String, TypeSourceError> {
        panic!("no live source reads")
    }
}
struct Echo(Arc<AtomicUsize>);
impl Invoker for Echo {
    fn invoke(&self, call: Call, _: CancellationToken) -> InvocationFuture {
        self.0.fetch_add(1, Ordering::SeqCst);
        Box::pin(async move { Ok(call.arguments["value"].clone()) })
    }
}
type Install =
    Arc<dyn Fn(&mut Workspace) -> Result<(), wes_engine::workspace::WorkspaceError> + Send + Sync>;
struct Fixture {
    root: tempfile::TempDir,
    app: ApplicationHandle,
    task: ApplicationTask,
    worker: StoreWorker,
    writer: StoreWorkerTask,
    server: wes::web::Server,
    client: reqwest::Client,
    calls: Arc<AtomicUsize>,
}
impl Fixture {
    async fn new() -> Self {
        Self::with_services(|_| wes::web::Services::default()).await
    }
    async fn with_services(configure: impl FnOnce(&std::path::Path) -> wes::web::Services) -> Self {
        Self::configured(configure, Arc::new(|_| Ok(()))).await
    }
    async fn configured(
        configure: impl FnOnce(&std::path::Path) -> wes::web::Services,
        install: Install,
    ) -> Self {
        Self::configured_store(configure, install, |values| {
            spawn_store(values, StoreWorkerLimits::default()).unwrap()
        })
        .await
    }
    async fn configured_store(
        configure: impl FnOnce(&std::path::Path) -> wes::web::Services,
        install: Install,
        storage: impl FnOnce(TieredValues) -> (StoreWorker, StoreWorkerTask),
    ) -> Self {
        let root = tempfile::tempdir().unwrap();
        let site = root.path().join("site");
        std::fs::create_dir(&site).unwrap();
        std::fs::write(site.join("index.html"), "<title>fixture</title>").unwrap();
        let values = TieredValues::open(
            &root.path().join("live"),
            &root.path().join("archive"),
            Limits::default(),
            Durability::File,
            None,
        )
        .unwrap();
        let (worker, writer) = storage(values);
        let calls = Arc::new(AtomicUsize::new(0));
        let invoked = calls.clone();
        let (app, task) = wes::open(wes::Config {
            directory: root.path().join("workspaces"),
            initial: WorkspaceName::new("default".into()).unwrap(),
            durability: Durability::File,
            max_streams: wes_engine::driver::DEFAULT_MAX_STREAMS,
            concurrency: NonZeroUsize::new(2).unwrap(),
            type_reader: Arc::new(NoFiles),
            storage: Some(SessionStorage {
                worker: worker.clone(),
                auto_keep: AutoKeep::default(),
            }),
            workspace: Arc::new(move || {
                let mut workspace =
                    Workspace::local(wes_engine::providers::LocalScope::new("fixture").unwrap())
                        .with_calculation_services(Arc::new(
                            wes_adapters::codec::CalculationServices,
                        ));
                let mut capability = Capability::new(["echo"], Shape::Unknown, Safety::Safe);
                capability
                    .parameters
                    .push(Parameter::new("value", Shape::Unknown, true));
                workspace.register_provider(
                    ProviderDescription::new("catalog", [capability], vec!["key".into()]).unwrap(),
                    Arc::new(Echo(invoked.clone())),
                )?;
                install(&mut workspace)?;
                Ok(workspace)
            }),
        })
        .await
        .unwrap();
        let server = wes::web::listen(wes::web::Config {
            services: configure(root.path()),
            port: 0,
            application: app.clone(),
            values: worker.clone(),
            credentials: Arc::new(MemoryCredentials::with_environment(
                CredentialLimits::default(),
                |_| Ok(None),
            )),
            site: Some(site),
        })
        .await
        .unwrap();
        let client = reqwest::Client::builder()
            .no_proxy()
            .timeout(Duration::from_secs(8))
            .build()
            .unwrap();
        Self {
            root,
            app,
            task,
            worker,
            writer,
            server,
            client,
            calls,
        }
    }
    fn url(&self, path: &str) -> String {
        format!("http://{}{path}", self.server.address())
    }
    fn completion_url(&self, pairs: &[(&str, &str)]) -> url::Url {
        let mut url = url::Url::parse(&self.url("/complete")).unwrap();
        url.query_pairs_mut().extend_pairs(pairs.iter().copied());
        url
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
        tokio::time::timeout(Duration::from_secs(5), self.server.shutdown())
            .await
            .unwrap()
            .unwrap();
        self.app.shutdown().await;
        self.task.join().await.unwrap();
        self.worker.shutdown().await.unwrap();
        self.writer.join().await.unwrap();
    }
}
struct Events {
    response: reqwest::Response,
    pending: String,
}
impl Events {
    async fn next(&mut self) -> Value {
        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                if let Some(end) = self.pending.find("\n\n") {
                    let frame = self.pending[..end].to_owned();
                    self.pending.drain(..end + 2);
                    if let Some(data) = frame.strip_prefix("data: ") {
                        return serde_json::from_str(data).unwrap();
                    }
                    continue;
                }
                let bytes = self
                    .response
                    .chunk()
                    .await
                    .unwrap()
                    .expect("event connection closed");
                self.pending.push_str(std::str::from_utf8(&bytes).unwrap());
            }
        })
        .await
        .unwrap()
    }
    async fn until(&mut self, name: &str) -> Value {
        loop {
            let event = self.next().await;
            if event["event"] == name {
                return event;
            }
            // History records now arrive in one atomic delta. Existing record-level
            // assertions consume the entries in wire order without inventing server frames.
            if event["event"] == "log-delta" && name.starts_with("log") {
                let entries = event["entries"].as_array().unwrap();
                self.pending = entries
                    .iter()
                    .map(|entry| format!("data: {entry}\n\n"))
                    .collect::<String>()
                    + &self.pending;
            }
        }
    }
    async fn generation(&mut self) -> String {
        self.until("session").await["generation"]
            .as_str()
            .unwrap()
            .into()
    }
}
#[tokio::test]
async fn source_requires_explicit_valid_client_without_admitting_work() {
    let fixture = Fixture::new().await;
    let mut events = fixture.stream().await;
    let generation = events.generation().await;
    for client in [
        None,
        Some(json!(null)),
        Some(json!("")),
        Some(json!("bad\nclient")),
        Some(json!("x".repeat(129))),
        Some(json!(17)),
    ] {
        let mut request = json!({"request":"submit","cell":"missing-owner","text":"catalog echo value:must-not-run"});
        if let Some(client) = client {
            request["client"] = client;
        }
        assert_eq!(fixture.post(&generation, request).await.status(), 400);
    }
    let current = fixture.app.current().unwrap();
    current.session.wait_idle().await.unwrap();
    assert!(current.session.observe().await.unwrap().cells.is_empty());
    assert_eq!(fixture.calls.load(Ordering::SeqCst), 0);
    fixture.close().await;
}

#[tokio::test]
async fn explicit_clients_keep_source_ownership_and_exact_retries_do_not_repeat_calls() {
    let fixture = Fixture::new().await;
    let mut events = fixture.stream().await;
    let generation = events.generation().await;
    for client in ["first-client", "second-client"] {
        let request = json!({"request":"submit","client":client,"cell":client,"text":"catalog echo value:hello"});
        assert_eq!(
            fixture.post(&generation, request.clone()).await.status(),
            202
        );
        fixture
            .app
            .current()
            .unwrap()
            .session
            .wait_idle()
            .await
            .unwrap();
        assert_eq!(fixture.post(&generation, request).await.status(), 202);
    }
    let current = fixture.app.current().unwrap();
    current.session.wait_idle().await.unwrap();
    let observation = current.session.observe().await.unwrap();
    assert_eq!(observation.cells.len(), 2);
    for cell in &observation.cells {
        assert_eq!(cell.input.client(), cell.input.cell());
    }
    assert_eq!(fixture.calls.load(Ordering::SeqCst), 2);
    fixture.close().await;
}

#[tokio::test]
async fn browser_command_result_reconnect_and_workspace_switch_do_not_repeat_effects() {
    let fixture = Fixture::new().await;
    let mut events = fixture.stream().await;
    let generation = events.generation().await;
    let context = events.until("workspace-context").await;
    assert_eq!(context["name"], "default");
    assert_eq!(context["saved"], json!(["default"]));
    assert_eq!(
        fixture
            .source(&generation, "first", "catalog echo value:hello > result")
            .await,
        202
    );
    let planned = events.until("planned").await;
    assert_eq!(planned["cell"], "first");
    let ready = events.until("ready").await;
    assert_eq!(ready["kept"], true);
    assert_eq!(ready["publication"]["state"], "available");
    assert_eq!(ready["publication"]["handle"], ready["handle"]);
    let result = fixture
        .client
        .get(fixture.url(&format!("/values/{}", ready["handle"].as_str().unwrap())))
        .send()
        .await
        .unwrap();
    let result: Value = serde_json::from_slice(&result.bytes().await.unwrap()).unwrap();
    assert_eq!(
        result,
        json!({"type":{"kind":"unknown"},"provenance":{},"data":"hello"})
    );
    assert_eq!(
        fixture
            .source(&generation, "first", "catalog echo value:hello > result")
            .await,
        202
    );
    assert_eq!(
        fixture
            .source(&generation, "first", "catalog echo value:different")
            .await,
        409
    );
    let mut reconnected = fixture.stream().await;
    assert_eq!(reconnected.generation().await, generation);
    let replayed_node = reconnected.until("created").await;
    assert!(replayed_node["startedAt"].is_string());
    let replayed = reconnected.until("ready").await;
    assert_eq!(replayed["handle"], ready["handle"]);
    assert_eq!(replayed["publication"], ready["publication"]);
    loop {
        let entry = reconnected.until("log").await;
        if entry["record"]["node"] == replayed_node["node"] && entry["record"]["state"] == "RUNNING"
        {
            assert_eq!(replayed_node["startedAt"], entry["record"]["at"]);
            break;
        }
    }
    let mut another = fixture.stream().await;
    another.generation().await;
    assert_eq!(
        another.until("created").await["startedAt"],
        replayed_node["startedAt"]
    );
    assert_eq!(fixture.calls.load(Ordering::SeqCst), 1);
    assert_eq!(
        fixture
            .source(&generation, "save", ":workspace save \"checkpoint\"")
            .await,
        202
    );
    assert_eq!(
        fixture
            .source(&generation, "second", "catalog echo value:second")
            .await,
        202
    );
    fixture
        .app
        .current()
        .unwrap()
        .session
        .wait_idle()
        .await
        .unwrap();
    assert_eq!(
        fixture
            .source(&generation, "load", ":workspace load \"checkpoint\"")
            .await,
        202
    );
    let next = events.generation().await;
    assert_ne!(next, generation);
    let context = events.until("workspace-context").await;
    assert_eq!(context["name"], "checkpoint");
    assert!(
        context["saved"]
            .as_array()
            .unwrap()
            .contains(&json!("checkpoint"))
    );
    assert_eq!(
        events.until("created").await["startedAt"],
        replayed_node["startedAt"]
    );
    let recovered = events.until("ready").await;
    assert_eq!(recovered["handle"], ready["handle"]);
    assert_eq!(recovered["publication"]["state"], "available");
    assert_eq!(recovered["publication"]["handle"], ready["handle"]);
    assert!(recovered["publication"]["problem"].is_null());
    assert_eq!(
        fixture
            .source(&generation, "late", "catalog echo value:must-not-run")
            .await,
        409
    );
    assert_eq!(fixture.calls.load(Ordering::SeqCst), 2);
    assert_eq!(
        fixture
            .app
            .current()
            .unwrap()
            .session
            .observe()
            .await
            .unwrap()
            .state
            .execution
            .graph
            .len(),
        1
    );
    fixture.close().await;
}

#[tokio::test]
async fn value_read_failures_explain_context_without_claiming_execution_failure() {
    let fixture = Fixture::new().await;
    for (handle, code, canonical) in [
        (
            "invalid-reference".to_owned(),
            "VALUE_INVALID_HANDLE",
            false,
        ),
        (
            wes_engine::storage::ValueHandle::fresh().to_string(),
            "VALUE_UNAVAILABLE",
            true,
        ),
    ] {
        let response = fixture
            .client
            .get(fixture.url(&format!("/values/{handle}")))
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), 404);
        let body: Value = serde_json::from_slice(&response.bytes().await.unwrap()).unwrap();
        assert_eq!(body["error"]["code"], code);
        assert_eq!(body["error"]["retryable"], false);
        assert_eq!(body["error"]["context"]["operation"], "read-value");
        assert_eq!(
            body["error"]["context"]["handle"],
            if canonical {
                json!(handle)
            } else {
                Value::Null
            }
        );
    }
    assert_eq!(fixture.calls.load(Ordering::SeqCst), 0);
    fixture.close().await;
}
#[tokio::test]
async fn loopback_origin_media_type_limits_and_unsupported_requests_fail_before_execution() {
    let fixture = Fixture::new().await;
    let mut events = fixture.stream().await;
    let generation = events.generation().await;
    let submit = fixture.url("/submit");
    for content in [
        "text/plain",
        "application/jsonp",
        "application/x-www-form-urlencoded",
    ] {
        let response = fixture
            .client
            .post(&submit)
            .header("X-Wes-Session", &generation)
            .header("Content-Type", content)
            .body("{}")
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), 415);
    }
    for (header, value) in [
        ("Origin", "http://evil.invalid"),
        ("Origin", "null"),
        ("Host", "evil.invalid"),
        ("Sec-Fetch-Site", "cross-site"),
    ] {
        let response = fixture
            .client
            .get(fixture.url("/events"))
            .header(header, value)
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), 403);
    }
    let native_origin = format!("http://{}", fixture.server.address());
    assert_eq!(
        fixture
            .client
            .get(fixture.url("/"))
            .header("Origin", native_origin)
            .send()
            .await
            .unwrap()
            .status(),
        200
    );
    assert_eq!(
        fixture
            .client
            .request(reqwest::Method::OPTIONS, &submit)
            .send()
            .await
            .unwrap()
            .status(),
        405
    );
    assert_eq!(
        fixture
            .post(&generation, json!({"request":"storage"}))
            .await
            .status(),
        501
    );
    assert_eq!(
        fixture
            .post(
                &generation,
                json!({"request":"input","node":"n","text":"private"})
            )
            .await
            .status(),
        400
    );
    assert_eq!(
        fixture
            .post(
                &generation,
                json!({"request":"submit","client":"web-fixture","cell":"x","text":":help","extra":true})
            )
            .await
            .status(),
        400
    );
    assert_eq!(
        fixture
            .post(
                &generation,
                json!({"request":"submit","client":"web-fixture","cell":"big","text":"x".repeat(8*1024*1024)})
            )
            .await
            .status(),
        413
    );
    assert_eq!(
        fixture
            .client
            .get(fixture.url("/values/%2e%2e%2fprivate"))
            .send()
            .await
            .unwrap()
            .status(),
        404
    );
    assert_eq!(
        fixture
            .client
            .get(fixture.url("/%2e%2e/private"))
            .send()
            .await
            .unwrap()
            .status(),
        404
    );
    assert_eq!(fixture.calls.load(Ordering::SeqCst), 0);
    fixture.close().await;
}
#[tokio::test]
async fn credential_updates_are_one_way_and_never_enter_cells_or_history() {
    let fixture = Fixture::new().await;
    let mut events = fixture.stream().await;
    let generation = events.generation().await;
    assert_eq!(
        events.until("vocabulary").await["providers"][0]["ready"],
        false
    );
    let secret = "test-secret-never-in-an-event";
    assert_eq!(
        fixture
            .post(
                &generation,
                json!({"request":"secret","name":"key","value":secret})
            )
            .await
            .status(),
        202
    );
    let vocabulary = events.until("vocabulary").await;
    assert_eq!(vocabulary["providers"][0]["ready"], true);
    assert!(!vocabulary.to_string().contains(secret));
    let observation = fixture
        .app
        .current()
        .unwrap()
        .session
        .observe()
        .await
        .unwrap();
    assert!(observation.cells.is_empty());
    assert!(observation.log.entries.is_empty());
    let checkpoint = fixture
        .app
        .current()
        .unwrap()
        .session
        .checkpoint()
        .await
        .unwrap();
    assert!(checkpoint.history().journal().is_empty());
    drop(checkpoint);
    assert_eq!(
        fixture
            .post(
                &generation,
                json!({"request":"secret","name":"key","value":""})
            )
            .await
            .status(),
        202
    );
    assert_eq!(
        events.until("vocabulary").await["providers"][0]["ready"],
        false
    );
    fixture.close().await;
}
#[tokio::test]
async fn connected_and_slow_clients_are_bounded_and_do_not_prevent_joined_shutdown() {
    let fixture = Fixture::new().await;
    let mut streams = Vec::new();
    for _ in 0..8 {
        let mut stream = fixture.stream().await;
        stream.generation().await;
        streams.push(stream);
    }
    assert_eq!(
        fixture
            .client
            .get(fixture.url("/events"))
            .send()
            .await
            .unwrap()
            .status(),
        503
    );
    let mut slow = tokio::net::TcpStream::connect(fixture.server.address())
        .await
        .unwrap();
    slow.write_all(b"POST /submit HTTP/1.1\r\nHost: ")
        .await
        .unwrap();
    // Active SSE responses and an unfinished HTTP header remain open throughout shutdown.
    fixture.close().await;
    drop((slow, streams));
}
#[cfg(unix)]
#[tokio::test]
async fn site_snapshot_rejects_symlinks_instead_of_serving_outside_files() {
    let fixture = Fixture::new().await;
    let site = fixture.root.path().join("unsafe-site");
    std::fs::create_dir(&site).unwrap();
    std::os::unix::fs::symlink(fixture.root.path().join("workspaces"), site.join("outside"))
        .unwrap();
    let result = wes::web::listen(wes::web::Config {
        services: wes::web::Services::default(),
        port: 0,
        application: fixture.app.clone(),
        values: fixture.worker.clone(),
        credentials: Arc::new(MemoryCredentials::new(CredentialLimits::default())),
        site: Some(site),
    })
    .await;
    assert!(result.is_err());
    fixture.close().await;
}

#[tokio::test]
async fn a_body_that_finishes_after_loading_stays_bound_to_its_original_session() {
    use tokio::io::AsyncReadExt;
    let fixture = Fixture::new().await;
    let mut events = fixture.stream().await;
    let generation = events.generation().await;
    assert_eq!(
        fixture
            .source(&generation, "save", ":workspace save \"empty\"")
            .await,
        202
    );
    let body =
        json!({"request":"submit","client":"web-fixture","cell":"delayed","text":"catalog echo value:must-not-run"})
            .to_string();
    let mut socket = tokio::net::TcpStream::connect(fixture.server.address())
        .await
        .unwrap();
    let headers = format!(
        "POST /submit HTTP/1.1\r\nHost: {}\r\nContent-Type: application/json\r\nX-Wes-Session: {generation}\r\nContent-Length: {}\r\nExpect: 100-continue\r\nConnection: close\r\n\r\n",
        fixture.server.address(),
        body.len()
    );
    socket.write_all(headers.as_bytes()).await.unwrap();
    let mut interim = Vec::new();
    tokio::time::timeout(Duration::from_secs(3), async {
        while !interim.ends_with(b"\r\n\r\n") {
            interim.push(socket.read_u8().await.unwrap());
        }
    })
    .await
    .unwrap();
    assert!(String::from_utf8(interim).unwrap().contains("100 Continue"));
    // The handler has bound the session and is now awaiting bytes from the old request.
    assert_eq!(
        fixture
            .source(&generation, "load", ":workspace load \"empty\"")
            .await,
        202
    );
    socket.write_all(body.as_bytes()).await.unwrap();
    let mut response = String::new();
    tokio::time::timeout(Duration::from_secs(3), socket.read_to_string(&mut response))
        .await
        .unwrap()
        .unwrap();
    assert!(response.starts_with("HTTP/1.1 202"), "{response}");
    let original = fixture.app.bound("default").unwrap().current().unwrap();
    original.session.wait_idle().await.unwrap();
    let original_state = original.session.observe().await.unwrap();
    let selected_state = fixture
        .app
        .current()
        .unwrap()
        .session
        .observe()
        .await
        .unwrap();
    assert!(
        original_state
            .cells
            .iter()
            .any(|cell| cell.input.cell() == "delayed")
    );
    assert!(
        !selected_state
            .cells
            .iter()
            .any(|cell| cell.input.cell() == "delayed")
    );
    assert_eq!(fixture.calls.load(Ordering::SeqCst), 1);
    fixture.close().await;
}

fn local_services(root: &std::path::Path) -> wes::web::Services {
    let mut inventory = wes_adapters::inventory::FileInventory::new();
    inventory
        .add(
            "workspaces",
            &root.join("workspaces"),
            "All saved workspace generations",
            true,
            1,
        )
        .unwrap();
    inventory
        .add("live", &root.join("live"), "Evictable values", false, 0)
        .unwrap();
    let mut services = wes::web::Services {
        inventory: Some(inventory),
        ..Default::default()
    };
    services.completers.insert(
        "sh".into(),
        Arc::new(
            wes_adapters::completion::ShellCompleter::new(root.into(), Some(root.into()), vec![])
                .unwrap(),
        ),
    );
    services
}

#[tokio::test]
async fn storage_reports_actual_metadata_for_the_selected_workspace_without_admitting_commands() {
    let fixture = Fixture::with_services(local_services).await;
    let mut events = fixture.stream().await;
    let generation = events.generation().await;
    assert_eq!(
        fixture
            .post(&generation, json!({"request":"storage"}))
            .await
            .status(),
        202
    );
    let storage = events.until("storage").await;
    assert_eq!(storage["workspace"], "default");
    assert_eq!(
        storage["places"][0]["where"],
        fixture
            .root
            .path()
            .join("workspaces")
            .canonicalize()
            .unwrap()
            .to_str()
            .unwrap()
    );
    assert!(storage["places"][0]["files"].as_u64().unwrap() > 0);
    assert!(storage["places"][0]["bytes"].as_u64().unwrap() > 0);
    let observation = fixture
        .app
        .current()
        .unwrap()
        .session
        .observe()
        .await
        .unwrap();
    assert!(observation.cells.is_empty());
    assert!(observation.log.entries.is_empty());
    let checkpoint = fixture
        .app
        .current()
        .unwrap()
        .session
        .checkpoint()
        .await
        .unwrap();
    assert!(checkpoint.history().journal().is_empty());
    drop(checkpoint);
    assert_eq!(
        fixture
            .source(&generation, "save", ":workspace save \"other\"")
            .await,
        202
    );
    assert_eq!(
        fixture
            .source(&generation, "load", ":workspace load \"other\"")
            .await,
        202
    );
    let next = events.generation().await;
    assert_eq!(
        fixture
            .post(&generation, json!({"request":"storage"}))
            .await
            .status(),
        409
    );
    assert_eq!(
        fixture
            .post(&next, json!({"request":"storage"}))
            .await
            .status(),
        202
    );
    assert_eq!(events.until("storage").await["workspace"], "other");
    assert_eq!(fixture.calls.load(Ordering::SeqCst), 0);
    fixture.close().await;
}

#[tokio::test]
async fn completion_converts_utf16_carets_and_never_runs_a_provider_or_records_source() {
    let fixture = Fixture::with_services(local_services).await;
    std::fs::write(fixture.root.path().join("candidate"), "fixture").unwrap();
    for (language, line, caret, status, expected) in [
        ("sh", "pri", "3", 200, Some((0, "printf"))),
        ("sh", "echo 😀 can", "11", 200, Some((8, "candidate"))),
        ("sh", "echo 😀 can", "6", 400, None),
        ("sh", "pri", "9999", 200, Some((0, "printf"))),
        ("sh", "pri", "invalid", 200, Some((0, "printf"))),
        ("unknown", "pri", "3", 200, None),
    ] {
        let response = fixture
            .client
            .get(fixture.completion_url(&[("in", language), ("line", line), ("caret", caret)]))
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), status);
        if status == 200 {
            let answer: Value = serde_json::from_slice(&response.bytes().await.unwrap()).unwrap();
            if let Some((from, text)) = expected {
                assert_eq!(answer["from"], from);
                assert_eq!(answer["items"][0]["text"], text);
            } else {
                assert_eq!(answer["items"], json!([]));
            }
        }
    }
    let response = fixture
        .client
        .get(fixture.completion_url(&[("in", "sh"), ("line", &"x".repeat(16 * 1024 + 1))]))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 413);
    let observation = fixture
        .app
        .current()
        .unwrap()
        .session
        .observe()
        .await
        .unwrap();
    assert!(observation.cells.is_empty());
    assert!(observation.log.entries.is_empty());
    assert_eq!(fixture.calls.load(Ordering::SeqCst), 0);
    fixture.close().await;
}

struct BlockingCompletion {
    entered: tokio::sync::mpsc::UnboundedSender<()>,
    completed: Arc<AtomicUsize>,
}
impl wes_engine::completion::Completer for BlockingCompletion {
    fn complete(
        &self,
        _: &str,
        _: usize,
        cancelled: &CancellationToken,
    ) -> Result<wes_engine::completion::Suggestions, wes_engine::completion::CompletionError> {
        self.entered.send(()).unwrap();
        let deadline = std::time::Instant::now() + Duration::from_secs(7);
        while !cancelled.is_cancelled() && std::time::Instant::now() < deadline {
            std::thread::park_timeout(Duration::from_millis(5));
        }
        self.completed.fetch_add(1, Ordering::SeqCst);
        Err(wes_engine::completion::CompletionError::Cancelled)
    }
}
#[tokio::test]
async fn disconnected_completion_requests_keep_their_physical_budget_and_join_on_shutdown() {
    let (entered, mut entries) = tokio::sync::mpsc::unbounded_channel();
    let completed = Arc::new(AtomicUsize::new(0));
    let joined = completed.clone();
    let fixture = Fixture::with_services(move |_| {
        let mut services = wes::web::Services::default();
        services.completers.insert(
            "blocked".into(),
            Arc::new(BlockingCompletion {
                entered,
                completed: joined,
            }),
        );
        services
    })
    .await;
    for _ in 0..4 {
        let request = fixture.client.get(fixture.url("/complete?in=blocked"));
        let task = tokio::spawn(async move { request.send().await });
        tokio::time::timeout(Duration::from_secs(3), entries.recv())
            .await
            .unwrap()
            .unwrap();
        task.abort();
        let _ = task.await;
    }
    assert_eq!(
        fixture
            .client
            .get(fixture.url("/complete?in=blocked"))
            .send()
            .await
            .unwrap()
            .status(),
        503
    );
    assert_eq!(
        fixture
            .client
            .get(fixture.url("/"))
            .send()
            .await
            .unwrap()
            .status(),
        200
    );
    assert_eq!(completed.load(Ordering::SeqCst), 0);
    fixture.close().await;
    assert_eq!(completed.load(Ordering::SeqCst), 4);
}

#[path = "web/streams.rs"]
mod streams;

#[path = "web/conversations.rs"]
mod conversations;

#[path = "web/history.rs"]
mod history;

#[tokio::test]
async fn batch_nodes_preserve_original_cell_source_on_creation_and_held_reopen() {
    let fixture = Fixture::new().await;
    let mut events = fixture.stream().await;
    let generation = events.generation().await;
    let source = "catalog echo value:\"😀\" > first\n  catalog echo value:second > second";
    assert_eq!(fixture.source(&generation, "batch", source).await, 202);
    for _ in 0..2 {
        assert_eq!(events.until("created").await["command"], source);
    }
    fixture
        .app
        .current()
        .unwrap()
        .session
        .wait_idle()
        .await
        .unwrap();
    assert_eq!(
        fixture
            .source(&generation, "save", ":workspace save \"batch-copy\"")
            .await,
        202
    );
    assert_eq!(
        fixture
            .source(&generation, "load", ":workspace load \"batch-copy\"")
            .await,
        202
    );
    assert_ne!(events.generation().await, generation);
    for _ in 0..2 {
        assert_eq!(events.until("created").await["command"], source);
    }
    assert_eq!(fixture.calls.load(Ordering::SeqCst), 2);
    drop(events);
    fixture.close().await;
}

#[tokio::test]
async fn oversized_projection_keeps_server_and_values_available_and_load_recovers_the_client() {
    let fixture = Fixture::new().await;
    let mut events = fixture.stream().await;
    let generation = events.generation().await;
    assert_eq!(
        fixture
            .source(&generation, "small", "catalog echo value:small > small")
            .await,
        202
    );
    let kept = events.until("ready").await;
    assert_eq!(
        fixture
            .source(&generation, "save", ":workspace save \"smaller\"")
            .await,
        202
    );
    fixture
        .app
        .current()
        .unwrap()
        .session
        .wait_idle()
        .await
        .unwrap();
    // One accepted source is repeated in per-node compatibility descriptions; the resulting client
    // metadata exceeds 8 MiB even though the command and history remain within engine budgets.
    let source = " ".repeat(700_000)
        + &(0..12)
            .map(|i| format!("catalog echo value:item{i} > item{i}\n"))
            .collect::<String>();
    assert_eq!(fixture.source(&generation, "large", &source).await, 202);
    let unavailable = events.until("projection-unavailable").await;
    fixture
        .app
        .current()
        .unwrap()
        .session
        .wait_idle()
        .await
        .unwrap();
    assert_eq!(fixture.calls.load(Ordering::SeqCst), 13);
    assert_eq!(
        fixture
            .client
            .get(fixture.url("/"))
            .send()
            .await
            .unwrap()
            .status(),
        200
    );
    assert_eq!(
        fixture
            .client
            .get(fixture.url(&format!("/values/{}", kept["handle"].as_str().unwrap())))
            .send()
            .await
            .unwrap()
            .status(),
        200
    );
    let mut reconnected = fixture.stream().await;
    assert_eq!(reconnected.generation().await, generation);
    assert_eq!(
        reconnected.until("projection-unavailable").await,
        unavailable
    );
    assert_eq!(
        fixture
            .source(&generation, "recover", ":workspace load \"smaller\"")
            .await,
        202
    );
    assert_ne!(events.generation().await, generation);
    assert_eq!(events.until("ready").await["handle"], kept["handle"]);
    assert_eq!(fixture.calls.load(Ordering::SeqCst), 13);
    drop(events);
    drop(reconnected);
    fixture.close().await;
}

#[tokio::test]
async fn operational_notice_preserves_context_and_missing_restore_is_visible_without_reexecution() {
    let fixture = Fixture::new().await;
    let mut events = fixture.stream().await;
    let generation = events.generation().await;
    assert_eq!(
        fixture
            .source(&generation, "value", "catalog echo value:retained > result")
            .await,
        202
    );
    let ready = events.until("ready").await;
    let handle = wes_engine::storage::ValueHandle::new(ready["handle"].as_str().unwrap()).unwrap();
    assert_eq!(
        fixture
            .source(&generation, "save", ":workspace save \"before-loss\"")
            .await,
        202
    );
    fixture
        .app
        .current()
        .unwrap()
        .session
        .wait_idle()
        .await
        .unwrap();
    // Test-owned storage loss after a valid checkpoint. Never touch a live user's store.
    assert!(fixture.worker.release(handle.clone()).await.unwrap());
    assert!(
        !fixture
            .post(
                &generation,
                json!({"request":"keep", "handle":handle.as_str()})
            )
            .await
            .status()
            .is_success()
    );
    let notice = loop {
        let notice = events.until("log-notice").await;
        if notice["durable"] == true {
            break notice;
        }
    };
    assert_eq!(notice["record"]["context"]["kind"], "keep");
    assert_eq!(notice["record"]["context"]["handle"], handle.as_str());
    assert_eq!(notice["record"]["context"]["mayHaveApplied"], true);
    assert_eq!(notice["durable"], true);
    assert!(
        notice["record"]["error"]["id"]
            .as_str()
            .is_some_and(|id| !id.is_empty())
    );
    assert!(notice["record"].get("diagnostic").is_none());
    let snapshot = fixture
        .app
        .current()
        .unwrap()
        .session
        .snapshot()
        .await
        .unwrap();
    assert!(snapshot.execution.errors.is_empty());
    assert_eq!(fixture.calls.load(Ordering::SeqCst), 1);
    let mut reconnected = fixture.stream().await;
    assert_eq!(reconnected.generation().await, generation);
    assert_eq!(reconnected.until("log-notice").await, notice);
    assert_eq!(
        fixture
            .source(&generation, "load", ":workspace load \"before-loss\"")
            .await,
        202
    );
    assert_ne!(events.generation().await, generation);
    let warning = events.until("startup-warning").await;
    assert_eq!(warning["node"], ready["node"]);
    assert_eq!(warning["handle"], handle.as_str());
    assert!(
        warning["message"]
            .as_str()
            .unwrap()
            .contains("was not repeated")
    );
    let mut restored = fixture.stream().await;
    restored.generation().await;
    assert_eq!(restored.until("startup-warning").await, warning);
    assert_eq!(fixture.calls.load(Ordering::SeqCst), 1);
    drop(events);
    drop(reconnected);
    drop(restored);
    fixture.close().await;
}

#[tokio::test]
async fn api_library_settings_are_backend_owned_and_mutations_require_same_origin_json() {
    let fixture = Fixture::with_services(|root| wes::web::Services {
        api_library: Some(wes::api_library::ApiLibrary::new(
            root.canonicalize().unwrap().join("library-settings"),
        )),
        ..Default::default()
    })
    .await;
    let url = fixture.url("/api-library");
    let response = fixture.client.get(&url).send().await.unwrap();
    assert_eq!(response.status(), 200);
    let value: Value = serde_json::from_slice(&response.bytes().await.unwrap()).unwrap();
    assert!(value["settings"].is_null());
    let body = json!({"action":"configure","expectedRevision":null,"settings":{"localDirectory":fixture.root.path().canonicalize().unwrap().join("library-settings/api-library")}});
    assert_eq!(
        fixture
            .client
            .post(&url)
            .header("content-type", "text/plain")
            .body(body.to_string())
            .send()
            .await
            .unwrap()
            .status(),
        415
    );
    assert_eq!(
        fixture
            .client
            .post(&url)
            .header("content-type", "application/json")
            .header("origin", "https://cross-site.invalid")
            .body(body.to_string())
            .send()
            .await
            .unwrap()
            .status(),
        403
    );
    assert_eq!(
        fixture
            .client
            .post(&url)
            .header("content-type", "application/json")
            .body(body.to_string())
            .send()
            .await
            .unwrap()
            .status(),
        200
    );
    assert_eq!(
        fixture
            .client
            .post(&url)
            .header("content-type", "application/json")
            .body(body.to_string())
            .send()
            .await
            .unwrap()
            .status(),
        400
    );
    assert_eq!(
        fixture
            .client
            .post(&url)
            .header("content-type", "application/json")
            .body("{\"action\":\"push\"}")
            .send()
            .await
            .unwrap()
            .status(),
        400
    );
    fixture.close().await;
}

#[tokio::test]
async fn desktop_preferences_reject_invalid_writes_and_report_storage_failures() {
    let fixture = Fixture::with_services(|root| wes::web::Services {
        desktop_preferences: Some(wes::web::DesktopPreferences::new(
            root.join("desktop-ui.json"),
        )),
        ..Default::default()
    })
    .await;
    let url = fixture.url("/client-preferences");
    let file = fixture.root.path().join("desktop-ui.json");
    let saved = r#"{"theme":"light","fontSize":18}"#;
    assert_eq!(
        fixture
            .client
            .put(&url)
            .header("Content-Type", "application/json")
            .body(saved)
            .send()
            .await
            .unwrap()
            .status(),
        200
    );
    let original = std::fs::read(&file).unwrap();
    for (body, media, origin, status) in [
        ("[]".into(), "application/json", None, 400),
        (
            "{\"theme\":1,\"theme\":2}".into(),
            "application/json",
            None,
            400,
        ),
        (saved.into(), "text/plain", None, 415),
        (
            saved.into(),
            "application/json",
            Some("https://foreign.invalid"),
            403,
        ),
        (
            "x".repeat(4 * 1024 * 1024 + 1),
            "application/json",
            None,
            413,
        ),
        // Fits the request node budget, but not the persisted version envelope's read budget.
        (
            json!({"numbers": vec![0; 49_997]}).to_string(),
            "application/json",
            None,
            500,
        ),
    ] {
        let mut request = fixture
            .client
            .put(&url)
            .header("Content-Type", media)
            .body(body);
        if let Some(origin) = origin {
            request = request.header("Origin", origin);
        }
        assert_eq!(request.send().await.unwrap().status().as_u16(), status);
        assert_eq!(std::fs::read(&file).unwrap(), original);
    }
    std::fs::write(&file, "corrupted").unwrap();
    assert_eq!(fixture.client.get(&url).send().await.unwrap().status(), 500);
    assert_eq!(std::fs::read(&file).unwrap(), b"corrupted");
    // A directory in the fixed file slot simulates an actual publication failure.
    std::fs::rename(&file, fixture.root.path().join("previous-ui.json")).unwrap();
    std::fs::create_dir(&file).unwrap();
    assert_eq!(
        fixture
            .client
            .put(&url)
            .header("Content-Type", "application/json")
            .body(saved)
            .send()
            .await
            .unwrap()
            .status(),
        500
    );
    assert_eq!(fixture.calls.load(Ordering::SeqCst), 0);
    fixture.close().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn environment_input_explanation_reaches_desktop_events_and_absolute_path_recovers() {
    let root = tempfile::tempdir().unwrap();
    let base = root.path().canonicalize().unwrap();
    let package_dir = base.join("example");
    std::fs::create_dir(&package_dir).unwrap();
    std::fs::write(
        package_dir.join("environments.yaml"),
        include_str!("../../../examples/http-inspection/environments.yaml"),
    )
    .unwrap();
    std::fs::write(
        package_dir.join("sensor.json"),
        include_str!("../../../examples/http-inspection/sensor.json"),
    )
    .unwrap();
    let runtime = wes::runtime::launch(wes::runtime::RuntimeOptions::new(
        base.join("home"),
        base.clone(),
    ))
    .await
    .unwrap();
    let server = runtime.serve_desktop(None).await.unwrap();
    let client = reqwest::Client::builder()
        .no_proxy()
        .timeout(Duration::from_secs(10))
        .build()
        .unwrap();
    let url = format!("http://{}", server.address());
    let mut events = Events {
        response: client.get(format!("{url}/events")).send().await.unwrap(),
        pending: String::new(),
    };
    let generation = events.generation().await;
    for (cell, text) in [
        (
            "missing",
            ":env plan file:environments.yaml > proposed".to_owned(),
        ),
        (
            "corrected",
            // A path is data: the language's own encoder writes it as a literal.
            format!(
                ":env plan file:{} > proposed",
                wes_language::quote_text(
                    &package_dir.join("environments.yaml").display().to_string()
                )
            ),
        ),
    ] {
        let response = client
            .post(format!("{url}/submit"))
            .header("X-Wes-Session", &generation)
            .header("Content-Type", "application/json")
            .body(
                json!({"request":"submit","client":"web-fixture","cell":cell,"text":text})
                    .to_string(),
            )
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), if cell == "missing" { 400 } else { 202 });
        if cell == "missing" {
            assert!(
                response
                    .text()
                    .await
                    .unwrap()
                    .contains("environment package: file not found")
            );
        }
        let report = events.until("reported").await;
        assert_eq!(report["cell"], cell);
        let diagnostics = report["diagnostics"].as_array().unwrap();
        if cell == "missing" {
            assert_eq!(diagnostics.len(), 1);
            // Existing envelope and domain code remain wire-compatible.
            assert_eq!(diagnostics[0]["code"], "ENG007");
            let message = diagnostics[0]["message"].as_str().unwrap();
            assert!(
                message.contains("ENV010: environment package: file not found"),
                "{message}"
            );
            assert!(message.contains(&format!("{:?}", base.join("environments.yaml"))));
            assert!(message.contains("Relative-path base:"));
            assert!(message.contains("absolute file path"));
        } else {
            assert!(
                !diagnostics.iter().any(|d| d["severity"] == "error"),
                "{report}"
            );
        }
    }
    drop(events);
    server.shutdown().await.unwrap();
    runtime.shutdown().await.unwrap();
}

#[tokio::test]
async fn vocabulary_projects_adapter_inputs_and_existing_subcommand_specs() {
    use wes_adapters::{
        http::HttpConfig,
        imports::{ProcessImporter, SpecImporter},
        process::ProcessConfig,
    };
    let base = tempfile::tempdir().unwrap();
    let path = base.path().to_owned();
    let fixture = Fixture::configured(
        |_| wes::web::Services::default(),
        Arc::new(move |workspace| {
            let credentials = Arc::new(MemoryCredentials::with_environment(
                CredentialLimits::default(),
                |_| Ok(None),
            ));
            workspace.register_importer(
                "spec".into(),
                Arc::new(SpecImporter::new(&path, credentials, HttpConfig::default()).unwrap()),
            )?;
            workspace.register_importer(
                "process".into(),
                Arc::new(ProcessImporter::new(&path, ProcessConfig::default()).unwrap()),
            )?;
            let package = workspace.prepare_type_package(
                "types: {Customer: {base: Record, fields: {name: Text}}}",
                wes_language::Span::at(0),
            )?;
            workspace.commit(package)?;
            Ok(())
        }),
    )
    .await;
    let mut events = fixture.stream().await;
    let vocabulary = events.until("vocabulary").await;
    let commands = vocabulary["commands"].as_array().unwrap();
    let refresh = commands
        .iter()
        .find(|command| command["name"] == "refresh")
        .unwrap();
    assert_eq!(refresh["implemented"], true);
    assert_eq!(refresh["open"], false);
    assert_eq!(
        refresh["parameters"],
        json!([{
            "name": "scope", "type": "Text", "required": false,
            "allowed": ["downstream"], "content": ""
        }])
    );
    let params = |command: &str, word: &str| -> Vec<String> {
        commands.iter().find(|c| c["name"] == command).unwrap()["variants"]
            .as_array()
            .unwrap()
            .iter()
            .find(|v| v["word"] == word)
            .unwrap()["parameters"]
            .as_array()
            .unwrap()
            .iter()
            .map(|p| p["name"].as_str().unwrap().to_owned())
            .collect()
    };
    assert_eq!(
        params("import", "spec"),
        ["as", "replace", "file", "url", "endpoint"]
    );
    assert_eq!(params("import", "process"), ["as", "replace", "bin"]);
    let import = commands.iter().find(|c| c["name"] == "import").unwrap();
    for name in ["spec", "process", "plan", "apply"] {
        assert!(import["takes"].as_array().unwrap().contains(&json!(name)));
    }
    let plan = import["variants"]
        .as_array()
        .unwrap()
        .iter()
        .find(|v| v["word"] == "plan")
        .unwrap();
    assert_eq!(plan["takes"], json!(["spec", "process"]));
    assert_eq!(params("import", "plan"), ["as"]);
    assert_eq!(params("import", "apply"), ["replace"]);
    let spec = plan["variants"]
        .as_array()
        .unwrap()
        .iter()
        .find(|v| v["word"] == "spec")
        .unwrap();
    assert_eq!(
        spec["parameters"]
            .as_array()
            .unwrap()
            .iter()
            .map(|p| p["name"].as_str().unwrap())
            .collect::<Vec<_>>(),
        ["as", "file", "url", "endpoint"]
    );
    assert_eq!(spec["takes"], json!([]));
    assert_eq!(spec["variants"], json!([]));
    assert_eq!(params("package", "load"), ["path", "source", "origin"]);
    assert_eq!(params("type", "check"), ["as"]);
    let inspect = commands.iter().find(|c| c["name"] == "inspect").unwrap();
    let selector = inspect["parameters"]
        .as_array()
        .unwrap()
        .iter()
        .find(|p| p["name"] == "type")
        .unwrap();
    for name in ["Int", "Customer", "List", "Map", "Option", "Iter"] {
        assert!(
            selector["allowed"]
                .as_array()
                .unwrap()
                .contains(&serde_json::json!(name)),
            "{selector}"
        );
    }
    assert_eq!(params("list", "types"), Vec::<String>::new());
    let types = vocabulary["types"].as_array().unwrap();
    for name in ["Int", "Text", "Customer", "List", "Map", "Option", "Iter"] {
        assert!(
            types.contains(&serde_json::json!(name)),
            "expected {name} in vocabulary.types: {types:?}"
        );
    }
    assert_eq!(fixture.calls.load(Ordering::SeqCst), 0);
    assert_eq!(std::fs::read_dir(base.path()).unwrap().count(), 0);
    fixture.close().await;
}

#[tokio::test]
async fn edit_files_save_writes_under_its_context_and_rejects_unsafe_or_oversized_requests() {
    let fixture = Fixture::with_services(|root| wes::web::Services {
        edit_home: Some(root.join("edit")),
        ..Default::default()
    })
    .await;
    let url = fixture.url("/edit-files");
    let post = |body: serde_json::Value| {
        let request = fixture
            .client
            .post(&url)
            .header("Content-Type", "application/json")
            .body(body.to_string());
        async move { request.send().await.unwrap() }
    };

    let response =
        post(json!({"context": "types", "name": "monitor", "content": "types: {}"})).await;
    assert_eq!(response.status(), 200);
    let body: serde_json::Value = serde_json::from_str(&response.text().await.unwrap()).unwrap();
    let saved_path = std::path::PathBuf::from(body["path"].as_str().unwrap());
    assert_eq!(
        saved_path,
        fixture.root.path().join("edit/types/monitor.yaml")
    );
    assert_eq!(std::fs::read_to_string(&saved_path).unwrap(), "types: {}");

    // Overwriting the same name replaces the file rather than appending or erroring.
    let response = post(
        json!({"context": "types", "name": "monitor", "content": "types: {A: {base: Record}}"}),
    )
    .await;
    assert_eq!(response.status(), 200);
    assert_eq!(
        std::fs::read_to_string(&saved_path).unwrap(),
        "types: {A: {base: Record}}"
    );

    for (context, name, content, status) in [
        ("bogus", "monitor", "x", 400),
        ("views", "monitor", "types: {}", 400), // not env/types
        ("types", "../escape", "x", 400),       // path traversal
        ("types", "a/b", "x", 400),             // embedded separator
        ("types", "", "x", 400),                // empty name
        ("types", &"x".repeat(200), "x", 400),  // name too long
        ("types", "big", &"x".repeat(1024 * 1024 + 8192), 413), // just over the 1 MiB content limit
    ] {
        let response = post(json!({"context": context, "name": name, "content": content})).await;
        assert_eq!(response.status().as_u16(), status, "{context}/{name}");
    }
    assert_eq!(fixture.calls.load(Ordering::SeqCst), 0);
    fixture.close().await;
}

#[tokio::test]
async fn edit_files_accepts_escaped_content_at_the_decoded_limit() {
    let fixture = Fixture::with_services(|root| wes::web::Services {
        edit_home: Some(root.join("edit")),
        ..Default::default()
    })
    .await;
    let content = "\u{1}".repeat(1024 * 1024);
    let response = fixture
        .client
        .post(fixture.url("/edit-files"))
        .header("Content-Type", "application/json")
        .body(json!({"context":"env", "name":"escaped", "content":content}).to_string())
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 200);
    assert_eq!(
        std::fs::read_to_string(fixture.root.path().join("edit/env/escaped.yaml")).unwrap(),
        content
    );
    fixture.close().await;
}

#[cfg(unix)]
#[tokio::test]
async fn edit_files_rejects_symlink_directories_and_does_not_follow_target_links() {
    use std::os::unix::fs::symlink;
    let fixture = Fixture::with_services(|root| wes::web::Services {
        edit_home: Some(root.join("edit")),
        ..Default::default()
    })
    .await;
    let outside = tempfile::tempdir().unwrap();
    symlink(outside.path(), fixture.root.path().join("edit")).unwrap();
    let post = || {
        fixture
            .client
            .post(fixture.url("/edit-files"))
            .header("Content-Type", "application/json")
            .body(json!({"context":"types", "name":"safe", "content":"new"}).to_string())
            .send()
    };
    assert_eq!(post().await.unwrap().status(), 500);
    assert_eq!(std::fs::read_dir(outside.path()).unwrap().count(), 0);
    std::fs::remove_file(fixture.root.path().join("edit")).unwrap();
    std::fs::create_dir(fixture.root.path().join("edit")).unwrap();
    symlink(outside.path(), fixture.root.path().join("edit/types")).unwrap();
    assert_eq!(post().await.unwrap().status(), 500);
    assert_eq!(std::fs::read_dir(outside.path()).unwrap().count(), 0);
    std::fs::remove_file(fixture.root.path().join("edit/types")).unwrap();
    std::fs::create_dir(fixture.root.path().join("edit/types")).unwrap();
    let target = outside.path().join("protected.yaml");
    std::fs::write(&target, "old").unwrap();
    symlink(&target, fixture.root.path().join("edit/types/safe.yaml")).unwrap();
    assert_eq!(post().await.unwrap().status(), 200);
    assert_eq!(std::fs::read_to_string(&target).unwrap(), "old");
    assert_eq!(
        std::fs::read_to_string(fixture.root.path().join("edit/types/safe.yaml")).unwrap(),
        "new"
    );
    fixture.close().await;
}

// Read-only workspace sockets use the exact projection/authority path of SSE while
// releasing HTTP/1 request slots for terminal input and finite queries.
type EventSocket =
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;
async fn socket_event(socket: &mut EventSocket, kind: &str) -> Value {
    use futures_util::{SinkExt, StreamExt};
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let frame = socket.next().await.unwrap().unwrap();
            if let tokio_tungstenite::tungstenite::Message::Text(text) = frame {
                let event: Value = serde_json::from_str(&text).unwrap();
                socket
                    .send(tokio_tungstenite::tungstenite::Message::Text(
                        format!("ack:{}", event["sequence"]).into(),
                    ))
                    .await
                    .unwrap();
                if let Some(found) = event["events"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .find(|event| event["event"] == kind)
                {
                    return found.clone();
                }
            }
        }
    })
    .await
    .unwrap()
}
async fn socket_open(fixture: &Fixture, suffix: &str) -> EventSocket {
    tokio_tungstenite::connect_async(
        fixture
            .url(&format!("/events/socket{suffix}"))
            .replacen("http:", "ws:", 1),
    )
    .await
    .unwrap()
    .0
}

#[tokio::test]
async fn workspace_socket_matches_sse_and_joins_shutdown_without_resubmission() {
    use futures_util::StreamExt;
    let fixture = Fixture::new().await;
    let mut events = fixture.stream().await;
    let generation = events.generation().await;
    let mut socket = socket_open(&fixture, "").await;
    assert!(
        !fixture.server.has_pending_io(),
        "read-only subscriptions must not block data-home switching"
    );
    assert_eq!(
        socket_event(&mut socket, "session").await["generation"],
        generation
    );
    assert_eq!(
        fixture
            .source(&generation, "socket-echo", "catalog echo value:synthetic")
            .await,
        202
    );
    let ready = socket_event(&mut socket, "ready").await;
    assert_eq!(ready["handle"], events.until("ready").await["handle"]);
    assert_eq!(fixture.calls.load(Ordering::SeqCst), 1);
    drop(socket);
    let mut socket = socket_open(&fixture, "").await;
    assert_eq!(
        socket_event(&mut socket, "ready").await["handle"],
        ready["handle"]
    );
    assert_eq!(fixture.calls.load(Ordering::SeqCst), 1);
    fixture.close().await;
    // Drain already queued metadata until the closed transport is observed.
    tokio::time::timeout(Duration::from_secs(2), async {
        while let Some(Ok(message)) = socket.next().await {
            if message.is_close() {
                break;
            }
        }
    })
    .await
    .unwrap();
}

#[tokio::test]
async fn workspace_socket_enforces_origin_binding_and_read_only_admission() {
    use futures_util::{SinkExt, StreamExt};
    use tokio_tungstenite::tungstenite::{Error, Message, client::IntoClientRequest};
    let fixture = Fixture::new().await;
    for (suffix, origin, status) in [
        ("", Some("https://foreign.invalid"), 403),
        ("?workspace=../bad", None, 400),
        ("?workspace=missing", None, 404),
        ("?workspace=default&workspace=other", None, 400),
    ] {
        let mut request = fixture
            .url(&format!("/events/socket{suffix}"))
            .replacen("http:", "ws:", 1)
            .into_client_request()
            .unwrap();
        if let Some(origin) = origin {
            request
                .headers_mut()
                .insert("Origin", origin.parse().unwrap());
        }
        let error = tokio_tungstenite::connect_async(request).await.unwrap_err();
        assert!(
            matches!(error, Error::Http(ref response) if response.status().as_u16() == status),
            "{error}"
        );
    }
    let mut socket = socket_open(&fixture, "").await;
    socket
        .send(Message::Text(
            r#"{"request":"submit","client":"web-fixture","text":"catalog echo value:forbidden"}"#
                .into(),
        ))
        .await
        .unwrap();
    tokio::time::timeout(Duration::from_secs(2), async {
        while let Some(Ok(message)) = socket.next().await {
            if message.is_close() {
                break;
            }
        }
    })
    .await
    .unwrap();
    assert_eq!(fixture.calls.load(Ordering::SeqCst), 0);
    fixture.close().await;
}

#[tokio::test]
async fn workspace_sockets_share_sse_capacity_and_have_a_global_bound() {
    use tokio_tungstenite::tungstenite::Error;
    let fixture = Fixture::new().await;
    let mut sockets = Vec::new();
    for _ in 0..8 {
        sockets.push(socket_open(&fixture, "").await);
    }
    assert_eq!(
        fixture
            .client
            .get(fixture.url("/events"))
            .send()
            .await
            .unwrap()
            .status(),
        503
    );
    for name in ["peer", "third"] {
        let response = fixture
            .client
            .post(fixture.url("/workspaces"))
            .header("Content-Type", "application/json")
            .body(json!({"name":name,"create":true}).to_string())
            .send()
            .await
            .unwrap();
        assert!(response.status().is_success());
    }
    for _ in 0..8 {
        sockets.push(socket_open(&fixture, "?workspace=peer").await);
    }
    let error = tokio_tungstenite::connect_async(
        fixture
            .url("/events/socket?workspace=third")
            .replacen("http:", "ws:", 1),
    )
    .await
    .unwrap_err();
    assert!(matches!(error, Error::Http(ref response) if response.status().as_u16() == 503));
    assert_eq!(
        socket_event(&mut sockets[8], "workspace-context").await["name"],
        "peer"
    );
    // An idle full set of read-only sockets must not prevent orderly server shutdown.
    fixture.close().await;
}

#[tokio::test]
async fn stale_reasons_reach_reconnected_clients_and_inspect_without_rerunning_work() {
    use wes_engine::source::SourceInput;
    let fixture = Fixture::new().await;
    let session = fixture.app.current().unwrap().session;
    for (cell, source) in [
        ("source", "catalog echo value:original > origin"),
        ("child", "catalog echo value:$origin > child"),
        ("change", ":change $origin value:changed"),
    ] {
        session
            .submit(SourceInput::new(cell.into(), source.into()).unwrap())
            .await
            .unwrap();
        session.wait_idle().await.unwrap();
    }
    let snapshot = session.snapshot().await.unwrap();
    let root = snapshot.names["origin"].node.as_str();
    let child = snapshot.names["child"].node.as_str();
    let calls = fixture.calls.load(Ordering::SeqCst);
    let mut events = fixture.stream().await;
    let mut received = std::collections::BTreeMap::new();
    while received.len() < 2 {
        let event = events.until("node").await;
        if event["state"] == "stale" {
            received.insert(
                event["node"].as_str().unwrap().to_owned(),
                event["staleReason"].clone(),
            );
        }
    }
    assert_eq!(received[root]["code"], "definition_changed");
    assert_eq!(received[child]["code"], "dependency_changed");
    assert!(
        received[child]["message"]
            .as_str()
            .unwrap()
            .contains("upstream definition changed")
    );
    let reply = session
        .submit(SourceInput::new("inspect".into(), ":inspect $child".into()).unwrap())
        .await
        .unwrap();
    session.wait_idle().await.unwrap();
    let inspected = session.snapshot().await.unwrap();
    let data = inspected.execution.values[&reply.nodes[0]].data();
    let encoded = wes_adapters::codec::encode_json(data, Default::default()).unwrap();
    let value: Value = serde_json::from_slice(&encoded).unwrap();
    assert_eq!(value["staleReason"], received[child]);
    assert_eq!(fixture.calls.load(Ordering::SeqCst), calls);
    drop(events);
    fixture.close().await;
}

#[tokio::test]
async fn structured_request_diagnostics_survive_events_history_and_reconnect() {
    struct Rejected;
    impl Invoker for Rejected {
        fn invoke(&self, _: Call, _: CancellationToken) -> InvocationFuture {
            Box::pin(async {
                Err(wes_engine::providers::InvocationError::Failed(
                    wes_core::ErrorValue::new(
                        wes_core::ErrorId::new("synthetic-http-diagnostics").unwrap(),
                        "HTTP001",
                        "Request validation failed",
                        vec![
                            wes_core::ValidationIssue {
                                path: "/arguments/body/count".into(),
                                code: "TYP005".into(),
                                message: "number is outside Count's bounds".into(),
                            },
                            wes_core::ValidationIssue {
                                path: "/arguments/feed".into(),
                                code: "TYP005".into(),
                                message: "value is not in Feed's enum".into(),
                            },
                        ],
                        None,
                    )
                    .unwrap(),
                ))
            })
        }
    }
    let fixture = Fixture::configured(
        |_| wes::web::Services::default(),
        Arc::new(|workspace| {
            workspace.register_provider(
                ProviderDescription::new(
                    "rejected",
                    [Capability::new(["request"], Shape::Unknown, Safety::Safe)],
                    vec![],
                )
                .unwrap(),
                Arc::new(Rejected),
            )?;
            Ok(())
        }),
    )
    .await;
    let mut events = fixture.stream().await;
    let generation = events.generation().await;
    assert_eq!(
        fixture
            .source(&generation, "request", "rejected request")
            .await,
        202
    );
    let first = events.until("failed").await;
    assert_eq!(first["error"]["code"], "HTTP001");
    assert_eq!(first["error"]["issues"][0]["path"], "/arguments/body/count");
    assert_eq!(
        first["error"]["issues"][1]["message"],
        "value is not in Feed's enum"
    );
    let session = fixture.app.current().unwrap().session;
    session.wait_idle().await.unwrap();
    let mut reconnected = fixture.stream().await;
    assert_eq!(reconnected.generation().await, generation);
    assert_eq!(reconnected.until("failed").await, first);
    loop {
        let frame = reconnected.until("log").await;
        if frame["record"]["node"] == first["node"] && frame["record"]["state"] == "FAILED" {
            assert_eq!(frame["record"]["error"], first["error"]);
            break;
        }
    }
    drop(events);
    drop(reconnected);
    fixture.close().await;
}

#[path = "web/workspace_deletion.rs"]
mod workspace_deletion;

#[tokio::test]
async fn slow_socket_receives_no_second_batch_until_it_acknowledges_the_first() {
    use futures_util::{SinkExt, StreamExt};
    use tokio_tungstenite::tungstenite::Message;
    let fixture = Fixture::new().await;
    let mut socket = socket_open(&fixture, "").await;
    let first = socket.next().await.unwrap().unwrap();
    let Message::Text(text) = first else {
        panic!("batch")
    };
    let batch: Value = serde_json::from_str(&text).unwrap();
    let generation = batch["events"]
        .as_array()
        .unwrap()
        .iter()
        .find(|e| e["event"] == "session")
        .unwrap()["generation"]
        .as_str()
        .unwrap();
    assert_eq!(
        fixture
            .source(generation, "slow-reader", "catalog echo value:bounded")
            .await,
        202
    );
    assert!(
        tokio::time::timeout(Duration::from_millis(300), socket.next())
            .await
            .is_err()
    );
    socket
        .send(Message::Text(format!("ack:{}", batch["sequence"]).into()))
        .await
        .unwrap();
    let ready = socket_event(&mut socket, "ready").await;
    assert!(ready["handle"].is_string());
    drop(socket);
    fixture.close().await;
}

#[tokio::test]
async fn capacity_changes_from_a_hidden_workspace_reach_an_idle_browser() {
    struct Gated(Arc<tokio::sync::Semaphore>);
    impl Invoker for Gated {
        fn invoke(&self, _: Call, cancelled: CancellationToken) -> InvocationFuture {
            let released = self.0.clone();
            Box::pin(async move {
                tokio::select! {
                    _ = cancelled.cancelled() => {},
                    permit = released.acquire_owned() => { permit.unwrap().forget(); },
                }
                Ok(
                    wes_core::Value::new(
                        Shape::Unknown,
                        wes_core::Data::Int(1),
                        Default::default(),
                    )
                    .unwrap(),
                )
            })
        }
    }
    let released = Arc::new(tokio::sync::Semaphore::new(0));
    let gate = released.clone();
    let fixture = Fixture::configured(
        |_| wes::web::Services::default(),
        Arc::new(move |workspace| {
            workspace.register_provider(
                ProviderDescription::new(
                    "gate",
                    [Capability::new(["wait"], Shape::Unknown, Safety::Safe)],
                    vec![],
                )
                .unwrap(),
                Arc::new(Gated(gate.clone())),
            )?;
            Ok(())
        }),
    )
    .await;
    fixture
        .app
        .open_workspace(WorkspaceName::new("background".into()).unwrap(), true)
        .await
        .unwrap();
    let peer = fixture.app.bound("background").unwrap().current().unwrap();
    let mut events = fixture.stream().await;
    events.generation().await;
    assert_eq!(
        events.until("execution-capacity").await["operations"],
        json!({"used":0,"limit":2})
    );
    peer.session
        .submit(
            wes_engine::source::SourceInput::new("background-job".into(), "gate wait".into())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(
        events.until("execution-capacity").await["operations"]["used"],
        1
    );
    released.add_permits(1);
    assert_eq!(
        events.until("execution-capacity").await["operations"]["used"],
        0
    );
    assert_eq!(fixture.app.current().unwrap().name.as_str(), "default");
    fixture.close().await;
}
