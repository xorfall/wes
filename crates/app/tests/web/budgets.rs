use super::*;
#[tokio::test]
async fn budgets_use_same_origin_cas_and_save_no_source_commands() {
    let fixture = Fixture::configured(
        |home| wes::web::Services {
            budgets: Some(Arc::new(wes::budgets::Store::for_user_home(home))),
            ..Default::default()
        },
        Arc::new(|_| Ok(())),
    )
    .await;
    let response = fixture
        .client
        .get(fixture.url("/operating-budgets"))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 200);
    let initial: Value = serde_json::from_str(&response.text().await.unwrap()).unwrap();
    assert_eq!(initial["revision"], 0);
    assert_eq!(
        initial["entries"].as_array().unwrap().len(),
        wes_budgets::catalogue().len()
    );
    let change = json!({"revision":0,"values":{"execution.operations":8,"view.instances":256,"history.window.entries":20}});
    let forbidden = fixture
        .client
        .put(fixture.url("/operating-budgets"))
        .header("Origin", "https://elsewhere.invalid")
        .header("Content-Type", "application/json")
        .body(change.to_string())
        .send()
        .await
        .unwrap();
    assert_eq!(forbidden.status(), 403);
    let malformed = fixture
        .client
        .put(fixture.url("/operating-budgets"))
        .header("Content-Type", "application/json")
        .body(json!({"revision":0,"values":{"execution.operations":0}}).to_string())
        .send()
        .await
        .unwrap();
    assert_eq!(malformed.status(), 400);
    let response = fixture
        .client
        .put(fixture.url("/operating-budgets"))
        .header("Content-Type", "application/json")
        .body(change.to_string())
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 200);
    let saved: Value = serde_json::from_str(&response.text().await.unwrap()).unwrap();
    assert_eq!(saved["revision"], 1);
    let row = saved["entries"]
        .as_array()
        .unwrap()
        .iter()
        .find(|row| row["id"] == "execution.operations")
        .unwrap();
    assert_eq!(row["active"], 2);
    assert_eq!(row["saved"], 8);
    let stale = fixture
        .client
        .put(fixture.url("/operating-budgets"))
        .header("Content-Type", "application/json")
        .body(change.to_string())
        .send()
        .await
        .unwrap();
    assert_eq!(stale.status(), 409);
    let structural = fixture
        .client
        .put(fixture.url("/operating-budgets"))
        .header("Content-Type", "application/json")
        .body(json!({"revision":1,"values":{"protocol.version":2}}).to_string())
        .send()
        .await
        .unwrap();
    assert_eq!(structural.status(), 400);
    let duplicate = fixture
        .client
        .put(fixture.url("/operating-budgets"))
        .header("Content-Type", "application/json")
        .body("{\"revision\":1,\"values\":{\"execution.operations\":4,\"execution.operations\":8}}")
        .send()
        .await
        .unwrap();
    assert_eq!(duplicate.status(), 400);
    let oversized = fixture
        .client
        .put(fixture.url("/operating-budgets"))
        .header("Content-Type", "application/json")
        .body(" ".repeat(wes::budgets::PROFILE_BYTES + 1))
        .send()
        .await
        .unwrap();
    assert_eq!(oversized.status(), 413);
    assert_eq!(fixture.calls.load(Ordering::SeqCst), 0);
    fixture.close().await;
}
