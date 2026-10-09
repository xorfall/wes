use super::*;

#[tokio::test]
async fn view_input_contract_issues_survive_web_events_and_reconnect() {
    let fixture = Fixture::new().await;
    let mut events = fixture.stream().await;
    let generation = events.generation().await;
    assert_eq!(
        fixture
            .source(
                &generation,
                "invalid",
                ":calc { return {view: \"wrong-card\", value: \"bad\"}; } > invalid"
            )
            .await,
        202
    );
    events.until("ready").await;
    assert_eq!(
        fixture
            .source(&generation, "view", ":view create Metric input:$invalid")
            .await,
        202
    );
    let failed = events.until("failed").await;
    assert_eq!(failed["error"]["code"], "TYP005");
    assert_eq!(
        failed["error"]["message"],
        "View input does not satisfy Metric"
    );
    let issues = failed["error"]["issues"].as_array().unwrap();
    assert_eq!(issues.len(), 2);
    assert!(issues.iter().any(|issue| issue["path"] == "/view"
        && issue["message"].as_str().is_some_and(
            |message| message.contains("allowed values:") && message.contains("metric")
        )));
    assert!(issues.iter().any(|issue| issue["path"] == "/value"));
    fixture
        .app
        .current()
        .unwrap()
        .session
        .wait_idle()
        .await
        .unwrap();
    let mut reconnected = fixture.stream().await;
    assert_eq!(reconnected.generation().await, generation);
    assert_eq!(reconnected.until("failed").await, failed);
    assert_eq!(fixture.calls.load(Ordering::SeqCst), 0);
    fixture.close().await;
}

#[tokio::test]
async fn instance_frames_are_session_bound_revisioned_and_never_call_providers() {
    let fixture = Fixture::new().await;
    let mut events = fixture.stream().await;
    let generation = events.generation().await;
    assert_eq!(
        fixture
            .source(
                &generation,
                "sample",
                ":calc { return {view: \"metric\", value: 1250}; } > sample"
            )
            .await,
        202
    );
    events.until("ready").await;
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
    let ready = events.until("ready").await;
    let node = ready["node"].as_str().unwrap();
    let raw = fixture
        .client
        .get(fixture.url(&format!("/values/{}", ready["handle"].as_str().unwrap())))
        .send()
        .await
        .unwrap()
        .text()
        .await
        .unwrap();
    let value: Value = serde_json::from_str(&raw).unwrap();
    let instance = value["data"]["instance"].as_str().unwrap();
    let url = fixture.url(&format!("/view-instances/{node}/{instance}"));
    assert_eq!(
        fixture
            .client
            .get(fixture.url(&format!("/view-instances/{node}/different-instance")))
            .header("X-Wes-Session", &generation)
            .send()
            .await
            .unwrap()
            .status()
            .as_u16(),
        409
    );
    assert_eq!(
        fixture
            .client
            .get(&url)
            .send()
            .await
            .unwrap()
            .status()
            .as_u16(),
        409
    );
    let response = fixture
        .client
        .get(&url)
        .header("X-Wes-Session", &generation)
        .send()
        .await
        .unwrap();
    assert_eq!(response.status().as_u16(), 200);
    let etag = response.headers()["ETag"].to_str().unwrap().to_string();
    let frame: Value = serde_json::from_str(&response.text().await.unwrap()).unwrap();
    assert_eq!(frame["root"], node);
    assert_eq!(frame["instances"][0]["input"]["data"]["value"], 1250);
    assert_eq!(
        fixture
            .client
            .get(&url)
            .header("X-Wes-Session", &generation)
            .header("If-None-Match", &etag)
            .send()
            .await
            .unwrap()
            .status()
            .as_u16(),
        304
    );
    let response = fixture
        .client
        .get(&url)
        .header("X-Wes-Session", &generation)
        .header(
            "X-Wes-View-Revisions",
            json!([[node, instance, "0", "0"]]).to_string(),
        )
        .header(
            "X-Wes-View-Epoch",
            frame["authorityEpoch"].as_str().unwrap(),
        )
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 200);
    let delta: Value = serde_json::from_str(&response.text().await.unwrap()).unwrap();
    assert_eq!(delta["instances"][0]["unchanged"], true);
    assert!(delta["instances"][0]["input"].is_null());
    assert_eq!(
        fixture
            .source(
                &generation,
                "rebind",
                ":view bind $chart input:$sample revision:0"
            )
            .await,
        202
    );
    events.until("ready").await;
    let response = fixture
        .client
        .get(&url)
        .header("X-Wes-Session", &generation)
        .header("If-None-Match", &etag)
        .send()
        .await
        .unwrap();
    assert_eq!(response.status().as_u16(), 200);
    assert_ne!(response.headers()["ETag"].to_str().unwrap(), etag);
    let session = fixture.app.current().unwrap().session.clone();
    let view_node = wes_engine::graph::NodeId::new(node).unwrap();
    let lease = session
        .view_mount(
            view_node.clone(),
            instance.into(),
            wes_engine::views::MountAction::Open,
        )
        .await
        .unwrap()
        .unwrap();
    session
        .view_mount(
            view_node,
            instance.into(),
            wes_engine::views::MountAction::Close(lease.clone()),
        )
        .await
        .unwrap();
    let refused = fixture
        .client
        .get(&url)
        .header("X-Wes-Session", &generation)
        .header("X-Wes-View-Mount", lease)
        .send()
        .await
        .unwrap();
    assert_eq!(refused.status(), 403);
    assert!(
        refused
            .text()
            .await
            .unwrap()
            .contains("display session expired or closed")
    );
    let current = fixture
        .client
        .get(&url)
        .header("X-Wes-Session", &generation)
        .send()
        .await
        .unwrap();
    assert_eq!(
        current.status(),
        200,
        "closing a display lease does not remove the View"
    );
    assert_eq!(fixture.calls.load(Ordering::SeqCst), 0);
    fixture.close().await;
}

