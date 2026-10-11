use super::*;

#[tokio::test]
async fn language_route_serves_the_package_the_engine_prepares_commands_with() {
    let fixture = Fixture::new().await;
    let response = fixture
        .client
        .get(fixture.url("/language/calc"))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status().as_u16(), 200);
    let package: Value = serde_json::from_str(&response.text().await.unwrap()).unwrap();

    assert_eq!(package["language"], "calc");
    assert_eq!(package["version"], 1);
    assert_eq!(package["lexical"]["numbers"], "exact");
    // The eleven keywords of "safely JavaScript", each named with the production it stands for.
    let statements = package["statements"].as_object().unwrap();
    assert_eq!(statements.len(), 11);
    assert_eq!(statements["const"], "binding");
    assert_eq!(statements["let"], "binding-mut");
    // Arity is the package's, not the client's guess: reduce takes three, filter and map two.
    assert_eq!(
        package["operations"]["reduce"],
        json!({"operation":"reduce","min":3,"max":3,"method":true})
    );
    assert_eq!(
        package["operations"]["filter"],
        json!({"operation":"filter","min":2,"max":2,"method":true})
    );
    assert_eq!(
        package["operations"]["map"],
        json!({"operation":"map","min":2,"max":2,"method":true})
    );
    assert_eq!(
        package["operations"]["regexTest"],
        json!({"operation":"regex-test","min":2,"max":2,"method":false})
    );
    assert_eq!(
        package["operations"]["stripAnsi"],
        json!({"operation":"strip-ansi","min":1,"max":1,"method":false})
    );
    assert_eq!(
        package["operators"]["||"],
        json!({"operation":"or","precedence":1})
    );
    assert_eq!(
        package["operators"]["*"],
        json!({"operation":"mul","precedence":6})
    );
    // The package must not advertise unsupported operations or operators.
    for absent in ["group_by", "sort_desc", "sum"] {
        assert!(package["operations"][absent].is_null());
    }
    assert!(package["operators"]["|"].is_null());
    // The source is the package's capture identity, and it parses back into the same package.
    let source = package["source"].as_str().unwrap();
    let reloaded = wes_language::calc::Package::load(source).unwrap();
    assert_eq!(
        wes::web::language::published(&reloaded),
        wes::web::language::published(&wes_language::calc::Package::standard())
    );
    fixture.close().await;
}

#[test]
fn bundled_package_matches_the_served_package() {
    let bundled: Value = serde_json::from_str(include_str!(
        "../../../../gui/src/surface/language-default.json"
    ))
    .expect("the bundled client package is JSON");
    assert_eq!(
        bundled,
        wes::web::language::published(&wes_language::calc::Package::standard()),
        "gui/src/surface/language-default.json is stale; regenerate it with \
         `cargo run -p wes --example language-package > gui/src/surface/language-default.json`"
    );
}

#[tokio::test]
async fn yaml_language_route_publishes_authoritative_schema_and_rejects_cross_origin() {
    let fixture = Fixture::new().await;
    let response = fixture
        .client
        .get(fixture.url("/language/yaml"))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status().as_u16(), 200);
    assert_eq!(response.headers()["cache-control"], "no-store");
    assert_eq!(response.headers()["content-type"], "application/json");
    let schema: Value = serde_json::from_str(&response.text().await.unwrap()).unwrap();
    assert_eq!(schema, wes::web::language::published_yaml());
    assert_eq!(schema["roots"]["env"], "env.package");
    assert_eq!(
        schema["definitions"]["env.package"]["fields"]["package"]["schema"],
        "identifier"
    );
    let response = fixture
        .client
        .get(fixture.url("/language/yaml"))
        .header("Origin", "https://unrelated.invalid")
        .send()
        .await
        .unwrap();
    assert_eq!(response.status().as_u16(), 403);
    fixture.close().await;
}

