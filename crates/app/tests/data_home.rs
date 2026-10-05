//! Portable data homes: real disk/loopback boundaries, synthetic data, no Keychain or user fixtures.
use serde_json::{Value, json};
use std::{
    fs,
    path::{Path, PathBuf},
    time::Duration,
};
use wes::{
    data_home::{
        self, DataHome,
        host::{DesktopHost, Location},
    },
    runtime::{RuntimeOptions, launch},
};

fn root() -> tempfile::TempDir {
    tempfile::Builder::new()
        .prefix("wes-data-home-")
        .tempdir_in(std::env::temp_dir().canonicalize().unwrap())
        .unwrap()
}
fn read(path: &Path) -> Value {
    serde_json::from_slice(&fs::read(path).unwrap()).unwrap()
}
fn client() -> reqwest::Client {
    reqwest::Client::builder()
        .no_proxy()
        .timeout(Duration::from_secs(20))
        .build()
        .unwrap()
}
async fn api(home: PathBuf, value: Value) -> std::io::Result<Value> {
    tokio::task::spawn_blocking(move || {
        wes::api_library::ApiLibrary::new(home).perform(wes::api_library::parse_request(
            &serde_json::to_vec(&value).unwrap(),
        )?)
    })
    .await
    .unwrap()
}
async fn post(
    client: &reqwest::Client,
    location: &Location,
    route: &str,
    value: Value,
    generation: Option<&str>,
) -> reqwest::Response {
    let mut request = client
        .post(format!("{}{route}", location.url))
        .header("Content-Type", "application/json")
        .body(value.to_string());
    if let Some(generation) = generation {
        request = request.header("X-Wes-Session", generation);
    }
    request.send().await.unwrap()
}
async fn generation(client: &reqwest::Client, location: &Location) -> String {
    let mut response = client
        .get(format!("{}events", location.url))
        .send()
        .await
        .unwrap();
    let mut buffer = String::new();
    loop {
        buffer.push_str(std::str::from_utf8(&response.chunk().await.unwrap().unwrap()).unwrap());
        for line in buffer.lines() {
            if let Some(text) = line.strip_prefix("data: ") {
                if let Ok(value) = serde_json::from_str::<Value>(text) {
                    if value["event"] == "session" {
                        return value["generation"].as_str().unwrap().into();
                    }
                }
            }
        }
    }
}
async fn switch(host: &DesktopHost, path: &Path) {
    let mut updates = host.subscribe();
    let response = post(
        &client(),
        &host.location(),
        "data-home",
        json!({"path":path}),
        None,
    )
    .await;
    let status = response.status();
    let text = response.text().await.unwrap();
    assert_eq!(status, 200, "{text}");
    tokio::time::timeout(Duration::from_secs(15), updates.changed())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(host.location().path, path.canonicalize().unwrap());
}

#[test]
fn identity_is_private_locked_relocatable_and_distinct_for_new_homes() {
    let temp = root();
    let home = temp.path().join("one");
    let opened = DataHome::open(&home).unwrap();
    let account = opened.identity.keychain_account();
    let id = opened.identity.id.clone();
    assert!(DataHome::open(&home).is_err());
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(fs::metadata(&home).unwrap().permissions().mode() & 0o077, 0);
    }
    drop(opened);
    let moved = temp.path().join("renamed");
    fs::rename(&home, &moved).unwrap();
    let reopened = DataHome::open(&moved).unwrap();
    assert_eq!(reopened.identity.id, id);
    assert_eq!(data_home::keychain_account(&moved).unwrap(), account);
    let different = DataHome::open(&home).unwrap();
    assert_ne!(different.identity.keychain_account(), account);
    assert_eq!(
        read(&moved.join("api-library-settings.json"))["settings"]["localDirectory"],
        "api-library"
    );
}

#[test]
fn saved_editor_files_survive_reopening_an_owned_data_home() {
    let temp = root();
    let home = temp.path().join("editor-home");
    let opened = DataHome::open(&home).unwrap();
    let identity = opened.identity.id.clone();
    // Match the directory and file layout POST /edit-files writes.
    let saved = home.join("edit/env/synthetic.yaml");
    let content = "version: 1\npackage: synthetic\nenvironments: {}\n";
    fs::create_dir_all(saved.parent().unwrap()).unwrap();
    fs::write(&saved, content).unwrap();
    drop(opened);

    let reopened = DataHome::open(&home).unwrap();
    assert_eq!(reopened.identity.id, identity);
    assert_eq!(fs::read_to_string(&saved).unwrap(), content);
    drop(reopened);
    fs::write(home.join("unrelated.txt"), "preserve unrelated data").unwrap();
    assert!(DataHome::open(&home).is_err());
    assert_eq!(fs::read_to_string(&saved).unwrap(), content);
}

