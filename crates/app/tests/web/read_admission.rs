use super::*;

#[tokio::test]
async fn unread_stored_value_bodies_do_not_hold_display_or_interaction_admission() {
    use tokio::io::AsyncReadExt;

    let fixture = Fixture::new().await;
    let current = fixture.app.current().unwrap();
    let generation = &current.generation;
    let mut roots = Vec::new();
    for name in ["chart1", "chart2", "chart3"] {
        assert_eq!(
            fixture
                .source(generation, name, &format!(":view create Timeline > {name}"))
                .await,
            202
        );
        current.session.wait_idle().await.unwrap();
        let node = current.session.snapshot().await.unwrap().names[name]
            .node
            .clone();
        let identity = current
            .session
            .view_frame(node.clone())
            .await
            .unwrap()
            .instances[0]
            .identity
            .to_string();
        roots.push((node, identity, String::new()));
    }

    // Inline record contract and synthetic payload. The body exceeds the loopback send buffer:
    // reading only its headers leaves encoding complete but delivery backpressured.
    let mut contracts = wes_core::contracts::ContractRegistry::new();
    contracts
        .load(
            "types:\n  StoredRow:\n    base: Record\n    fields: {ordinal: Int, raw: Text, text: Text}\n",
        )
        .unwrap();
    let contract = contracts.resolve("List<StoredRow>").unwrap();
    let text = "x".repeat(4096);
    let rows: Vec<_> = (1..=2048)
        .map(|ordinal| json!({"ordinal":ordinal,"raw":text,"text":text}))
        .collect();
    let data = wes_adapters::codec::decode_json_for_contract(
        &serde_json::to_vec(&rows).unwrap(),
        Limits::default(),
        &contract,
    )
    .unwrap();
    assert!(contract.issues(&data).is_empty());
    let value = wes_core::Value::new(contract.shape(), data, Default::default()).unwrap();
    let expected = wes_adapters::codec::encode_display_value(&value, Limits::default()).unwrap();
    assert!(expected.len() > 16 * 1024 * 1024);
    let handle = fixture.worker.store(value).await.unwrap();
    let mut bodies = Vec::new();
    assert_eq!(wes_budgets::get("transport.reads"), 2);
    for _ in 0..2 {
        let socket = tokio::net::TcpSocket::new_v4().unwrap();
        socket.set_recv_buffer_size(64 * 1024).unwrap();
        let mut socket = socket.connect(fixture.server.address()).await.unwrap();
        socket.write_all(format!("GET /values/{handle} HTTP/1.1\r\nHost: {}\r\nX-Wes-Session: {generation}\r\nConnection: close\r\n\r\n", fixture.server.address()).as_bytes()).await.unwrap();
        let headers = tokio::time::timeout(Duration::from_secs(8), async {
            let mut headers = Vec::new();
            while !headers.ends_with(b"\r\n\r\n") {
                headers.push(socket.read_u8().await.unwrap());
                assert!(headers.len() < 8192);
            }
            String::from_utf8(headers).unwrap()
        })
        .await
        .unwrap();
        assert!(headers.starts_with("HTTP/1.1 200"), "{headers}");
        bodies.push(socket);
    }
    assert!(
        !fixture.server.has_pending_io(),
        "the encoders have finished before polling"
    );

    // Open fresh leases after the large fixture encodes; readiness does not rely on a sleep
    // or on extending the production lease deadline while constructing the test payload.
    for (node, identity, token) in &mut roots {
        let response = fixture
            .client
            .post(fixture.url(&format!("/view-mounts/{node}/{identity}")))
            .header("X-Wes-Session", generation)
            .header("Content-Type", "application/json")
            .body(json!({"action":"open"}).to_string())
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), 200);
        let mount: Value = serde_json::from_slice(&response.bytes().await.unwrap()).unwrap();
        *token = mount["token"].as_str().unwrap().to_owned();
    }

    let (chart, identity, _) = &roots[1];
    let state_url = fixture.url(&format!("/view-interaction/{chart}/{identity}"));
    for _ in 0..3 {
        let mut requests = vec![
            fixture
                .client
                .get(&state_url)
                .header("X-Wes-Session", generation),
        ];
        for (node, identity, token) in &roots {
            for path in [
                format!("/view-instances/{node}/{identity}"),
                format!("/live-view/{node}"),
                format!("/view-inputs/{node}/{identity}"),
            ] {
                requests.push(
                    fixture
                        .client
                        .get(fixture.url(&path))
                        .header("X-Wes-Session", generation)
                        .header("X-Wes-View-Mount", token),
                );
            }
        }
        for response in futures_util::future::join_all(requests.into_iter().map(|r| r.send())).await
        {
            let response = response.unwrap();
            assert_eq!(
                response.status(),
                200,
                "unread stored bodies must not retain encoder slots"
            );
            let _: Value = serde_json::from_slice(&response.bytes().await.unwrap()).unwrap();
        }
    }
    let viewport = json!({"start":"2026-01-01T00:00:00Z","end":"2026-01-01T00:10:00Z"});
    let edit = json!({"owner":chart.as_str(),"identity":identity,"definitionRevision":"0","revision":"0",
        "fields":{"viewport":viewport,"selection":null,"selectedItem":null},
        "outputs":{"selection":null,"selectedItem":null},"events":[]});
    let response = fixture
        .client
        .put(&state_url)
        .header("X-Wes-Session", generation)
        .header("Content-Type", "application/json")
        .body(edit.to_string())
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 200);
    let changed: Value = serde_json::from_slice(&response.bytes().await.unwrap()).unwrap();
    assert_eq!(changed["revision"], "1");
    let response = fixture
        .client
        .get(&state_url)
        .header("X-Wes-Session", generation)
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 200);
    let changed: Value = serde_json::from_slice(&response.bytes().await.unwrap()).unwrap();
    assert_eq!(changed["fields"]["viewport"], viewport);
    assert_eq!(changed["revision"], "1");

    // Clients can resume delivery after the commit; neither held body is cancelled or truncated.
    for mut socket in bodies {
        let mut body = Vec::new();
        tokio::time::timeout(Duration::from_secs(8), socket.read_to_end(&mut body))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(body, expected);
    }
    assert_eq!(fixture.calls.load(Ordering::SeqCst), 0);
    fixture.close().await;
}

