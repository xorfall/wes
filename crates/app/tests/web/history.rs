use super::*;

async fn read(fixture: &Fixture, generation: &str, cursor: Option<&str>) -> Value {
    let mut url = url::Url::parse(&fixture.url("/history")).unwrap();
    if let Some(cursor) = cursor {
        url.query_pairs_mut().append_pair("cursor", cursor);
    }
    let response = fixture
        .client
        .get(url)
        .header("X-Wes-Session", generation)
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 200);
    assert_eq!(response.headers()["cache-control"], "no-store");
    serde_json::from_slice(&response.bytes().await.unwrap()).unwrap()
}

#[tokio::test]
async fn saved_pages_hold_a_prefix_across_appends_and_expire_on_workspace_replacement() {
    let fixture = Fixture::new().await;
    let generation = fixture.app.current().unwrap().generation;
    let source = (0..120)
        .map(|i| format!("catalog echo value:item{i} > item{i}\n"))
        .collect::<String>();
    assert_eq!(fixture.source(&generation, "batch", &source).await, 202);
    fixture
        .app
        .current()
        .unwrap()
        .session
        .wait_idle()
        .await
        .unwrap();
    let first = read(&fixture, &generation, None).await;
    assert_eq!(first["generation"], generation);
    assert_eq!(first["entries"].as_array().unwrap().len(), 200);
    assert!(first["through"].is_string());
    assert_eq!(first["unconfirmedWrites"], "0");
    let cursor = first["next"].as_str().unwrap().to_owned();
    // Append after capture; following the cursor must not extend the earlier snapshot.
    assert_eq!(
        fixture
            .source(&generation, "later", "catalog echo value:later > later")
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
    let mut entries = first["entries"].as_array().unwrap().clone();
    let mut next = Some(cursor.clone());
    while let Some(cursor) = next {
        let page = read(&fixture, &generation, Some(&cursor)).await;
        assert_eq!(page["through"], first["through"]);
        entries.extend(page["entries"].as_array().unwrap().iter().cloned());
        next = page["next"].as_str().map(str::to_owned);
    }
    let ids: std::collections::BTreeSet<_> = entries
        .iter()
        .map(|entry| {
            assert_eq!(entry["event"], "log");
            assert_eq!(entry["durable"], true);
            entry["record"]["id"].as_str().unwrap()
        })
        .collect();
    assert_eq!(ids.len(), entries.len());
    assert!(
        !entries
            .iter()
            .any(|entry| entry.to_string().contains("later"))
    );
    let fresh = read(&fixture, &generation, None).await;
    assert_ne!(fresh["through"], first["through"]);
    assert_eq!(fixture.calls.load(Ordering::SeqCst), 121);
    assert_eq!(
        fixture
            .app
            .current()
            .unwrap()
            .session
            .observe()
            .await
            .unwrap()
            .cells
            .len(),
        2
    );
    assert_eq!(
        fixture
            .source(&generation, "save", ":workspace save \"copied\"")
            .await,
        202
    );
    assert_eq!(
        fixture
            .source(&generation, "load", ":workspace load \"copied\"")
            .await,
        202
    );
    let selected = fixture.app.current().unwrap().generation;
    assert_ne!(selected, generation);
    for header in [&generation, &selected] {
        let mut url = url::Url::parse(&fixture.url("/history")).unwrap();
        url.query_pairs_mut().append_pair("cursor", &cursor);
        let response = fixture
            .client
            .get(url)
            .header("X-Wes-Session", header)
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), 409);
    }
    assert_eq!(
        read(&fixture, &selected, None).await["generation"],
        selected
    );
    assert_eq!(fixture.calls.load(Ordering::SeqCst), 121);
    fixture.close().await;
}

#[tokio::test]
async fn history_refuses_invalid_queries_and_occupied_credit_without_source_or_shutdown_waits() {
    let fixture = Fixture::new().await;
    let generation = fixture.app.current().unwrap().generation;
    assert_eq!(
        fixture
            .client
            .get(fixture.url("/history"))
            .send()
            .await
            .unwrap()
            .status(),
        409
    );
    for (query, status) in [
        ("cursor=invalid".to_owned(), 400),
        ("other=1".to_owned(), 400),
        (format!("cursor={}", "x".repeat(1025)), 413),
        ("cursor=00000000-0000-0000-0000-000000000000:0:0&cursor=00000000-0000-0000-0000-000000000000:0:0".to_owned(), 400),
    ] {
        assert_eq!(fixture.client.get(fixture.url(&format!("/history?{query}")))
            .header("X-Wes-Session", &generation).send().await.unwrap().status(), status);
    }
    let held = fixture
        .app
        .current()
        .unwrap()
        .session
        .history_page(None, Default::default())
        .await
        .unwrap();
    assert_eq!(
        tokio::time::timeout(
            Duration::from_secs(1),
            fixture
                .client
                .get(fixture.url("/history"))
                .header("X-Wes-Session", &generation)
                .send()
        )
        .await
        .unwrap()
        .unwrap()
        .status(),
        429
    );
    drop(held);
    let empty = read(&fixture, &generation, None).await;
    assert_eq!(empty["entries"], json!([]));
    assert_eq!(empty["next"], Value::Null);
    let held = fixture
        .app
        .current()
        .unwrap()
        .session
        .history_page(None, Default::default())
        .await
        .unwrap();
    assert!(
        fixture
            .app
            .current()
            .unwrap()
            .session
            .observe()
            .await
            .unwrap()
            .cells
            .is_empty()
    );
    assert_eq!(fixture.calls.load(Ordering::SeqCst), 0);
    fixture.close().await;
    drop(held);
}