#[test]
fn yaml_editor_test_fixture_matches_the_authoritative_schema() {
    let fixture: Value = serde_json::from_str(include_str!(
        "../../../../gui/src/surface/testing/yaml-schema.json"
    ))
    .unwrap();
    assert_eq!(
        fixture,
        wes::web::language::published_yaml(),
        "regenerate with cargo run -p wes --example yaml-schema > gui/src/surface/testing/yaml-schema.json; this is a test fixture, never a runtime fallback"
    );
}

#[tokio::test]
async fn view_catalogue_matches_generated_client_contracts() {
    let fixture = Fixture::new().await;
    let response = fixture
        .client
        .get(fixture.url("/language/views"))
        .send()
        .await
        .unwrap();
    assert!(response.status().is_success());
    let actual: Value = serde_json::from_str(&response.text().await.unwrap()).unwrap();
    let bundled: Value = serde_json::from_str(include_str!(
        "../../../../gui/src/value-views/catalogue.json"
    ))
    .unwrap();
    assert_eq!(actual, bundled, "Run node tools/view-build.mjs");
    assert_eq!(actual, wes::web::language::published_views());
    fixture.close().await;
}

#[tokio::test]
async fn selection_template_is_read_only_and_activation_transport_preserves_one_node() {
    let fixture = Fixture::new().await;
    let mut events = fixture.stream().await;
    let generation = events.generation().await;
    assert_eq!(
        fixture
            .source(
                &generation,
                "rows",
                ":calc pure { return [{value:'first'}, {value:'last'}]; } > rows"
            )
            .await,
        202
    );
    events.until("ready").await;
    let session = fixture.app.current().unwrap().session;
    session.wait_idle().await.unwrap();
    let snapshot = session.snapshot().await.unwrap();
    let source = &snapshot.names["rows"].node;
    let run = &snapshot.execution.runs[source];
    let text = "catalog echo value:$rows.selected.value > detail";
    let response = fixture
        .client
        .post(fixture.url("/language/selection-template"))
        .header("Content-Type", "application/json")
        .header("X-Wes-Session", &generation)
        .body(json!({"text":text}).to_string())
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 200);
    let analysis: Value = serde_json::from_str(&response.text().await.unwrap()).unwrap();
    assert_eq!(analysis["source"], source.as_str());
    assert_eq!(analysis["generation"], generation);
    assert_eq!(session.snapshot().await.unwrap().execution.graph.len(), 1);
    assert_eq!(fixture.calls.load(Ordering::SeqCst), 0);
    let selection = json!({"kind":"row","source":source.as_str(),"run":run.as_str(),"row":1});
    let request = json!({"request":"submit","client":"web-fixture","cell":"select","text":text,"selection":selection});
    assert_eq!(
        fixture.post(&generation, request.clone()).await.status(),
        202
    );
    let planned = loop {
        let event = events.until("planned").await;
        if event["cell"] == "select" {
            break event;
        }
    };
    assert_eq!(planned["selectionSource"], source.as_str());
    let created = loop {
        let event = events.until("created").await;
        if event["name"] == "detail" {
            break event;
        }
    };
    assert_eq!(created["selectionSources"], json!([source.as_str()]));
    assert_eq!(
        created["dependsOn"],
        json!([]),
        "selection is not an execution dependency"
    );
    session.wait_idle().await.unwrap();
    assert_eq!(fixture.post(&generation, request).await.status(), 202);
    assert_eq!(fixture.calls.load(Ordering::SeqCst), 1);
    assert_eq!(fixture.post(&generation,json!({"request":"submit","client":"web-fixture","cell":"again","text":text,"selection":selection,"repeat":"select","acknowledge_effects":true})).await.status(),202);
    session.wait_idle().await.unwrap();
    assert_eq!(fixture.calls.load(Ordering::SeqCst), 2);
    assert_eq!(session.snapshot().await.unwrap().execution.graph.len(), 2);
    drop(events);
    fixture.close().await;
}
