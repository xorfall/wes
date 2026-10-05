use super::*;

#[tokio::test]
async fn kept_pure_conversion_reopens_held_and_refreshes_without_provider_calls() {
    let fixture =
        Fixture::configured(|_| wes::web::Services::default(), Arc::new(|_| Ok(()))).await;
    let mut events = fixture.stream().await;
    let generation = events.generation().await;
    assert_eq!(
        fixture
            .source(&generation, "rows", ":calc { return [1,2]; } > rows")
            .await,
        202
    );
    let rows = events.until("ready").await;
    assert_eq!(
        fixture
            .source(
                &generation,
                "view",
                ":calc pure { return \"Items: \" + $rows.reduce((s, n) => s + text(n) + \";\", \"\"); } > shown"
            )
            .await,
        202
    );
    let shown = events.until("ready").await;
    for ready in [&rows, &shown] {
        assert!(
            fixture
                .post(
                    &generation,
                    json!({"request":"keep", "handle":ready["handle"]})
                )
                .await
                .status()
                .is_success()
        );
    }
    assert_eq!(
        fixture
            .source(&generation, "save", ":workspace save \"view-copy\"")
            .await,
        202
    );
    assert_eq!(
        fixture
            .source(&generation, "load", ":workspace load \"view-copy\"")
            .await,
        202
    );
    let next = events.generation().await;
    assert_ne!(next, generation);
    // Consume both held results before requesting a new local redraw.
    events.until("ready").await;
    events.until("ready").await;
    assert_eq!(fixture.calls.load(Ordering::SeqCst), 0);
    assert_eq!(
        fixture.source(&next, "redraw", ":refresh $shown").await,
        202
    );
    let ready = events.until("ready").await;
    let bytes = fixture
        .client
        .get(fixture.url(&format!("/values/{}", ready["handle"].as_str().unwrap())))
        .send()
        .await
        .unwrap()
        .bytes()
        .await
        .unwrap();
    let data: Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(data["data"], "Items: 1;2;");
    assert_eq!(fixture.calls.load(Ordering::SeqCst), 0);
    fixture.close().await;
}
