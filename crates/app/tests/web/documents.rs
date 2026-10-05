use super::*;

#[tokio::test]
async fn editor_document_transport_projects_exact_bytes_semantic_diagnostics_and_retry_identity() {
    let fixture = Fixture::new().await;
    let mut events = fixture.stream().await;
    let generation = events.generation().await;
    let yaml = "# ĞğİıŞşÇçÖöÜü\r\ntypes:\n  CapturedText: {base: Text}\n";
    let request = json!({"request":"submit","client":"web-fixture", "cell":"editor", "text":":package load source:\"\" origin:\"editor:stable\"", "document":{"source":yaml}});
    assert_eq!(
        fixture.post(&generation, request.clone()).await.status(),
        202
    );
    let planned = events.until("planned").await;
    assert_eq!(planned["document"]["source"], yaml);
    assert!(planned["failure"].is_null());
    assert!(
        planned["diagnostics"]
            .as_array()
            .unwrap()
            .iter()
            .all(|d| d["severity"] != "error")
    );
    assert_eq!(
        fixture.post(&generation, request.clone()).await.status(),
        202
    );
    let mut changed = request;
    changed["document"]["source"] = json!("types: {}");
    assert_eq!(fixture.post(&generation, changed).await.status(), 409);
    assert_eq!(fixture.post(&generation, json!({"request":"submit","client":"web-fixture", "cell":"invalid", "text":":package load source:\"\"", "document":{"source":"types: {Broken: {base: Missing}}"}})).await.status(), 202);
    let planned = loop {
        let p = events.until("planned").await;
        if p["cell"] == "invalid" {
            break p;
        }
    };
    assert!(
        planned["diagnostics"]
            .as_array()
            .unwrap()
            .iter()
            .any(|d| d["severity"] == "error")
    );
    assert!(
        !fixture
            .app
            .current()
            .unwrap()
            .session
            .observe()
            .await
            .unwrap()
            .types
            .iter()
            .any(|t| t == "Broken")
    );
    assert_eq!(fixture.post(&generation, json!({"request":"submit","client":"web-fixture", "cell":"large", "text":":package load source:\"\"", "document":{"source":"x".repeat(1024*1024+1)}})).await.status(), 400);
    assert_eq!(fixture.post(&generation, json!({"request":"submit","client":"web-fixture", "cell":"console", "text":":package load source:\"\"", "console":true, "document":{"source":yaml}})).await.status(), 400);
    fixture.close().await;
}
