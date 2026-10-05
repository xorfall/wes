//! Real desktop host/HTTP path with synthetic homes; no webview or user services.
#[path = "../src/debug.rs"]
mod debug;

use std::{path::PathBuf, time::Duration};
use wes::data_home::host::DesktopHost;

#[tokio::test(flavor = "multi_thread")]
async fn isolated_debug_host_serves_ui_and_accepts_commands_without_touching_regular_home() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().canonicalize().unwrap();
    let regular = root.join("regular");
    std::fs::create_dir(&regular).unwrap();
    std::fs::write(regular.join("sentinel"), "unchanged").unwrap();
    let debug_home = root.join("debug user");
    let paths = debug::Paths::resolve(
        true,
        Some(debug_home.clone().into()),
        Some(regular.clone()),
        Some(regular.join("config")),
    )
    .unwrap();
    assert!(paths.isolated);
    assert_eq!(paths.diagnostics, Some(debug_home.join("diagnostics")));
    let site = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../gui/dist");
    let index = std::fs::read_to_string(site.join("index.html"))
        .expect("Build the static UI first: npm --prefix gui run build");
    let host = DesktopHost::start(paths.user_home.clone(), Some(site.clone()))
        .await
        .unwrap();
    let location = host.location();
    assert_eq!(location.path, debug_home.join(".wes"));
    let client = reqwest::Client::builder()
        .no_proxy()
        .timeout(Duration::from_secs(15))
        .build()
        .unwrap();
    let response = client.get(&location.url).send().await.unwrap();
    assert_eq!(response.status(), 200);
    assert_eq!(response.text().await.unwrap(), index);
    let mut events = client
        .get(format!("{}events", location.url))
        .send()
        .await
        .unwrap();
    let mut buffer = Vec::new();
    let generation = loop {
        buffer.extend_from_slice(&events.chunk().await.unwrap().expect("session frame"));
        let text = String::from_utf8_lossy(&buffer);
        if let Some(generation) = text
            .lines()
            .filter_map(|line| {
                let value: serde_json::Value =
                    serde_json::from_str(line.strip_prefix("data: ")?).ok()?;
                (value["event"] == "session")
                    .then(|| value["generation"].as_str().unwrap().to_owned())
            })
            .next()
        {
            break generation;
        }
    };
    drop(events);
    let response = client
        .post(format!("{}submit", location.url))
        .header("Content-Type", "application/json")
        .header("X-Wes-Session", generation)
        .body(r#"{"request":"submit","client":"debug-test","cell":"debug-example","text":"1 + 2"}"#)
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 202, "{}", response.text().await.unwrap());
    host.shutdown().await.unwrap();
    let reopened = DesktopHost::start(paths.user_home, Some(site))
        .await
        .unwrap();
    assert_eq!(reopened.location().identity, location.identity);
    reopened.shutdown().await.unwrap();
    assert_eq!(std::fs::read_dir(&regular).unwrap().count(), 1);
    assert_eq!(
        std::fs::read_to_string(regular.join("sentinel")).unwrap(),
        "unchanged"
    );
}
