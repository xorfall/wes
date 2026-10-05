use super::*;

/* A synthetic data home: the fixture's own temporary root, never a user's. */

#[tokio::test]
async fn presentations_route_serves_the_directory_read_only_and_answers_a_long_poll_on_change() {
    // Arrange
    let fixture = Fixture::with_services(|root| wes::web::Services {
        presentations: Some(root.join("presentations")),
        ..Default::default()
    })
    .await;
    let directory = fixture.root.path().join("presentations");
    std::fs::create_dir_all(&directory).unwrap();
    std::fs::write(
        directory.join("sample.yaml"),
        "version: 1\ntype: Sample\npresent: {kind: table}\n",
    )
    .unwrap();
    // Act
    let first: Value = serde_json::from_str(
        &fixture
            .client
            .get(fixture.url("/presentations"))
            .send()
            .await
            .unwrap()
            .text()
            .await
            .unwrap(),
    )
    .unwrap();
    let revision = first["revision"].as_str().unwrap().to_owned();
    let writer = directory.clone();
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(300)).await;
        std::fs::write(
            writer.join("sample.yaml"),
            "version: 1\ntype: Sample\npresent: {kind: fields}\n",
        )
        .unwrap();
    });
    let changed: Value = serde_json::from_str(
        &fixture
            .client
            .get(fixture.url(&format!("/presentations?after={revision}")))
            .send()
            .await
            .unwrap()
            .text()
            .await
            .unwrap(),
    )
    .unwrap();
    // Assert: the text is transported as written; the change is announced with a new revision.
    assert_eq!(
        first["files"],
        json!([{"name":"sample.yaml","text":"version: 1\ntype: Sample\npresent: {kind: table}\n"}])
    );
    assert_ne!(changed["revision"], first["revision"]);
    assert_eq!(
        changed["files"][0]["text"],
        "version: 1\ntype: Sample\npresent: {kind: fields}\n"
    );
    // Read-only: there is no way to write through the route.
    let posted = fixture
        .client
        .post(fixture.url("/presentations"))
        .body("{}")
        .send()
        .await
        .unwrap();
    assert_eq!(posted.status().as_u16(), 405);
}