#[tokio::test]
async fn shared_interaction_is_typed_revisioned_separate_from_input_and_has_no_execution() {
    let fixture = Fixture::new().await;
    let mut events = fixture.stream().await;
    let generation = events.generation().await;
    install_range_summary(&fixture, &generation).await;
    assert_eq!(
        fixture
            .source(&generation, "view", ":view create Timeline > chart")
            .await,
        202
    );
    let ready = events.until("ready").await;
    let node = ready["node"].as_str().unwrap();
    let response = fixture
        .client
        .get(fixture.url(&format!("/values/{}", ready["handle"].as_str().unwrap())))
        .send()
        .await
        .unwrap();
    let value: Value = serde_json::from_str(&response.text().await.unwrap()).unwrap();
    let identity = value["data"]["instance"].as_str().unwrap();
    let url = fixture.url(&format!("/view-interaction/{node}/{identity}"));
    assert_eq!(
        fixture
            .client
            .get(&url)
            .send()
            .await
            .unwrap()
            .status()
            .as_u16(),
        409
    );
    let response = fixture
        .client
        .get(&url)
        .header("X-Wes-Session", &generation)
        .send()
        .await
        .unwrap();
    assert_eq!(response.status().as_u16(), 200);
    let initial: Value = serde_json::from_str(&response.text().await.unwrap()).unwrap();
    assert_eq!(initial["revision"], "0");
    let edit = json!({"owner":node,"identity":identity,"definitionRevision":"0","revision":"0",
        "fields":{"viewport":{"start":"2026-01-01T00:00:00Z","end":"2026-01-01T00:10:00Z"},"selection":null,"selectedItem":null},
        "outputs":{"selection":null,"selectedItem":null}});
    let send = |body: Value| {
        fixture
            .client
            .put(&url)
            .header("X-Wes-Session", &generation)
            .header("Content-Type", "application/json")
            .body(body.to_string())
    };
    let response = send(edit.clone()).send().await.unwrap();
    assert_eq!(response.status().as_u16(), 200);
    let changed: Value = serde_json::from_str(&response.text().await.unwrap()).unwrap();
    assert_eq!(changed["revision"], "1");
    assert!(changed.get("input").is_none());
    assert_eq!(
        send(edit.clone()).send().await.unwrap().status().as_u16(),
        409
    );
    let mut invalid = edit.clone();
    invalid["revision"] = "1".into();
    invalid["fields"]["cursor"] = json!(null);
    assert_eq!(send(invalid).send().await.unwrap().status().as_u16(), 400);
    let mut invalid = edit;
    invalid["revision"] = "1".into();
    invalid["outputs"]["selection"] = json!("not-an-interval");
    assert_ne!(send(invalid).send().await.unwrap().status().as_u16(), 200);
    assert_eq!(
        fixture
            .source(
                &generation,
                "output",
                ":view output $chart port:selection > selectedRange"
            )
            .await,
        202
    );
    let ready = events.until("ready").await;
    let response = fixture
        .client
        .get(fixture.url(&format!("/values/{}", ready["handle"].as_str().unwrap())))
        .send()
        .await
        .unwrap();
    let value: Value = serde_json::from_str(&response.text().await.unwrap()).unwrap();
    assert_eq!(value["type"]["kind"], "option");
    assert_eq!(value["type"]["element"]["name"], "INTERVAL");
    assert_eq!(fixture.source(&generation,"detail-data",":calc { return {view: \"range-summary\", title: \"Selection\", selection: none}; } > detailData").await,202);
    events.until("ready").await;
    assert_eq!(
        fixture
            .source(
                &generation,
                "detail",
                ":view create RangeSummary input:$detailData > detail"
            )
            .await,
        202
    );
    let detail = events.until("ready").await;
    let detail_node = detail["node"].as_str().unwrap();
    let response = fixture
        .client
        .get(fixture.url(&format!("/values/{}", detail["handle"].as_str().unwrap())))
        .send()
        .await
        .unwrap();
    let value: Value = serde_json::from_str(&response.text().await.unwrap()).unwrap();
    let detail_identity = value["data"]["instance"].as_str().unwrap();
    assert_eq!(
        fixture
            .source(
                &generation,
                "link",
                ":view link $chart output:selection to:$detail field:selection"
            )
            .await,
        202
    );
    events.until("ready").await;
    let inputs = fixture.url(&format!("/view-inputs/{detail_node}/{detail_identity}"));
    let response = fixture
        .client
        .get(&inputs)
        .header("X-Wes-Session", &generation)
        .send()
        .await
        .unwrap();
    assert_eq!(response.status().as_u16(), 200);
    let patch: Value = serde_json::from_str(&response.text().await.unwrap()).unwrap();
    assert_eq!(
        patch["values"][detail_node]["selection"]["data"]["kind"],
        "none"
    );
    assert!(patch.get("input").is_none());
    assert_eq!(patch["problems"], json!({}));
    assert_eq!(fixture.calls.load(Ordering::SeqCst), 0);
    fixture.close().await;
}

