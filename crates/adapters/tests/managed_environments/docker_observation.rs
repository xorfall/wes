use super::*;
use serde_json::json;
use std::sync::Mutex;
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::UnixListener,
};
use wes_engine::completion::retained_resources;
struct Fake {
    _root: tempfile::TempDir,
    socket: String,
    requests: Arc<Mutex<Vec<String>>>,
    version: Arc<Mutex<serde_json::Value>>,
    items: Arc<Mutex<serde_json::Value>>,
    status: Arc<Mutex<u16>>,
    task: tokio::task::JoinHandle<()>,
}
impl Drop for Fake {
    fn drop(&mut self) {
        self.task.abort();
    }
}
impl Fake {
    fn new() -> Self {
        let root = tempfile::Builder::new()
            .prefix("wes-dobs-")
            .tempdir_in("/tmp")
            .unwrap();
        let socket = root.path().join("d.sock");
        let listener = UnixListener::bind(&socket).unwrap();
        let requests = Arc::new(Mutex::new(vec![]));
        let version = Arc::new(Mutex::new(
            json!({"ApiVersion":"1.47","MinAPIVersion":"1.24"}),
        ));
        let items = Arc::new(Mutex::new(
            json!([{"Id":"a".repeat(64),"Names":["/api"],"ImageID":format!("sha256:{}","b".repeat(64)),"State":"running","Created":1,"Labels":{"com.docker.compose.project":"qa","secret":"DO_NOT_EXPORT"},"Env":["SECRET=DO_NOT_EXPORT"]}]),
        ));
        let status = Arc::new(Mutex::new(200));
        let (rq, v, it, st) = (
            requests.clone(),
            version.clone(),
            items.clone(),
            status.clone(),
        );
        let task = tokio::spawn(async move {
            loop {
                let (mut stream, _) = listener.accept().await.unwrap();
                let mut header = vec![];
                while !header.ends_with(b"\r\n\r\n") {
                    let mut byte = [0];
                    if stream.read_exact(&mut byte).await.is_err() {
                        break;
                    }
                    header.push(byte[0]);
                    assert!(header.len() < 16384);
                }
                let request = String::from_utf8(header).unwrap();
                let is_version = request.starts_with("GET /version ");
                let inspect = request.contains("/containers/aaaa");
                rq.lock().unwrap().push(request);
                let body = if is_version {
                    v.lock().unwrap().clone()
                } else if inspect {
                    json!({"Id":"a".repeat(64),"Name":"/api","Image":format!("sha256:{}","b".repeat(64)),"Created":"2026-09-24T00:00:00Z","RestartCount":2,"State":{"Status":"running","Running":true,"ExitCode":0,"OOMKilled":false,"Health":{"Status":"healthy","FailingStreak":0,"Log":[{"Output":"DO_NOT_EXPORT"}]}},"Config":{"Env":["SECRET=DO_NOT_EXPORT"],"Labels":{"com.docker.compose.project":"qa"}},"Mounts":["DO_NOT_EXPORT"]})
                } else {
                    it.lock().unwrap().clone()
                };
                let code = if is_version { 200 } else { *st.lock().unwrap() };
                let body = body.to_string();
                let reply = format!(
                    "HTTP/1.1 {code} Result\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                );
                let _ = stream.write_all(reply.as_bytes()).await;
            }
        });
        Self {
            _root: root,
            socket: socket.to_string_lossy().into_owned(),
            requests,
            version,
            items,
            status,
            task,
        }
    }
    fn recipe(&self, private: bool) -> String {
        format!(
            "version: 1\ntargets: {{local: {{kind: local}}}}\nenvironments:\n  dev:\n    imports:\n      docker:\n        source: {{kind: docker, socket: '{}'}}\n        bind: {{target: local, output: {}}}\n",
            self.socket,
            if private { "private" } else { "public" }
        )
    }
}
#[tokio::test]
async fn inventory_inspect_and_completion_share_only_authorized_retained_results() {
    let fake = Fake::new();
    let f = Fixture::new(&fake.recipe(false));
    f.install().await;
    assert!(
        fake.requests.lock().unwrap().is_empty(),
        "planning/building must not contact Docker"
    );
    f.submit("one", ":env use \"dev\"").await;
    f.submit("one", "docker containers project:qa > inventory")
        .await;
    let resources = retained_resources(&f.handle.observe().await.unwrap());
    assert_eq!(resources.len(), 1);
    assert_eq!(resources[0].items[0].label, "api");
    assert_eq!(resources[0].items[0].value, "a".repeat(64));
    let inventory = f.value("inventory").await;
    assert!(!format!("{inventory:?}").contains("DO_NOT_EXPORT"));
    {
        let requests = fake.requests.lock().unwrap();
        let path = requests[1].split_whitespace().nth(1).unwrap();
        let url = url::Url::parse(&format!("http://localhost{path}")).unwrap();
        let query: std::collections::BTreeMap<_, _> = url.query_pairs().collect();
        assert_eq!(query["all"], "true");
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(&query["filters"]).unwrap(),
            json!({"label":["com.docker.compose.project=qa"]})
        );
    }
    let counts = fake.requests.lock().unwrap().len();
    for _ in 0..10 {
        assert_eq!(
            retained_resources(&f.handle.observe().await.unwrap()).len(),
            1
        );
    }
    assert_eq!(
        counts,
        fake.requests.lock().unwrap().len(),
        "completion never performs I/O"
    );
    f.submit(
        "one",
        &format!("docker inspect container:{} > inspected", "a".repeat(64)),
    )
    .await;
    let inspected = f.value("inspected").await;
    assert!(!format!("{inspected:?}").contains("DO_NOT_EXPORT"));
    assert!(format!("{inspected:?}").contains("healthy"));
    assert_eq!(
        fake.requests
            .lock()
            .unwrap()
            .iter()
            .filter(|r| r.starts_with("GET /version "))
            .count(),
        1
    );
    f.submit("one", ":refresh $inventory").await;
    assert_eq!(
        retained_resources(&f.handle.observe().await.unwrap()).len(),
        1
    );
    f.submit("one", ":node remove $inventory scope:downstream")
        .await;
    assert!(retained_resources(&f.handle.observe().await.unwrap()).is_empty());
    f.stop().await;
}
#[tokio::test]
async fn private_inventory_is_not_a_completion_side_channel() {
    let fake = Fake::new();
    let f = Fixture::new(&fake.recipe(true));
    f.install().await;
    f.submit("one", ":env use \"dev\"").await;
    f.submit("one", "docker containers > inventory").await;
    assert!(
        f.value("inventory")
            .await
            .provenance()
            .policy()
            .is_private()
    );
    assert!(retained_resources(&f.handle.observe().await.unwrap()).is_empty());
    f.stop().await;
}
#[tokio::test]
async fn version_identity_and_argument_errors_do_not_publish_partial_values() {
    let fake = Fake::new();
    *fake.version.lock().unwrap() = json!({"ApiVersion":"1.43","MinAPIVersion":"1.24"});
    let f = Fixture::new(&fake.recipe(false));
    f.install().await;
    f.submit("one", ":env use \"dev\"").await;
    f.submit("one", "docker containers > rejected").await;
    assert!(
        f.handle
            .snapshot()
            .await
            .unwrap()
            .execution
            .errors
            .values()
            .any(|e| e.code() == "DOCKER_VERSION")
    );
    assert_eq!(fake.requests.lock().unwrap().len(), 1);
    *fake.version.lock().unwrap() = json!({"ApiVersion":"1.47","MinAPIVersion":"1.24"});
    f.submit("one", "docker containers limit:0 > invalid").await;
    assert_eq!(fake.requests.lock().unwrap().len(), 1);
    *fake.items.lock().unwrap() = json!([{"Id":"incomplete"}]);
    f.submit("one", "docker containers > malformed").await;
    assert!(retained_resources(&f.handle.observe().await.unwrap()).is_empty());
    *fake.status.lock().unwrap() = 404;
    f.submit(
        "one",
        &format!("docker inspect container:{} > missing", "a".repeat(64)),
    )
    .await;
    assert!(
        f.handle
            .snapshot()
            .await
            .unwrap()
            .execution
            .errors
            .values()
            .any(|e| e.code() == "DOCKER_MISSING" && e.message().contains(&"a".repeat(64)))
    );
    f.stop().await;
}
#[tokio::test]
async fn managed_docker_import_edit_is_inert_and_validates_local_source_contract() {
    let fake = Fake::new();
    let f = Fixture::new(&fake.recipe(false));
    f.install().await;
    f.submit("one", ":env use \"dev\"").await;
    f.submit(
        "one",
        &format!(
            ":import docker socket:\"{}\" as:other target:local",
            fake.socket
        ),
    )
    .await;
    assert!(fake.requests.lock().unwrap().is_empty());
    assert!(
        f.handle.observe().await.unwrap().environment_catalogues["dev"]
            .provider("other")
            .is_some()
    );
    f.stop().await;
    for target in ["{kind: local, cwd: /}", "{kind: local, env: {IGNORED: no}}"] {
        let yaml = fake.recipe(false).replace("{kind: local}", target);
        let invalid = Fixture::new(&yaml);
        // Adapter construction is part of session plan admission; invalid targets still
        // reject before application, without putting provider-specific rules in core.
        assert!(
            invalid
                .handle
                .plan_environment_file("env.yaml".into(), false)
                .await
                .is_err()
        );
        assert!(
            invalid
                .handle
                .environment_revisions()
                .await
                .unwrap()
                .is_empty()
        );
        assert!(fake.requests.lock().unwrap().is_empty());
        invalid.stop().await;
    }
}

