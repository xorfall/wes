use super::*;
use tokio::sync::mpsc;
use wes_core::{Data, Primitive, Provenance};
use wes_engine::streams::{StreamFuture, StreamSink, StreamingInvoker};
struct Source {
    entered: mpsc::UnboundedSender<(StreamSink, CancellationToken)>,
    calls: Arc<AtomicUsize>,
}
fn item(n: i64) -> wes_core::Value {
    wes_core::Value::new(
        Shape::Primitive(Primitive::Int),
        Data::Int(n),
        Provenance::default(),
    )
    .unwrap()
}
impl StreamingInvoker for Source {
    fn subscribe(&self, _: Call, sink: StreamSink, token: CancellationToken) -> StreamFuture {
        self.calls.fetch_add(1, Ordering::SeqCst);
        sink.push(item(0)).unwrap();
        sink.opened().unwrap();
        self.entered.send((sink, token.clone())).unwrap();
        Box::pin(async move {
            token.cancelled().await;
            Ok(())
        })
    }
}
async fn frame_value(fixture: &Fixture, frame: &Value) -> Value {
    let response = fixture
        .client
        .get(fixture.url(&format!("/values/{}", frame["handle"].as_str().unwrap())))
        .send()
        .await
        .unwrap();
    assert!(response.status().is_success());
    serde_json::from_slice(&response.bytes().await.unwrap()).unwrap()
}
#[tokio::test]
async fn browser_stream_windows_survive_reconnect_save_and_held_load_without_implicit_resubscription()
 {
    let (entered, mut arrivals) = mpsc::unbounded_channel();
    let calls = Arc::new(AtomicUsize::new(0));
    let invoked = calls.clone();
    let fixture = Fixture::configured(
        |_| wes::web::Services::default(),
        Arc::new(move |workspace| {
            let mut capability =
                Capability::new(["watch"], Shape::Primitive(Primitive::Int), Safety::Safe);
            capability.streaming = true;
            workspace.register_provider_ports(
                ProviderDescription::new("events", [capability], vec![]).unwrap(),
                Arc::new(Echo(Arc::new(AtomicUsize::new(0)))),
                Some(Arc::new(Source {
                    entered: entered.clone(),
                    calls: invoked.clone(),
                })),
            )?;
            Ok(())
        }),
    )
    .await;
    let mut events = fixture.stream().await;
    let generation = events.generation().await;
    assert_eq!(
        fixture
            .source(&generation, "stream", "events watch > live")
            .await,
        202
    );
    let created = events.until("created").await;
    assert_eq!(created["streamSource"], true);
    assert_eq!(created["streamOutput"], true);
    let (sink, token) = tokio::time::timeout(Duration::from_secs(3), arrivals.recv())
        .await
        .unwrap()
        .unwrap();
    let first = events.until("ready").await;
    assert_eq!(first["kept"], false);
    assert_eq!(frame_value(&fixture, &first).await["data"], json!([0]));
    sink.push(item(1)).unwrap();
    sink.reject_item().unwrap();
    let next = events.until("ready").await;
    assert_ne!(next["handle"], first["handle"]);
    assert_eq!(frame_value(&fixture, &next).await["data"], json!([0, 1]));
    assert!(
        next["cautions"]
            .as_array()
            .unwrap()
            .contains(&json!("Stream rejected 1 invalid items."))
    );
    let mut reconnected = fixture.stream().await;
    assert_eq!(reconnected.generation().await, generation);
    assert_eq!(reconnected.until("ready").await["handle"], next["handle"]);
    assert_eq!(
        fixture
            .source(&generation, "stream", "events watch > live")
            .await,
        202
    );
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    assert_eq!(
        fixture
            .source(&generation, "save", ":workspace save \"stream-copy\"")
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
    let kept = events.until("ready").await;
    assert_eq!(kept["kept"], true);
    assert_eq!(kept["handle"], next["handle"]);
    assert!(!token.is_cancelled());
    assert_eq!(
        fixture
            .source(&generation, "load", ":workspace load \"stream-copy\"")
            .await,
        202
    );
    let replacement = events.generation().await;
    assert_ne!(replacement, generation);
    let restored = events.until("ready").await;
    assert_eq!(restored["handle"], kept["handle"]);
    assert_eq!(
        frame_value(&fixture, &restored).await["data"],
        json!([0, 1])
    );
    assert!(!token.is_cancelled());
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    sink.push(item(2)).unwrap();
    assert_eq!(
        fixture
            .source(&replacement, "refresh", ":refresh $live")
            .await,
        202
    );
    let (_, next_token) = tokio::time::timeout(Duration::from_secs(3), arrivals.recv())
        .await
        .unwrap()
        .unwrap();
    let refreshed = events.until("ready").await;
    assert_ne!(refreshed["handle"], kept["handle"]);
    assert_eq!(frame_value(&fixture, &refreshed).await["data"], json!([0]));
    assert_eq!(calls.load(Ordering::SeqCst), 2);
    drop(events);
    drop(reconnected);
    fixture.close().await;
    assert!(next_token.is_cancelled());
    assert!(token.is_cancelled());
    assert!(sink.push(item(3)).is_err());
}

const FAILURE_MESSAGE: &str = include_str!("../fixtures/stream-error.txt");
struct FailureProbe(Arc<AtomicUsize>);
impl Invoker for FailureProbe {
    fn invoke(&self, _: Call, _: CancellationToken) -> InvocationFuture {
        self.0.fetch_add(1, Ordering::SeqCst);
        Box::pin(async {
            Err(wes_engine::providers::InvocationError::Failed(
                wes_core::ErrorValue::new(
                    wes_core::ErrorId::new("synthetic-stream-failure").unwrap(),
                    "SYN001",
                    FAILURE_MESSAGE,
                    vec![],
                    None,
                )
                .unwrap(),
            ))
        })
    }
}

#[tokio::test]
async fn failed_branch_message_survives_live_history_eviction_reconnect_and_held_restore() {
    let (entered, mut arrivals) = mpsc::unbounded_channel();
    let calls = Arc::new(AtomicUsize::new(0));
    let failures = Arc::new(AtomicUsize::new(0));
    let invoked = calls.clone();
    let failed = failures.clone();
    let fixture = Fixture::configured(
        |_| wes::web::Services::default(),
        Arc::new(move |workspace| {
            let mut capability =
                Capability::new(["watch"], Shape::Primitive(Primitive::Int), Safety::Safe);
            capability.streaming = true;
            workspace.register_provider_ports(
                ProviderDescription::new("events", [capability], vec![]).unwrap(),
                Arc::new(Echo(Arc::new(AtomicUsize::new(0)))),
                Some(Arc::new(Source {
                    entered: entered.clone(),
                    calls: invoked.clone(),
                })),
            )?;
            let mut capability = Capability::new(["fail"], Shape::Unknown, Safety::Safe);
            capability.parameters = vec![Parameter::new("value", Shape::Unknown, true)];
            workspace.register_provider(
                ProviderDescription::new("fault", [capability], vec![]).unwrap(),
                Arc::new(FailureProbe(failed.clone())),
            )?;
            Ok(())
        }),
    )
    .await;
    let mut events = fixture.stream().await;
    let generation = events.generation().await;
    assert_eq!(fixture.source(&generation, "fork", "events watch | :fork { { fault fail value:input > broken } { :calc { return input; } > healthy } }").await, 202);
    let (sink, _) = tokio::time::timeout(Duration::from_secs(3), arrivals.recv())
        .await
        .unwrap()
        .unwrap();
    let first = events.until("failed").await;
    assert_eq!(first["reason"], FAILURE_MESSAGE);
    assert!(!first["error"]["id"].as_str().unwrap().is_empty());
    assert_eq!(first["error"]["message"], FAILURE_MESSAGE);
    let failed_id = first["node"].as_str().unwrap().to_owned();
    drop(events);

    // More than the bounded live log can retain. No UI subscriber drives execution.
    for n in 1..=6000 {
        sink.send(item(n)).await.unwrap();
    }
    let session = fixture.app.current().unwrap().session;
    session.wait_idle().await.unwrap();
    let state = session.snapshot().await.unwrap();
    let healthy = &state.names["healthy"].node;
    assert_eq!(state.execution.values[healthy].data(), &Data::Int(6000));
    let log = session.log().await.unwrap();
    assert!(log.omitted > 0);
    assert!(!log.entries.iter().any(|entry| matches!(entry.entry(),
        wes_engine::history::JournalEntry::Observed(record)
        if record.node().as_str() == failed_id && record.error().is_some())));

    let mut reconnected = fixture.stream().await;
    assert_eq!(reconnected.generation().await, generation);
    let again = reconnected.until("failed").await;
    assert_eq!(
        again, first,
        "current failure is independent of evicted log records"
    );
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    assert_eq!(failures.load(Ordering::SeqCst), 1);
    drop(reconnected);

    assert_eq!(
        fixture
            .source(&generation, "save-error", ":workspace save \"error-copy\"")
            .await,
        202
    );
    session.wait_idle().await.unwrap();
    assert_eq!(
        fixture
            .source(&generation, "load-error", ":workspace load \"error-copy\"")
            .await,
        202
    );
    let mut restored = fixture.stream().await;
    assert_ne!(restored.generation().await, generation);
    assert_eq!(restored.until("failed").await["reason"], FAILURE_MESSAGE);
    assert_eq!(
        calls.load(Ordering::SeqCst),
        1,
        "held restore never reopens the stream"
    );
    assert_eq!(
        failures.load(Ordering::SeqCst),
        1,
        "held restore never repeats the failed call"
    );
    drop(restored);
    fixture.close().await;
}

struct FailingSource {
    entered: mpsc::UnboundedSender<tokio::sync::oneshot::Sender<()>>,
    calls: Arc<AtomicUsize>,
}
impl StreamingInvoker for FailingSource {
    fn subscribe(&self, _: Call, sink: StreamSink, _: CancellationToken) -> StreamFuture {
        self.calls.fetch_add(1, Ordering::SeqCst);
        sink.opened().unwrap();
        sink.push(item(7)).unwrap();
        let (fail, failed) = tokio::sync::oneshot::channel();
        self.entered.send(fail).unwrap();
        Box::pin(async move {
            failed.await.unwrap();
            Err(wes_engine::providers::InvocationError::Failed(
                wes_core::ErrorValue::new(
                    wes_core::ErrorId::new("synthetic-source-failure").unwrap(),
                    "SYN001",
                    FAILURE_MESSAGE,
                    vec![],
                    None,
                )
                .unwrap(),
            ))
        })
    }
}

#[tokio::test]
async fn source_failure_after_a_visible_window_reaches_ui_as_failure_with_complete_message() {
    let (entered, mut arrivals) = mpsc::unbounded_channel();
    let calls = Arc::new(AtomicUsize::new(0));
    let invoked = calls.clone();
    let fixture = Fixture::configured(
        |_| wes::web::Services::default(),
        Arc::new(move |workspace| {
            let mut capability =
                Capability::new(["watch"], Shape::Primitive(Primitive::Int), Safety::Safe);
            capability.streaming = true;
            workspace.register_provider_ports(
                ProviderDescription::new("events", [capability], vec![]).unwrap(),
                Arc::new(Echo(Arc::new(AtomicUsize::new(0)))),
                Some(Arc::new(FailingSource {
                    entered: entered.clone(),
                    calls: invoked.clone(),
                })),
            )?;
            Ok(())
        }),
    )
    .await;
    let mut events = fixture.stream().await;
    let generation = events.generation().await;
    assert_eq!(
        fixture
            .source(&generation, "source-failure", "events watch > live")
            .await,
        202
    );
    let ready = events.until("ready").await;
    assert_eq!(frame_value(&fixture, &ready).await["data"], json!([7]));
    let url = fixture.url(&format!("/live-view/{}", ready["node"].as_str().unwrap()));
    let displayed = fixture
        .client
        .get(&url)
        .header("X-Wes-Session", &generation)
        .send()
        .await
        .unwrap();
    assert_eq!(displayed.status(), 200);
    let metadata: Value =
        serde_json::from_str(displayed.headers()["X-Wes-Display"].to_str().unwrap()).unwrap();
    assert_eq!(metadata["sources"][0]["counts"]["accepted"], "1");
    assert_eq!(
        serde_json::from_slice::<Value>(&displayed.bytes().await.unwrap()).unwrap()["data"],
        json!([7])
    );
    arrivals.recv().await.unwrap().send(()).unwrap();
    let failed = events.until("failed").await;
    assert_eq!(failed["node"], ready["node"]);
    assert_eq!(failed["reason"], FAILURE_MESSAGE);
    let retained = fixture
        .client
        .get(&url)
        .header("X-Wes-Session", &generation)
        .send()
        .await
        .unwrap();
    assert_eq!(retained.status(), 200);
    let terminal: Value =
        serde_json::from_str(retained.headers()["X-Wes-Display"].to_str().unwrap()).unwrap();
    assert_eq!(terminal["sources"][0]["phase"], "failed");
    assert_eq!(terminal["revision"], metadata["revision"]);
    assert_eq!(
        serde_json::from_slice::<Value>(&retained.bytes().await.unwrap()).unwrap()["data"],
        json!([7])
    );
    drop(events);
    let mut reconnected = fixture.stream().await;
    assert_eq!(reconnected.generation().await, generation);
    assert_eq!(reconnected.until("failed").await, failed);
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    drop(reconnected);
    fixture.close().await;
}

#[tokio::test]
async fn explicit_live_views_sample_fast_ordered_branches_without_waiting_for_storage_publication()
{
    let (entered, mut arrivals) = mpsc::unbounded_channel();
    let fixture = Fixture::configured(
        |_| wes::web::Services::default(),
        Arc::new(move |workspace| {
            let mut capability =
                Capability::new(["watch"], Shape::Primitive(Primitive::Int), Safety::Safe);
            capability.streaming = true;
            workspace.register_provider_ports(
                ProviderDescription::new("events", [capability], vec![]).unwrap(),
                Arc::new(Echo(Arc::new(AtomicUsize::new(0)))),
                Some(Arc::new(Source {
                    entered: entered.clone(),
                    calls: Arc::new(AtomicUsize::new(0)),
                })),
            )?;
            Ok(())
        }),
    )
    .await;
    let mut events = fixture.stream().await;
    let generation = events.generation().await;
    let source = "events watch | :stream accumulate limit:120 overflow:drop-oldest | :fork { { :calc pure { return {view:\"time-series\",x:\"event\",y:\"value\",points:input.items.map(n => {return {x:n,y:decimal(n)};})}; } > chart } { :calc pure { const last=input.items[length(input.items)-1]; return {view:\"table\",columns:[\"event\"],rows:input.items.filter(n => n > last - 8).map(n => [text(n)])}; } > table } }";
    assert_eq!(fixture.source(&generation, "dashboard", source).await, 202);
    let (sink, token) = arrivals.recv().await.unwrap();
    let current = fixture.app.current().unwrap();
    let observation = current.session.observe().await.unwrap();
    let chart = observation.state.names["chart"].node.clone();
    let table = observation.state.names["table"].node.clone();
    let producer = tokio::spawn(async move {
        for n in 1..600 {
            if sink.send(item(n)).await.is_err() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(2)).await;
        }
        sink
    });
    let mut charts = 0;
    let mut tables = 0;
    for _ in 0..12 {
        for (node, count, field, limit) in [
            (&chart, &mut charts, "points", 120),
            (&table, &mut tables, "rows", 8),
        ] {
            let response = tokio::time::timeout(
                Duration::from_secs(1),
                fixture
                    .client
                    .get(fixture.url(&format!("/live-view/{}", node.as_str())))
                    .header("X-Wes-Session", &generation)
                    .send(),
            )
            .await
            .unwrap()
            .unwrap();
            if response.status() == 204 {
                continue;
            }
            assert_eq!(response.status(), 200);
            let metadata: Value = serde_json::from_str(
                response
                    .headers()
                    .get("X-Wes-Display")
                    .unwrap()
                    .to_str()
                    .unwrap(),
            )
            .unwrap();
            assert!(
                metadata["revision"]
                    .as_str()
                    .unwrap()
                    .parse::<u64>()
                    .unwrap()
                    > 0
            );
            assert_eq!(metadata["epochs"].as_array().unwrap().len(), 1);
            assert_eq!(metadata["sources"][0]["phase"], "open");
            assert_eq!(metadata["sources"][0]["run"], metadata["epochs"][0][1]);
            let value: Value = serde_json::from_slice(&response.bytes().await.unwrap()).unwrap();
            assert!(value["data"][field].as_array().unwrap().len() <= limit);
            *count += 1;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    assert!(
        charts >= 3 && tables >= 3,
        "live display must make progress during continued input"
    );
    assert_eq!(
        fixture
            .client
            .get(fixture.url(&format!("/live-view/{}", chart.as_str())))
            .header("X-Wes-Session", "wrong-generation")
            .send()
            .await
            .unwrap()
            .status(),
        409
    );
    let sink = producer.await.unwrap();
    let private = item(601).with_provenance(
        Provenance::default().with_policy(&wes_core::flow::FlowPolicy::default().private()),
    );
    sink.send(private).await.unwrap();
    tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            let response = fixture
                .client
                .get(fixture.url(&format!("/live-view/{}", chart.as_str())))
                .header("X-Wes-Session", &generation)
                .send()
                .await
                .unwrap();
            if response.status() == 403 {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    tokio::time::timeout(
        Duration::from_secs(1),
        current.session.cancel_work("dashboard".into()),
    )
    .await
    .unwrap()
    .unwrap();
    tokio::time::timeout(Duration::from_secs(1), token.cancelled())
        .await
        .unwrap();
    drop(events);
    fixture.close().await;
}

#[tokio::test]
async fn live_instances_sample_bounded_frames_share_mounts_and_leave_external_stream_cancellation_explicit()
 {
    let (entered, mut arrivals) = mpsc::unbounded_channel();
    let calls = Arc::new(AtomicUsize::new(0));
    let invoked = calls.clone();
    let fixture = Fixture::configured(
        |_| wes::web::Services::default(),
        Arc::new(move |workspace| {
            let mut capability =
                Capability::new(["watch"], Shape::Primitive(Primitive::Int), Safety::Safe);
            capability.streaming = true;
            workspace.register_provider_ports(
                ProviderDescription::new("events", [capability], vec![]).unwrap(),
                Arc::new(Echo(Arc::new(AtomicUsize::new(0)))),
                Some(Arc::new(Source {
                    entered: entered.clone(),
                    calls: invoked.clone(),
                })),
            )?;
            Ok(())
        }),
    )
    .await;
    let mut events = fixture.stream().await;
    let generation = events.generation().await;
    let source = "events watch | :stream accumulate limit:8 overflow:drop-oldest | :calc pure { return {view:\"metric\",value:input.items[length(input.items)-1]}; } > sample";
    assert_eq!(fixture.source(&generation, "live", source).await, 202);
    let (sink, token) = arrivals.recv().await.unwrap();
    let current = fixture.app.current().unwrap();
    current.session.wait_idle().await.unwrap();
    assert_eq!(
        fixture
            .source(
                &generation,
                "view",
                ":view create Metric input:$sample > chart"
            )
            .await,
        202
    );
    current.session.wait_idle().await.unwrap();
    let state = current.session.snapshot().await.unwrap();
    let node = state.names["chart"].node.clone();
    let initial = current.session.view_frame(node.clone()).await.unwrap();
    let identity = initial.instances[0].identity.to_string();
    let url = fixture.url(&format!("/view-instances/{node}/{identity}"));
    let mounts = fixture.url(&format!("/view-mounts/{node}/{identity}"));
    let action = |body: Value| {
        fixture
            .client
            .post(&mounts)
            .header("X-Wes-Session", &generation)
            .header("Content-Type", "application/json")
            .body(body.to_string())
    };
    let mut leases = Vec::new();
    for _ in 0..2 {
        let response = action(json!({"action":"open"})).send().await.unwrap();
        assert_eq!(response.status(), 200);
        let value: Value = serde_json::from_str(&response.text().await.unwrap()).unwrap();
        leases.push(value["token"].as_str().unwrap().to_string());
    }
    assert_eq!(
        action(json!({"action":"start"}))
            .send()
            .await
            .unwrap()
            .status(),
        200
    );
    let producer = tokio::spawn(async move {
        for n in 1..=5000 {
            if sink.send(item(n)).await.is_err() {
                break;
            }
            if n % 50 == 0 {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        }
        sink
    });
    let mut distinct = std::collections::BTreeSet::new();
    for _ in 0..8 {
        let response = tokio::time::timeout(
            Duration::from_secs(1),
            fixture
                .client
                .get(&url)
                .header("X-Wes-Session", &generation)
                .header("X-Wes-View-Mount", &leases[0])
                .send(),
        )
        .await
        .unwrap()
        .unwrap();
        assert_eq!(response.status(), 200);
        let bytes = response.bytes().await.unwrap();
        assert!(
            bytes.len() < 4096,
            "bounded finite presentation, not stream history"
        );
        let frame: Value = serde_json::from_slice(&bytes).unwrap();
        let entry = &frame["instances"][0];
        assert_eq!(
            entry["revision"], "0",
            "data does not invalidate configuration edits"
        );
        assert_eq!(entry["observing"], true);
        distinct.insert(entry["input"]["data"]["value"].as_i64().unwrap());
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    assert!(distinct.len() >= 3, "display progresses under fast input");
    assert_eq!(
        calls.load(Ordering::SeqCst),
        1,
        "mounting and starting observation never subscribes again"
    );
    for (index, lease) in leases.iter().enumerate() {
        assert_eq!(
            action(json!({"action":"close","token":lease}))
                .send()
                .await
                .unwrap()
                .status(),
            200
        );
        let frame = current.session.view_frame(node.clone()).await.unwrap();
        assert_eq!(frame.instances[0].observing, index == 0);
        assert!(
            !token.is_cancelled(),
            "closing a view never cancels an external stream"
        );
    }
    let response = action(json!({"action":"open"})).send().await.unwrap();
    assert_eq!(response.status(), 200);
    assert!(
        current
            .session
            .view_frame(node.clone())
            .await
            .unwrap()
            .instances[0]
            .observing,
        "validated reopen resumes reads while preserving the original producer"
    );
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    assert!(!token.is_cancelled());
    tokio::time::timeout(
        Duration::from_secs(1),
        current.session.cancel_work("live".into()),
    )
    .await
    .unwrap()
    .unwrap();
    tokio::time::timeout(Duration::from_secs(3), token.cancelled())
        .await
        .unwrap();
    let _ = producer.await.unwrap();
    drop(events);
    fixture.close().await;
}