struct Samples(Arc<AtomicUsize>);
impl Invoker for Samples {
    fn invoke(&self, _: Call, _: CancellationToken) -> InvocationFuture {
        let n = self.0.fetch_add(1, Ordering::SeqCst) + 1;
        Box::pin(async move {
            Ok(wes_core::Value::new(
                wes_views::named("Metric").unwrap().input().shape(),
                wes_core::Data::Record(
                    [
                        ("view".into(), wes_core::Data::Text("metric".into())),
                        ("value".into(), wes_core::Data::Int(n as i64 * 1000)),
                    ]
                    .into(),
                ),
                wes_core::Provenance::default(),
            )
            .unwrap())
        })
    }
}
#[tokio::test]
async fn saved_views_restore_pinned_and_current_references_and_committed_state_without_starting_sources()
 {
    let calls = Arc::new(AtomicUsize::new(0));
    let invoked = calls.clone();
    let fixture = Fixture::configured(
        |_| wes::web::Services::default(),
        Arc::new(move |w| {
            w.register_provider(
                ProviderDescription::new(
                    "metrics",
                    [Capability::new(
                        ["sample"],
                        wes_views::named("Metric").unwrap().input().shape(),
                        Safety::Safe,
                    )],
                    vec![],
                )
                .unwrap(),
                Arc::new(Samples(invoked.clone())),
            )?;
            Ok(())
        }),
    )
    .await;
    let mut events = fixture.stream().await;
    let generation = events.generation().await;
    install_range_summary(&fixture, &generation).await;
    let commands = [
        "metrics sample > sample",
        ":view create Metric input:$sample > frozen",
        ":view pin $frozen > frozenInput",
        ":view create Metric input:$sample > latest",
        ":calc { return {view:\"dashboard\",title:\"Service overview\"}; } > layout",
        ":view create Dashboard input:$layout > board",
        ":view connect $frozen to:$board",
        ":read $frozen > receipt",
        ":view connect $latest to:$board",
        ":view create Timeline > chart",
        ":calc { return {view:\"range-summary\",title:\"Selection\",selection:none}; } > details",
        ":view create RangeSummary input:$details > summary",
        ":view link $chart output:selection to:$summary field:selection",
        ":refresh $sample",
    ];
    for (i, command) in commands.iter().enumerate() {
        assert_eq!(
            fixture
                .source(&generation, &format!("step-{i}"), command)
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
    let state = current.session.snapshot().await.unwrap();
    let board = state.names["board"].node.clone();
    let chart = state.names["chart"].node.clone();
    let summary = state.names["summary"].node.clone();
    let frozen = state.names["frozen"].node.clone();
    let latest = state.names["latest"].node.clone();
    let chart_frame = current.session.view_frame(chart.clone()).await.unwrap();
    let owner = &chart_frame.instances[0];
    let interaction = fixture.url(&format!("/view-interaction/{chart}/{}", owner.identity));
    let edit = json!({"owner":chart.as_str(),"identity":owner.identity.as_ref(),"definitionRevision":"0","revision":"0",
        "fields":{"viewport":{"start":"2030-01-01T00:00:00Z","end":"2030-01-01T01:00:00Z"},"selection":{"start":"2030-01-01T00:10:00Z","end":"2030-01-01T00:20:00Z"},"selectedItem":null},"outputs":{"selection":"2030-01-01T00:10:00Z/2030-01-01T00:20:00Z","selectedItem":null}});
    assert_eq!(
        fixture
            .client
            .put(&interaction)
            .header("X-Wes-Session", &generation)
            .header("Content-Type", "application/json")
            .body(edit.to_string())
            .send()
            .await
            .unwrap()
            .status(),
        200
    );
    // A previously independent chart may join a coordinator after interaction.
    for (key, command) in [
        (
            "group-layout",
            ":calc { return {view:\"timeline-group\",title:\"Coordinated charts\",range:interval(instant(\"2030-01-01T00:00:00Z\"),instant(\"2030-01-01T01:00:00Z\"))}; } > groupLayout",
        ),
        (
            "group-create",
            ":view create TimelineGroup input:$groupLayout > chartGroup",
        ),
        ("group-connect", ":view connect $chart to:$chartGroup"),
    ] {
        assert_eq!(fixture.source(&generation, key, command).await, 202);
        current.session.wait_idle().await.unwrap();
    }
    let state = current.session.snapshot().await.unwrap();
    let chart_group = state.names["chartGroup"].node.clone();
    let grouped = current
        .session
        .view_frame(chart_group.clone())
        .await
        .unwrap();
    let group_owner = &grouped.instances[0];
    let (coordinator, empty) = current
        .session
        .view_interaction(chart.clone(), owner.identity.to_string(), None)
        .await
        .unwrap();
    assert_eq!(coordinator.id, chart_group);
    assert_eq!(empty.revision, 0);
    let mut grouped_edit = edit.clone();
    grouped_edit["owner"] = json!(chart_group.as_str());
    grouped_edit["identity"] = json!(group_owner.identity.as_ref());
    grouped_edit["definitionRevision"] = json!("1");
    assert_eq!(
        fixture
            .client
            .put(&interaction)
            .header("X-Wes-Session", &generation)
            .header("Content-Type", "application/json")
            .body(grouped_edit.to_string())
            .send()
            .await
            .unwrap()
            .status(),
        200
    );
    let before = current.session.view_frame(board.clone()).await.unwrap();
    let identity = before.instances[0].identity.clone();
    let mounts = fixture.url(&format!("/view-mounts/{board}/{identity}"));
    for action in ["open", "start"] {
        assert_eq!(
            fixture
                .client
                .post(&mounts)
                .header("X-Wes-Session", &generation)
                .header("Content-Type", "application/json")
                .body(json!({"action":action}).to_string())
                .send()
                .await
                .unwrap()
                .status(),
            200
        );
    }
    assert_eq!(
        fixture
            .source(
                &generation,
                "save-board",
                ":workspace save \"dashboard-copy\""
            )
            .await,
        202
    );
    current.session.wait_idle().await.unwrap();
    assert_eq!(
        fixture
            .source(
                &generation,
                "load-board",
                ":workspace load \"dashboard-copy\""
            )
            .await,
        202
    );
    let replacement = events.generation().await;
    assert_ne!(replacement, generation);
    let restored = fixture.app.current().unwrap();
    restored.session.wait_idle().await.unwrap();
    let frame = restored.session.view_frame(board.clone()).await.unwrap();
    assert_eq!(frame.instances[0].identity, identity);
    assert_eq!(frame.instances[0].members["members"].len(), 2);
    let number = |id: &wes_engine::graph::NodeId| {
        let input = frame
            .instances
            .iter()
            .find(|i| &i.id == id)
            .unwrap()
            .input
            .as_ref()
            .unwrap()
            .value()
            .unwrap();
        let wes_core::Data::Record(fields) = input.data() else {
            panic!("record")
        };
        fields["value"].clone()
    };
    assert_eq!(
        number(&frozen),
        wes_core::Data::Int(1000),
        "pinned input must not silently become the source's newer run"
    );
    assert_eq!(number(&latest), wes_core::Data::Int(2000));
    assert!(matches!(
        frame
            .instances
            .iter()
            .find(|i| i.id == frozen)
            .unwrap()
            .input
            .as_ref()
            .unwrap()
            .binding,
        wes_engine::views::InputBinding::Retained(_)
    ));
    assert!(matches!(
        frame
            .instances
            .iter()
            .find(|i| i.id == latest)
            .unwrap()
            .input
            .as_ref()
            .unwrap()
            .binding,
        wes_engine::views::InputBinding::Current(_)
    ));
    assert!(
        frame.instances.iter().all(|i| !i.observing),
        "restoration never resumes observations"
    );
    let (_, shared) = restored
        .session
        .view_interaction(chart.clone(), owner.identity.to_string(), None)
        .await
        .unwrap();
    assert_eq!(shared.revision, 1);
    assert_eq!(shared.outputs.len(), 2);
    let group_frame = restored
        .session
        .view_frame(chart_group.clone())
        .await
        .unwrap();
    assert_eq!(group_frame.instances[0].members["members"], [chart.clone()]);
    let summary_frame = restored.session.view_frame(summary.clone()).await.unwrap();
    let patches = restored
        .session
        .view_input_patches(summary, summary_frame.instances[0].identity.to_string())
        .await
        .unwrap();
    assert!(patches.problems.is_empty());
    assert_eq!(patches.values.len(), 1);
    assert_eq!(
        calls.load(Ordering::SeqCst),
        2,
        "restore and mounting never invoke a provider"
    );
    assert_eq!(
        fixture
            .source(
                &replacement,
                "edit-restored",
                ":view bind $receipt input:$sample revision:0"
            )
            .await,
        202
    );
    restored.session.wait_idle().await.unwrap();
    assert_eq!(
        restored.session.view_frame(frozen).await.unwrap().instances[0].revision,
        1,
        "restored reference has workspace-owned authority"
    );
    drop(events);
    fixture.close().await;
}

#[tokio::test]
async fn restored_view_with_missing_retained_source_preserves_definition_without_fabricating_a_result()
 {
    let fixture = Fixture::new().await;
    let mut events = fixture.stream().await;
    let generation = events.generation().await;
    for (cell, source) in [
        (
            "sample",
            ":calc { return {view:\"metric\",value:1250}; } > sample",
        ),
        ("chart", ":view create Metric input:$sample > chart"),
    ] {
        assert_eq!(fixture.source(&generation, cell, source).await, 202);
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
    let state = current.session.observe().await.unwrap();
    let source = &state.state.names["sample"].node;
    let handle = state.values.as_ref().unwrap().outputs[source]
        .handle()
        .unwrap()
        .clone();
    let chart = state.state.names["chart"].node.clone();
    assert_eq!(
        fixture
            .source(
                &generation,
                "save-missing",
                ":workspace save \"missing-input\""
            )
            .await,
        202
    );
    current.session.wait_idle().await.unwrap();
    assert!(fixture.worker.release(handle).await.unwrap());
    assert_eq!(
        fixture
            .source(
                &generation,
                "load-missing",
                ":workspace load \"missing-input\""
            )
            .await,
        202
    );
    let replacement = events.generation().await;
    assert_ne!(replacement, generation);
    let frame = fixture
        .app
        .current()
        .unwrap()
        .session
        .view_frame(chart)
        .await
        .unwrap();
    assert!(frame.instances[0].input.as_ref().unwrap().value().is_none());
    assert!(
        frame.instances[0]
            .input_problem
            .as_ref()
            .unwrap()
            .contains("unavailable")
    );
    assert!(!frame.instances[0].observing);
    assert_eq!(fixture.calls.load(Ordering::SeqCst), 0);
    drop(events);
    fixture.close().await;
}

struct QuerySamples {
    calls: Arc<std::sync::Mutex<Vec<i64>>>,
    active: Arc<AtomicUsize>,
}
struct ActiveQuery(Arc<AtomicUsize>);
impl Drop for ActiveQuery {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::SeqCst);
    }
}
impl Invoker for QuerySamples {
    fn invoke(&self, call: Call, token: CancellationToken) -> InvocationFuture {
        let wes_core::Data::Option(Some(range)) = call.arguments["range"].data() else {
            panic!("selected interval")
        };
        let wes_core::Data::Interval(range) = range.as_ref() else {
            panic!("interval")
        };
        let n = (range.start().total_nanos() / 1_000_000_000) as i64;
        assert_eq!(
            self.active.fetch_add(1, Ordering::SeqCst),
            0,
            "replacement started before predecessor joined"
        );
        self.calls.lock().unwrap().push(n);
        let active = ActiveQuery(self.active.clone());
        Box::pin(async move {
            let _active = active;
            if n == 1 {
                token.cancelled().await;
            }
            Ok(wes_core::Value::new(
                wes_views::named("Metric").unwrap().input().shape(),
                wes_core::Data::Record(
                    [
                        ("view".into(), wes_core::Data::Text("metric".into())),
                        ("value".into(), wes_core::Data::Int(n)),
                    ]
                    .into(),
                ),
                wes_core::Provenance::default(),
            )
            .unwrap())
        })
    }
}
#[tokio::test]
async fn query_bindings_are_explicit_latest_only_mount_owned_and_restore_without_execution() {
    let calls = Arc::new(std::sync::Mutex::new(Vec::new()));
    let active = Arc::new(AtomicUsize::new(0));
    let invoked = calls.clone();
    let running = active.clone();
    let fixture = Fixture::configured(
        |_| wes::web::Services::default(),
        Arc::new(move |w| {
            let types = w.prepare_type_package(
                include_str!("../../../../views/metric/types.yaml"),
                wes_language::Span::at(0),
            )?;
            w.commit(types)?;
            let mut cap = Capability::new(
                ["sample"],
                wes_views::named("Metric").unwrap().input().shape(),
                Safety::Safe,
            );
            cap.parameters.push(Parameter::new(
                "range",
                wes_core::Shape::Option(Box::new(wes_core::Shape::Primitive(
                    wes_core::Primitive::Interval,
                ))),
                true,
            ));
            w.register_provider(
                ProviderDescription::new("metrics", [cap], vec![]).unwrap(),
                Arc::new(QuerySamples {
                    calls: invoked.clone(),
                    active: running.clone(),
                }),
            )?;
            Ok(())
        }),
    )
    .await;
    let mut events = fixture.stream().await;
    let generation = events.generation().await;
    for (i,source) in [
        ":def Observe(input: Option<Interval>) -> Metric as :calc { return call('metrics',['sample'],{range:input}); }",
        ":view create Timeline > chart", ":view create Metric > detail",
        ":view query $detail template:Observe from:$chart output:selection trigger:commit",
    ].into_iter().enumerate(){
        assert_eq!(fixture.source(&generation,&format!("query-setup-{i}"),source).await,202);
        fixture.app.current().unwrap().session.wait_idle().await.unwrap();
    }
    assert!(
        calls.lock().unwrap().is_empty(),
        "configuration must not execute a provider"
    );
    let current = fixture.app.current().unwrap();
    let snapshot = current.session.snapshot().await.unwrap();
    let chart = snapshot.names["chart"].node.clone();
    let detail = snapshot.names["detail"].node.clone();
    let frame = current.session.view_frame(detail.clone()).await.unwrap();
    let target = frame.instances[0].identity.to_string();
    assert_eq!(
        frame.instances[0].query.as_ref().unwrap().trigger,
        wes_engine::views::QueryTrigger::Commit
    );
    let chart_frame = current.session.view_frame(chart.clone()).await.unwrap();
    let identity = chart_frame.instances[0].identity.to_string();
    async fn select(
        fixture: &Fixture,
        generation: &str,
        node: &wes_engine::graph::NodeId,
        identity: &str,
        revision: u64,
        n: u64,
    ) {
        let start = format!("1970-01-01T00:00:{n:02}Z");
        let end = "1970-01-01T00:01:00Z";
        let edit = json!({"owner":node.as_str(),"identity":identity,"definitionRevision":"0","revision":revision.to_string(),"fields":{"viewport":{"start":"1970-01-01T00:00:00Z","end":end},"selection":{"start":start,"end":end},"selectedItem":null},"outputs":{"selection":format!("{start}/{end}"),"selectedItem":null}});
        let response = fixture
            .client
            .put(fixture.url(&format!("/view-interaction/{node}/{identity}")))
            .header("X-Wes-Session", generation)
            .header("Content-Type", "application/json")
            .body(edit.to_string())
            .send()
            .await
            .unwrap();
        assert_eq!(
            response.status().as_u16(),
            200,
            "{}",
            response.text().await.unwrap()
        );
    }
    select(&fixture, &generation, &chart, &identity, 0, 1).await;
    let first = current
        .session
        .view_mount(
            detail.clone(),
            target.clone(),
            wes_engine::views::MountAction::Open,
        )
        .await
        .unwrap()
        .unwrap();
    let second = current
        .session
        .view_mount(
            detail.clone(),
            target.clone(),
            wes_engine::views::MountAction::Open,
        )
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        fixture
            .source(&generation, "query-apply", ":view apply $detail")
            .await,
        202
    );
    tokio::time::timeout(std::time::Duration::from_secs(3), async {
        while calls.lock().unwrap().is_empty() {
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    select(&fixture, &generation, &chart, &identity, 1, 2).await;
    select(&fixture, &generation, &chart, &identity, 2, 3).await;
    async fn value(
        session: &wes_engine::session::SessionHandle,
        node: &wes_engine::graph::NodeId,
        n: i64,
    ) {
        tokio::time::timeout(std::time::Duration::from_secs(3),async{loop{
            let frame=session.view_frame(node.clone()).await.unwrap();
            if matches!(frame.instances[0].input.as_ref().and_then(|i|i.value()).map(|v|v.data()),Some(wes_core::Data::Record(fields)) if fields.get("value")==Some(&wes_core::Data::Int(n))){break;}
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }}).await.unwrap();
    }
    value(&current.session, &detail, 3).await;
    assert_eq!(calls.lock().unwrap().last(), Some(&3));
    assert_eq!(active.load(Ordering::SeqCst), 0);
    current
        .session
        .view_mount(
            detail.clone(),
            target.clone(),
            wes_engine::views::MountAction::Close(first),
        )
        .await
        .unwrap();
    assert!(
        current
            .session
            .view_frame(detail.clone())
            .await
            .unwrap()
            .instances[0]
            .observing
    );
    select(&fixture, &generation, &chart, &identity, 3, 4).await;
    value(&current.session, &detail, 4).await;
    current
        .session
        .view_mount(
            detail.clone(),
            target.clone(),
            wes_engine::views::MountAction::Close(second),
        )
        .await
        .unwrap();
    let count = calls.lock().unwrap().len();
    select(&fixture, &generation, &chart, &identity, 4, 5).await;
    tokio::time::sleep(std::time::Duration::from_millis(350)).await;
    assert_eq!(
        calls.lock().unwrap().len(),
        count,
        "last close revokes automatic execution"
    );
    assert_eq!(
        fixture
            .source(&generation, "save-query", ":workspace save \"query-board\"")
            .await,
        202
    );
    current.session.wait_idle().await.unwrap();
    assert_eq!(
        fixture
            .source(&generation, "load-query", ":workspace load \"query-board\"")
            .await,
        202
    );
    let replacement = events.generation().await;
    assert_ne!(replacement, generation);
    let restored = fixture.app.current().unwrap();
    restored.session.wait_idle().await.unwrap();
    let frame = restored.session.view_frame(detail.clone()).await.unwrap();
    assert_eq!(frame.instances[0].identity.as_ref(), target);
    assert!(frame.instances[0].query.is_some());
    assert!(!frame.instances[0].observing);
    assert!(
        frame.instances[0]
            .input
            .as_ref()
            .and_then(|i| i.value())
            .is_none(),
        "transient query data must not enter definition storage"
    );
    restored
        .session
        .view_mount(detail, target, wes_engine::views::MountAction::Open)
        .await
        .unwrap();
    assert_eq!(
        calls.lock().unwrap().len(),
        count,
        "restore/open must wait for Apply"
    );
    drop(events);
    fixture.close().await;
}

struct OwnedFeed {
    opened:
        tokio::sync::mpsc::UnboundedSender<(wes_engine::streams::StreamSink, CancellationToken)>,
    calls: Arc<AtomicUsize>,
}
impl wes_engine::streams::StreamingInvoker for OwnedFeed {
    fn subscribe(
        &self,
        _: Call,
        sink: wes_engine::streams::StreamSink,
        token: CancellationToken,
    ) -> wes_engine::streams::StreamFuture {
        self.calls.fetch_add(1, Ordering::SeqCst);
        sink.opened().unwrap();
        self.opened.send((sink, token.clone())).unwrap();
        Box::pin(async move {
            token.cancelled().await;
            Ok(())
        })
    }
}
#[tokio::test]
async fn owned_live_queries_keep_idle_streams_open_bound_frames_and_join_on_last_mount_close() {
    use wes_core::{Data, Primitive, Provenance};
    let calls = Arc::new(AtomicUsize::new(0));
    let invoked = calls.clone();
    let (opened, mut arrivals) = tokio::sync::mpsc::unbounded_channel();
    let fixture = Fixture::configured(
        |_| wes::web::Services::default(),
        Arc::new(move |w| {
            let types = w.prepare_type_package(
                include_str!("../../../../views/metric/types.yaml"),
                wes_language::Span::at(0),
            )?;
            w.commit(types)?;
            let mut cap =
                Capability::new(["watch"], Shape::Primitive(Primitive::Int), Safety::Safe);
            cap.streaming = true;
            cap.parameters.push(Parameter::new(
                "range",
                Shape::Option(Box::new(Shape::Primitive(Primitive::Interval))),
                true,
            ));
            w.register_provider_ports(
                ProviderDescription::new("metrics", [cap], vec![]).unwrap(),
                Arc::new(Echo(Arc::new(AtomicUsize::new(0)))),
                Some(Arc::new(OwnedFeed {
                    opened: opened.clone(),
                    calls: invoked.clone(),
                })),
            )?;
            Ok(())
        }),
    )
    .await;
    let mut events = fixture.stream().await;
    let generation = events.generation().await;
    for (i, source) in include_str!("../../../../examples/view-instances/live-query.wes")
        .lines()
        .filter(|line| !line.trim().is_empty() && !line.trim_start().starts_with("//"))
        .enumerate()
    {
        assert_eq!(
            fixture
                .source(&generation, &format!("live-setup-{i}"), source)
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
    assert_eq!(calls.load(Ordering::SeqCst), 0);
    let current = fixture.app.current().unwrap();
    let snapshot = current.session.snapshot().await.unwrap();
    let chart = snapshot.names["chart"].node.clone();
    let detail = snapshot.names["detail"].node.clone();
    let source_identity = current
        .session
        .view_frame(chart.clone())
        .await
        .unwrap()
        .instances[0]
        .identity
        .to_string();
    let target = current
        .session
        .view_frame(detail.clone())
        .await
        .unwrap()
        .instances[0]
        .identity
        .to_string();
    let edit = json!({"owner":chart.as_str(),"identity":source_identity,"definitionRevision":"0","revision":"0","fields":{"viewport":{"start":"2030-01-01T00:00:00Z","end":"2030-01-01T01:00:00Z"},"selection":null,"selectedItem":null},"outputs":{"selection":null,"selectedItem":null}});
    assert_eq!(
        fixture
            .client
            .put(fixture.url(&format!("/view-interaction/{chart}/{source_identity}")))
            .header("X-Wes-Session", &generation)
            .header("Content-Type", "application/json")
            .body(edit.to_string())
            .send()
            .await
            .unwrap()
            .status(),
        200
    );
    let first = current
        .session
        .view_mount(
            detail.clone(),
            target.clone(),
            wes_engine::views::MountAction::Open,
        )
        .await
        .unwrap()
        .unwrap();
    let second = current
        .session
        .view_mount(
            detail.clone(),
            target.clone(),
            wes_engine::views::MountAction::Open,
        )
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        fixture
            .source(&generation, "start-live", ":view apply $detail")
            .await,
        202
    );
    let (sink, token) = tokio::time::timeout(Duration::from_secs(3), arrivals.recv())
        .await
        .unwrap()
        .unwrap();
    for n in 0..5000 {
        sink.send(
            wes_core::Value::new(
                Shape::Primitive(Primitive::Int),
                Data::Int(n),
                Provenance::default(),
            )
            .unwrap(),
        )
        .await
        .unwrap();
    }
    tokio::time::timeout(Duration::from_secs(3),async{loop {
        let frame=current.session.view_frame(detail.clone()).await.unwrap();
        if let Some(value)=frame.instances[0].input.as_ref().and_then(|i|i.value()) {
            if matches!(value.data(),Data::Record(fields) if fields.get("value")==Some(&Data::Int(4999))) {
                assert!(value.provenance().cautions().iter().any(|s|s.contains("omitted")),"window loss must remain visible after adaptation");
                assert!(frame.instances[0].query_running);
                for key in ["view.query.origin","view.query.inputDigest","view.query.runs","view.query.templateRevision"] { assert!(value.provenance().fact(key).is_some_and(|s|!s.is_empty()),"missing {key}"); }
                break;
            }
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }}).await.unwrap();
    let response = fixture
        .client
        .get(fixture.url(&format!("/view-instances/{detail}/{target}")))
        .header("X-Wes-Session", &generation)
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 200);
    let bytes = response.bytes().await.unwrap();
    assert!(bytes.len() < 4096);
    let json: Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(json["instances"][0]["query"]["mode"], "live");
    assert!(
        !json["instances"][0]["inputCautions"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    current
        .session
        .view_mount(
            detail.clone(),
            target.clone(),
            wes_engine::views::MountAction::Close(first),
        )
        .await
        .unwrap();
    assert!(!token.is_cancelled());
    let stopped = fixture
        .client
        .post(fixture.url(&format!("/view-mounts/{detail}/{target}")))
        .header("X-Wes-Session", &generation)
        .header("Content-Type", "application/json")
        .body(json!({"action":"close","token":second}).to_string())
        .send()
        .await
        .unwrap();
    assert_eq!(stopped.status(), 200);
    tokio::time::timeout(Duration::from_secs(1), token.cancelled())
        .await
        .unwrap();
    tokio::time::timeout(Duration::from_secs(2), async {
        while current
            .session
            .view_frame(detail.clone())
            .await
            .unwrap()
            .instances[0]
            .query_running
        {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    current
        .session
        .view_mount(detail, target, wes_engine::views::MountAction::Open)
        .await
        .unwrap();
    assert_eq!(
        calls.load(Ordering::SeqCst),
        1,
        "reopening does not restart a stream"
    );
    for kind in ["cancel", "cancel-work"] {
        let cell = format!("owned-{kind}");
        assert_eq!(
            fixture
                .source(&generation, &cell, ":view apply $detail > ownedApply")
                .await,
            202
        );
        let (_sink, token) = tokio::time::timeout(Duration::from_secs(3), arrivals.recv())
            .await
            .unwrap()
            .unwrap();
        current.session.wait_idle().await.unwrap();
        let node = current.session.snapshot().await.unwrap().names["ownedApply"]
            .node
            .clone();
        let command = if kind == "cancel" {
            json!({"request":kind,"node":node.as_str()})
        } else {
            json!({"request":kind,"origin":cell})
        };
        assert!(
            fixture
                .post(&generation, command)
                .await
                .status()
                .is_success()
        );
        tokio::time::timeout(Duration::from_secs(1), token.cancelled())
            .await
            .unwrap();
    }
    assert_eq!(
        fixture
            .source(
                &generation,
                "save-live-query",
                ":workspace save \"live-query-board\""
            )
            .await,
        202
    );
    current.session.wait_idle().await.unwrap();
    assert_eq!(
        fixture
            .source(
                &generation,
                "load-live-query",
                ":workspace load \"live-query-board\""
            )
            .await,
        202
    );
    let replacement = events.generation().await;
    assert_ne!(replacement, generation);
    let restored = fixture.app.current().unwrap();
    restored.session.wait_idle().await.unwrap();
    let state = restored.session.snapshot().await.unwrap();
    let frame = restored
        .session
        .view_frame(state.names["detail"].node.clone())
        .await
        .unwrap();
    let saved = &frame.instances[0];
    assert_eq!(
        saved
            .query
            .as_ref()
            .unwrap()
            .adapter
            .as_ref()
            .unwrap()
            .template,
        "Draw"
    );
    assert!(!saved.observing && !saved.query_running);
    assert!(saved.input.as_ref().and_then(|i| i.value()).is_none());
    assert_eq!(
        calls.load(Ordering::SeqCst),
        3,
        "restoring a live binding never restarts its provider"
    );
    drop(events);
    fixture.close().await;
}

#[tokio::test]
async fn event_outputs_are_atomic_ordered_typed_snapshots_and_are_never_replayed_on_restore() {
    let fixture = Fixture::new().await;
    let mut events = fixture.stream().await;
    let generation = events.generation().await;
    assert_eq!(
        fixture
            .source(&generation, "event-chart", ":view create Timeline > chart")
            .await,
        202
    );
    let current = fixture.app.current().unwrap();
    current.session.wait_idle().await.unwrap();
    let node = current.session.snapshot().await.unwrap().names["chart"]
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
    let picked = json!({"source":"requests","series":"latency","id":"sample-1","at":"2030-01-01T00:00:10Z","value":"42"});
    let edit = json!({"owner":node.as_str(),"identity":identity,"definitionRevision":"0","revision":"0","fields":{"viewport":{"start":"2030-01-01T00:00:00Z","end":"2030-01-01T01:00:00Z"},"selection":null,"selectedItem":null},"outputs":{"selection":null,"selectedItem":null},"events":[{"port":"picked","value":picked},{"port":"picked","value":picked}]});
    let url = fixture.url(&format!("/view-interaction/{node}/{identity}"));
    let send = |value: Value| {
        fixture
            .client
            .put(&url)
            .header("X-Wes-Session", &generation)
            .header("Content-Type", "application/json")
            .body(value.to_string())
    };
    assert_eq!(send(edit.clone()).send().await.unwrap().status(), 200);
    assert_eq!(
        send(edit.clone()).send().await.unwrap().status(),
        409,
        "duplicate CAS must not append again"
    );
    let mut invalid = edit;
    invalid["revision"] = "1".into();
    invalid["events"][1]["value"] = json!("wrong");
    let rejected = send(invalid).send().await.unwrap();
    assert_eq!(rejected.status(), 409);
    let rejected: Value = serde_json::from_slice(&rejected.bytes().await.unwrap()).unwrap();
    assert!(rejected["problem"].as_str().is_some());
    assert_eq!(rejected["revision"], "1");
    assert_eq!(
        fixture
            .source(
                &generation,
                "read-events",
                ":view output $chart port:picked > pickedEvents"
            )
            .await,
        202
    );
    current.session.wait_idle().await.unwrap();
    let state = current.session.snapshot().await.unwrap();
    let value = &state.execution.values[&state.names["pickedEvents"].node];
    assert!(matches!(value.shape(), wes_core::Shape::List(_)));
    let wes_core::Data::List(items) = value.data() else {
        panic!("event snapshot")
    };
    assert_eq!(items.len(), 2);
    assert_eq!(items[0], items[1]);
    assert_eq!(
        fixture
            .source(
                &generation,
                "save-event-window",
                ":workspace save \"event-window\""
            )
            .await,
        202
    );
    current.session.wait_idle().await.unwrap();
    assert_eq!(
        fixture
            .source(
                &generation,
                "load-event-window",
                ":workspace load \"event-window\""
            )
            .await,
        202
    );
    let replacement = events.generation().await;
    assert_eq!(
        fixture
            .source(
                &replacement,
                "read-restored-events",
                ":view output $chart port:picked > restoredEvents"
            )
            .await,
        202
    );
    let restored = fixture.app.current().unwrap();
    restored.session.wait_idle().await.unwrap();
    let state = restored.session.snapshot().await.unwrap();
    assert_eq!(
        state.execution.values[&state.names["restoredEvents"].node].data(),
        &wes_core::Data::List(vec![])
    );
    assert_eq!(fixture.calls.load(Ordering::SeqCst), 0);
    drop(events);
    fixture.close().await;
}

#[tokio::test]
async fn evidence_is_a_frozen_typed_result_with_overlays_missing_inputs_and_normal_retention() {
    let fixture = Fixture::new().await;
    let mut events = fixture.stream().await;
    let generation = events.generation().await;
    install_range_summary(&fixture, &generation).await;
    for (i, command) in [
        ":calc { return {view:\"dashboard\",title:\"Review\"}; } > layout",
        ":view create Dashboard input:$layout > board",
        ":view create Timeline > chart",
        ":view connect $chart to:$board",
        ":calc { return {view:\"range-summary\",title:\"Selected\",selection:none}; } > detailData",
        ":view create RangeSummary input:$detailData > detail",
        ":view link $chart output:selection to:$detail field:selection",
        ":view connect $detail to:$board",
    ]
    .iter()
    .enumerate()
    {
        assert_eq!(
            fixture
                .source(&generation, &format!("setup-{i}"), command)
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
    let state = current.session.snapshot().await.unwrap();
    let chart = state.names["chart"].node.clone();
    let detail = state.names["detail"].node.clone();
    let frame = current.session.view_frame(chart.clone()).await.unwrap();
    let identity = frame.instances[0].identity.clone();
    let interval = "2030-01-01T00:10:00Z/2030-01-01T00:20:00Z";
    let edit = json!({"owner":chart.as_str(),"identity":identity.as_ref(),"definitionRevision":"0","revision":"0",
        "fields":{"viewport":{"start":"2030-01-01T00:00:00Z","end":"2030-01-01T01:00:00Z"},"selection":{"start":"2030-01-01T00:10:00Z","end":"2030-01-01T00:20:00Z"},"selectedItem":null},"outputs":{"selection":interval,"selectedItem":null}});
    let url = fixture.url(&format!("/view-interaction/{chart}/{identity}"));
    assert_eq!(
        fixture
            .client
            .put(&url)
            .header("X-Wes-Session", &generation)
            .header("Content-Type", "application/json")
            .body(edit.to_string())
            .send()
            .await
            .unwrap()
            .status(),
        200
    );
    assert_eq!(
        fixture
            .source(&generation, "capture", ":view capture $board > evidence")
            .await,
        202
    );
    current.session.wait_idle().await.unwrap();
    async fn result(fixture: &Fixture, name: &str) -> (String, Value) {
        let observed = fixture
            .app
            .current()
            .unwrap()
            .session
            .observe()
            .await
            .unwrap();
        let node = &observed.state.names[name].node;
        let handle = observed.values.as_ref().unwrap().outputs[node]
            .handle()
            .expect("ready evidence")
            .to_string();
        let value: Value = serde_json::from_str(
            &fixture
                .client
                .get(fixture.url(&format!("/values/{handle}")))
                .send()
                .await
                .unwrap()
                .text()
                .await
                .unwrap(),
        )
        .unwrap();
        (handle, value)
    }
    let (handle, evidence) = result(&fixture, "evidence").await;
    assert_eq!(evidence["type"]["name"], "wes.ViewEvidence");
    assert_eq!(evidence["data"]["inputsComplete"], false);
    let entries = &evidence["data"]["entries"];
    assert!(
        entries[chart.as_str()]["warnings"]
            .to_string()
            .contains("Input unavailable")
    );
    assert_eq!(
        entries[detail.as_str()]["input"]["selection"]["value"],
        interval
    );
    assert_eq!(
        entries[chart.as_str()]["outputs"]["selection"]["value"],
        interval
    );
    assert!(
        entries[detail.as_str()]["source"]["run"]
            .as_str()
            .is_some_and(|s| !s.is_empty())
    );
    assert!(
        evidence["data"]["retention"]
            .as_str()
            .unwrap()
            .contains("Keep/Pin")
    );
    assert_eq!(
        fixture
            .post(&generation, json!({"request":"keep","handle":handle}))
            .await
            .status(),
        202
    );
    let mut next = edit;
    next["revision"] = json!("1");
    next["fields"]["selection"] = Value::Null;
    next["outputs"]["selection"] = Value::Null;
    assert_eq!(
        fixture
            .client
            .put(&url)
            .header("X-Wes-Session", &generation)
            .header("Content-Type", "application/json")
            .body(next.to_string())
            .send()
            .await
            .unwrap()
            .status(),
        200
    );
    assert_eq!(
        result(&fixture, "evidence").await.1["data"],
        evidence["data"],
        "new selection cannot rewrite evidence"
    );
    assert_eq!(
        fixture
            .source(&generation, "save", ":workspace save \"evidence-copy\"")
            .await,
        202
    );
    current.session.wait_idle().await.unwrap();
    assert_eq!(
        fixture
            .source(&generation, "load", ":workspace load \"evidence-copy\"")
            .await,
        202
    );
    let replacement = events.generation().await;
    assert_ne!(replacement, generation);
    fixture
        .app
        .current()
        .unwrap()
        .session
        .wait_idle()
        .await
        .unwrap();
    assert_eq!(
        result(&fixture, "evidence").await.1["data"],
        evidence["data"]
    );
    assert_eq!(fixture.calls.load(Ordering::SeqCst), 0);
    drop(events);
    fixture.close().await;
}

async fn install_range_summary(fixture: &Fixture, generation: &str) {
    let manifest =
        include_str!("../../../../examples/view-packages/source/range-summary/view.json");
    let types = include_str!("../../../../examples/view-packages/source/range-summary/types.yaml");
    let package = wes_views::Package::parse(manifest, types).unwrap();
    let artifact=json!({"format":1,"sdk":wes_views::sdk_version(),"manifest":manifest,"types":types,"definition":package.digest,"javascript":"throw new Error('synthetic renderer is never evaluated by the backend');","css":""}).to_string();
    assert_eq!(
        fixture
            .source(
                generation,
                "range-summary-package",
                &format!(
                    r#":package load source:{} origin:"range-summary.wes-view.json""#,
                    json!(artifact)
                )
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

#[tokio::test]
async fn evidence_slots_are_independent_bounded_and_restricted_members_are_unavailable() {
    struct Evidence;
    impl Invoker for Evidence {
        fn invoke(&self, call: Call, _: CancellationToken) -> InvocationFuture {
            let restricted = matches!(
                call.arguments.get("restricted").map(|v| v.data()),
                Some(wes_core::Data::Bool(true))
            );
            Box::pin(async move {
                let value = wes_core::Value::new(
                    wes_views::named("Metric").unwrap().input().shape(),
                    wes_core::Data::Record(
                        [
                            ("view".into(), wes_core::Data::Text("metric".into())),
                            (
                                "value".into(),
                                wes_core::Data::Int(if restricted { 987654321 } else { 42 }),
                            ),
                        ]
                        .into(),
                    ),
                    wes_core::Provenance::default(),
                )
                .unwrap();
                Ok(if restricted {
                    value.with_provenance(
                        wes_core::Provenance::default().with_policy(
                            &wes_core::flow::FlowPolicy::default()
                                .confidential(wes_core::flow::Residence::Retainable),
                        ),
                    )
                } else {
                    value
                })
            })
        }
    }
    let f = Fixture::configured(
        |_| wes::web::Services::default(),
        Arc::new(|w| {
            w.register_provider(
                ProviderDescription::new(
                    "evidence",
                    [{
                        let mut capability = Capability::new(
                            ["sample"],
                            wes_views::named("Metric").unwrap().input().shape(),
                            Safety::Safe,
                        );
                        capability.parameters = vec![wes_core::capability::Parameter::new(
                            "restricted",
                            wes_core::Shape::Primitive(wes_core::Primitive::Bool),
                            true,
                        )];
                        capability
                    }],
                    vec![],
                )
                .unwrap(),
                Arc::new(Evidence),
            )?;
            Ok(())
        }),
    )
    .await;
    let mut events = f.stream().await;
    let generation = events.generation().await;
    for (i, source) in [
        ":view create Dashboard > board",
        "evidence sample restricted:false > first",
        ":view create Metric input:$first > card",
        ":view connect $card to:$board",
        "evidence sample restricted:true > second",
        ":view create Metric input:$second > hidden",
        ":view connect $hidden to:$board",
        ":view create Timeline > empty",
        ":view connect $empty to:$board",
    ]
    .iter()
    .enumerate()
    {
        assert_eq!(
            f.source(&generation, &format!("evidence-{i}"), source)
                .await,
            202
        );
        f.app.current().unwrap().session.wait_idle().await.unwrap();
    }
    let current = f.app.current().unwrap();
    let state = current.session.snapshot().await.unwrap();
    let root = state.names["board"].node.clone();
    let frame = current.session.view_frame(root.clone()).await.unwrap();
    let owner = &frame.instances[0];
    let hidden = frame
        .instances
        .iter()
        .find(|v| v.id == state.names["hidden"].node)
        .unwrap();
    assert!(hidden.input.is_none());
    assert!(current.session.view_frame(hidden.id.clone()).await.is_err());
    let read = |slot: &str, index: usize, epoch: &str| {
        f.client
            .get(f.url(&format!(
                "/view-evidence/{root}/{}/{root}/{slot}/{index}",
                owner.identity
            )))
            .header("X-Wes-Session", &generation)
            .header("X-Wes-View-Epoch", epoch)
            .header("X-Wes-View-Revision", owner.revision.to_string())
            .header("X-Wes-Input-Revision", owner.input_revision.to_string())
            .send()
    };
    let mut available = 0;
    for index in 0..3 {
        let reply = read("members", index, &frame.authority_epoch)
            .await
            .unwrap();
        assert_eq!(reply.status(), 200);
        let text = reply.text().await.unwrap();
        assert!(!text.contains("987654321"));
        let input: Value = serde_json::from_str(&text).unwrap();
        if input["available"] == true {
            available += 1;
            assert!(input["input"]["type"].is_object());
        } else {
            assert!(input["input"].is_null());
            assert_eq!(input["complete"], false);
        }
    }
    assert_eq!(available, 1);
    assert_eq!(
        read("other", 0, &frame.authority_epoch)
            .await
            .unwrap()
            .status(),
        403
    );
    assert_eq!(read("members", 0, "old-realm").await.unwrap().status(), 409);
    assert_eq!(
        read("members", 32, &frame.authority_epoch)
            .await
            .unwrap()
            .status(),
        400
    );
    let raw = f
        .client
        .get(f.url(&format!("/view-instances/{root}/{}", owner.identity)))
        .header("X-Wes-Session", &generation)
        .send()
        .await
        .unwrap()
        .text()
        .await
        .unwrap();
    assert!(!raw.contains("987654321"));
    assert!(raw.contains(&frame.authority_epoch));
    assert_eq!(
        f.source(
            &generation,
            "capture-partial",
            ":view capture $board > collected"
        )
        .await,
        202
    );
    current.session.wait_idle().await.unwrap();
    let state = current.session.snapshot().await.unwrap();
    assert!(
        state
            .execution
            .values
            .contains_key(&state.names["collected"].node)
    );
    f.close().await;
}