#[tokio::test]
async fn bounded_inventory_revision_and_disable_withdraw_suggestions() {
    let fake = Fake::new();
    let item = fake.items.lock().unwrap()[0].clone();
    let mut second = item.clone();
    second["Id"] = json!("b".repeat(64));
    *fake.items.lock().unwrap() = json!([item, second]);
    let f = Fixture::new(&fake.recipe(false));
    f.install().await;
    f.submit("one", ":env use \"dev\"").await;
    f.submit("one", "docker containers limit:1 > inventory")
        .await;
    let value = f.value("inventory").await;
    let Data::Record(fields) = value.data() else {
        panic!("envelope")
    };
    assert_eq!(fields["returned"], Data::Int(1));
    assert_eq!(fields["omitted"], Data::Int(1));
    assert_eq!(fields["complete"], Data::Bool(false));
    f.submit("one", ":env disable \"dev\"").await;
    assert!(retained_resources(&f.handle.observe().await.unwrap()).is_empty());
    f.submit("one", ":env enable \"dev\"").await;
    assert_eq!(
        retained_resources(&f.handle.observe().await.unwrap()).len(),
        1
    );
    f.submit(
        "one",
        &format!(
            ":import docker socket:\"{}\" as:other target:local",
            fake.socket
        ),
    )
    .await;
    assert!(
        retained_resources(&f.handle.observe().await.unwrap()).is_empty(),
        "new revision cannot offer an old revision's inventory"
    );
    f.submit("one", ":env use \"dev\"").await;
    f.submit("one", "docker containers > current").await;
    assert_eq!(
        retained_resources(&f.handle.observe().await.unwrap()).len(),
        1
    );
    *fake.items.lock().unwrap() = json!({"oversized":"x".repeat(1024*1024)});
    f.submit("one", "docker containers > latest").await;
    assert!(
        retained_resources(&f.handle.observe().await.unwrap()).is_empty(),
        "failed latest declaration cannot revive an old list"
    );
    assert!(
        f.handle
            .snapshot()
            .await
            .unwrap()
            .execution
            .errors
            .values()
            .any(|e| e.code() == "DOCKER_RESPONSE")
    );
    f.stop().await;
}