#[test]
fn editor_directory_does_not_allow_files_links_or_unidentified_adoption() {
    let temp = root();
    let file_home = temp.path().join("file-home");
    drop(DataHome::open(&file_home).unwrap());
    fs::write(file_home.join("edit"), "ordinary file").unwrap();
    assert!(DataHome::open(&file_home).is_err());
    assert_eq!(
        fs::read_to_string(file_home.join("edit")).unwrap(),
        "ordinary file"
    );

    let unidentified = temp.path().join("unidentified");
    fs::create_dir_all(unidentified.join("edit/env")).unwrap();
    assert!(DataHome::open(&unidentified).is_err());
    assert!(!unidentified.join("identity.json").exists());

    #[cfg(unix)]
    {
        let linked_home = temp.path().join("linked-home");
        drop(DataHome::open(&linked_home).unwrap());
        let outside = temp.path().join("outside");
        fs::create_dir(&outside).unwrap();
        fs::write(outside.join("keep.txt"), "unchanged").unwrap();
        std::os::unix::fs::symlink(&outside, linked_home.join("edit")).unwrap();
        assert!(DataHome::open(&linked_home).is_err());
        assert_eq!(
            fs::read_to_string(outside.join("keep.txt")).unwrap(),
            "unchanged"
        );
    }
}

#[test]
fn unrelated_corrupt_linked_and_legacy_occupied_roots_are_rejected() {
    let temp = root();
    let unrelated = temp.path().join("unrelated");
    fs::create_dir(&unrelated).unwrap();
    fs::write(unrelated.join("notes.txt"), "keep me").unwrap();
    assert!(DataHome::open(&unrelated).is_err());
    assert!(!unrelated.join("identity.json").exists());
    let corrupt = temp.path().join("corrupt");
    drop(DataHome::open(&corrupt).unwrap());
    fs::write(corrupt.join("identity.json"), "{}").unwrap();
    assert!(DataHome::open(&corrupt).is_err());
    #[cfg(unix)]
    {
        std::os::unix::fs::symlink(&corrupt, temp.path().join("link")).unwrap();
        assert!(DataHome::open(&temp.path().join("link")).is_err());
    }
    let legacy = temp.path().join("legacy");
    let store = wes_adapters::storage::FileValues::open(
        &legacy.join("values/live"),
        wes_adapters::codec::Limits::default(),
        wes_adapters::journal::Durability::File,
        None,
    )
    .unwrap();
    let workspaces = wes_adapters::workspaces::FileWorkspaces::open(
        &legacy.join("workspaces"),
        wes_adapters::journal::ReadLimits::default(),
        wes_adapters::journal::Durability::File,
    )
    .unwrap();
    assert!(DataHome::open(&legacy).is_err());
    assert!(!legacy.join("identity.json").exists());
    drop(workspaces);
    drop(store);
    assert!(DataHome::open(&legacy).is_err());
    assert!(data_home::keychain_account(&legacy).is_err());
    assert!(!legacy.join("identity.json").exists());
}

