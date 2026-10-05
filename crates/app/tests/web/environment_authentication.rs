use super::*;
use tokio::io::AsyncReadExt;
use wes::runtime::{LaunchedRuntime, RuntimeOptions, launch};
use wes_engine::source::SourceInput;

struct AuthFixture {
    runtime: LaunchedRuntime,
    server: wes::web::Server,
    client: reqwest::Client,
}
impl AuthFixture {
    async fn open(base: &std::path::Path) -> Self {
        Self::with_store(base, Arc::new(TestStore::default())).await
    }
    async fn with_store(base: &std::path::Path, store: Arc<TestStore>) -> Self {
        Self::with_options(base, |options| options.credential_store = Some(store)).await
    }
    async fn with_vault(base: &std::path::Path) -> Self {
        let vault = wes::credential_vault::CredentialVault::new(
            base.join("home"),
            wes::credential_vault::VaultCost {
                memory_kib: 64,
                iterations: 1,
                parallelism: 1,
            },
        );
        Self::with_options(base, |options| options.credential_vault = Some(vault)).await
    }
    async fn with_options(
        base: &std::path::Path,
        configure: impl FnOnce(&mut RuntimeOptions),
    ) -> Self {
        let mut options = RuntimeOptions::new(base.join("home"), base.to_owned());
        configure(&mut options);
        options.docker_candidates = Some(vec![base.join("absent.sock")]);
        let runtime = launch(options).await.unwrap();
        let server = runtime.serve(0, None).await.unwrap();
        Self {
            runtime,
            server,
            client: reqwest::Client::builder()
                .no_proxy()
                .timeout(Duration::from_secs(10))
                .build()
                .unwrap(),
        }
    }
    async fn request(&self, body: Option<Value>) -> reqwest::Response {
        let current = self.runtime.handle.current().unwrap();
        let url = format!(
            "http://{}/environment-authentication",
            self.server.address()
        );
        let req = if let Some(body) = body {
            self.client
                .post(url)
                .header("Content-Type", "application/json")
                .body(body.to_string())
        } else {
            self.client.get(url)
        };
        req.header("X-Wes-Workspace", "default")
            .header("X-Wes-Session", current.generation)
            .send()
            .await
            .unwrap()
    }
    async fn vault(&self, body: Option<Value>) -> (u16, String) {
        let url = format!("http://{}/credential-vault", self.server.address());
        let request = match body {
            Some(body) => self
                .client
                .post(url)
                .header("Content-Type", "application/json")
                .body(body.to_string()),
            None => self.client.get(url),
        };
        let response = request.send().await.unwrap();
        (response.status().as_u16(), response.text().await.unwrap())
    }
    async fn vault_state(&self) -> Value {
        let (status, body) = self.vault(None).await;
        assert_eq!(status, 200, "{body}");
        serde_json::from_str::<Value>(&body).unwrap()["state"].clone()
    }
    async fn report(&self) -> Value {
        let response = self.request(None).await;
        assert_eq!(response.status(), 200);
        serde_json::from_slice(&response.bytes().await.unwrap()).unwrap()
    }
    async fn change(&self, provider: &Value, mut action: Value) -> reqwest::Response {
        for key in ["environment", "revision", "provider"] {
            action[key] = provider[key].clone();
        }
        self.request(Some(action)).await
    }
    async fn submit(&self, source: String) -> Arc<wes_engine::session::SubmissionResult> {
        let session = self.runtime.handle.current().unwrap().session;
        let result = session
            .submit(SourceInput::new(uuid::Uuid::new_v4().to_string(), source).unwrap())
            .await
            .unwrap();
        session.wait_idle().await.unwrap();
        assert!(
            !result
                .diagnostics
                .diagnostics
                .iter()
                .any(|d| d.severity == wes_language::Severity::Error),
            "{result:?}"
        );
        result
    }
    async fn close(self) {
        self.server.shutdown().await.unwrap();
        self.runtime.shutdown().await.unwrap();
    }
}
fn spec() -> String {
    json!({"version":1,"provider":"demo","types":{},"operations":[{"path":["latest"],"method":"GET","route":"/latest","parameters":[],"responses":{"200":"Text"},"auth":[],"authOptions":[
        {"schemes":["key","secret"],"auth":[{"header":"X-Test-Key","scheme":"","secret":"key"},{"header":"X-Test-Secret","scheme":"","secret":"secret"}]},
        {"schemes":["basic"],"auth":[{"userSecret":"basic.username","secret":"basic.password"}]}]}]}).to_string()
}
#[tokio::test]
async fn authentication_controls_select_bind_supply_grant_and_restore_without_material() {
    let root = tempfile::tempdir().unwrap();
    std::fs::write(root.path().join("api.json"), spec()).unwrap();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let endpoint = format!("http://{}", listener.local_addr().unwrap());
    let f = AuthFixture::open(root.path()).await;
    f.submit(format!(
        ":import spec file:api.json as:demo endpoint:{endpoint:?}"
    ))
    .await;
    let observation = f
        .runtime
        .handle
        .current()
        .unwrap()
        .session
        .observe()
        .await
        .unwrap();
    assert_eq!(
        observation.environment_providers["default"]["demo"],
        ("spec".into(), "local".into(), Some(endpoint.clone()))
    );
    let initial = f.report().await;
    let original = &initial["providers"][0];
    assert_eq!(original["operations"][0]["state"], "selection-required");
    assert_eq!(original["operations"][0]["selected"], Value::Null);
    assert_eq!(
        original["operations"][0]["options"]
            .as_array()
            .unwrap()
            .len(),
        2
    );
    assert_eq!(
        f.change(
            original,
            json!({"action":"configure","auth":{"latest":["nonexistent"]}})
        )
        .await
        .status(),
        409
    );
    assert_eq!(
        f.report().await["providers"][0]["revision"],
        original["revision"]
    );
    // Editing must use retained source, even when the original disappears.
    std::fs::remove_file(root.path().join("api.json")).unwrap();
    let response = f
        .change(
            original,
            json!({"action":"configure","auth":{"latest":["key","secret"]}}),
        )
        .await;
    assert_eq!(response.status(), 200, "{}", response.text().await.unwrap());
    let configured = f.report().await;
    let provider = &configured["providers"][0];
    assert_ne!(provider["revision"], original["revision"]);
    assert_eq!(
        provider["operations"][0]["selected"],
        json!(["key", "secret"])
    );
    assert_eq!(provider["credentials"].as_array().unwrap().len(), 2);
    // Refresh is read-only even after binding a method: no material or grant appears.
    for _ in 0..3 {
        let refreshed = f.report().await;
        let refreshed = &refreshed["providers"][0];
        assert_eq!(refreshed["grantSeconds"], 0);
        assert_eq!(refreshed["revision"], provider["revision"]);
        assert!(
            refreshed["credentials"]
                .as_array()
                .unwrap()
                .iter()
                .all(|credential| credential["present"] == false)
        );
    }
    assert_eq!(
        f.change(
            original,
            json!({"action":"supply","slot":"key","value":"stale-private"})
        )
        .await
        .status(),
        409
    );
    for (slot, value) in [
        ("key", "synthetic-private-key"),
        ("secret", "synthetic-private-secret"),
    ] {
        assert_eq!(
            f.change(
                provider,
                json!({"action":"supply","slot":slot,"value":value})
            )
            .await
            .status(),
            200
        );
    }
    let supplied = f.report().await;
    assert!(
        supplied["providers"][0]["credentials"]
            .as_array()
            .unwrap()
            .iter()
            .all(|s| s["present"] == true)
    );
    assert!(!supplied.to_string().contains("synthetic-private"));
    assert_eq!(
        f.change(provider, json!({"action":"grant"})).await.status(),
        200
    );
    assert!(
        f.report().await["providers"][0]["grantSeconds"]
            .as_u64()
            .unwrap()
            > 0
    );
    let serving = tokio::spawn(async move {
        let (mut socket, _) = tokio::time::timeout(Duration::from_secs(5), listener.accept())
            .await
            .unwrap()
            .unwrap();
        let mut bytes = [0; 4096];
        let length = socket.read(&mut bytes).await.unwrap();
        let headers = String::from_utf8_lossy(&bytes[..length]).to_lowercase();
        assert!(headers.contains("x-test-key: synthetic-private-key"));
        assert!(headers.contains("x-test-secret: synthetic-private-secret"));
        socket.write_all(b"HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: 4\r\nConnection: close\r\n\r\n\"ok\"").await.unwrap();
        listener
    });
    f.submit("demo latest".into()).await;
    let listener = serving.await.unwrap();
    assert_eq!(
        f.change(provider, json!({"action":"revoke"}))
            .await
            .status(),
        200
    );
    assert_eq!(f.report().await["providers"][0]["grantSeconds"], 0);
    f.submit("demo latest".into()).await;
    assert!(
        tokio::time::timeout(Duration::from_millis(100), listener.accept())
            .await
            .is_err()
    );
    let session = f.runtime.handle.current().unwrap().session;
    let documents = session.environment_documents().await.unwrap();
    assert!(!format!("{documents:?}").contains("synthetic-private"));
    assert!(!format!("{:?}", session.observe().await.unwrap().cells).contains("synthetic-private"));
    f.close().await;
    let restored = AuthFixture::open(root.path()).await;
    let state = restored.report().await;
    let provider = &state["providers"][0];
    assert_eq!(
        provider["operations"][0]["selected"],
        json!(["key", "secret"])
    );
    // The application may enable its default namespace at startup; credentials/grants never restore.
    assert_eq!(provider["grantSeconds"], 0);
    assert!(
        provider["credentials"]
            .as_array()
            .unwrap()
            .iter()
            .all(|s| s["present"] == false)
    );
    restored.submit(r#":env disable "default""#.into()).await;
    assert_eq!(restored.report().await["providers"][0]["enabled"], false);
    assert_eq!(
        restored
            .change(provider, json!({"action":"grant"}))
            .await
            .status(),
        409
    );
    assert_eq!(
        restored
            .change(provider, json!({"action":"enable"}))
            .await
            .status(),
        200
    );
    restored.close().await;
}

#[tokio::test]
async fn inherited_authentication_is_a_local_override_and_fixed_auth_can_be_bound() {
    let root = tempfile::tempdir().unwrap();
    std::fs::write(root.path().join("api.json"), spec()).unwrap();
    let mut fixed: Value = serde_json::from_str(&spec()).unwrap();
    fixed["operations"][0]["auth"] = json!([{"header":"X-Fixed-Key","scheme":"","secret":"token"}]);
    fixed["operations"][0]
        .as_object_mut()
        .unwrap()
        .remove("authOptions");
    std::fs::write(root.path().join("fixed.json"), fixed.to_string()).unwrap();
    let f = AuthFixture::open(root.path()).await;
    std::fs::write(root.path().join("env.yaml"),"version: 1\ntargets: {local: {kind: local}}\nenvironments:\n  parent:\n    imports:\n      demo:\n        source: {kind: spec, file: api.json}\n        bind: {target: local, endpoint: 'http://127.0.0.1:1'}\n  child: {extends: {env: parent, track: latest}}\n").unwrap();
    let session = f.runtime.handle.current().unwrap().session;
    let plan = session
        .plan_environment_file("env.yaml".into(), false)
        .await
        .unwrap();
    session.apply_environments(plan).await.unwrap();
    let initial = f.report().await;
    let child = initial["providers"]
        .as_array()
        .unwrap()
        .iter()
        .find(|p| p["environment"] == "child")
        .unwrap();
    let before_parent = initial["providers"]
        .as_array()
        .unwrap()
        .iter()
        .find(|p| p["environment"] == "parent")
        .unwrap()["revision"]
        .clone();
    std::fs::remove_file(root.path().join("api.json")).unwrap();
    let response = f
        .change(
            child,
            json!({"action":"configure","auth":{"latest":["basic"]}}),
        )
        .await;
    assert_eq!(response.status(), 200, "{}", response.text().await.unwrap());
    let updated = f.report().await;
    let parent = updated["providers"]
        .as_array()
        .unwrap()
        .iter()
        .find(|p| p["environment"] == "parent")
        .unwrap();
    assert_eq!(parent["revision"], before_parent);
    assert_eq!(parent["operations"][0]["state"], "selection-required");
    let child = updated["providers"]
        .as_array()
        .unwrap()
        .iter()
        .find(|p| p["environment"] == "child")
        .unwrap();
    assert_eq!(child["operations"][0]["selected"], json!(["basic"]));
    assert_eq!(child["credentials"].as_array().unwrap().len(), 2);
    assert!(
        session
            .environment_documents()
            .await
            .unwrap()
            .iter()
            .any(|d| d.source.contains("overrides"))
    );
    f.submit(":import spec file:fixed.json as:fixed endpoint:\"http://127.0.0.1:1\"".into())
        .await;
    let fixed = f.report().await;
    let fixed = fixed["providers"]
        .as_array()
        .unwrap()
        .iter()
        .find(|p| p["provider"] == "fixed")
        .unwrap();
    assert_eq!(fixed["operations"][0]["state"], "fixed");
    let response = f
        .change(fixed, json!({"action":"configure","auth":{}}))
        .await;
    assert_eq!(response.status(), 200, "{}", response.text().await.unwrap());
    let report = f.report().await;
    let fixed = report["providers"]
        .as_array()
        .unwrap()
        .iter()
        .find(|p| p["provider"] == "fixed")
        .unwrap();
    assert_eq!(fixed["credentials"][0]["slot"], "token");
    assert_eq!(
        f.change(
            fixed,
            json!({"action":"supply","slot":"token","value":"synthetic-value"})
        )
        .await
        .status(),
        200
    );
    assert_eq!(
        f.change(fixed, json!({"action":"forget","slot":"token"}))
            .await
            .status(),
        200
    );
    let report = f.report().await;
    let fixed = report["providers"]
        .as_array()
        .unwrap()
        .iter()
        .find(|p| p["provider"] == "fixed")
        .unwrap();
    assert_eq!(fixed["credentials"][0]["present"], false);
    let current = f.runtime.handle.current().unwrap();
    let url = format!("http://{}/environment-authentication", f.server.address());
    assert_eq!(
        f.client
            .get(&url)
            .header("X-Wes-Session", "stale")
            .send()
            .await
            .unwrap()
            .status(),
        409
    );
    assert_eq!(
        f.client
            .get(&url)
            .header("X-Wes-Session", &current.generation)
            .header("Origin", "https://foreign.invalid")
            .send()
            .await
            .unwrap()
            .status(),
        403
    );
    let malformed = f
        .request(Some(
            json!({"action":"supply","value":"synthetic-private-sentinel","extra":"bad"}),
        ))
        .await;
    assert_eq!(malformed.status(), 400);
    assert!(!malformed.text().await.unwrap().contains("sentinel"));
    f.close().await;
}

#[derive(Default)]
struct TestStore(
    std::sync::Mutex<std::collections::BTreeMap<String, wes_engine::credentials::Secret>>,
);
impl wes_engine::credentials::material::SecureStore for TestStore {
    fn supported(&self) -> bool {
        true
    }
    fn get(
        &self,
        name: &str,
    ) -> Result<Option<wes_engine::credentials::Secret>, wes_engine::credentials::CredentialError>
    {
        Ok(self.0.lock().unwrap().get(name).cloned())
    }
    fn set(
        &self,
        name: &str,
        value: &wes_engine::credentials::Secret,
    ) -> Result<(), wes_engine::credentials::CredentialError> {
        self.0.lock().unwrap().insert(name.into(), value.clone());
        Ok(())
    }
    fn remove(&self, name: &str) -> Result<(), wes_engine::credentials::CredentialError> {
        self.0.lock().unwrap().remove(name);
        Ok(())
    }
}
#[tokio::test]
async fn optional_saved_credentials_restore_presence_without_grants_and_forget_keeps_bindings() {
    let root = tempfile::tempdir().unwrap();
    let store = Arc::new(TestStore::default());
    std::fs::write(root.path().join("api.json"), spec()).unwrap();
    let f = AuthFixture::with_store(root.path(), store.clone()).await;
    f.submit(":import spec file:api.json as:demo endpoint:\"http://127.0.0.1:1\"".into())
        .await;
    let original = f.report().await;
    assert_eq!(
        f.change(
            &original["providers"][0],
            json!({"action":"configure","auth":{"latest":["key","secret"]}})
        )
        .await
        .status(),
        200
    );
    f.submit(":sandbox { :calc { return 1; } > value } > preview".into())
        .await;
    f.submit(":refresh $preview".into()).await;
    let report = f.report().await;
    let p = &report["providers"][0];
    assert_eq!(
        p["enabled"], true,
        "refreshing a sandbox cannot close parent authority"
    );
    assert_eq!(p["persistenceSupported"], true);
    for (slot, remember) in [("key", true), ("secret", false)] {
        assert_eq!(f.change(p,json!({"action":"supply","slot":slot,"value":"synthetic-material","remember":remember})).await.status(),200);
    }
    assert_eq!(f.change(p, json!({"action":"grant"})).await.status(), 200);
    assert!(
        f.report().await["providers"][0]["grantSeconds"]
            .as_u64()
            .unwrap()
            > 0
    );
    assert_eq!(f.change(p, json!({"action":"revoke"})).await.status(), 200);
    assert_eq!(store.0.lock().unwrap().len(), 1);
    f.close().await;
    let f = AuthFixture::with_store(root.path(), store.clone()).await;
    let report = f.report().await;
    let p = &report["providers"][0];
    assert_eq!(p["grantSeconds"], 0);
    let slots = p["credentials"].as_array().unwrap();
    assert_eq!(
        slots.iter().find(|s| s["slot"] == "key").unwrap()["saved"],
        true
    );
    assert_eq!(
        slots.iter().find(|s| s["slot"] == "key").unwrap()["present"],
        true
    );
    assert_eq!(
        slots.iter().find(|s| s["slot"] == "secret").unwrap()["present"],
        false
    );
    assert!(!report.to_string().contains("synthetic-material"));
    assert_eq!(
        f.change(p, json!({"action":"forget","slot":"key"}))
            .await
            .status(),
        200
    );
    let forgotten = f.report().await;
    assert_eq!(
        forgotten["providers"][0]["credentials"]
            .as_array()
            .unwrap()
            .len(),
        2
    );
    assert!(store.0.lock().unwrap().is_empty());
    f.close().await;
    let f = AuthFixture::with_store(root.path(), store).await;
    assert!(
        f.report().await["providers"][0]["credentials"]
            .as_array()
            .unwrap()
            .iter()
            .all(|s| s["present"] == false)
    );
    f.close().await;
}

#[tokio::test]
async fn vault_remembers_credentials_only_while_unlocked_and_reset_keeps_workspaces() {
    // Arrange
    let root = tempfile::tempdir().unwrap();
    std::fs::write(root.path().join("api.json"), spec()).unwrap();
    let f = AuthFixture::with_vault(root.path()).await;
    f.submit(":import spec file:api.json as:demo endpoint:\"http://127.0.0.1:1\"".into())
        .await;
    let original = f.report().await;
    assert_eq!(
        f.change(
            &original["providers"][0],
            json!({"action":"configure","auth":{"latest":["key","secret"]}})
        )
        .await
        .status(),
        200
    );
    let provider = f.report().await["providers"][0].clone();
    let saved = |report: &Value| report["providers"][0]["credentials"][0]["saved"].clone();
    let present = |report: &Value| report["providers"][0]["credentials"][0]["present"].clone();
    let remember =
        json!({"action":"supply","slot":"key","value":"synthetic-vault-value","remember":true});
    assert_eq!(provider["persistenceSupported"], true);
    assert_eq!(f.vault_state().await, "absent");

    // Act / Assert: nothing can be remembered before a vault exists.
    let refused = f.change(&provider, remember.clone()).await;
    assert_eq!(refused.status(), 409);
    assert!(
        refused
            .text()
            .await
            .unwrap()
            .contains("vault is locked or not set up")
    );
    let (weak, _) = f
        .vault(Some(json!({"action":"create","password":"short"})))
        .await;
    assert_eq!(weak, 400);
    let (created, body) = f
        .vault(Some(
            json!({"action":"create","password":"synthetic password"}),
        ))
        .await;
    assert_eq!(
        (created, body.as_str()),
        (200, r#"{"kind":"vault","state":"unlocked"}"#)
    );
    assert_eq!(f.change(&provider, remember.clone()).await.status(), 200);
    assert_eq!(saved(&f.report().await), true);

    // Locking hides remembered values while the report keeps working.
    assert_eq!(f.vault(Some(json!({"action":"lock"}))).await.0, 200);
    let locked = f.report().await;
    assert_eq!(
        (present(&locked), saved(&locked)),
        (json!(false), json!(false))
    );
    let (wrong, body) = f
        .vault(Some(json!({"action":"unlock","password":"wrong password"})))
        .await;
    assert_eq!(wrong, 403);
    assert!(!body.contains("wrong password"));
    assert_eq!(f.vault_state().await, "locked");
    assert_eq!(
        f.vault(Some(
            json!({"action":"unlock","password":"synthetic password"})
        ))
        .await
        .0,
        200
    );
    let unlocked = f.report().await;
    assert_eq!(
        (present(&unlocked), saved(&unlocked)),
        (json!(true), json!(true))
    );
    f.close().await;

    // A restart starts locked and needs the password again.
    let restarted = AuthFixture::with_vault(root.path()).await;
    assert_eq!(restarted.vault_state().await, "locked");
    assert_eq!(present(&restarted.report().await), false);
    assert_eq!(
        restarted
            .vault(Some(
                json!({"action":"unlock","password":"synthetic password"})
            ))
            .await
            .0,
        200
    );
    assert_eq!(present(&restarted.report().await), true);
    let home = root.path().join("home");
    let file = std::fs::read_to_string(home.join(wes::credential_vault::VAULT_FILE)).unwrap();
    assert!(!file.contains("synthetic-vault-value") && !file.contains("synthetic password"));

    // Reset needs an explicit confirmation and removes only the vault.
    let (unconfirmed, _) = restarted
        .vault(Some(json!({"action":"reset","confirm":"yes"})))
        .await;
    assert_eq!(unconfirmed, 400);
    let (reset, _) = restarted
        .vault(Some(json!({"action":"reset","confirm":"reset"})))
        .await;
    assert_eq!(reset, 200);
    assert_eq!(restarted.vault_state().await, "absent");
    assert!(!home.join(wes::credential_vault::VAULT_FILE).exists());
    assert!(home.join("workspaces").is_dir());
    let after_reset = restarted.report().await;
    assert_eq!(
        (present(&after_reset), saved(&after_reset)),
        (json!(false), json!(false))
    );
    let (malformed, body) = restarted
        .vault(Some(
            json!({"action":"unlock","password":"synthetic-sentinel","extra":1}),
        ))
        .await;
    assert_eq!(malformed, 400);
    assert!(!body.contains("sentinel"));
    restarted.close().await;
}

#[cfg(target_os = "macos")]
#[tokio::test]
async fn system_secure_store_reports_no_application_vault() {
    // Arrange
    let root = tempfile::tempdir().unwrap();
    let f = AuthFixture::with_options(root.path(), |_| {}).await;

    // Act
    let (status, body) = f.vault(None).await;
    let (refused, _) = f.vault(Some(json!({"action":"lock"}))).await;

    // Assert
    assert_eq!((status, body.as_str()), (200, r#"{"kind":"system"}"#));
    assert_eq!(refused, 501);
    f.close().await;
}