#[tokio::test]
async fn observation_source_roundtrips_without_reacquiring_authority_or_contacting_daemon() {
    let fake = Fake::new();
    let f = Fixture::new(&fake.recipe(false));
    f.install().await;
    f.submit("one", ":env use \"dev\"").await;
    f.submit("one", "docker containers > inventory").await;
    f.submit("one", ":env export file:captured.lock.json").await;
    let loader = LocalEnvironments::new(f.root.path()).unwrap();
    assert_eq!(loader.read_lock("captured.lock.json").unwrap().len(), 1);
    let captured = f.handle.checkpoint().await.unwrap();
    let mut copy = wes_engine::history::HistoryCapture::new(Default::default());
    for entry in captured.history().journal() {
        copy.push(wes_engine::history::Record::Journal(entry.clone()))
            .unwrap();
    }
    for entry in captured.history().recovery() {
        copy.push(wes_engine::history::Record::Recovery(entry.clone()))
            .unwrap();
    }
    let image = copy.finish(captured.history().checkpoint());
    drop(captured);
    let count = fake.requests.lock().unwrap().len();
    let restored = session::restore(
        Workspace::local(wes_engine::providers::LocalScope::new("fixture").unwrap())
            .with_environment_loader(Arc::new(loader)),
        RecordingMode::Ephemeral,
        None,
        image,
        CancellationToken::new(),
    )
    .await
    .unwrap();
    let (handle, task) = restored
        .spawn(
            Arc::new(FileTypeSources::new(f.root.path()).unwrap()),
            1.try_into().unwrap(),
        )
        .unwrap();
    handle.wait_idle().await.unwrap();
    let observation = handle.observe().await.unwrap();
    assert!(!observation.environment_enabled["dev"]);
    assert!(retained_resources(&observation).is_empty());
    assert_eq!(count, fake.requests.lock().unwrap().len());
    handle.shutdown().await.unwrap();
    task.join().await.unwrap();
    f.stop().await;
}
