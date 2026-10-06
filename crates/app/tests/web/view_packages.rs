use super::*;
#[tokio::test]
async fn compiled_packages_share_discovery_and_creation_and_assets_require_current_workspace_generation()
 {
    let fixture = Fixture::new().await;
    let mut events = fixture.stream().await;
    let generation = events.generation().await;
    let manifest=json!({"name":"TaskBadge","id":"task-badge","summary":"Task count","renderer":"View.tsx","input":"TaskBadge","outputs":{},"interaction":null}).to_string();
    let types = "types: {TaskBadge: {base: Record, fields: {count: Int}}}";
    let p = wes_views::Package::parse(&manifest, types).unwrap();
    let source=json!({"format":1,"sdk":wes_views::sdk_version(),"manifest":manifest,"types":types,"definition":p.digest,"javascript":"throw new Error('not executed');","css":""}).to_string();
    let artifact = wes_views::Artifact::parse(source.as_bytes()).unwrap();
    let text = format!(
        ":package load source:{} origin:\"badge.json\"",
        json!(source)
    );
    assert_eq!(fixture.source(&generation, "install", &text).await, 202);
    events.until("planned").await;
    let catalogue = fixture
        .client
        .get(fixture.url("/language/views"))
        .send()
        .await
        .unwrap()
        .text()
        .await
        .unwrap();
    let catalogue: Value = serde_json::from_str(&catalogue).unwrap();
    assert!(
        catalogue
            .as_array()
            .unwrap()
            .iter()
            .any(|d| d["name"] == "TaskBadge" && d["artifact"] == artifact.digest)
    );
    assert_eq!(
        fixture.source(&generation, "list", ":list views").await,
        202
    );
    let listed = events.until("ready").await;
    let response = fixture
        .client
        .get(fixture.url(&format!("/values/{}", listed["handle"].as_str().unwrap())))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 200);
    let value: Value = serde_json::from_str(&response.text().await.unwrap()).unwrap();
    assert_eq!(
        value["type"],
        json!({"kind":"list","element":{"kind":"primitive","name":"TEXT"}})
    );
    assert!(
        value["data"]
            .as_array()
            .unwrap()
            .contains(&json!("TaskBadge"))
    );
    let url = fixture.url(&format!("/view-packages/{}", artifact.digest));
    assert_eq!(fixture.client.get(&url).send().await.unwrap().status(), 409);
    assert_eq!(
        fixture
            .client
            .get(&url)
            .header("X-Wes-Session", "different")
            .send()
            .await
            .unwrap()
            .status(),
        409
    );
    let response = fixture
        .client
        .get(&url)
        .header("X-Wes-Session", &generation)
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 200);
    let body: Value = serde_json::from_str(&response.text().await.unwrap()).unwrap();
    assert_eq!(body["digest"], artifact.digest);
    assert_eq!(body["definition"]["name"], "TaskBadge");
    assert!(body["javascript"].as_str().unwrap().starts_with("throw"));
    // Asset discovery must work before any managed instance exists.
    assert_eq!(
        fixture
            .source(&generation, "create", ":def badgeData(input:Int) -> TaskBadge as :calc { return {count:input}; }\n:calc { return 7; } | badgeData | :view create TaskBadge > badge")
            .await,
        202
    );
    let current = fixture.app.current().unwrap();
    let mut replay = fixture.stream().await;
    assert_eq!(replay.generation().await, generation);
    current.session.wait_idle().await.unwrap();
    let creator = loop {
        let created = replay.until("created").await;
        if created["name"] == "badge" {
            assert_eq!(created["dependencyLifetime"], "creation");
            assert_eq!(created["dependsOn"].as_array().unwrap().len(), 1);
            break created["node"].clone();
        }
    };
    loop {
        let state = replay.next().await;
        if state["node"] == creator && matches!(state["event"].as_str(), Some("node" | "ready")) {
            // Engine idle does not flush older projection frames queued for this
            // subscriber. Completion must be checked on its actual ready frame.
            assert!(
                state["event"] == "ready"
                    || matches!(
                        state["state"].as_str(),
                        Some("pending" | "running" | "ready")
                    ),
                "{state}"
            );
            if state["state"] == "ready" || state["event"] == "ready" {
                assert_eq!(state["constructionComplete"], true, "{state}");
            }
            if state["event"] == "ready" {
                break;
            }
        }
    }
    // Publication replaces the node-state frame with a ready frame. A fresh
    // subscriber must see completed construction even after that replacement.
    let mut published = fixture.stream().await;
    assert_eq!(published.generation().await, generation);
    loop {
        let state = published.until("ready").await;
        if state["node"] == creator {
            assert_eq!(state["constructionComplete"], true);
            break;
        }
    }
    assert_eq!(fixture.calls.load(Ordering::SeqCst), 0);
    fixture.close().await;
}