#[tokio::test(flavor = "multi_thread")]
async fn managed_library_survives_move_and_rejects_external_reconfiguration() {
    let temp = root();
    let home = temp.path().join("data");
    let owned = DataHome::open(&home).unwrap();
    let home = owned.path.clone();
    let source = temp.path().join("inventory.json");
    fs::write(
        &source,
        include_bytes!("../../../examples/api-import/inventory.json"),
    )
    .unwrap();
    let key = json!({"service":"fixture","apiVersion":"v1","scope":"default"});
    let added = api(
        home.clone(),
        json!({"action":"add","file":source,"key":key}),
    )
    .await
    .unwrap();
    let status = api(home.clone(), json!({"action":"status"})).await.unwrap();
    assert_eq!(status["managed"], true);
    assert!(api(home.clone(), json!({"action":"configure","expectedRevision":status["revision"],"settings":{"localDirectory":temp.path().join("outside")}})).await.is_err());
    drop(owned);
    let moved = temp.path().join("moved");
    fs::rename(&home, &moved).unwrap();
    let moved_home = DataHome::open(&moved).unwrap();
    let resolved = api(
        moved_home.path.clone(),
        json!({"action":"resolve","key":key}),
    )
    .await
    .unwrap();
    assert_eq!(
        resolved["package"]["revision"],
        added["package"]["revision"]
    );
    assert!(
        resolved["descriptorPath"]
            .as_str()
            .unwrap()
            .starts_with(moved.to_str().unwrap())
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn unsupported_external_library_settings_are_rejected_without_changing_originals() {
    let temp = root();
    let home = temp.path().join("home");
    let library = temp.path().join("external");
    drop(DataHome::open(&home).unwrap());
    fs::create_dir(&library).unwrap();
    fs::write(library.join("index.json"), b"unrelated original index").unwrap();
    let settings_file = home.join("api-library-settings.json");
    let identity = fs::read(home.join("identity.json")).unwrap();
    for (version, directory) in [
        (1, json!(library)),
        (2, json!(library)),
        (2, json!("../external")),
        (3, json!("api-library")),
    ] {
        let bytes =
            serde_json::to_vec(&json!({"version":version,"settings":{"localDirectory":directory}}))
                .unwrap();
        fs::write(&settings_file, &bytes).unwrap();
        assert!(api(home.clone(), json!({"action":"status"})).await.is_err());
        assert!(DataHome::open(&home).is_err());
        assert_eq!(fs::read(&settings_file).unwrap(), bytes);
        assert_eq!(fs::read(home.join("identity.json")).unwrap(), identity);
        assert_eq!(
            fs::read(library.join("index.json")).unwrap(),
            b"unrelated original index"
        );
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn default_and_a_b_a_switch_preserve_history_values_preferences_and_remember_selection() {
    let temp = root();
    let user = temp.path().canonicalize().unwrap();
    let a = data_home::default_home(&user);
    let b = user.join("other");
    let runtime = launch(RuntimeOptions::new(a.clone(), user.clone()))
        .await
        .unwrap();
    runtime
        .handle
        .current()
        .unwrap()
        .session
        .submit(
            wes_engine::source::SourceInput::new(
                "original".into(),
                ":calc { return 42; } > answer".into(),
            )
            .unwrap(),
        )
        .await
        .unwrap();
    runtime
        .handle
        .current()
        .unwrap()
        .session
        .checkpoint()
        .await
        .unwrap()
        .resume()
        .await;
    runtime.shutdown().await.unwrap();
    fs::write(
        a.join("desktop-ui.json"),
        json!({"version":1,"settings":{"theme":"light"}}).to_string(),
    )
    .unwrap();
    let host = DesktopHost::start(user.clone(), None).await.unwrap();
    assert_eq!(host.location().path, a);
    switch(&host, &b).await;
    assert_ne!(
        data_home::identity(&a).unwrap().unwrap().id,
        host.location().identity
    );
    assert_eq!(data_home::selected_home(&user).unwrap(), b);
    assert!(!b.join("desktop-ui.json").exists());
    let response = client()
        .get(format!("{}client-preferences", host.location().url))
        .send()
        .await
        .unwrap();
    assert_eq!(
        serde_json::from_slice::<Value>(&response.bytes().await.unwrap()).unwrap()["settings"],
        Value::Null
    );
    switch(&host, &a).await;
    host.shutdown().await.unwrap();
    let restored = launch(RuntimeOptions::new(a.clone(), user.clone()))
        .await
        .unwrap();
    let observation = restored
        .handle
        .current()
        .unwrap()
        .session
        .observe()
        .await
        .unwrap();
    assert_eq!(observation.cells.len(), 1);
    assert_eq!(observation.cells[0].input.cell(), "original");
    let values = observation.values.as_ref().unwrap();
    let handle = values.outputs[&observation.state.names["answer"].node]
        .handle()
        .unwrap();
    assert_eq!(
        restored
            .worker
            .read(handle.clone())
            .await
            .unwrap()
            .unwrap()
            .value
            .data(),
        &wes_core::Data::Int(42)
    );
    restored.shutdown().await.unwrap();
    assert_eq!(
        read(&a.join("desktop-ui.json"))["settings"]["theme"],
        "light"
    );
    let host = DesktopHost::start(user.clone(), None).await.unwrap();
    switch(&host, &b).await;
    host.shutdown().await.unwrap();
    let restarted = DesktopHost::start(user, None).await.unwrap();
    assert_eq!(restarted.location().path, b);
    restarted.shutdown().await.unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn invalid_busy_and_cross_origin_targets_leave_the_active_home_usable() {
    let temp = root();
    let user = temp.path().canonicalize().unwrap();
    let host = DesktopHost::start(user.clone(), None).await.unwrap();
    let before = host.location();
    let busy = launch(RuntimeOptions::new(user.join("busy"), user.clone()))
        .await
        .unwrap();
    let unrelated = user.join("unrelated");
    fs::create_dir(&unrelated).unwrap();
    fs::write(unrelated.join("notes"), "original").unwrap();
    let broken = user.join("broken");
    drop(DataHome::open(&broken).unwrap());
    fs::write(broken.join("identity.json"), "bad").unwrap();
    for path in [
        user.join("busy"),
        unrelated.clone(),
        broken,
        before.path.join("nested"),
        user.clone(),
        PathBuf::from("relative"),
    ] {
        let response = post(&client(), &before, "data-home", json!({"path":path}), None).await;
        assert_eq!(response.status(), 409, "{}", response.text().await.unwrap());
        assert_eq!(host.location().identity, before.identity);
    }
    let response = client()
        .post(format!("{}data-home", before.url))
        .header("Origin", "https://example.invalid")
        .header("Content-Type", "application/json")
        .body(json!({"path":user.join("cross-origin")}).to_string())
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 403);
    assert!(!user.join("cross-origin").exists());
    let gen_id = generation(&client(), &before).await;
    assert_eq!(
        post(
            &client(),
            &before,
            "submit",
            json!({"request":"submit","client":"web-fixture","cell":"still-working","text":":calc { return 7; } > seven"}),
            Some(&gen_id)
        )
        .await
        .status(),
        202
    );
    assert_eq!(
        fs::read_to_string(unrelated.join("notes")).unwrap(),
        "original"
    );
    busy.shutdown().await.unwrap();
    host.shutdown().await.unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn running_jobs_and_terminals_must_be_finished_before_switching() {
    let temp = root();
    let user = temp.path().canonicalize().unwrap();
    let host = DesktopHost::start(user.clone(), None).await.unwrap();
    let here = host.location();
    let client = client();
    let gen_id = generation(&client, &here).await;
    let submitted = post(
        &client,
        &here,
        "submit",
        json!({"request":"submit","client":"web-fixture","cell":"slow","text":"sh run cmd:\"sleep 1\" > slow"}),
        Some(&gen_id),
    )
    .await;
    assert_eq!(submitted.status(), 202);
    let refused = post(
        &client,
        &here,
        "data-home",
        json!({"path":user.join("next")}),
        None,
    )
    .await;
    assert_eq!(refused.status(), 409, "{}", refused.text().await.unwrap());
    let terminal = post(
        &client,
        &here,
        "terminals",
        json!({"action":"start","client":"fixture"}),
        Some(&gen_id),
    )
    .await;
    assert_eq!(terminal.status(), 200, "{}", terminal.text().await.unwrap());
    let terminal: Value = serde_json::from_slice(&terminal.bytes().await.unwrap()).unwrap();
    assert_eq!(
        post(
            &client,
            &here,
            "data-home",
            json!({"path":user.join("next")}),
            None
        )
        .await
        .status(),
        409
    );
    assert_eq!(
        post(
            &client,
            &here,
            "terminals",
            json!({"action":"close","client":"fixture","id":terminal["id"]}),
            Some(&gen_id)
        )
        .await
        .status(),
        200
    );
    host.shutdown().await.unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn an_open_stream_refuses_switching_without_replacing_the_session() {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let temp = root();
    let user = temp.path().to_path_buf();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let endpoint = format!("http://{}", listener.local_addr().unwrap());
    let descriptor = user.join("events.json");
    fs::write(
        &descriptor,
        json!({
            "version":1,"provider":"events","types":{},
            "operations":[{"path":["watch"],"stream":true,"method":"GET","route":"/events",
                "auth":[],"parameters":[],"responses":{"200":"Int"}}]
        })
        .to_string(),
    )
    .unwrap();
    let (release, wait) = tokio::sync::oneshot::channel();
    let provider = tokio::spawn(async move {
        tokio::time::timeout(Duration::from_secs(15), async {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut request = Vec::new();
            while !request.windows(4).any(|p| p == b"\r\n\r\n") {
                let mut bytes = [0; 1024];
                let n = socket.read(&mut bytes).await.unwrap();
                assert_ne!(n, 0);
                request.extend_from_slice(&bytes[..n]);
                assert!(request.len() < 8192);
            }
            socket.write_all(b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nConnection: close\r\n\r\ndata: 42\n\n").await.unwrap();
            let _ = wait.await;
        }).await.unwrap();
    });
    let host = DesktopHost::start(user.clone(), None).await.unwrap();
    let here = host.location();
    let client = client();
    let gen_id = generation(&client, &here).await;
    let mut events = client
        .get(format!("{}events", here.url))
        .send()
        .await
        .unwrap();
    let source = format!(
        ":import spec file:{} endpoint:{endpoint:?}\nevents watch > live",
        json!(descriptor)
    );
    assert_eq!(
        post(
            &client,
            &here,
            "submit",
            json!({
                "request":"submit","client":"web-fixture","cell":"stream","text":source
            }),
            Some(&gen_id)
        )
        .await
        .status(),
        202
    );
    tokio::time::timeout(Duration::from_secs(10), async {
        let mut buffer = String::new();
        loop {
            buffer.push_str(std::str::from_utf8(&events.chunk().await.unwrap().unwrap()).unwrap());
            if buffer
                .lines()
                .filter_map(|line| line.strip_prefix("data: "))
                .filter_map(|text| serde_json::from_str::<Value>(text).ok())
                .any(|event| event["event"] == "ready")
            {
                break;
            }
        }
    })
    .await
    .unwrap();
    let response = post(
        &client,
        &here,
        "data-home",
        json!({"path":user.join("next")}),
        None,
    )
    .await;
    assert_eq!(response.status(), 409, "{}", response.text().await.unwrap());
    assert_eq!(generation(&client, &here).await, gen_id);
    assert!(!user.join("next").exists());
    drop(events);
    host.shutdown().await.unwrap();
    let _ = release.send(());
    provider.await.unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn admitted_switch_is_owned_even_when_requesting_client_disconnects() {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let temp = root();
    let user = temp.path().canonicalize().unwrap();
    let host = DesktopHost::start(user.clone(), None).await.unwrap();
    let mut updates = host.subscribe();
    let url = url::Url::parse(&host.location().url).unwrap();
    let mut socket = tokio::net::TcpStream::connect(("127.0.0.1", url.port().unwrap()))
        .await
        .unwrap();
    let body = json!({"path":user.join("detached")}).to_string();
    socket.write_all(format!("POST /data-home HTTP/1.1\r\nHost: 127.0.0.1:{}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",url.port().unwrap(),body.len(),body).as_bytes()).await.unwrap();
    // Wait for the first response byte: disconnect only after the request was admitted.
    // Closing before admission can legitimately make Hyper discard the entire request.
    let mut first = [0];
    socket.read_exact(&mut first).await.unwrap();
    socket.shutdown().await.unwrap();
    drop(socket);
    tokio::time::timeout(Duration::from_secs(15), updates.changed())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(host.location().path, user.join("detached"));
    host.shutdown().await.unwrap();
}

#[test]
fn cli_default_is_isolated_by_user_home_and_explicit_home_takes_precedence() {
    let temp = root();
    let user = temp.path().join("user");
    fs::create_dir(&user).unwrap();
    let former = user.join(".unrelated-app");
    fs::create_dir(&former).unwrap();
    fs::write(
        former.join("desktop-location.json"),
        b"not a current launcher",
    )
    .unwrap();
    assert_eq!(data_home::selected_home(&user).unwrap(), user.join(".wes"));
    let run = |home: Option<&Path>| {
        let mut command = std::process::Command::new(env!("CARGO_BIN_EXE_wes"));
        command.env("HOME", &user).current_dir(temp.path());
        if let Some(home) = home {
            command.arg("--home").arg(home);
        }
        command
            .args(["--command", ":calc { return 3; } > three"])
            .output()
            .unwrap()
    };
    let output = run(None);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(user.join(".wes/identity.json").exists());
    let other = temp.path().join("explicit");
    assert!(run(Some(&other)).status.success());
    assert!(other.join("identity.json").exists());
    assert_eq!(
        fs::read(former.join("desktop-location.json")).unwrap(),
        b"not a current launcher"
    );
    assert!(!former.join("identity.json").exists());
}

#[test]
fn documented_example_runs_with_its_actual_source_and_isolated_environment() {
    let script = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../examples/data-home/check.py");
    let result = std::process::Command::new("python3")
        .arg(script)
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

#[tokio::test(flavor = "multi_thread")]
async fn incompatible_default_offers_recovery_without_touching_old_data() {
    let temp = root();
    let user = temp.path().to_owned();
    let old = user.join(".wes");
    fs::create_dir_all(old.join("live")).unwrap();
    fs::write(old.join("live/legacy-sentinel"), "old data stays").unwrap();
    let host = DesktopHost::start(user.clone(), None).await.unwrap();
    assert!(host.location().identity.is_empty());
    let page = client()
        .get(&host.location().url)
        .send()
        .await
        .unwrap()
        .text()
        .await
        .unwrap();
    assert!(page.contains("Open a data folder"));
    assert!(!old.join("identity.json").exists());
    assert_eq!(
        fs::read_to_string(old.join("live/legacy-sentinel")).unwrap(),
        "old data stays"
    );
    switch(&host, &user.join("fresh")).await;
    assert_eq!(
        fs::read_to_string(old.join("live/legacy-sentinel")).unwrap(),
        "old data stays"
    );
    host.shutdown().await.unwrap();
    assert_eq!(data_home::selected_home(&user).unwrap(), user.join("fresh"));
}

#[tokio::test(flavor = "multi_thread")]
async fn recovery_and_running_host_require_an_explicit_path_and_never_move_rejected_data() {
    let temp = root();
    let user = temp.path().to_path_buf();
    let rejected = user.join(".wes");
    fs::create_dir_all(rejected.join("unrelated")).unwrap();
    fs::write(rejected.join("unrelated/sentinel"), "leave in place").unwrap();
    let host = DesktopHost::start(user.clone(), None).await.unwrap();
    let here = host.location();
    assert!(here.identity.is_empty());
    let response = client()
        .get(format!("{}data-home", here.url))
        .send()
        .await
        .unwrap();
    let status: Value = serde_json::from_slice(&response.bytes().await.unwrap()).unwrap();
    assert_eq!(status["setup"], true);
    assert!(status.get("backupPath").is_none());
    for opened in [false, true] {
        if opened {
            switch(&host, &user.join("fresh")).await;
            assert!(!host.location().identity.is_empty());
        }
        let current = host.location();
        for request in [
            json!({}),
            json!({"path":null}),
            json!({"backupDefault":true}),
            json!({"path":user.join("unrequested"),"backupDefault":false}),
        ] {
            assert_eq!(
                post(&client(), &current, "data-home", request, None)
                    .await
                    .status(),
                400
            );
        }
        assert_eq!(host.location().path, current.path);
        assert!(!user.join("unrequested").exists());
        assert!(!rejected.join("identity.json").exists());
        assert_eq!(
            fs::read_to_string(rejected.join("unrelated/sentinel")).unwrap(),
            "leave in place"
        );
    }
    host.shutdown().await.unwrap();
    let reopened = DesktopHost::start(user.clone(), None).await.unwrap();
    assert_eq!(reopened.location().path, user.join("fresh"));
    assert!(!reopened.location().identity.is_empty());
    reopened.shutdown().await.unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn missing_remembered_home_opens_recovery_instead_of_silently_creating_replacement() {
    let temp = root();
    let user = temp.path().to_owned();
    let missing = user.join("missing");
    fs::create_dir(&missing).unwrap();
    data_home::remember_home(&user, &missing).unwrap();
    fs::remove_dir(&missing).unwrap();
    let host = DesktopHost::start(user.clone(), None).await.unwrap();
    assert!(host.location().identity.is_empty());
    assert!(!missing.exists());
    switch(&host, &user.join("recovered")).await;
    host.shutdown().await.unwrap();
}

#[test]
fn copying_a_closed_home_keeps_its_identity_and_keychain_association() {
    fn copy(from: &Path, to: &Path) {
        fs::create_dir(to).unwrap();
        for entry in fs::read_dir(from).unwrap() {
            let entry = entry.unwrap();
            let target = to.join(entry.file_name());
            if entry.file_type().unwrap().is_dir() {
                copy(&entry.path(), &target);
            } else {
                fs::copy(entry.path(), target).unwrap();
            }
        }
    }
    let temp = root();
    let a = temp.path().join("a");
    let b = temp.path().join("b");
    let home = DataHome::open(&a).unwrap();
    let id = home.identity.id.clone();
    let account = home.identity.keychain_account();
    drop(home);
    copy(&a, &b);
    let reopened = DataHome::open(&b).unwrap();
    assert_eq!(reopened.identity.id, id);
    assert_eq!(reopened.identity.keychain_account(), account);
}

#[test]
fn cli_diagnostics_belong_to_an_identified_home_but_do_not_authorize_adoption() {
    let temp = root();
    let home = temp.path().join("valid");
    let opened = DataHome::open(&home).unwrap();
    fs::create_dir(home.join("diagnostics")).unwrap();
    drop(opened);
    assert!(DataHome::open(&home).is_ok());
    let unrelated = temp.path().join("unrelated");
    fs::create_dir_all(unrelated.join("diagnostics")).unwrap();
    assert!(DataHome::open(&unrelated).is_err());
    assert!(!unrelated.join("identity.json").exists());
}

#[test]
fn terminal_history_belongs_only_to_an_identified_home() {
    let temp = root();
    let home = temp.path().join("valid");
    let opened = DataHome::open(&home).unwrap();
    fs::create_dir(home.join("terminal-history")).unwrap();
    drop(opened);
    assert!(DataHome::open(&home).is_ok());
    let unrelated = temp.path().join("unrelated");
    fs::create_dir_all(unrelated.join("terminal-history")).unwrap();
    assert!(DataHome::open(&unrelated).is_err());
    assert!(!unrelated.join("identity.json").exists());
    #[cfg(unix)]
    {
        fs::remove_dir(home.join("terminal-history")).unwrap();
        std::os::unix::fs::symlink(&unrelated, home.join("terminal-history")).unwrap();
        assert!(DataHome::open(&home).is_err());
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn running_work_in_a_hidden_named_workspace_refuses_data_home_switching() {
    let temp = root();
    let user = temp.path().canonicalize().unwrap();
    let host = DesktopHost::start(user.clone(), None).await.unwrap();
    let here = host.location();
    let client = client();
    let opened = post(
        &client,
        &here,
        "workspaces",
        json!({"name":"hidden","create":true}),
        None,
    )
    .await;
    assert_eq!(opened.status(), 200);
    let opened: Value = serde_json::from_slice(&opened.bytes().await.unwrap()).unwrap();
    let submitted = client.post(format!("{}submit", here.url))
        .header("Content-Type", "application/json")
        .header("X-Wes-Workspace", "hidden")
        .header("X-Wes-Session", opened["generation"].as_str().unwrap())
        .body(json!({"request":"submit","client":"web-fixture","cell":"hidden-slow","text":"sh run cmd:\"sleep 60\" > slow"}).to_string())
        .send().await.unwrap();
    assert_eq!(submitted.status(), 202);
    let refused = post(
        &client,
        &here,
        "data-home",
        json!({"path":user.join("next")}),
        None,
    )
    .await;
    assert_eq!(refused.status(), 409, "{}", refused.text().await.unwrap());
    assert_eq!(host.location().identity, here.identity);
    host.shutdown().await.unwrap();
}

#[test]
fn unidentified_settings_are_not_adopted_or_given_an_identity() {
    let temp = root();
    let home = temp.path().join("unidentified-settings");
    fs::create_dir(&home).unwrap();
    let file = home.join("api-library-settings.json");
    let bytes = br#"{"version":1,"settings":{"localDirectory":"/unused"}}"#;
    fs::write(&file, bytes).unwrap();
    assert!(DataHome::open(&home).is_err());
    assert!(!home.join("identity.json").exists());
    assert_eq!(fs::read(file).unwrap(), bytes);
}