#[tokio::test]
async fn three_timeline_instances_can_read_and_commit_during_concurrent_display_reads() {
    let fixture = Fixture::new().await;
    let generation = fixture.app.current().unwrap().generation;
    for name in ["chart1", "chart2", "chart3"] {
        assert_eq!(
            fixture
                .source(
                    &generation,
                    name,
                    &format!(":view create Timeline > {name}")
                )
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
    }
    let current = fixture.app.current().unwrap();
    let observed = current.session.observe().await.unwrap();
    let mut roots = Vec::new();
    for name in ["chart1", "chart2", "chart3"] {
        let node = observed.state.names[name].node.clone();
        let frame = current.session.view_frame(node.clone()).await.unwrap();
        roots.push((node, frame.instances[0].identity.to_string()));
    }
    let (chart, identity) = &roots[1];
    let state_url = fixture.url(&format!("/view-interaction/{chart}/{identity}"));
    let viewport = json!({"start":"2026-01-01T00:00:00Z","end":"2026-01-01T00:10:00Z"});
    let edit = json!({"owner":chart.as_str(),"identity":identity,"definitionRevision":"0","revision":"0",
        "fields":{"viewport":viewport,"selection":null,"selectedItem":null},
        "outputs":{"selection":null,"selectedItem":null},"events":[]});
    // Independent GUI readers start these together. Two in-flight frame reads must not
    // permanently refuse another instance's interaction GET or its viewport commit.
    let mut requests = Vec::new();
    for (node, instance) in &roots {
        for path in [
            format!("/view-instances/{node}/{instance}"),
            format!("/live-view/{node}"),
            format!("/view-inputs/{node}/{instance}"),
        ] {
            requests.push(
                fixture
                    .client
                    .get(fixture.url(&path))
                    .header("X-Wes-Session", &generation),
            );
        }
    }
    requests.push(
        fixture
            .client
            .get(&state_url)
            .header("X-Wes-Session", &generation),
    );
    requests.push(
        fixture
            .client
            .put(&state_url)
            .header("X-Wes-Session", &generation)
            .header("Content-Type", "application/json")
            .body(edit.to_string()),
    );
    let responses =
        futures_util::future::join_all(requests.into_iter().map(|request| async move {
            let response = request.send().await.unwrap();
            let status = response.status();
            let body = response.text().await.unwrap();
            (status, body)
        }))
        .await;
    for (status, body) in &responses {
        assert_eq!(*status, 200, "concurrent read or commit refused: {body}");
    }
    let changed: Value = serde_json::from_str(&responses.last().unwrap().1).unwrap();
    assert_eq!(changed["revision"], "1");
    assert_eq!(changed["fields"]["viewport"], viewport);
    assert_eq!(changed["outputs"]["selection"], Value::Null);
    let response = fixture
        .client
        .get(&state_url)
        .header("X-Wes-Session", &generation)
        .send()
        .await
        .unwrap();
    let persisted: Value = serde_json::from_str(&response.text().await.unwrap()).unwrap();
    assert_eq!(persisted["revision"], "1");
    assert_eq!(persisted["fields"]["viewport"], viewport);
    assert_eq!(fixture.calls.load(Ordering::SeqCst), 0);
    fixture.close().await;
}
