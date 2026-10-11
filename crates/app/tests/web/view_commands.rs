use super::*;
#[tokio::test]
async fn transient_command_and_evidence_ports_require_the_current_scope_owner() {
    let f = Fixture::datasets().await;
    let current = f.app.current().unwrap();
    let generation = &current.generation;
    assert_eq!(
        f.source(
            generation,
            "scoped-input",
            ":calc pure { return {view:'metric',value:42}; } > input"
        )
        .await,
        202
    );
    current.session.wait_idle().await.unwrap();
    let made = f.post(generation, json!({"request":"submit","client":"web-fixture","cell":"scoped-ports","text":r#":transient {
:def send(value:Text) as catalog echo value:?value
:view create Dashboard > board
:view create Metric input:$input > card
} > study"#})).await;
    assert_eq!(made.status(), 200, "{}", made.text().await.unwrap());
    let observed = current
        .session
        .read_sandbox_export("study.board", false)
        .await
        .unwrap();
    let scope = observed.scope.unwrap();
    let (child, _) = current.session.resolve_read_scope(&scope).await.unwrap();
    // View edits capture completed instances; they deliberately do not start their sources.
    let connected = child
        .submit(
            wes_engine::source::SourceInput::new(
                "scoped-connect".into(),
                ":view connect $card to:$board".into(),
            )
            .unwrap(),
        )
        .await
        .unwrap();
    assert!(
        !connected
            .diagnostics
            .diagnostics
            .iter()
            .any(|d| d.severity == wes_language::Severity::Error)
    );
    child.wait_idle().await.unwrap();
    let state = child.snapshot().await.unwrap();
    assert!(
        state.execution.errors.is_empty(),
        "{:?}",
        state.execution.errors
    );
    let root = &state.names["board"].node;
    let frame = child.view_frame(root.clone()).await.unwrap();
    let owner = &frame.instances[0];
    let card = frame
        .instances
        .iter()
        .find(|v| v.id == state.names["card"].node)
        .expect("the completed scoped card belongs to the board");
    let body = json!({"root":root.as_str(),"instance":owner.identity.as_ref(),"member":card.id.as_str(),
        "revision":card.revision.to_string(),"inputRevision":card.input_revision.to_string(),
        "template":"send","arguments":{"value":"synthetic-scoped-draft"}});
    let prepare = |scope: Option<&str>| {
        let request = f
            .client
            .post(f.url("/view-commands"))
            .header("X-Wes-Session", generation)
            .header("Content-Type", "application/json")
            .body(body.to_string());
        scope
            .map_or(request.try_clone().unwrap(), |s| {
                request.header("X-Wes-Scope", s)
            })
            .send()
    };
    assert_eq!(prepare(None).await.unwrap().status(), 409);
    let prepared = prepare(Some(&scope)).await.unwrap();
    assert_eq!(prepared.status(), 200, "{}", prepared.text().await.unwrap());
    let evidence = |scope: Option<&str>| {
        let request = f
            .client
            .get(f.url(&format!(
                "/view-evidence/{root}/{}/{root}/members/0",
                owner.identity
            )))
            .header("X-Wes-Session", generation)
            .header("X-Wes-View-Epoch", &frame.authority_epoch)
            .header("X-Wes-View-Revision", owner.revision.to_string())
            .header("X-Wes-Input-Revision", owner.input_revision.to_string());
        scope
            .map_or(request.try_clone().unwrap(), |s| {
                request.header("X-Wes-Scope", s)
            })
            .send()
    };
    assert_eq!(evidence(None).await.unwrap().status(), 403);
    let read = evidence(Some(&scope)).await.unwrap();
    assert_eq!(read.status(), 200, "{}", read.text().await.unwrap());
    let body: Value = serde_json::from_str(&read.text().await.unwrap()).unwrap();
    assert_eq!(body["available"], true);
    assert_eq!(body["input"]["data"]["value"], 42);
    assert_eq!(f.calls.load(Ordering::SeqCst), 0);
    assert_eq!(f.post(generation,json!({"request":"submit","client":"web-fixture","cell":"close-scoped-ports","text":":cancel $study"})).await.status(),200);
    assert_eq!(prepare(Some(&scope)).await.unwrap().status(), 410);
    assert_eq!(evidence(Some(&scope)).await.unwrap().status(), 410);
    assert_eq!(f.calls.load(Ordering::SeqCst), 0);
    f.close().await;
}

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
