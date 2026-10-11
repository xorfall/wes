use super::*;
#[tokio::test]
async fn prepare_command_is_owned_bounded_typed_and_effect_free_until_submit() {
    let f = Fixture::new().await;
    let mut events = f.stream().await;
    let generation = events.generation().await;
    assert_eq!(
        f.source(
            &generation,
            "definition",
            ":def send(value:Text) as catalog echo value:?value"
        )
        .await,
        202
    );
    f.app.current().unwrap().session.wait_idle().await.unwrap();
    assert_eq!(
        f.source(&generation, "view", ":view create Metric > card")
            .await,
        202
    );
    let ready = events.until("ready").await;
    let node = ready["node"].as_str().unwrap();
    let frame = f
        .app
        .current()
        .unwrap()
        .session
        .view_frame(wes_engine::graph::NodeId::new(node).unwrap())
        .await
        .unwrap();
    let entry = &frame.instances[0];
    let body = json!({"root":node,"instance":entry.identity.as_ref(),"member":node,"revision":entry.revision.to_string(),"inputRevision":entry.input_revision.to_string(),"template":"send","arguments":{"value":"literal\"\n:drop $card"}});
    let prepare = |generation: String, body: Value| {
        f.client
            .post(f.url("/view-commands"))
            .header("X-Wes-Session", generation)
            .header("Content-Type", "application/json")
            .body(body.to_string())
            .send()
    };
    assert_eq!(
        prepare("other".into(), body.clone())
            .await
            .unwrap()
            .status()
            .as_u16(),
        409
    );
    let reply = prepare(generation.clone(), body.clone()).await.unwrap();
    assert_eq!(reply.status().as_u16(), 200);
    assert_eq!(reply.headers()["cache-control"], "no-store");
    let reply: Value = serde_json::from_str(&reply.text().await.unwrap()).unwrap();
    assert_eq!(f.calls.load(Ordering::SeqCst), 0);
    let mut stale = body.clone();
    stale["revision"] = json!("01");
    assert_eq!(
        prepare(generation.clone(), stale)
            .await
            .unwrap()
            .status()
            .as_u16(),
        400
    );
    let mut stale = body.clone();
    stale["member"] = json!("other");
    assert_eq!(
        prepare(generation.clone(), stale)
            .await
            .unwrap()
            .status()
            .as_u16(),
        409
    );
    let mut stale = body;
    stale["source"] = json!("catalog echo value:injected");
    assert_eq!(
        prepare(generation.clone(), stale)
            .await
            .unwrap()
            .status()
            .as_u16(),
        400
    );
    assert_eq!(
        f.source(
            &generation,
            "explicit-submit",
            reply["source"].as_str().unwrap()
        )
        .await,
        202
    );
    f.app.current().unwrap().session.wait_idle().await.unwrap();
    assert_eq!(f.calls.load(Ordering::SeqCst), 1);
    f.close().await;
}
