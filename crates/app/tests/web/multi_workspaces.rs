use super::*;

async fn open(f: &Fixture, name: &str) -> Value {
    let response = f
        .client
        .post(f.url("/workspaces"))
        .header("Content-Type", "application/json")
        .body(json!({"name":name,"create":true}).to_string())
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 200);
    serde_json::from_slice(&response.bytes().await.unwrap()).unwrap()
}
async fn events(f: &Fixture, name: &str) -> Events {
    let mut url = url::Url::parse(&f.url("/events")).unwrap();
    url.query_pairs_mut().append_pair("workspace", name);
    let response = f.client.get(url).send().await.unwrap();
    assert_eq!(response.status(), 200);
    Events {
        response,
        pending: String::new(),
    }
}
async fn source(f: &Fixture, name: &str, generation: &str, cell: &str, text: &str) -> u16 {
    f.client
        .post(f.url("/submit"))
        .header("Content-Type", "application/json")
        .header("X-Wes-Workspace", name)
        .header("X-Wes-Session", generation)
        .body(
            json!({"request":"submit","cell":cell,"text":text,"client":format!("client-{name}")})
                .to_string(),
        )
        .send()
        .await
        .unwrap()
        .status()
        .as_u16()
}

#[tokio::test]
async fn bound_clients_share_live_workspace_without_changing_other_sessions() {
    let f = Fixture::new().await;
    let initial = f.app.current().unwrap();
    let second = open(&f, "second").await;
    let second_generation = second["generation"].as_str().unwrap();
    assert_ne!(second_generation, initial.generation);
    assert_eq!(open(&f, "second").await, second);
    assert_eq!(f.app.current().unwrap().generation, initial.generation);
    let mut a = events(&f, "default").await;
    let mut b = events(&f, "second").await;
    let mut b2 = events(&f, "second").await;
    assert_eq!(a.generation().await, initial.generation);
    assert_eq!(b.generation().await, second_generation);
    assert_eq!(b2.generation().await, second_generation);
    assert_eq!(
        source(
            &f,
            "default",
            &initial.generation,
            "first-cell",
            "catalog echo value:first > only_first"
        )
        .await,
        202
    );
    assert_eq!(
        source(
            &f,
            "second",
            second_generation,
            "second-cell",
            "catalog echo value:second > only_second"
        )
        .await,
        202
    );
    assert_eq!(a.until("planned").await["cell"], "first-cell");
    assert_eq!(b.until("planned").await["cell"], "second-cell");
    assert_eq!(b2.until("planned").await["cell"], "second-cell");
    a.until("ready").await;
    let ready = b.until("ready").await;
    b2.until("ready").await;
    let stale_value = f
        .client
        .get(f.url(&format!("/values/{}", ready["handle"].as_str().unwrap())))
        .header("X-Wes-Workspace", "second")
        .header("X-Wes-Session", &initial.generation)
        .send()
        .await
        .unwrap();
    assert_eq!(stale_value.status(), 409);

    assert_eq!(
        source(
            &f,
            "second",
            &initial.generation,
            "wrong",
            "catalog echo value:wrong"
        )
        .await,
        409
    );
    for (name, generation, expected, absent) in [
        (
            "default",
            initial.generation.as_str(),
            "first-cell",
            "second-cell",
        ),
        ("second", second_generation, "second-cell", "first-cell"),
    ] {
        let response = f
            .client
            .get(f.url("/history"))
            .header("X-Wes-Workspace", name)
            .header("X-Wes-Session", generation)
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), 200);
        let page: Value = serde_json::from_slice(&response.bytes().await.unwrap()).unwrap();
        assert_eq!(page["generation"], generation);
        assert_eq!(page["entries"].as_array().unwrap().len(), 2);
        let observation = f
            .app
            .bound(name)
            .unwrap()
            .current()
            .unwrap()
            .session
            .observe()
            .await
            .unwrap();
        assert!(
            observation
                .cells
                .iter()
                .any(|cell| cell.input.cell() == expected)
        );
        assert!(
            !observation
                .cells
                .iter()
                .any(|cell| cell.input.cell() == absent)
        );
    }
    assert_eq!(f.calls.load(Ordering::SeqCst), 2);
    drop((a, b, b2));
    f.close().await;
}

#[tokio::test]
async fn explicit_invalid_unknown_and_conflicting_bindings_never_fall_back() {
    let f = Fixture::new().await;
    let generation = f.app.current().unwrap().generation;
    assert_eq!(
        source(
            &f,
            "unknown",
            &generation,
            "wrong",
            "catalog echo value:wrong"
        )
        .await,
        404
    );
    assert_eq!(
        source(
            &f,
            "../default",
            &generation,
            "wrong",
            "catalog echo value:wrong"
        )
        .await,
        400
    );
    for (path, header, expected) in [
        ("/events?workspace=unknown", None, 404),
        ("/events?workspace=", None, 400),
        ("/events?workspace=%FF", None, 400),
        ("/events?workspace=%GG", None, 400),
        ("/events", Some("%FF"), 400),
        ("/events", Some("%GG"), 400),
        ("/events?workspace=default&workspace=default", None, 400),
        ("/events?workspace=default", Some("other"), 400),
        ("/client-preferences", Some("unknown"), 404),
    ] {
        let mut request = f.client.get(f.url(path));
        if let Some(header) = header {
            request = request.header("X-Wes-Workspace", header);
        }
        assert_eq!(request.send().await.unwrap().status().as_u16(), expected);
    }
    let cross_origin = f
        .client
        .post(f.url("/workspaces"))
        .header("Origin", "https://untrusted.invalid")
        .header("Content-Type", "application/json")
        .body(json!({"name":"other","create":true}).to_string())
        .send()
        .await
        .unwrap();
    assert_eq!(cross_origin.status(), 403);
    assert!(f.app.bound("other").is_err());
    assert_eq!(f.calls.load(Ordering::SeqCst), 0);
    f.close().await;
}

#[tokio::test]
async fn encoded_workspace_headers_and_event_queries_support_unicode_names() {
    let f = Fixture::new().await;
    let name = "çalışma + 100%";
    let opened = open(&f, name).await;
    let mut stream = events(&f, name).await;
    let generation = stream.generation().await;
    assert_eq!(generation, opened["generation"]);
    assert_eq!(stream.until("workspace-context").await["name"], name);
    let response = f
        .client
        .get(f.url("/history"))
        .header("X-Wes-Workspace", "%C3%A7al%C4%B1%C5%9Fma%20%2B%20100%25")
        .header("X-Wes-Session", generation)
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 200);
    drop(stream);
    f.close().await;
}
