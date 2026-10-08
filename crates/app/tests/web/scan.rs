use super::*;
use tokio::sync::mpsc;

mod recording_gate {
    use wes_core::DatasetRef;
    use wes_engine::storage::{StoreError, ValueHandle, datasets::*};
    pub struct Gate {
        pub entered: tokio::sync::oneshot::Sender<()>,
        pub release: std::sync::mpsc::Receiver<()>,
    }
    pub struct Storage {
        pub inner: wes_adapters::datasets::DatasetStore,
        pub gate: Option<Gate>,
    }
    impl DatasetStorage for Storage {
        fn read_access(&self) -> Result<std::collections::BTreeSet<String>, StoreError> {
            self.inner.read_access()
        }
        fn check_read_origins(
            &self,
            origins: &std::collections::BTreeSet<wes_core::flow::DatasetReadOrigin>,
        ) -> Result<(), StoreError> {
            self.inner.check_read_origins(origins)
        }
        fn value_root(&self, h: &ValueHandle) -> Result<Option<DatasetRootRequest>, StoreError> {
            self.inner.value_root(h)
        }
        fn protect_workspace(
            &mut self,
            generation: &str,
            handles: &[ValueHandle],
        ) -> Result<(), StoreError> {
            self.inner.protect_workspace(generation, handles)
        }
        fn retire_workspace(&mut self, generation: &str) -> Result<(), StoreError> {
            self.inner.retire_workspace(generation)
        }
        fn protects_value(&self, h: &ValueHandle) -> Result<bool, StoreError> {
            self.inner.protects_value(h)
        }
        fn changes(&self) -> Option<tokio::sync::watch::Receiver<DatasetChanges>> {
            self.inner.changes()
        }
        fn read_charge(&self) -> u64 {
            self.inner.read_charge()
        }
        fn inspect(&self, r: &DatasetRef) -> Result<DatasetInfo, StoreError> {
            self.inner.inspect(r)
        }
        fn retention_size(&self, r: &DatasetRef) -> Result<u64, StoreError> {
            self.inner.retention_size(r)
        }
        fn page(&self, r: &DatasetRef, q: PageRequest) -> Result<DatasetPage, StoreError> {
            DatasetStorage::page(&self.inner, r, q)
        }
        fn set_root(&mut self, q: DatasetRootRequest) -> Result<DatasetRootReceipt, StoreError> {
            self.inner.set_root(q)
        }
        fn root_covers(
            &self,
            h: &ValueHandle,
            p: &[DatasetRef],
            r: wes_engine::storage::Retention,
        ) -> Result<bool, StoreError> {
            self.inner.root_covers(h, p, r)
        }
        fn create(&mut self, q: DatasetCreate) -> Result<DatasetWriterAdmission, StoreError> {
            let prefix = self.inner.create(q)?;
            if let Some(gate) = self.gate.take() {
                let _ = gate.entered.send(());
                gate.release
                    .recv_timeout(std::time::Duration::from_secs(10))
                    .map_err(|_| StoreError::Closed)?;
            }
            Ok(prefix)
        }
        fn append(&mut self, q: DatasetAppend) -> Result<DatasetRef, StoreError> {
            self.inner.append(q)
        }
        fn reconcile_owned(
            &mut self,
            owner: &wes_engine::storage::datasets::DatasetWriteSelection,
        ) -> Result<
            wes_engine::storage::datasets::DatasetReconciliation,
            wes_engine::storage::StoreError,
        > {
            wes_engine::storage::datasets::DatasetStorage::reconcile_owned(&mut self.inner, owner)
        }
    }
}

async fn recording_launch_fixture_gated(
    durable: bool,
    gate: Option<recording_gate::Gate>,
) -> (
    Fixture,
    Arc<AtomicUsize>,
    mpsc::UnboundedReceiver<(wes_engine::streams::StreamSink, CancellationToken)>,
) {
    recording_launch_fixture_inner(durable, gate).await
}

async fn recording_launch_fixture(
    durable: bool,
) -> (
    Fixture,
    Arc<AtomicUsize>,
    mpsc::UnboundedReceiver<(wes_engine::streams::StreamSink, CancellationToken)>,
) {
    recording_launch_fixture_inner(durable, None).await
}

async fn recording_launch_fixture_inner(
    durable: bool,
    gate: Option<recording_gate::Gate>,
) -> (
    Fixture,
    Arc<AtomicUsize>,
    mpsc::UnboundedReceiver<(wes_engine::streams::StreamSink, CancellationToken)>,
) {
    use wes_engine::streams::{StreamFuture, StreamSink, StreamingInvoker};
    struct Immediate {
        calls: Arc<AtomicUsize>,
        entered: mpsc::UnboundedSender<(StreamSink, CancellationToken)>,
    }
    impl StreamingInvoker for Immediate {
        fn subscribe(&self, _: Call, sink: StreamSink, token: CancellationToken) -> StreamFuture {
            self.calls.fetch_add(1, Ordering::SeqCst);
            sink.push(
                wes_core::Value::new(
                    Shape::Primitive(wes_core::Primitive::Int),
                    wes_core::Data::Int(7),
                    Default::default(),
                )
                .unwrap(),
            )
            .unwrap();
            sink.opened().unwrap();
            self.entered.send((sink, token.clone())).unwrap();
            Box::pin(async move {
                token.cancelled().await;
                Ok(())
            })
        }
    }
    let calls = Arc::new(AtomicUsize::new(0));
    let counted = calls.clone();
    let (entered, arrivals) = mpsc::unbounded_channel();
    let gate = Arc::new(std::sync::Mutex::new(gate));
    let fixture = Fixture::configured_owner_concurrency(
        |_| wes::web::Services::default(),
        Arc::new(move |workspace| {
            let mut capability = Capability::new(
                ["watch"],
                Shape::Primitive(wes_core::Primitive::Int),
                Safety::Safe,
            );
            capability.streaming = true;
            capability.parameters = vec![wes_core::capability::Parameter::new(
                "tag",
                Shape::Primitive(wes_core::Primitive::Text),
                false,
            )];
            workspace.register_provider_ports(
                ProviderDescription::new("events", [capability], vec![]).unwrap(),
                Arc::new(Echo(Arc::new(AtomicUsize::new(0)))),
                Some(Arc::new(Immediate {
                    calls: counted.clone(),
                    entered: entered.clone(),
                })),
            )?;
            Ok(())
        }),
        move |root, values| {
            if !durable {
                return wes_engine::storage::spawn_store(values, StoreWorkerLimits::default())
                    .unwrap();
            }
            let datasets = wes_adapters::datasets::DatasetStore::open(
                &root.join("datasets"),
                Durability::File,
                Default::default(),
            )
            .unwrap();
            let Some(gate) = gate.lock().unwrap().take() else {
                return wes_engine::storage::spawn_storage(
                    values,
                    datasets,
                    StoreWorkerLimits::default(),
                )
                .unwrap();
            };
            wes_engine::storage::spawn_storage(
                values,
                recording_gate::Storage {
                    inner: datasets,
                    gate: Some(gate),
                },
                StoreWorkerLimits::default(),
            )
            .unwrap()
        },
        NonZeroUsize::new(1).unwrap(),
    )
    .await;
    (fixture, calls, arrivals)
}

#[tokio::test]
async fn native_recording_setup_and_launch_capture_the_first_event_without_starting_on_read() {
    for launch in [false, true] {
        let (fixture, calls, mut arrivals) = recording_launch_fixture(true).await;
        let mut events = fixture.stream().await;
        let generation = events.generation().await;
        let session = fixture.app.current().unwrap().session;
        if launch {
            assert_eq!(
                fixture
                    .source(
                        &generation,
                        "launch",
                        "events watch > logs | :dataset record budget:Capture > recording"
                    )
                    .await,
                202
            );
        } else {
            assert_eq!(
                fixture
                    .source(&generation, "held", "@hold events watch > logs")
                    .await,
                202
            );
            session.wait_idle().await.unwrap();
            assert_eq!(calls.load(Ordering::SeqCst), 0);
            assert_eq!(
                fixture
                    .source(
                        &generation,
                        "setup",
                        ":dataset record source:$logs from:start budget:Capture > recording"
                    )
                    .await,
                202
            );
            tokio::time::timeout(Duration::from_secs(5), session.wait_idle())
                .await
                .unwrap()
                .unwrap();
            let snapshot = session.snapshot().await.unwrap();
            let node = &snapshot.names["recording"].node;
            let wes_core::Data::Record(setup) = snapshot.execution.values[node].data() else {
                panic!("setup receipt");
            };
            assert_eq!(setup["phase"], wes_core::Data::Text("prepared".into()));
            assert!(!setup.contains_key("dataset"));
            assert!(!setup.contains_key("sourceRun"));
            let control = snapshot
                .execution
                .graph
                .node(node)
                .unwrap()
                .payload()
                .recording_control(snapshot.execution.runs[node].as_str())
                .unwrap();
            assert!(
                control.active
                    && control.status_available
                    && control.discard_available
                    && !control.stop_available
            );
            assert_eq!(
                fixture
                    .source(
                        &generation,
                        "setup-status",
                        ":dataset recording-status $recording > setupStatus"
                    )
                    .await,
                202
            );
            session.wait_idle().await.unwrap();
            assert_eq!(calls.load(Ordering::SeqCst), 0);
            assert!(arrivals.try_recv().is_err());
            assert_eq!(
                fixture
                    .source(&generation, "refresh", ":refresh $logs")
                    .await,
                202
            );
        }
        let (sink, source_cancel) = tokio::time::timeout(Duration::from_secs(5), arrivals.recv())
            .await
            .unwrap()
            .unwrap();
        tokio::time::timeout(Duration::from_secs(5), session.wait_idle())
            .await
            .unwrap()
            .unwrap();
        let snapshot = session.snapshot().await.unwrap();
        let node = snapshot.names["recording"].node.clone();
        let run = snapshot.execution.runs[&node].clone();
        let control = snapshot
            .execution
            .graph
            .node(&node)
            .unwrap()
            .payload()
            .recording_control(run.as_str())
            .unwrap();
        assert!(control.active && control.stop_available && !control.discard_available);
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        sink.push(
            wes_core::Value::new(
                Shape::Primitive(wes_core::Primitive::Int),
                wes_core::Data::Int(8),
                Default::default(),
            )
            .unwrap(),
        )
        .unwrap();
        assert_eq!(
            fixture
                .source(
                    &generation,
                    "discard-attached",
                    ":dataset discard $recording > invalidDiscard"
                )
                .await,
            202
        );
        session.wait_idle().await.unwrap();
        let refused = session.snapshot().await.unwrap();
        assert_eq!(
            refused
                .execution
                .graph
                .node(&refused.names["invalidDiscard"].node)
                .unwrap()
                .state(),
            wes_engine::graph::NodeState::Failed
        );
        assert_eq!(
            fixture
                .source(&generation, "stop", ":dataset stop $recording > stopped")
                .await,
            202
        );
        tokio::time::timeout(Duration::from_secs(5), session.wait_idle())
            .await
            .unwrap()
            .unwrap();
        let snapshot = session.snapshot().await.unwrap();
        assert_eq!(snapshot.execution.runs[&node], run);
        let wes_core::Data::Record(stopped) =
            snapshot.execution.values[&snapshot.names["stopped"].node].data()
        else {
            panic!("stopped receipt");
        };
        let wes_core::Data::Dataset(reference) = &stopped["dataset"] else {
            panic!("dataset");
        };
        let info = fixture
            .worker
            .dataset_inspect(reference.as_ref().clone())
            .await
            .unwrap();
        assert_eq!(info.recording.unwrap().first, 1);
        let page = fixture
            .worker
            .dataset_page(
                reference.as_ref().clone(),
                wes_engine::storage::datasets::PageRequest {
                    work: None,
                    from: 0,
                    rows: 100,
                    bytes: 65536,
                    segments: 8,
                },
            )
            .await
            .unwrap();
        assert_eq!(
            page.rows
                .iter()
                .map(|row| row.value.data().clone())
                .collect::<Vec<_>>(),
            [wes_core::Data::Int(7), wes_core::Data::Int(8)]
        );
        assert!(
            !source_cancel.is_cancelled(),
            "Stop recording does not cancel its source"
        );
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        if launch {
            assert_eq!(
                fixture
                    .source(&generation, "changed-launch", ":change $logs tag:changed")
                    .await,
                202
            );
            session.wait_idle().await.unwrap();
            let changed = session.snapshot().await.unwrap();
            assert!(matches!(
                changed
                    .execution
                    .graph
                    .node(&changed.names["logs"].node)
                    .unwrap()
                    .payload(),
                wes_engine::tasks::BoundTask::SourceLaunch(_)
            ));
            assert_eq!(
                fixture
                    .source(&generation, "refresh-consumed", ":refresh $logs")
                    .await,
                202
            );
            tokio::time::timeout(Duration::from_secs(5), session.wait_idle())
                .await
                .unwrap()
                .unwrap();
            assert_eq!(
                calls.load(Ordering::SeqCst),
                1,
                "consumed setup cannot replay a producer"
            );
        }
        fixture.close().await;
    }
}

#[tokio::test]
async fn native_recording_discard_releases_unused_setup_and_copies_grant_no_controls() {
    let (fixture, calls, mut arrivals) = recording_launch_fixture(true).await;
    let mut events = fixture.stream().await;
    let generation = events.generation().await;
    let session = fixture.app.current().unwrap().session;
    assert_eq!(
        fixture
            .source(
                &generation,
                "prepare",
                "@hold events watch > logs\n:dataset record source:$logs from:start > recording"
            )
            .await,
        202
    );
    tokio::time::timeout(Duration::from_secs(5), session.wait_idle())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        fixture
            .source(
                &generation,
                "copy",
                ":calc { return $recording; } > copy\n:dataset discard $copy > copiedControl"
            )
            .await,
        202
    );
    session.wait_idle().await.unwrap();
    let snapshot = session.snapshot().await.unwrap();
    assert_eq!(
        snapshot
            .execution
            .graph
            .node(&snapshot.names["copiedControl"].node)
            .unwrap()
            .state(),
        wes_engine::graph::NodeState::Failed
    );
    assert_eq!(calls.load(Ordering::SeqCst), 0);
    assert_eq!(
        fixture
            .source(
                &generation,
                "discard",
                ":dataset discard $recording > discarded"
            )
            .await,
        202
    );
    tokio::time::timeout(Duration::from_secs(5), session.wait_idle())
        .await
        .unwrap()
        .unwrap();
    let snapshot = session.snapshot().await.unwrap();
    for name in ["recording", "discarded"] {
        let wes_core::Data::Record(value) =
            snapshot.execution.values[&snapshot.names[name].node].data()
        else {
            panic!("terminal setup");
        };
        assert_eq!(value["phase"], wes_core::Data::Text("discarded".into()));
        assert!(!value.contains_key("dataset"));
    }
    assert!(arrivals.try_recv().is_err());
    assert_eq!(calls.load(Ordering::SeqCst), 0);
    assert_eq!(
        fixture
            .source(
                &generation,
                "retry-explicit",
                ":dataset record source:$logs from:start > fresh"
            )
            .await,
        202
    );
    session.wait_idle().await.unwrap();
    assert_eq!(
        fixture
            .source(&generation, "fresh-refresh", ":refresh $logs")
            .await,
        202
    );
    let _ = tokio::time::timeout(Duration::from_secs(5), arrivals.recv())
        .await
        .unwrap()
        .unwrap();
    session.wait_idle().await.unwrap();
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    fixture.close().await;
}

#[tokio::test]
async fn native_recording_launch_preparation_failure_issues_zero_provider_calls() {
    let (fixture, calls, mut arrivals) = recording_launch_fixture(false).await;
    let mut events = fixture.stream().await;
    let generation = events.generation().await;
    let session = fixture.app.current().unwrap().session;
    assert_eq!(
        fixture
            .source(
                &generation,
                "unavailable-writer",
                "events watch > logs | :dataset record > recording"
            )
            .await,
        202
    );
    tokio::time::timeout(Duration::from_secs(5), session.wait_idle())
        .await
        .unwrap()
        .unwrap();
    let snapshot = session.snapshot().await.unwrap();
    for name in ["logs", "recording"] {
        assert_eq!(
            snapshot
                .execution
                .graph
                .node(&snapshot.names[name].node)
                .unwrap()
                .state(),
            wes_engine::graph::NodeState::Failed
        );
    }
    assert!(arrivals.try_recv().is_err());
    assert_eq!(calls.load(Ordering::SeqCst), 0);
    assert_eq!(
        fixture
            .source(
                &generation,
                "failed-status",
                ":dataset recording-status $recording > status"
            )
            .await,
        202
    );
    session.wait_idle().await.unwrap();
    assert_eq!(
        calls.load(Ordering::SeqCst),
        0,
        "reading a failed setup cannot retry an external source"
    );
    fixture.close().await;
}
#[tokio::test]
async fn native_recording_refused_producer_joins_prepared_storage_without_a_ready_dataset() {
    let (entered, receive) = tokio::sync::oneshot::channel();
    let (release, proceed) = std::sync::mpsc::channel();
    let (fixture, calls, mut arrivals) = recording_launch_fixture_gated(
        true,
        Some(recording_gate::Gate {
            entered,
            release: proceed,
        }),
    )
    .await;
    let mut events = fixture.stream().await;
    let generation = events.generation().await;
    let session = fixture.app.current().unwrap().session;
    assert_eq!(
        fixture
            .source(
                &generation,
                "launch",
                "events watch > logs | :dataset record > recording"
            )
            .await,
        202
    );
    tokio::time::timeout(Duration::from_secs(5), receive)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        fixture
            .source(&generation, "cancel-before-dispatch", ":cancel $logs")
            .await,
        202
    );
    release.send(()).unwrap();
    let snapshot = session.snapshot().await.unwrap();
    let recording = snapshot.names["recording"].node.clone();
    loop {
        let event = events.next().await;
        if event["node"] == recording.as_str() {
            // A browser projection may coalesce the transient ready state with failure.
            // Check the actual immutable value, rather than counting intermediate wakeups.
            if event["event"] == "ready" || event["event"] == "evidence" {
                let handle = event["handle"].as_str().expect("published setup handle");
                let response = fixture
                    .client
                    .get(fixture.url(&format!("/values/{handle}")))
                    .send()
                    .await
                    .unwrap()
                    .bytes()
                    .await
                    .unwrap();
                let value: Value = serde_json::from_slice(&response).unwrap();
                let fields = value["data"].as_object().expect("recording setup record");
                assert_eq!(
                    fields
                        .keys()
                        .map(String::as_str)
                        .collect::<std::collections::BTreeSet<_>>(),
                    ["phase", "remainingMs", "sourceNode"].into_iter().collect(),
                    "a refused writer must expose only its real setup, never an initial Dataset"
                );
            }
            if event["event"] == "evidence" && event["state"] == "failed" {
                break;
            }
        }
    }
    session.wait_idle().await.unwrap();
    let snapshot = session.snapshot().await.unwrap();
    assert_eq!(
        snapshot.execution.graph.node(&recording).unwrap().state(),
        wes_engine::graph::NodeState::Failed
    );
    assert!(
        snapshot.execution.errors[&recording]
            .message()
            .contains("refused")
    );
    assert_eq!(calls.load(Ordering::SeqCst), 0);
    assert!(arrivals.try_recv().is_err());
    fixture.close().await;
}

#[tokio::test]
async fn native_recording_fresh_setup_after_source_join_never_replays_without_explicit_refresh() {
    for launch in [false, true] {
        let (fixture, calls, mut arrivals) = recording_launch_fixture(true).await;
        let mut events = fixture.stream().await;
        let generation = events.generation().await;
        let session = fixture.app.current().unwrap().session;
        let declaration = if launch {
            "events watch > logs | :dataset record > recording"
        } else {
            "@hold events watch > logs\n:dataset record source:$logs from:start > recording"
        };
        assert_eq!(
            fixture.source(&generation, "initial", declaration).await,
            202
        );
        session.wait_idle().await.unwrap();
        if !launch {
            assert_eq!(
                fixture
                    .source(&generation, "first-start", ":refresh $logs")
                    .await,
                202
            );
        }
        tokio::time::timeout(Duration::from_secs(5), arrivals.recv())
            .await
            .unwrap()
            .unwrap();
        session.wait_idle().await.unwrap();
        assert_eq!(
            fixture
                .source(&generation, "close-first", ":cancel $logs")
                .await,
            202
        );
        assert_eq!(
            fixture
                .source(
                    &generation,
                    "join-first-recording",
                    ":dataset stop $recording > stoppedFirst"
                )
                .await,
            202
        );
        session.wait_idle().await.unwrap();
        assert_eq!(
            fixture
                .source(&generation, "second-setup", ":refresh $recording")
                .await,
            202
        );
        session.wait_idle().await.unwrap();
        assert_eq!(
            calls.load(Ordering::SeqCst),
            1,
            "preparing the next run must never dispatch its source"
        );
        assert!(arrivals.try_recv().is_err());
        let snapshot = session.snapshot().await.unwrap();
        let node = &snapshot.names["recording"].node;
        assert!(
            snapshot.execution.values.contains_key(node),
            "rearmed launch={launch} state={:?} errors={:?}",
            snapshot.execution.graph.node(node).unwrap().state(),
            snapshot.execution.errors
        );
        let wes_core::Data::Record(setup) = snapshot.execution.values[node].data() else {
            panic!("setup")
        };
        assert_eq!(setup["phase"], wes_core::Data::Text("prepared".into()));
        assert!(!setup.contains_key("dataset"));
        assert_eq!(
            fixture
                .source(&generation, "second-start", ":refresh $logs")
                .await,
            202
        );
        tokio::time::timeout(Duration::from_secs(5), arrivals.recv())
            .await
            .unwrap()
            .unwrap();
        session.wait_idle().await.unwrap();
        assert_eq!(calls.load(Ordering::SeqCst), 2);
        fixture.close().await;
    }
}

#[tokio::test]
async fn native_recording_optional_writer_failure_does_not_refuse_an_explicit_source_run() {
    let (fixture, calls, mut arrivals) = recording_launch_fixture(false).await;
    let mut events = fixture.stream().await;
    let generation = events.generation().await;
    let session = fixture.app.current().unwrap().session;
    assert_eq!(
        fixture
            .source(
                &generation,
                "setup",
                "@hold events watch > logs\n:dataset record source:$logs from:start > recording"
            )
            .await,
        202
    );
    session.wait_idle().await.unwrap();
    assert_eq!(
        fixture
            .source(&generation, "explicit-start", ":refresh $logs")
            .await,
        202
    );
    let (_, token) = tokio::time::timeout(Duration::from_secs(5), arrivals.recv())
        .await
        .unwrap()
        .unwrap();
    session.wait_idle().await.unwrap();
    let snapshot = session.snapshot().await.unwrap();
    assert_eq!(
        snapshot
            .execution
            .graph
            .node(&snapshot.names["recording"].node)
            .unwrap()
            .state(),
        wes_engine::graph::NodeState::Failed
    );
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    assert!(!token.is_cancelled());
    fixture.close().await;
}

#[tokio::test]
async fn native_recording_replaced_optional_setup_does_not_consume_its_fresh_sibling() {
    let (fixture, calls, mut arrivals) = recording_launch_fixture(true).await;
    let mut events = fixture.stream().await;
    let generation = events.generation().await;
    let session = fixture.app.current().unwrap().session;
    assert_eq!(
        fixture
            .source(
                &generation,
                "old-setup",
                "@hold events watch > logs\n:dataset record source:$logs from:start > old"
            )
            .await,
        202
    );
    session.wait_idle().await.unwrap();
    assert_eq!(
        fixture
            .source(&generation, "replace", ":change $logs tag:changed")
            .await,
        202
    );
    session.wait_idle().await.unwrap();
    assert_eq!(
        fixture
            .source(
                &generation,
                "fresh-setup",
                ":dataset record source:$logs from:start > fresh"
            )
            .await,
        202
    );
    session.wait_idle().await.unwrap();
    assert_eq!(
        fixture
            .source(&generation, "explicit-start", ":refresh $logs")
            .await,
        202
    );
    let (_, token) = tokio::time::timeout(Duration::from_secs(5), arrivals.recv())
        .await
        .unwrap()
        .unwrap();
    session.wait_idle().await.unwrap();
    let snapshot = session.snapshot().await.unwrap();
    assert_eq!(
        snapshot
            .execution
            .graph
            .node(&snapshot.names["old"].node)
            .unwrap()
            .state(),
        wes_engine::graph::NodeState::Failed
    );
    assert!(
        snapshot
            .execution
            .values
            .contains_key(&snapshot.names["fresh"].node)
    );
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    assert!(!token.is_cancelled());
    assert_eq!(
        fixture
            .source(&generation, "stop", ":dataset stop $fresh > stopped")
            .await,
        202
    );
    session.wait_idle().await.unwrap();
    assert!(!token.is_cancelled());
    fixture.close().await;
}

#[tokio::test]
async fn dataset_read_reports_only_its_committed_recording_coverage() {
    use wes_engine::storage::datasets::*;
    let fixture = Fixture::datasets().await;
    let mut events = fixture.stream().await;
    let generation = events.generation().await;
    let registry = wes_core::contracts::ContractRegistry::new();
    let schema = wes_core::contracts::ResolvedContractBundle::capture(
        registry.resolve("Int").unwrap(),
        Default::default(),
    )
    .unwrap();
    let run = uuid::Uuid::new_v4().to_string();
    let dataset = uuid::Uuid::new_v4().to_string();
    let mut source = SourceExtent {
        identity: run.clone(),
        unit: SourceUnit::Records,
        start: 2,
        end: 2,
    };
    let mut coverage = EventLogCoverage {
        run,
        epoch: dataset.clone(),
        first: 3,
        accepted_through: 2,
        committed_through: 2,
        pending: Some(0),
        rejected: 0,
        termination: None,
    };
    let admission = fixture
        .worker
        .dataset_create(DatasetCreate {
            owner: None,
            dataset,
            transaction: uuid::Uuid::new_v4().to_string(),
            kind: DatasetKind::EventLog,
            schema,
            source: source.clone(),
            policy: Default::default(),
            checkpoint: None,
            recording: Some(coverage.clone()),
        })
        .await
        .unwrap();
    let empty = admission.reference;
    let _writer = admission.lease;
    source.end = 3;
    coverage.accepted_through = 4;
    coverage.committed_through = 3;
    coverage.pending = Some(1);
    coverage.termination = Some(RecordingEnd::Overloaded);
    let committed = fixture
        .worker
        .enqueue_dataset_append(DatasetAppend {
            owner: None,
            previous: empty.clone(),
            transaction: uuid::Uuid::new_v4().to_string(),
            rows: vec![DatasetRow {
                ordinal: 0,
                source_start: 2,
                source_end: 3,
                value: wes_core::Value::new(
                    wes_core::Shape::Primitive(wes_core::Primitive::Int),
                    wes_core::Data::Int(7),
                    Default::default(),
                )
                .unwrap(),
            }],
            source,
            lifecycle: DatasetLifecycle::Incomplete,
            policy: Default::default(),
            checkpoint: None,
            recording: Some(coverage),
        })
        .await
        .unwrap()
        .wait()
        .await
        .unwrap();
    for (reference, accepted, saved, pending, termination) in [
        (empty, "2", "2", "0", Value::Null),
        (committed.clone(), "4", "3", "1", json!("overloaded")),
    ] {
        let value = wes_core::Value::new(
            wes_core::Shape::Dataset(Box::new(wes_core::Shape::Primitive(
                wes_core::Primitive::Int,
            ))),
            wes_core::Data::Dataset(Arc::new(reference.clone())),
            Default::default(),
        )
        .unwrap();
        let handle = fixture.worker.store(value).await.unwrap();
        let response = fixture
            .client
            .get(fixture.url(&format!("/datasets/{handle}?inspect=true")))
            .header("X-Wes-Session", &generation)
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), 200);
        let data: Value = serde_json::from_slice(&response.bytes().await.unwrap()).unwrap();
        assert_eq!(data["protected"], reference == committed);
        assert_eq!(data["recording"]["epoch"], reference.dataset());
        assert_eq!(data["recording"]["acceptedThrough"], accepted);
        assert_eq!(data["recording"]["committedThrough"], saved);
        assert_eq!(data["recording"]["pending"], pending);
        assert_eq!(data["recording"]["termination"], termination);
        // Opening inspected the saved prefix only. A head is an explicit local
        // read of the same recording epoch, and its page stays frozen afterward.
        let head = fixture
            .client
            .get(fixture.url(&format!("/datasets/{handle}?inspect=true&head=true")))
            .header("X-Wes-Session", &generation)
            .send()
            .await
            .unwrap();
        assert_eq!(head.status(), 200);
        let latest: Value = serde_json::from_slice(&head.bytes().await.unwrap()).unwrap();
        assert_eq!(
            latest["reference"],
            serde_json::to_value(&committed).unwrap()
        );
        let query = url::form_urlencoded::Serializer::new(String::new())
            .append_pair("extent", &serde_json::to_string(&committed).unwrap())
            .append_pair("from", "0")
            .append_pair("limit", "1")
            .finish();
        let page = fixture
            .client
            .get(fixture.url(&format!("/datasets/{handle}?{query}")))
            .header("X-Wes-Session", &generation)
            .send()
            .await
            .unwrap();
        assert_eq!(page.status(), 200);
        let page: Value = serde_json::from_slice(&page.bytes().await.unwrap()).unwrap();
        assert_eq!(page["page"]["rows"][0]["value"]["data"], 7);
        let mut foreign = serde_json::to_value(&committed).unwrap();
        foreign["dataset"] = json!(uuid::Uuid::new_v4().to_string());
        let query = url::form_urlencoded::Serializer::new(String::new())
            .append_pair("extent", &foreign.to_string())
            .append_pair("from", "0")
            .append_pair("limit", "1")
            .finish();
        let denied = fixture
            .client
            .get(fixture.url(&format!("/datasets/{handle}?{query}")))
            .header("X-Wes-Session", &generation)
            .send()
            .await
            .unwrap();
        assert_eq!(denied.status(), 400);
    }
    assert_eq!(fixture.calls.load(Ordering::SeqCst), 0);
    fixture.worker.dataset_withdraw(committed).await.unwrap();
    drop(events);
    fixture.close().await;
}

#[tokio::test]
async fn dataset_analysis_commits_its_captured_state_and_pages_through_the_owned_read_route() {
    let fixture = Fixture::datasets().await;
    let mut events = fixture.stream().await;
    let generation = events.generation().await;
    assert_eq!(fixture.source(&generation,"types",r#":package load source:"types: {IntStep: {base: Record, fields: {state: Int, outputs: 'List<Int>'}}}""#).await,202);
    let source = ":def sum(state:Int, context:Int, item:Int) -> IntStep as :calc pure { return {state:state+item,outputs:[item]}; }\n:calc { return [1,2,3]; } > raw\n:scan source:$raw transition:sum initial:0 context:0 profile:TypedRecords sink:dataset > analysis";
    assert_eq!(
        fixture
            .source(&generation, "dataset-analysis", source)
            .await,
        202
    );
    let session = fixture.app.current().unwrap().session;
    session.wait_idle().await.unwrap();
    let snapshot = session.snapshot().await.unwrap();
    let node = &snapshot.names["analysis"].node;
    assert!(
        !snapshot.execution.errors.contains_key(node),
        "dataset scan failed: {:?}",
        snapshot.execution.errors.get(node)
    );
    let frame = loop {
        let frame = events.until("ready").await;
        if frame["node"] == node.as_str() {
            break frame;
        }
    };
    let handle = frame["handle"].as_str().unwrap();
    let body = fixture
        .client
        .get(fixture.url(&format!("/values/{handle}")))
        .send()
        .await
        .unwrap();
    assert_eq!(body.status(), 200);
    let value: Value = serde_json::from_slice(&body.bytes().await.unwrap()).unwrap();
    assert_eq!(value["data"]["state"], 6);
    assert_eq!(value["data"]["outputs"]["kind"], "dataset");
    let reference: wes_core::DatasetRef =
        serde_json::from_value(value["data"]["outputs"]["reference"].clone()).unwrap();
    assert_eq!(reference.records(), 3);
    let checkpoint = fixture
        .worker
        .dataset_checkpoint(reference.clone())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(checkpoint.next_ordinal, 3);
    assert_eq!(checkpoint.next_position, 3);
    assert_eq!(checkpoint.work.outstanding, 0);
    assert_eq!(checkpoint.work.charged, checkpoint.work.completed);
    assert!(
        checkpoint.work.granted > checkpoint.work.charged,
        "confirmed batches release their unused credit"
    );
    assert_eq!(checkpoint.state.data(), &wes_core::Data::Int(6));
    assert!(
        fixture
            .worker
            .release(wes_engine::storage::ValueHandle::new(&checkpoint.source.handle).unwrap())
            .await
            .is_err(),
        "a retained continuation protects its exact captured input"
    );
    let url = fixture.url(&format!("/datasets/{handle}?select=/outputs&limit=1"));
    assert_eq!(fixture.client.get(&url).send().await.unwrap().status(), 409);
    let response = fixture
        .client
        .get(&url)
        .header("X-Wes-Session", &generation)
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 200);
    let page: Value = serde_json::from_slice(&response.bytes().await.unwrap()).unwrap();
    assert_eq!(page["page"]["first"], "0");
    assert_eq!(page["page"]["next"], "1");
    assert_eq!(page["page"]["rows"][0]["value"]["data"], 1);
    assert_eq!(page["lifecycle"], "sealed");
    let cursor = page["page"]["cursor"].as_str().unwrap();
    let mut url = url::Url::parse(&fixture.url(&format!("/datasets/{handle}"))).unwrap();
    url.query_pairs_mut().extend_pairs([
        ("select", "/outputs"),
        ("cursor", cursor),
        ("limit", "2"),
    ]);
    let response = fixture
        .client
        .get(url)
        .header("X-Wes-Session", &generation)
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 200);
    let page: Value = serde_json::from_slice(&response.bytes().await.unwrap()).unwrap();
    assert_eq!(page["page"]["rows"].as_array().unwrap().len(), 2);
    assert_eq!(page["page"]["next"], "3");
    assert_eq!(page["page"]["cursor"], Value::Null);
    assert_eq!(page["page"]["extentExhausted"], true);
    for query in [
        vec![("select", "/outputs"), ("from", "01")],
        vec![("select", "/outputs"), ("limit", "101")],
        vec![("select", "/state"), ("cursor", cursor)],
    ] {
        let mut url = url::Url::parse(&fixture.url(&format!("/datasets/{handle}"))).unwrap();
        url.query_pairs_mut().extend_pairs(query);
        let response = fixture
            .client
            .get(url)
            .header("X-Wes-Session", &generation)
            .send()
            .await
            .unwrap();
        assert!(matches!(response.status().as_u16(), 400 | 403));
    }
    assert_eq!(
        fixture.calls.load(Ordering::SeqCst),
        0,
        "read and scan never acquire an external producer"
    );
    assert_eq!(fixture.source(&generation,"native-pages",":dataset inspect $analysis.outputs > datasetInfo\n:dataset page $analysis.outputs from:1 limit:2 > selectedPage").await,202);
    session.wait_idle().await.unwrap();
    let snapshot = session.snapshot().await.unwrap();
    for name in ["datasetInfo", "selectedPage"] {
        let node = &snapshot.names[name].node;
        assert!(
            !snapshot.execution.errors.contains_key(node),
            "native Dataset read failed: {:?}",
            snapshot.execution.errors.get(node)
        );
    }
    let page = &snapshot.execution.values[&snapshot.names["selectedPage"].node];
    let wes_core::Data::Record(fields) = page.data() else {
        panic!("page record");
    };
    assert_eq!(
        fields["rows"],
        wes_core::Data::List(vec![wes_core::Data::Int(2), wes_core::Data::Int(3)])
    );
    assert_eq!(fields["first"], wes_core::Data::Text("1".into()));
    assert_eq!(fields["next"], wes_core::Data::Text("3".into()));
    assert!(
        page.metadata().is_some(),
        "the normal result carries its pinned row contract"
    );
    assert_eq!(fixture.calls.load(Ordering::SeqCst), 0);
    assert_eq!(fixture.source(&generation,"dataset-source",":scan source:$analysis.outputs transition:sum initial:0 context:0 profile:TypedRecords sink:dataset > secondAnalysis").await,202);
    session.wait_idle().await.unwrap();
    let snapshot = session.snapshot().await.unwrap();
    let node = &snapshot.names["secondAnalysis"].node;
    assert!(
        !snapshot.execution.errors.contains_key(node),
        "dataset source scan: {:?}",
        snapshot.execution.errors.get(node)
    );
    let value = &snapshot.execution.values[node];
    let wes_core::Data::Record(fields) = value.data() else {
        panic!("analysis record")
    };
    assert_eq!(fields["state"], wes_core::Data::Int(6));
    let wes_core::Data::Dataset(output) = &fields["outputs"] else {
        panic!("paged output")
    };
    assert_eq!(output.records(), 3);
    let cp = fixture
        .worker
        .dataset_checkpoint(output.as_ref().clone())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(cp.next_position, 3);
    assert_eq!(cp.next_ordinal, 3);
    assert!(
        cp.work.charged > checkpoint.work.charged,
        "physical source validation shares the parent analysis work ledger"
    );
    assert_eq!(fixture.calls.load(Ordering::SeqCst), 0);
    // The copied page is an ordinary typed value: it does not retain the whole
    // source, but must not bypass a subsequent source withdrawal.
    let copied_node = snapshot.names["selectedPage"].node.clone();
    assert!(
        !snapshot.execution.values[&copied_node]
            .provenance()
            .policy()
            .dataset_reads()
            .is_empty()
    );
    let affected = fixture
        .worker
        .dataset_withdraw(reference.clone())
        .await
        .unwrap();
    assert!(affected.iter().any(|id| id == output.dataset()));
    let notice = events.until("result-access").await;
    assert_eq!(notice["readable"], false);
    assert!(notice.get("reference").is_none());
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let state = session.snapshot().await.unwrap();
            if [
                &state.names["analysis"].node,
                &state.names["secondAnalysis"].node,
                &copied_node,
            ]
            .into_iter()
            .all(|id| !state.execution.values.contains_key(id))
            {
                assert!(
                    state
                        .execution
                        .stale_reasons
                        .values()
                        .any(|r| r.code() == "result_withdrawn")
                );
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    assert!(
        fixture
            .worker
            .dataset_checkpoint(output.as_ref().clone())
            .await
            .is_err()
    );
    assert_eq!(
        fixture.calls.load(Ordering::SeqCst),
        0,
        "withdrawal never replays a producer"
    );
    drop(events);
    fixture.close().await;
}
#[tokio::test]
async fn finite_analysis_delivers_real_progress_and_labelled_incomplete_publication() {
    for fail in [false, true] {
        let fixture = Fixture::new().await;
        let mut events = fixture.stream().await;
        let generation = events.generation().await;
        assert_eq!(fixture.source(&generation,"types",r#":package load source:"types: {IntStep: {base: Record, fields: {state: Int, outputs: 'List<Int>'}}}""#).await,202);
        let body = if fail {
            "if(item==2) { return {state:1/0,outputs:[]}; } return {state:state+item,outputs:[item]};"
        } else {
            "return {state:state+item,outputs:[item]};"
        };
        let source = format!(
            ":def sum(state:Int, context:Int, item:Int) -> IntStep as :calc pure {{ {body} }}\n:calc {{ return [1,2,3]; }} > raw\n:scan source:$raw transition:sum initial:0 context:0 profile:TypedRecords > analysis"
        );
        assert_eq!(fixture.source(&generation, "analysis", &source).await, 202);
        let session = fixture.app.current().unwrap().session;
        session.wait_idle().await.unwrap();
        let snapshot = session.snapshot().await.unwrap();
        let node = &snapshot.names["analysis"].node;
        let kind = if fail { "evidence" } else { "ready" };
        let frame = loop {
            let frame = events.until(kind).await;
            if frame["node"] == node.as_str() {
                break frame;
            }
        };
        assert_eq!(frame["private"], false);
        if fail {
            assert_eq!(frame["kind"], "incomplete");
            assert_eq!(frame["state"], "failed");
            assert_eq!(frame["kept"], false);
            assert!(frame["error"]["code"].is_string());
        }
        let body = fixture
            .client
            .get(fixture.url(&format!("/values/{}", frame["handle"].as_str().unwrap())))
            .send()
            .await
            .unwrap();
        assert_eq!(body.status(), 200);
        let value: Value = serde_json::from_slice(&body.bytes().await.unwrap()).unwrap();
        assert_eq!(value["data"]["state"], json!(if fail { 1 } else { 6 }));
        assert_eq!(
            value["data"]["receipt"]["inputRecords"],
            json!(if fail { 1 } else { 3 })
        );
        assert_eq!(
            value["data"]["receipt"]["sourceComplete"],
            json!({"kind":"none"})
        );
        assert_eq!(value["data"]["receipt"]["durableResume"], false);
        let progress = &snapshot.execution.progress[node];
        assert_eq!(
            progress.phase,
            if fail {
                wes_engine::driver::progress::Phase::Stopped
            } else {
                wes_engine::driver::progress::Phase::Complete
            }
        );
        assert_eq!(
            progress.counters.as_ref().unwrap().input_records,
            if fail { 1 } else { 3 }
        );
        println!(
            "ANALYSIS_WIRE_SAMPLE={}",
            json!({"frame":frame,"value":value,"progress":progress})
        );
        drop(events);
        fixture.close().await;
    }
}

#[tokio::test]
async fn explicit_resume_uses_saved_pure_code_and_preserves_failed_coverage() {
    let fixture = Fixture::datasets().await;
    let mut events = fixture.stream().await;
    let generation = events.generation().await;
    assert_eq!(fixture.source(&generation,"types",r#":package load source:"types: {IntStep: {base: Record, fields: {state: Int, outputs: 'List<Int>'}}}""#).await,202);
    // One full commit batch precedes the failure. The following speculative item
    // must be discarded together with its unacknowledged state and outputs.
    let source = ":def step(state:Int, context:Int, item:Int) -> IntStep as :calc pure { if(item==130) { return {state:1/0,outputs:[]}; } return {state:state+item,outputs:[item]}; }\n:calc pure { return range(131).map(i=>i+1); } > raw\n:scan source:$raw transition:step initial:0 context:0 profile:TypedRecords sink:dataset > analysis";
    assert_eq!(
        fixture
            .source(&generation, "incomplete-analysis", source)
            .await,
        202
    );
    let session = fixture.app.current().unwrap().session;
    session.wait_idle().await.unwrap();
    let snapshot = session.snapshot().await.unwrap();
    let node = &snapshot.names["analysis"].node;
    let original = loop {
        let frame = events.until("evidence").await;
        if frame["node"] == node.as_str() {
            break frame;
        }
    };
    let response = fixture
        .client
        .get(fixture.url(&format!("/values/{}", original["handle"].as_str().unwrap())))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 200);
    let body: Value = serde_json::from_slice(&response.bytes().await.unwrap()).unwrap();
    assert_eq!(body["data"]["state"], 8256);
    assert_eq!(
        body["data"]["receipt"]["durableResume"], false,
        "the captured division-by-zero callback cannot be repaired by ordinary Resume"
    );
    let reference: wes_core::DatasetRef =
        serde_json::from_value(body["data"]["outputs"]["reference"].clone()).unwrap();
    let before = fixture
        .worker
        .dataset_checkpoint(reference.clone())
        .await
        .unwrap()
        .unwrap();
    // Replacing the live declaration does not replace the retained callback or silently acquire raw again.
    assert_eq!(
        body["data"]["receipt"]["attempt"],
        serde_json::json!({"kind":"some","value":before.attempt})
    );
    assert_eq!(
        body["data"]["receipt"]["previousAttempt"],
        serde_json::json!({"kind":"none"})
    );
    assert_eq!(
        body["data"]["receipt"]["measuredWork"],
        before.work.completed
    );
    assert_eq!(body["data"]["receipt"]["work"], before.work.charged);
    assert_eq!(
        body["data"]["receipt"]["outstandingWork"],
        serde_json::json!({"kind":"some","value":0})
    );
    assert_eq!(fixture.source(&generation,"new-definition",":def step(state:Int, context:Int, item:Int) -> IntStep as :calc pure { return {state:999,outputs:[999]}; }").await,202);
    assert_eq!(
        fixture
            .source(
                &generation,
                "continuation",
                ":scan resume $analysis > continued"
            )
            .await,
        202
    );
    session.wait_idle().await.unwrap();
    let snapshot = session.snapshot().await.unwrap();
    let node = &snapshot.names["continued"].node;
    assert!(
        snapshot.execution.errors.contains_key(node),
        "the captured callback must still fail on its original second item"
    );
    let frame = loop {
        let frame = events.until("evidence").await;
        if frame["node"] == node.as_str() {
            break frame;
        }
    };
    let old_handle = original["handle"].as_str().unwrap();
    let saved = fixture
        .client
        .get(fixture.url(&format!(
            "/datasets/{old_handle}?select=/outputs&inspect=true"
        )))
        .header("X-Wes-Session", &generation)
        .send()
        .await
        .unwrap();
    assert_eq!(
        saved.status(),
        200,
        "the selected immutable prefix is distinct from a new attempt"
    );
    let switched = fixture
        .client
        .get(fixture.url(&format!(
            "/datasets/{old_handle}?select=/outputs&inspect=true&head=true"
        )))
        .header("X-Wes-Session", &generation)
        .send()
        .await
        .unwrap();
    assert_eq!(switched.status(), 409);
    let refused: Value = serde_json::from_slice(&switched.bytes().await.unwrap()).unwrap();
    assert_eq!(refused["error"]["code"], "DATASET_CONTINUITY_CHANGED");
    assert_eq!(refused["error"]["retryable"], false);

    let response = fixture
        .client
        .get(fixture.url(&format!("/values/{}", frame["handle"].as_str().unwrap())))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 200);
    let body: Value = serde_json::from_slice(&response.bytes().await.unwrap()).unwrap();
    assert_eq!(body["data"]["state"], 8256);
    assert_eq!(body["data"]["receipt"]["inputRecords"], 128);
    assert_eq!(
        body["data"]["receipt"]["previousAttempt"],
        serde_json::json!({"kind":"some","value":before.attempt})
    );
    let resumed: wes_core::DatasetRef =
        serde_json::from_value(body["data"]["outputs"]["reference"].clone()).unwrap();
    assert_eq!(resumed.records(), 128);
    let command = |target: &wes_core::DatasetRef, name: &str| {
        format!(
            ":dataset snapshot $analysis.outputs basis:\"{}\" generation:\"{}\" digest:\"{}\" > {name}",
            reference.manifest_digest(),
            target.generation(),
            target.manifest_digest()
        )
    };
    assert_eq!(
        fixture
            .source(
                &generation,
                "same-attempt-capture",
                &command(&reference, "oldEvidence")
            )
            .await,
        202
    );
    assert_eq!(
        fixture
            .source(
                &generation,
                "cross-attempt-capture",
                &command(&resumed, "wrongAttempt")
            )
            .await,
        202
    );
    session.wait_idle().await.unwrap();
    let captured = session.snapshot().await.unwrap();
    assert_eq!(
        captured.execution.values[&captured.names["oldEvidence"].node].data(),
        &wes_core::Data::Dataset(Arc::new(reference.clone()))
    );
    assert!(
        captured
            .execution
            .errors
            .contains_key(&captured.names["wrongAttempt"].node)
    );
    let after = fixture
        .worker
        .dataset_checkpoint(resumed.clone())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(after.previous_attempt, Some(before.attempt));
    assert_eq!(after.step_revision, before.step_revision);
    assert_eq!(after.source, before.source);
    assert_eq!(after.usage.work_allowance, before.usage.work_allowance);
    assert!(after.work.charged > before.work.charged);
    assert_eq!(after.next_position, 128);
    assert_eq!(
        fixture
            .worker
            .dataset_page(
                resumed,
                wes_engine::storage::datasets::PageRequest {
                    work: None,
                    from: 127,
                    rows: 1,
                    bytes: 65536,
                    segments: wes_budgets::get("dataset.page.segments") as usize
                }
            )
            .await
            .unwrap()
            .rows
            .len(),
        1
    );
    assert_eq!(fixture.calls.load(Ordering::SeqCst), 0);
    drop(events);
    fixture.close().await;
}

#[tokio::test]
async fn dataset_deletion_requires_a_live_reviewed_plan_and_withdraws_reads_before_cleanup() {
    let fixture = Fixture::datasets().await;
    let mut events = fixture.stream().await;
    let generation = events.generation().await;
    assert_eq!(fixture.source(&generation,"types",r#":package load source:"types: {IntStep: {base: Record, fields: {state: Int, outputs: 'List<Int>'}}}""#).await,202);
    assert_eq!(fixture.source(&generation,"source",":def step(state:Int, context:Int, item:Int) -> IntStep as :calc pure { return {state:state+item,outputs:[item]}; }\n:calc { return [1,2,3]; } > raw\n:scan source:$raw transition:step initial:0 context:0 profile:TypedRecords sink:dataset > analysis").await,202);
    let session = fixture.app.current().unwrap().session;
    session.wait_idle().await.unwrap();
    let snapshot = session.snapshot().await.unwrap();
    let node = &snapshot.names["analysis"].node;
    assert!(
        !snapshot.execution.errors.contains_key(node),
        "analysis admission: {:?}",
        snapshot.execution.errors.get(node)
    );
    let frame = loop {
        let frame = events.until("ready").await;
        if frame["node"] == node.as_str() {
            break frame;
        }
    };
    let handle = frame["handle"].as_str().unwrap();
    let response = fixture
        .client
        .get(fixture.url(&format!("/values/{handle}")))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 200);
    let body: Value = serde_json::from_slice(&response.bytes().await.unwrap()).unwrap();
    let reference: wes_core::DatasetRef =
        serde_json::from_value(body["data"]["outputs"]["reference"].clone()).unwrap();
    // Reading a page gives subsequent values a revocable read origin. The delete
    // acknowledgement must survive its own withdrawal, while these read values do not.
    assert_eq!(fixture.source(&generation, "derive", ":dataset page $analysis.outputs > readPage\n:calc pure { const observed = $readPage; return $analysis.outputs; } > derived").await, 202);
    session.wait_idle().await.unwrap();
    let derived_state = session.snapshot().await.unwrap();
    assert!(
        !derived_state.execution.values[&derived_state.names["derived"].node]
            .provenance()
            .policy()
            .dataset_reads()
            .is_empty()
    );
    assert_eq!(
        fixture
            .source(
                &generation,
                "plan",
                ":dataset plan-delete $derived > deletion"
            )
            .await,
        202
    );
    session.wait_idle().await.unwrap();
    let snapshot = session.snapshot().await.unwrap();
    let node = &snapshot.names["deletion"].node;
    assert!(
        !snapshot.execution.errors.contains_key(node),
        "plan: {:?}",
        snapshot.execution.errors.get(node)
    );
    assert!(
        snapshot.execution.values[node]
            .management_authority()
            .is_some()
    );
    assert_eq!(
        fixture
            .source(&generation, "refuse", ":dataset delete $deletion > refused")
            .await,
        202
    );
    session.wait_idle().await.unwrap();
    let snapshot = session.snapshot().await.unwrap();
    assert!(
        snapshot
            .execution
            .errors
            .contains_key(&snapshot.names["refused"].node)
    );
    assert!(
        fixture
            .worker
            .dataset_inspect(reference.clone())
            .await
            .is_ok()
    );
    assert_eq!(
        fixture
            .source(
                &generation,
                "approve",
                ":dataset delete $deletion references:true protected:true > removed"
            )
            .await,
        202
    );
    session.wait_idle().await.unwrap();
    let snapshot = session.snapshot().await.unwrap();
    let node = &snapshot.names["removed"].node;
    assert!(
        !snapshot.execution.errors.contains_key(node),
        "delete: {:?}",
        snapshot.execution.errors.get(node)
    );
    let acknowledgement = snapshot
        .execution
        .values
        .get(node)
        .expect("confirmed deletion receipt remains readable");
    assert!(
        acknowledgement
            .provenance()
            .policy()
            .dataset_reads()
            .is_empty()
    );
    assert!(matches!(acknowledgement.data(), wes_core::Data::Record(_)));
    assert!(
        !snapshot
            .execution
            .values
            .contains_key(&snapshot.names["derived"].node)
    );
    assert!(matches!(
        fixture.worker.dataset_inspect(reference).await,
        Err(wes_engine::storage::StoreError::DatasetWithdrawn)
    ));
    let response = fixture
        .client
        .get(fixture.url(&format!("/datasets/{handle}?select=/outputs")))
        .header("X-Wes-Session", &generation)
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 410);
    assert_eq!(
        fixture
            .source(
                &generation,
                "used",
                ":dataset delete $deletion references:true protected:true > reused"
            )
            .await,
        202
    );
    session.wait_idle().await.unwrap();
    let snapshot = session.snapshot().await.unwrap();
    assert!(
        snapshot
            .execution
            .errors
            .contains_key(&snapshot.names["reused"].node)
    );
    assert_eq!(fixture.calls.load(Ordering::SeqCst), 0);
    drop(events);
    fixture.close().await;
}

#[tokio::test]
async fn dynamic_view_dataset_reads_bind_member_session_and_both_revisions() {
    let fixture = Fixture::datasets().await;
    let mut events = fixture.stream().await;
    let generation = events.generation().await;
    let manifest = json!({"name":"ArchiveRows", "id":"archive-rows", "summary":"Read a committed page", "renderer":"View.tsx", "input":"ArchiveRows", "outputs":{}, "interaction":null}).to_string();
    let types = "types: {ArchiveRows: {base: Record, fields: {outputs: 'Dataset<Int>'}}}";
    let package = wes_views::Package::parse(&manifest, types).unwrap();
    let artifact = json!({"format":1,"sdk":wes_views::sdk_version(),"manifest":manifest,"types":types,"definition":package.digest,"javascript":"throw new Error('not executed');","css":""}).to_string();
    let session = fixture.app.current().unwrap().session;
    let install = format!(
        ":package load source:{}\n:package load source:{}",
        json!(artifact),
        json!("types: {IntStep: {base: Record, fields: {state: Int, outputs: 'List<Int>'}}}")
    );
    assert_eq!(fixture.source(&generation, "install", &install).await, 202);
    session.wait_idle().await.unwrap();
    let source = ":def sum(state:Int, context:Int, item:Int) -> IntStep as :calc pure { return {state:state+item,outputs:[item]}; }\n:calc { return [1,2,3]; } > raw\n:scan source:$raw transition:sum initial:0 context:0 profile:TypedRecords sink:dataset > analysis";
    assert_eq!(fixture.source(&generation, "analysis", source).await, 202);
    session.wait_idle().await.unwrap();
    assert_eq!(
        fixture
            .source(
                &generation,
                "view",
                ":view create ArchiveRows input:$analysis > archive"
            )
            .await,
        202
    );
    session.wait_idle().await.unwrap();
    let snapshot = session.snapshot().await.unwrap();
    assert!(
        snapshot.execution.errors.is_empty(),
        "{:?}",
        snapshot.execution.errors
    );
    let node = match snapshot.names.get("archive") {
        Some(output) => output.node.clone(),
        None => loop {
            let p = events.until("planned").await;
            if p["cell"] == "view" {
                panic!("view admission: {p}");
            }
        },
    };
    let frame = session.view_frame(node.clone()).await.unwrap();
    let selected = &frame.instances[0];
    let url = fixture.url(&format!(
        "/view-datasets/{}/{}/{}?select=/outputs&from=1&limit=1",
        node, selected.identity, selected.id
    ));

    let wes_core::Data::Record(analysis_fields) =
        snapshot.execution.values[&snapshot.names["analysis"].node].data()
    else {
        panic!("scan")
    };
    let wes_core::Data::Dataset(prefix) = &analysis_fields["outputs"] else {
        panic!("dataset")
    };
    for query in [
        "select=/outputs&inspect=true&head=true".to_owned(),
        url::form_urlencoded::Serializer::new(String::new())
            .append_pair("select", "/outputs")
            .append_pair("extent", &serde_json::to_string(prefix.as_ref()).unwrap())
            .append_pair("from", "0")
            .finish(),
    ] {
        let denied = fixture
            .client
            .get(fixture.url(&format!(
                "/view-datasets/{}/{}/{}?{query}",
                node, selected.identity, selected.id
            )))
            .header("X-Wes-Session", &generation)
            .header("X-Wes-View-Revision", selected.revision.to_string())
            .header("X-Wes-Input-Revision", selected.input_revision.to_string())
            .send()
            .await
            .unwrap();
        assert_eq!(
            denied.status(),
            400,
            "views cannot advance beyond their actual drawn input"
        );
    }
    let read = |generation: String, revision: String, input: String| {
        fixture
            .client
            .get(&url)
            .header("X-Wes-Session", generation)
            .header("X-Wes-View-Revision", revision)
            .header("X-Wes-Input-Revision", input)
    };
    for (g, r, i, status) in [
        (
            generation.clone(),
            selected.revision.to_string(),
            selected.input_revision.to_string(),
            200,
        ),
        (
            "other".into(),
            selected.revision.to_string(),
            selected.input_revision.to_string(),
            409,
        ),
        (
            generation.clone(),
            (selected.revision + 1).to_string(),
            selected.input_revision.to_string(),
            409,
        ),
        (
            generation.clone(),
            selected.revision.to_string(),
            (selected.input_revision + 1).to_string(),
            409,
        ),
        (
            generation.clone(),
            "01".into(),
            selected.input_revision.to_string(),
            400,
        ),
    ] {
        let response = read(g, r, i).send().await.unwrap();
        assert_eq!(response.status(), status);
        if status == 200 {
            let value: Value = serde_json::from_slice(&response.bytes().await.unwrap()).unwrap();
            assert_eq!(value["page"]["first"], "1");
            assert_eq!(value["page"]["rows"][0]["value"]["data"], 2);
        }
    }
    let wrong = fixture.url(&format!(
        "/view-datasets/{}/{}/{}?select=/outputs",
        node, selected.identity, snapshot.names["raw"].node
    ));
    assert_eq!(
        fixture
            .client
            .get(wrong)
            .header("X-Wes-Session", &generation)
            .header("X-Wes-View-Revision", selected.revision.to_string())
            .header("X-Wes-Input-Revision", selected.input_revision.to_string())
            .send()
            .await
            .unwrap()
            .status(),
        409
    );
    let input = selected.input.as_ref().unwrap().value().unwrap();
    let wes_core::Data::Record(fields) = input.data() else {
        panic!("record input")
    };
    let wes_core::Data::Dataset(reference) = &fields["outputs"] else {
        panic!("Dataset input")
    };
    fixture
        .worker
        .dataset_withdraw(reference.as_ref().clone())
        .await
        .unwrap();
    let response = read(
        generation,
        selected.revision.to_string(),
        selected.input_revision.to_string(),
    )
    .send()
    .await
    .unwrap();
    assert!(matches!(response.status().as_u16(), 403 | 409 | 410));
    let denied: Value = serde_json::from_slice(&response.bytes().await.unwrap()).unwrap();
    assert!(denied.get("page").is_none() && denied.get("reference").is_none());
    assert_eq!(
        fixture.calls.load(Ordering::SeqCst),
        0,
        "paging never runs a provider"
    );
    fixture.close().await;
}

#[tokio::test]
async fn native_recording_has_one_joined_run_and_stop_leaves_the_source_open_at_one_operation_slot()
{
    use tokio::sync::mpsc;
    use wes_engine::streams::{StreamFuture, StreamSink, StreamingInvoker};
    struct Source(mpsc::UnboundedSender<(StreamSink, CancellationToken)>);
    impl StreamingInvoker for Source {
        fn subscribe(&self, _: Call, sink: StreamSink, token: CancellationToken) -> StreamFuture {
            sink.push(
                wes_core::Value::new(
                    Shape::Primitive(wes_core::Primitive::Int),
                    wes_core::Data::Int(0),
                    Default::default(),
                )
                .unwrap(),
            )
            .unwrap();
            sink.opened().unwrap();
            self.0.send((sink, token.clone())).unwrap();
            Box::pin(async move {
                token.cancelled().await;
                Ok(())
            })
        }
    }
    let (entered, mut arrivals) = mpsc::unbounded_channel();
    let fixture = Fixture::configured_owner_concurrency(
        |_| wes::web::Services::default(),
        Arc::new(move |workspace| {
            let mut capability = Capability::new(
                ["watch"],
                Shape::Primitive(wes_core::Primitive::Int),
                Safety::Safe,
            );
            capability.streaming = true;
            workspace.register_provider_ports(
                ProviderDescription::new("events", [capability], vec![]).unwrap(),
                Arc::new(Echo(Arc::new(AtomicUsize::new(0)))),
                Some(Arc::new(Source(entered.clone()))),
            )?;
            Ok(())
        }),
        |root, values| {
            let datasets = wes_adapters::datasets::DatasetStore::open(
                &root.join("datasets"),
                Durability::File,
                Default::default(),
            )
            .unwrap();
            wes_engine::storage::spawn_storage(values, datasets, StoreWorkerLimits::default())
                .unwrap()
        },
        NonZeroUsize::new(1).unwrap(),
    )
    .await;
    let mut events = fixture.stream().await;
    let generation = events.generation().await;
    assert_eq!(
        fixture
            .source(&generation, "source", "events watch > logs")
            .await,
        202
    );
    let (sink, source_cancel) = arrivals.recv().await.unwrap();
    let first = events.until("ready").await;
    let session = fixture.app.current().unwrap().session;
    let source_node = session.snapshot().await.unwrap().names["logs"].node.clone();
    assert_eq!(first["node"], source_node.as_str());
    assert_eq!(
        fixture
            .source(
                &generation,
                "record",
                ":dataset record source:$logs from:next budget:Capture > recording"
            )
            .await,
        202
    );
    let record_node = session.snapshot().await.unwrap().names["recording"]
        .node
        .clone();
    loop {
        let progress = events.until("node-progress").await;
        if progress["node"] == record_node.as_str() {
            break;
        }
    }
    // Its first exact prefix is usable before Stop; idle waits do not wait for an open lifetime.
    tokio::time::timeout(Duration::from_secs(5), session.wait_idle())
        .await
        .unwrap()
        .unwrap();
    let initial = session.snapshot().await.unwrap();
    assert_eq!(
        initial.execution.graph.node(&record_node).unwrap().state(),
        wes_engine::graph::NodeState::Ready
    );
    assert!(
        initial
            .execution
            .graph
            .node(&record_node)
            .unwrap()
            .payload()
            .recording_control(initial.execution.runs[&record_node].as_str())
            .unwrap()
            .active
    );
    let wes_core::Data::Record(descriptor) = initial.execution.values[&record_node].data() else {
        panic!("usable initial recording descriptor");
    };
    let wes_core::Data::Dataset(initial_prefix) = &descriptor["dataset"] else {
        panic!("typed initial prefix");
    };
    assert_eq!(initial_prefix.records(), 0);

    let recording_run = initial.execution.runs[&record_node].clone();
    let log = session.log().await.unwrap();
    assert!(
        !log.entries
            .iter()
            .filter_map(|entry| entry.entry().observation())
            .any(|entry| entry.node() == &record_node
                && entry.state() == wes_engine::graph::NodeState::Ready),
        "usable initial descriptor is not a completed execution history record"
    );
    assert_eq!(
        fixture
            .source(
                &generation,
                "stale-control",
                &format!(
                    ":dataset stop $recording run:{} > staleControl",
                    json!(uuid::Uuid::new_v4().to_string())
                )
            )
            .await,
        202
    );
    let refused = session.snapshot().await.unwrap().names["staleControl"]
        .node
        .clone();
    loop {
        let failed = events.until("failed").await;
        if failed["node"] == refused.as_str() {
            break;
        }
    }
    assert!(
        session
            .snapshot()
            .await
            .unwrap()
            .execution
            .graph
            .node(&record_node)
            .unwrap()
            .payload()
            .recording_control(recording_run.as_str())
            .unwrap()
            .active
    );

    // Ordinary consumption of the initial descriptor does not start or finish either owner.
    assert_eq!(
        fixture
            .source(
                &generation,
                "inspect-initial",
                ":dataset inspect $recording.dataset > initialInfo"
            )
            .await,
        202
    );
    tokio::time::timeout(Duration::from_secs(5), session.wait_idle())
        .await
        .unwrap()
        .unwrap();
    let current = session.snapshot().await.unwrap();
    assert!(
        current
            .execution
            .values
            .contains_key(&current.names["initialInfo"].node)
    );
    assert!(!source_cancel.is_cancelled());

    let control = initial
        .execution
        .graph
        .node(&record_node)
        .unwrap()
        .payload()
        .recording_control(recording_run.as_str())
        .expect("admitted writer control");
    assert!(control.status_available && control.stop_available);
    assert!(
        initial
            .execution
            .graph
            .node(&source_node)
            .unwrap()
            .payload()
            .recording_control(initial.execution.runs[&source_node].as_str())
            .is_none()
    );
    assert!(
        initial
            .execution
            .graph
            .node(&record_node)
            .unwrap()
            .payload()
            .recording_control("another-run")
            .is_none()
    );
    // Status is observational even while the writer holds a lifetime slot.
    assert_eq!(
        fixture
            .source(
                &generation,
                "status",
                ":dataset recording-status $recording > currentRecording"
            )
            .await,
        202
    );
    let status_node = session.snapshot().await.unwrap().names["currentRecording"]
        .node
        .clone();
    loop {
        let ready = events.until("ready").await;
        if ready["node"] == status_node.as_str() {
            break;
        }
    }

    assert_eq!(
        fixture
            .source(&generation, "ordinary", ":calc { return 42; } > answer")
            .await,
        202
    );
    let answer = session.snapshot().await.unwrap().names["answer"]
        .node
        .clone();
    loop {
        let ready = events.until("ready").await;
        if ready["node"] == answer.as_str() {
            break;
        }
    }
    // This event was admitted after attachment. The original window's zero must be excluded.
    sink.push(
        wes_core::Value::new(
            Shape::Primitive(wes_core::Primitive::Int),
            wes_core::Data::Int(7),
            Default::default(),
        )
        .unwrap(),
    )
    .unwrap();
    assert_eq!(
        fixture
            .source(&generation, "stop", ":dataset stop $recording > stopped")
            .await,
        202
    );
    let stopped_node = session.snapshot().await.unwrap().names["stopped"]
        .node
        .clone();
    loop {
        let ready = events.until("ready").await;
        if ready["node"] == stopped_node.as_str() {
            break;
        }
    }
    session.wait_idle().await.unwrap();
    let log = session.log().await.unwrap();
    assert_eq!(
        log.entries
            .iter()
            .filter_map(|entry| entry.entry().observation())
            .filter(|entry| entry.node() == &record_node
                && entry.state() == wes_engine::graph::NodeState::Ready)
            .count(),
        1,
        "one joined final execution history result, not one per event/prefix"
    );
    let stopped = session.snapshot().await.unwrap();
    let control = stopped
        .execution
        .graph
        .node(&record_node)
        .unwrap()
        .payload()
        .recording_control(recording_run.as_str())
        .expect("joined status remains readable");
    assert!(control.status_available && !control.stop_available && !control.active);

    assert_eq!(
        stopped.execution.progress[&record_node].phase,
        wes_engine::driver::progress::Phase::Complete
    );
    assert_eq!(
        stopped.execution.progress[&record_node]
            .recording
            .as_ref()
            .unwrap()
            .state,
        "stopped"
    );
    assert_eq!(
        stopped.execution.runs[&record_node], recording_run,
        "commit batches must not create another run"
    );
    let wes_core::Data::Record(record) = stopped.execution.values[&stopped_node].data() else {
        panic!("recording descriptor");
    };
    let wes_core::Data::Dataset(reference) = &record["dataset"] else {
        panic!("typed dataset");
    };
    let reference = reference.as_ref().clone();
    let page = fixture
        .worker
        .dataset_page(
            reference.clone(),
            wes_engine::storage::datasets::PageRequest {
                work: None,
                from: 0,
                rows: 100,
                bytes: 65536,
                segments: 8,
            },
        )
        .await
        .unwrap();
    assert_eq!(page.rows.len(), 1);
    assert_eq!(page.rows[0].value.data(), &wes_core::Data::Int(7));
    let info = fixture
        .worker
        .dataset_inspect(reference.clone())
        .await
        .unwrap();
    assert!(info.protected);
    assert_eq!(info.recording.unwrap().first, 2);
    assert!(
        !source_cancel.is_cancelled(),
        "Stop recording must not cancel a shared source"
    );
    sink.push(
        wes_core::Value::new(
            Shape::Primitive(wes_core::Primitive::Int),
            wes_core::Data::Int(8),
            Default::default(),
        )
        .unwrap(),
    )
    .unwrap();
    assert_eq!(
        fixture
            .source(
                &generation,
                "stop-again",
                ":dataset stop $recording > stoppedAgain"
            )
            .await,
        202
    );
    let again = session.snapshot().await.unwrap().names["stoppedAgain"]
        .node
        .clone();
    loop {
        let ready = events.until("ready").await;
        if ready["node"] == again.as_str() {
            break;
        }
    }
    let snapshot = session.snapshot().await.unwrap();
    assert_eq!(
        snapshot.execution.values[&again].data(),
        snapshot.execution.values[&stopped_node].data(),
        "repeat Stop acknowledges the same exact prefix"
    );
    // Copying the descriptor does not copy process-local writer authority.
    assert_eq!(
        fixture
            .source(
                &generation,
                "copy",
                ":calc pure { return $stopped; } > copy"
            )
            .await,
        202
    );
    let copied = session.snapshot().await.unwrap().names["copy"].node.clone();
    loop {
        let ready = events.until("ready").await;
        if ready["node"] == copied.as_str() {
            break;
        }
    }
    let copied_state = session.snapshot().await.unwrap();
    assert!(
        copied_state
            .execution
            .graph
            .node(&copied)
            .unwrap()
            .payload()
            .recording_control(copied_state.execution.runs[&copied].as_str())
            .is_none()
    );
    assert_eq!(
        fixture
            .source(&generation, "deny-copy", ":dataset stop $copy > denied")
            .await,
        202
    );
    let denied = session.snapshot().await.unwrap().names["denied"]
        .node
        .clone();
    loop {
        let failed = events.until("failed").await;
        if failed["node"] == denied.as_str() {
            break;
        }
    }
    assert!(!source_cancel.is_cancelled());
    // Workspace close must also join an attached writer that was never explicitly stopped.
    assert_eq!(
        fixture
            .source(
                &generation,
                "active-close",
                ":dataset record source:$logs from:next > active"
            )
            .await,
        202
    );
    let active = session.snapshot().await.unwrap().names["active"]
        .node
        .clone();
    loop {
        let progress = events.until("node-progress").await;
        if progress["node"] == active.as_str() {
            break;
        }
    }
    assert_eq!(fixture.calls.load(Ordering::SeqCst), 0);
    drop(events);
    fixture.close().await;
    assert!(
        source_cancel.is_cancelled(),
        "joined workspace close separately cancels its source"
    );
}

#[tokio::test]
async fn committed_live_analysis_drains_partial_batches_finishes_only_on_natural_eof_and_keeps_one_run()
 {
    use tokio::sync::{mpsc, oneshot};
    use wes_engine::streams::{StreamFuture, StreamSink, StreamingInvoker};
    struct Source {
        entered: mpsc::UnboundedSender<(StreamSink, CancellationToken, oneshot::Sender<()>)>,
        calls: Arc<AtomicUsize>,
    }
    impl StreamingInvoker for Source {
        fn subscribe(&self, _: Call, sink: StreamSink, token: CancellationToken) -> StreamFuture {
            self.calls.fetch_add(1, Ordering::SeqCst);
            sink.push(
                wes_core::Value::new(
                    Shape::Primitive(wes_core::Primitive::Int),
                    wes_core::Data::Int(0),
                    Default::default(),
                )
                .unwrap(),
            )
            .unwrap();
            sink.opened().unwrap();
            let (finish, completed) = oneshot::channel();
            self.entered.send((sink, token.clone(), finish)).unwrap();
            Box::pin(async move {
                tokio::select! { _ = token.cancelled() => Ok(()), _ = completed => Ok(()) }
            })
        }
    }
    for natural in [true, false] {
        let (entered, mut arrivals) = mpsc::unbounded_channel();
        let calls = Arc::new(AtomicUsize::new(0));
        let registered_calls = calls.clone();
        let fixture = Fixture::configured_owner_concurrency(
            |_| wes::web::Services::default(),
            Arc::new(move |workspace| {
                let mut capability = Capability::new(
                    ["watch"],
                    Shape::Primitive(wes_core::Primitive::Int),
                    Safety::Safe,
                );
                capability.streaming = true;
                workspace.register_provider_ports(
                    ProviderDescription::new("events", [capability], vec![]).unwrap(),
                    Arc::new(Echo(Arc::new(AtomicUsize::new(0)))),
                    Some(Arc::new(Source {
                        entered: entered.clone(),
                        calls: registered_calls.clone(),
                    })),
                )?;
                Ok(())
            }),
            |root, values| {
                let datasets = wes_adapters::datasets::DatasetStore::open(
                    &root.join("datasets"),
                    Durability::File,
                    Default::default(),
                )
                .unwrap();
                wes_engine::storage::spawn_storage(values, datasets, StoreWorkerLimits::default())
                    .unwrap()
            },
            NonZeroUsize::new(1).unwrap(),
        )
        .await;
        let mut events = fixture.stream().await;
        let generation = events.generation().await;
        let session = fixture.app.current().unwrap().session;
        assert_eq!(
            fixture
                .source(&generation, "source", "events watch > logs")
                .await,
            202
        );
        let (sink, source_cancel, finish) = arrivals.recv().await.unwrap();
        session.wait_idle().await.unwrap();
        assert_eq!(
            fixture
                .source(
                    &generation,
                    "record",
                    ":dataset record source:$logs from:next > recording"
                )
                .await,
            202
        );
        tokio::time::timeout(Duration::from_secs(5), session.wait_idle())
            .await
            .unwrap()
            .unwrap();
        let record_node = session.snapshot().await.unwrap().names["recording"]
            .node
            .clone();
        let code = r#":package load source:"types: {IntStep: {base: Record, fields: {state: Int, outputs: 'List<Int>'}}}"
:def fold(state:Int, context:Int, item:Int) -> IntStep as :calc pure { return {state:state+item,outputs:[item]}; }
:def finish(state:Int, context:Int, end:Unknown) -> IntStep as :calc pure { return {state:state+100,outputs:[999]}; }
:scan source:$recording.dataset follow:true budget:LiveAnalysis transition:fold finish:finish initial:0 context:0 profile:TypedRecords sink:dataset > analysis"#;
        assert_eq!(fixture.source(&generation, "analyze", code).await, 202);
        tokio::time::timeout(Duration::from_secs(5), session.wait_idle())
            .await
            .unwrap()
            .unwrap();
        let initial = session.snapshot().await.unwrap();
        let analysis = initial
            .names
            .get("analysis")
            .unwrap_or_else(|| panic!("scan declaration missing: {:?}", initial.execution.errors))
            .node
            .clone();
        assert!(
            !initial.execution.errors.contains_key(&analysis),
            "{:?}",
            initial.execution.errors
        );
        assert_eq!(
            initial.execution.graph.node(&analysis).unwrap().state(),
            wes_engine::graph::NodeState::Ready
        );
        assert!(initial.execution.executing.contains(&analysis));
        assert!(!initial.execution.streaming.contains(&analysis));
        let run = initial.execution.runs[&analysis].clone();
        let wes_core::Data::Record(prefix) = initial.execution.values[&analysis].data() else {
            panic!("usable initial analysis prefix")
        };
        let wes_core::Data::Dataset(empty_output) = &prefix["outputs"] else {
            panic!("dataset output")
        };
        assert_eq!(empty_output.records(), 0);
        assert_eq!(
            fixture
                .source(
                    &generation,
                    "ordinary",
                    ":calc pure { return 42; } > answer"
                )
                .await,
            202
        );
        tokio::time::timeout(Duration::from_secs(5), session.wait_idle())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            session.snapshot().await.unwrap().execution.values
                [&session.snapshot().await.unwrap().names["answer"].node]
                .data(),
            &wes_core::Data::Int(42)
        );
        for value in [7, 11] {
            sink.send(
                wes_core::Value::new(
                    Shape::Primitive(wes_core::Primitive::Int),
                    wes_core::Data::Int(value),
                    Default::default(),
                )
                .unwrap(),
            )
            .await
            .unwrap();
        }
        if natural {
            finish.send(()).unwrap();
        } else {
            assert_eq!(
                fixture
                    .source(&generation, "stop", ":dataset stop $recording > stopped")
                    .await,
                202
            );
        }
        let final_ = tokio::time::timeout(Duration::from_secs(10), async {
            loop {
                let snapshot = session.snapshot().await.unwrap();
                if !snapshot.execution.executing.contains(&record_node)
                    && !snapshot.execution.executing.contains(&analysis)
                {
                    break snapshot;
                }
                tokio::time::sleep(Duration::from_millis(2)).await;
            }
        })
        .await
        .expect("writer and analysis must join without occupying the ordinary operation slot");
        assert_eq!(final_.execution.runs[&analysis], run);
        let result = if natural {
            assert!(
                !final_.execution.errors.contains_key(&analysis),
                "{:?}",
                final_.execution.errors
            );
            &final_.execution.values[&analysis]
        } else {
            assert!(final_.execution.errors.contains_key(&analysis));
            &final_.execution.evidence_values[&analysis].value
        };
        let wes_core::Data::Record(result) = result.data() else {
            panic!("final analysis")
        };
        assert_eq!(
            result["state"],
            wes_core::Data::Int(if natural { 118 } else { 18 })
        );
        let wes_core::Data::Dataset(outputs) = &result["outputs"] else {
            panic!("final output")
        };
        assert_eq!(
            outputs.records(),
            if natural { 3 } else { 2 },
            "all consumed rows must be committed before an incomplete end"
        );
        let checkpoint = fixture
            .worker
            .dataset_checkpoint(outputs.as_ref().clone())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(checkpoint.finish_applied, natural);
        assert_eq!(checkpoint.next_ordinal, 2);
        assert_eq!(
            checkpoint
                .followed_source
                .as_ref()
                .unwrap()
                .prefix
                .records(),
            2
        );
        assert_eq!(
            calls.load(Ordering::SeqCst),
            1,
            "following must never dispatch a second producer"
        );
        if !natural {
            assert!(
                !source_cancel.is_cancelled(),
                "Stop recording must leave its producer open"
            );
        }
        drop(events);
        fixture.close().await;
    }
}

#[tokio::test]
async fn original_source_excerpts_preserve_bytes_ranges_and_owned_capture_after_restart() {
    let fixture = Fixture::datasets().await;
    let mut events = fixture.stream().await;
    let generation = events.generation().await;
    let source = r#":package load source:"types: {CountStep: {base: Record, fields: {state: Int, outputs: 'List<Int>'}}}"
:def count(state:Int, context:Int, item:Unknown) -> CountStep as :calc pure { return {state:state+1,outputs:[state+1]}; }
:calc pure { return "abc\n雪\nlast"; } > original
:scan source:$original transition:count initial:0 context:0 profile:LinesUtf8 sink:dataset > textAnalysis
:type check "abc\n雪\nlast" as:Bytes > originalBytes
:scan source:$originalBytes transition:count initial:0 context:0 profile:LinesUtf8 sink:dataset > byteAnalysis
:calc pure { return [10,20,30]; } > originalRows
:scan source:$originalRows transition:count initial:0 context:0 profile:TypedRecords sink:dataset > rowAnalysis
:scan source:$rowAnalysis.outputs transition:count initial:0 context:0 profile:TypedRecords sink:dataset > datasetAnalysis"#;
    assert_eq!(fixture.source(&generation, "analyses", source).await, 202);
    let session = fixture.app.current().unwrap().session;
    session.wait_idle().await.unwrap();
    let initial = session.snapshot().await.unwrap();
    let text_node = initial.names["textAnalysis"].node.clone();
    let run = initial.execution.runs[&text_node].to_string();
    let original = &initial.execution.values[&text_node];
    let wes_core::Data::Record(fields) = original.data() else {
        panic!("scan result")
    };
    let wes_core::Data::Dataset(output) = &fields["outputs"] else {
        panic!("dataset")
    };
    let output = output.as_ref().clone();
    let checkpoint = fixture
        .worker
        .dataset_checkpoint(output.clone())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        fixture
            .source(
                &generation,
                "excerpts",
                r#":scan excerpt $textAnalysis from:4 limit:3 > snow
:scan excerpt $textAnalysis from:0 limit:4 > first
:scan excerpt $byteAnalysis from:5 limit:1 > rawByte
:scan excerpt $rowAnalysis from:1 limit:2 > rows
:scan excerpt $datasetAnalysis from:1 limit:2 > datasetRows
:scan excerpt $textAnalysis from:12 limit:10 > ending
:scan excerpt $textAnalysis from:5 limit:2 > badBoundary
:scan excerpt $rowAnalysis from:0 limit:101 > badRows
:scan excerpt $textAnalysis from:99 limit:1 > outside"#
            )
            .await,
        202
    );
    session.wait_idle().await.unwrap();
    let snap = session.snapshot().await.unwrap();
    let data = |name: &str| match snap
        .execution
        .values
        .get(
            &snap
                .names
                .get(name)
                .unwrap_or_else(|| panic!("missing name {name}: {:?}", snap.names))
                .node,
        )
        .unwrap_or_else(|| panic!("missing {name}: {:?}", snap.execution.errors))
        .data()
    {
        wes_core::Data::Record(fields) => fields,
        _ => panic!("excerpt record"),
    };
    assert_eq!(data("snow")["data"], wes_core::Data::Text("雪".into()));
    assert_eq!(data("snow")["unit"], wes_core::Data::Text("bytes".into()));
    assert_eq!(data("snow")["from"], wes_core::Data::Text("4".into()));
    assert_eq!(data("snow")["next"], wes_core::Data::Text("7".into()));
    assert_eq!(
        data("snow")["captureDigest"],
        wes_core::Data::Text(checkpoint.source.digest.clone().into())
    );
    assert_eq!(data("first")["data"], wes_core::Data::Text("abc\n".into()));
    assert_eq!(
        data("rawByte")["data"],
        wes_core::Data::Bytes(vec![0x9b].into())
    );
    assert_eq!(
        data("rows")["data"],
        wes_core::Data::List(vec![wes_core::Data::Int(20), wes_core::Data::Int(30)])
    );
    assert_eq!(data("rows")["unit"], wes_core::Data::Text("records".into()));
    assert_eq!(
        data("datasetRows")["data"],
        wes_core::Data::List(vec![wes_core::Data::Int(2), wes_core::Data::Int(3)])
    );
    assert_eq!(data("ending")["data"], wes_core::Data::Text("".into()));
    assert_eq!(
        data("ending")["extentExhausted"],
        wes_core::Data::Bool(true)
    );
    for name in ["badBoundary", "badRows", "outside"] {
        assert!(
            snap.execution.errors.contains_key(&snap.names[name].node),
            "{name} was not refused"
        );
    }
    for (cell, command) in [
        (
            "field",
            ":scan excerpt $textAnalysis.outputs from:0 limit:1",
        ),
        (
            "literal",
            ":scan excerpt \"00000000-0000-4000-8000-000000000000\" from:0 limit:1",
        ),
        (
            "noncanonical",
            ":scan excerpt $textAnalysis from:\"01\" limit:1",
        ),
    ] {
        let reply = session
            .submit(wes_engine::source::SourceInput::new(cell.into(), command.into()).unwrap())
            .await
            .unwrap();
        assert!(
            reply
                .diagnostics
                .diagnostics
                .iter()
                .any(|d| d.severity == wes_language::Severity::Error),
            "{reply:?}"
        );
        session.wait_idle().await.unwrap();
        let refused = session.snapshot().await.unwrap();
        assert!(
            reply
                .nodes
                .iter()
                .all(|node| !refused.execution.values.contains_key(node))
        );
    }
    assert_eq!(fixture.source(&generation,"copy",":calc pure { return $textAnalysis.receipt; } > copied\n:scan excerpt $copied from:0 limit:1 > refusedCopy").await,202);
    session.wait_idle().await.unwrap();
    let snap = session.snapshot().await.unwrap();
    assert!(
        snap.execution
            .errors
            .contains_key(&snap.names["refusedCopy"].node)
    );
    // Rebinding the live variable does not replace the protected source capture.
    assert_eq!(fixture.source(&generation,"replace",":calc pure { return \"different\"; } > original\n:scan excerpt $textAnalysis from:4 limit:3 > stillSnow").await,202);
    session.wait_idle().await.unwrap();
    let snap = session.snapshot().await.unwrap();
    let wes_core::Data::Record(fields) =
        snap.execution.values[&snap.names["stillSnow"].node].data()
    else {
        panic!("excerpt")
    };
    assert_eq!(fields["data"], wes_core::Data::Text("雪".into()));
    assert_eq!(fixture.calls.load(Ordering::SeqCst), 0);
    drop(events);
    fixture.server.shutdown().await.unwrap();
    fixture.app.shutdown().await;
    fixture.task.join().await.unwrap();
    fixture.worker.shutdown().await.unwrap();
    fixture.writer.join().await.unwrap();
    let values = TieredValues::open(
        &fixture.root.path().join("live"),
        &fixture.root.path().join("archive"),
        Limits::default(),
        Durability::File,
        None,
    )
    .unwrap();
    let datasets = wes_adapters::datasets::DatasetStore::open(
        &fixture.root.path().join("datasets"),
        Durability::File,
        wes_adapters::datasets::StoreLimits::default(),
    )
    .unwrap();
    let (worker, task) =
        wes_engine::storage::spawn_storage(values, datasets, StoreWorkerLimits::default()).unwrap();
    let restored = worker
        .dataset_source_excerpt(run.clone(), 4, 3)
        .await
        .unwrap();
    let wes_core::Data::Record(fields) = restored.data() else {
        panic!("excerpt")
    };
    assert_eq!(fields["data"], wes_core::Data::Text("雪".into()));
    assert_eq!(
        fields["captureDigest"],
        wes_core::Data::Text(checkpoint.source.digest.into())
    );
    worker.dataset_withdraw(output).await.unwrap();
    assert!(
        worker.dataset_source_excerpt(run, 4, 3).await.is_err(),
        "withdrawal suppresses the complete reply, not only data"
    );
    worker.shutdown().await.unwrap();
    task.join().await.unwrap();
}

#[tokio::test]
async fn native_local_reconcile_selects_original_analysis_and_never_replays_its_producer() {
    let fixture = Fixture::datasets().await;
    let mut events = fixture.stream().await;
    let generation = events.generation().await;
    let session = fixture.app.current().unwrap().session;
    assert_eq!(fixture.source(&generation,"reconcile-types",r#":package load source:"types: {IntStep: {base: Record, fields: {state: Int, outputs: 'List<Int>'}}}""#).await,202);
    assert_eq!(fixture.source(&generation,"reconcile-analysis",":def sum(state:Int, context:Int, item:Int) -> IntStep as :calc pure { return {state:state+item,outputs:[item]}; }\n:calc { return [1,2,3]; } > raw\n:scan source:$raw transition:sum initial:0 context:0 profile:TypedRecords sink:dataset > analysis").await,202);
    session.wait_idle().await.unwrap();
    let snapshot = session.snapshot().await.unwrap();
    let analysis = snapshot.names["analysis"].node.clone();
    let run = snapshot.execution.runs[&analysis].to_string();
    let original = snapshot.execution.values[&analysis].clone();
    assert_eq!(fixture.source(&generation,"local-receipts",&format!(":scan reconcile $analysis run:\"{run}\" > receipt\n:scan reconcile $analysis > again\n:calc {{ return $analysis; }} > copied\n:scan reconcile $copied > copiedReceipt\n:scan reconcile $analysis run:\"{}\" > wrongRun",uuid::Uuid::new_v4())).await,202);
    session.wait_idle().await.unwrap();
    let snapshot = session.snapshot().await.unwrap();
    for name in ["receipt", "again"] {
        let node = &snapshot.names[name].node;
        let value = snapshot.execution.values.get(node).unwrap_or_else(|| {
            panic!(
                "reconcile failed: {:?}",
                snapshot.execution.errors.get(node)
            )
        });
        let wes_core::Data::Record(fields) = value.data() else {
            panic!("receipt record")
        };
        assert_eq!(
            fields["writeOutcome"],
            wes_core::Data::Text("committed".into())
        );
        assert_eq!(
            fields["selectedRun"],
            wes_core::Data::Text(run.clone().into())
        );
        assert_eq!(
            fields["committedRecords"],
            wes_core::Data::Option(Some(Box::new(wes_core::Data::Text("3".into()))))
        );
        assert_eq!(fields["executionUnknown"], wes_core::Data::Bool(true));
    }
    for name in ["copiedReceipt", "wrongRun"] {
        assert!(
            snapshot
                .execution
                .errors
                .contains_key(&snapshot.names[name].node),
            "invalid control {name}"
        );
    }
    assert_eq!(snapshot.execution.values[&analysis], original);
    assert_eq!(snapshot.execution.runs[&analysis].as_str(), run);
    assert_eq!(fixture.calls.load(Ordering::SeqCst), 0);
    drop(events);
    fixture.close().await;
}

#[tokio::test]
async fn native_local_reconcile_distinguishes_recording_owners_on_one_source_and_refuses_live_work()
{
    let (fixture, calls, mut arrivals) = recording_launch_fixture(true).await;
    let mut events = fixture.stream().await;
    let generation = events.generation().await;
    let session = fixture.app.current().unwrap().session;
    assert_eq!(
        fixture
            .source(
                &generation,
                "first-recording",
                "events watch > logs | :dataset record > first"
            )
            .await,
        202
    );
    let (sink, cancelled) = tokio::time::timeout(Duration::from_secs(5), arrivals.recv())
        .await
        .unwrap()
        .unwrap();
    session.wait_idle().await.unwrap();
    assert_eq!(fixture.source(&generation,"second-recording",":dataset record source:$logs from:next > second\n:dataset reconcile $first > activeRefusal").await,202);
    session.wait_idle().await.unwrap();
    let snapshot = session.snapshot().await.unwrap();
    assert!(
        snapshot
            .execution
            .errors
            .contains_key(&snapshot.names["activeRefusal"].node)
    );
    let first_run = snapshot.execution.runs[&snapshot.names["first"].node].to_string();
    let second_run = snapshot.execution.runs[&snapshot.names["second"].node].to_string();
    let source_run = snapshot.execution.runs[&snapshot.names["logs"].node].to_string();
    assert_ne!(first_run, second_run);
    assert_ne!(first_run, source_run);
    assert_ne!(second_run, source_run);
    for n in [8, 9] {
        sink.push(
            wes_core::Value::new(
                Shape::Primitive(wes_core::Primitive::Int),
                wes_core::Data::Int(n),
                Default::default(),
            )
            .unwrap(),
        )
        .unwrap();
    }
    assert_eq!(
        fixture
            .source(
                &generation,
                "stop-both",
                ":dataset stop $first > stoppedFirst\n:dataset stop $second > stoppedSecond"
            )
            .await,
        202
    );
    session.wait_idle().await.unwrap();
    assert_eq!(fixture.source(&generation,"recover-both",&format!(":dataset reconcile $first run:\"{first_run}\" > firstReceipt\n:dataset reconcile $second run:\"{second_run}\" > secondReceipt")).await,202);
    session.wait_idle().await.unwrap();
    let snapshot = session.snapshot().await.unwrap();
    for (name, run, count) in [
        ("firstReceipt", &first_run, "3"),
        ("secondReceipt", &second_run, "2"),
    ] {
        let node = &snapshot.names[name].node;
        let value = snapshot.execution.values.get(node).unwrap_or_else(|| {
            panic!(
                "reconcile failed: {:?}",
                snapshot.execution.errors.get(node)
            )
        });
        let wes_core::Data::Record(fields) = value.data() else {
            panic!("receipt record")
        };
        assert_eq!(
            fields["selectedRun"],
            wes_core::Data::Text(run.clone().into())
        );
        assert_eq!(
            fields["writeOutcome"],
            wes_core::Data::Text("committed".into())
        );
        assert_eq!(
            fields["committedRecords"],
            wes_core::Data::Option(Some(Box::new(wes_core::Data::Text(count.into()))))
        );
        assert_eq!(fields["executionUnknown"], wes_core::Data::Bool(true));
    }
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    assert!(!cancelled.is_cancelled());
    drop(sink);
    drop(events);
    fixture.close().await;
}

#[tokio::test]
async fn native_store_recovery_reports_only_owned_local_sync_and_never_starts_a_provider() {
    let fixture = Fixture::datasets().await;
    let mut events = fixture.stream().await;
    let generation = events.generation().await;
    let session = fixture.app.current().unwrap().session;
    assert_eq!(
        fixture
            .source(
                &generation,
                "store-recovery",
                ":dataset reconcile > recovery"
            )
            .await,
        202
    );
    session.wait_idle().await.unwrap();
    let snapshot = session.snapshot().await.unwrap();
    let value = snapshot
        .execution
        .values
        .get(&snapshot.names["recovery"].node)
        .unwrap_or_else(|| {
            panic!(
                "store recovery: {:?}",
                snapshot
                    .execution
                    .errors
                    .get(&snapshot.names["recovery"].node)
            )
        });
    let wes_core::Data::Record(fields) = value.data() else {
        panic!("recovery receipt");
    };
    assert_eq!(fields.len(), 4);
    assert_eq!(fields["scope"], wes_core::Data::Text("owned_store".into()));
    assert_eq!(fields["status"], wes_core::Data::Text("reconciled".into()));
    assert_eq!(fields["executionUnknown"], wes_core::Data::Bool(true));
    assert_eq!(fixture.calls.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn dataset_snapshot_captures_the_exact_shown_extension_without_keep_or_source_replay() {
    use wes_core::{Data, DatasetRef};
    let (fixture, calls, mut arrivals) = recording_launch_fixture(true).await;
    let mut events = fixture.stream().await;
    let generation = events.generation().await;
    let session = fixture.app.current().unwrap().session;
    assert_eq!(
        fixture
            .source(
                &generation,
                "launch",
                "events watch > logs | :dataset record > recording"
            )
            .await,
        202
    );
    let (sink, cancel) = tokio::time::timeout(Duration::from_secs(5), arrivals.recv())
        .await
        .unwrap()
        .unwrap();
    session.wait_idle().await.unwrap();
    let snapshot = session.snapshot().await.unwrap();
    let node = snapshot.names["recording"].node.clone();
    let Data::Record(fields) = snapshot.execution.values[&node].data() else {
        panic!("recording setup")
    };
    let Data::Dataset(anchor) = &fields["dataset"] else {
        panic!("dataset anchor")
    };
    let anchor = anchor.as_ref().clone();
    let metadata = snapshot.execution.values[&node].metadata().cloned();
    assert_eq!(anchor.records(), 0);
    let shown = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let head = fixture
                .worker
                .dataset_head(anchor.clone())
                .await
                .unwrap()
                .reference;
            if head.records() == 1 {
                break head;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    let command = |selection: &str, basis: &DatasetRef, target: &DatasetRef, name: &str| {
        format!(
            ":dataset snapshot {selection} basis:\"{}\" generation:\"{}\" digest:\"{}\" > {name}",
            basis.manifest_digest(),
            target.generation(),
            target.manifest_digest()
        )
    };
    assert_eq!(
        fixture
            .source(
                &generation,
                "capture",
                &command("$recording.dataset", &anchor, &shown, "shownPrefix")
            )
            .await,
        202
    );
    session.wait_idle().await.unwrap();
    let snapshot = session.snapshot().await.unwrap();
    let captured = &snapshot.execution.values[&snapshot.names["shownPrefix"].node];
    assert_eq!(captured.data(), &Data::Dataset(Arc::new(shown.clone())));
    assert!(
        matches!(captured.shape(),Shape::Dataset(inner) if inner.as_ref()==&Shape::Primitive(wes_core::Primitive::Int))
    );
    assert!(
        metadata.is_some(),
        "recording setup captures its schema metadata"
    );
    assert_eq!(
        captured.metadata().cloned(),
        metadata.as_ref().and_then(|m| m.project("/f:dataset")),
        "the exact selected schema metadata is preserved"
    );
    let captured_node = snapshot.names["shownPrefix"].node.clone();
    let receipt = loop {
        let frame = events.until("ready").await;
        if frame["node"] == captured_node.as_str() {
            break frame;
        }
    };
    assert_eq!(
        receipt["kept"], false,
        "an open prefix result is not automatically Kept even when a recording already protects its bytes"
    );
    sink.push(
        wes_core::Value::new(
            Shape::Primitive(wes_core::Primitive::Int),
            Data::Int(8),
            Default::default(),
        )
        .unwrap(),
    )
    .unwrap();
    let later = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let head = fixture
                .worker
                .dataset_head(anchor.clone())
                .await
                .unwrap()
                .reference;
            if head.records() == 2 {
                break head;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    assert_eq!(
        fixture
            .source(
                &generation,
                "old-shown",
                &command("$recording.dataset", &anchor, &shown, "stillShown")
            )
            .await,
        202
    );
    session.wait_idle().await.unwrap();
    let snapshot = session.snapshot().await.unwrap();
    assert_eq!(
        snapshot.execution.values[&snapshot.names["stillShown"].node].data(),
        captured.data()
    );
    assert_eq!(
        fixture
            .worker
            .dataset_inspect(shown.clone())
            .await
            .unwrap()
            .lifecycle,
        wes_engine::storage::datasets::DatasetLifecycle::Prefix
    );
    assert_eq!(
        fixture
            .source(&generation, "stop", ":dataset stop $recording > stopped")
            .await,
        202
    );
    session.wait_idle().await.unwrap();
    let terminal = fixture
        .worker
        .dataset_head(anchor.clone())
        .await
        .unwrap()
        .reference;
    assert_eq!(terminal.records(), later.records());
    assert!(terminal.generation() > later.generation());
    assert_eq!(
        fixture
            .source(
                &generation,
                "terminal",
                &command("$shownPrefix", &shown, &terminal, "sealedPrefix")
            )
            .await,
        202
    );
    session.wait_idle().await.unwrap();
    let snapshot = session.snapshot().await.unwrap();
    assert_eq!(
        snapshot
            .execution
            .values
            .get(&snapshot.names["sealedPrefix"].node)
            .unwrap_or_else(|| panic!(
                "sealed capture failed: {:?}",
                snapshot
                    .execution
                    .errors
                    .get(&snapshot.names["sealedPrefix"].node)
            ))
            .data(),
        &Data::Dataset(Arc::new(terminal.clone()))
    );
    for (i, text) in [
        command("$recording.dataset", &anchor, &terminal, "changedBasis"),
        command("$shownPrefix", &shown, &later, "wrongDigest").replace(
            &format!("digest:\"{}\"", later.manifest_digest()),
            &format!("digest:\"{}\"", shown.manifest_digest()),
        ),
        command("$shownPrefix", &shown, &shown, "unavailableGeneration").replace(
            &format!("generation:\"{}\"", shown.generation()),
            "generation:\"999999\"",
        ),
        command("$recording.dataset", &anchor, &shown, "malformedGeneration").replace(
            &format!("generation:\"{}\"", shown.generation()),
            "generation:\"01\"",
        ),
        format!(":dataset snapshot $shownPrefix basis:$logs generation:\"{}\" digest:\"{}\" > referenceConstraint", shown.generation(), shown.manifest_digest()),
    ]
    .iter()
    .enumerate()
    {
        assert_eq!(
            fixture
                .source(&generation, &format!("refused-{i}"), text)
                .await,
            202
        );
    }
    session.wait_idle().await.unwrap();
    let snapshot = session.snapshot().await.unwrap();
    for name in ["changedBasis", "wrongDigest", "unavailableGeneration"] {
        assert!(
            snapshot
                .execution
                .errors
                .contains_key(&snapshot.names[name].node),
            "{name} must refuse"
        );
    }
    assert!(!snapshot.names.contains_key("malformedGeneration"));
    assert!(!snapshot.names.contains_key("referenceConstraint"));
    fixture.worker.dataset_withdraw(terminal).await.unwrap();
    assert_eq!(
        fixture
            .source(
                &generation,
                "withdrawn",
                &command("$shownPrefix", &shown, &shown, "withdrawnPrefix")
            )
            .await,
        202
    );
    session.wait_idle().await.unwrap();
    let snapshot = session.snapshot().await.unwrap();
    assert!(
        snapshot
            .execution
            .errors
            .contains_key(&snapshot.names["withdrawnPrefix"].node)
    );
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    assert!(
        !cancel.is_cancelled(),
        "capture and Stop do not cancel the separately owned source"
    );
    drop(events);
    fixture.close().await;
}

#[tokio::test]
async fn dataset_retention_preview_reports_exact_transitive_cost_and_refuses_changed_or_withdrawn_inputs()
 {
    use wes_core::Data;
    let fixture = Fixture::datasets().await;
    let mut events = fixture.stream().await;
    let generation = events.generation().await;
    let session = fixture.app.current().unwrap().session;
    let source = r#":package load source:"types: {FootprintStep: {base: Record, fields: {state: Int, outputs: 'List<Int>'}}}"
:def footprint(state:Int, context:Int, item:Int) -> FootprintStep as :calc pure {return {state:state+1,outputs:[item]};}
:calc pure {return [10,20,30];} > original
:scan source:$original transition:footprint initial:0 context:0 profile:TypedRecords sink:dataset > analysis"#;
    assert_eq!(fixture.source(&generation, "analysis", source).await, 202);
    session.wait_idle().await.unwrap();
    let snapshot = session.snapshot().await.unwrap();
    let Data::Record(fields) = snapshot.execution.values[&snapshot.names["analysis"].node].data()
    else {
        panic!("analysis");
    };
    let Data::Dataset(reference) = &fields["outputs"] else {
        panic!("output");
    };
    let reference = reference.as_ref().clone();
    let cost = fixture
        .worker
        .dataset_retention_preview(reference.clone())
        .await
        .unwrap();
    assert!(cost.captured_source_bytes > 0);
    assert_eq!(cost.total_bytes, cost.shared_bytes + cost.exclusive_bytes);
    assert!(cost.total_bytes > reference.manifest_bytes());
    let command = format!(
        ":dataset retention $analysis.outputs basis:\"{}\" > retention",
        reference.manifest_digest()
    );
    assert_eq!(fixture.source(&generation, "preview", &command).await, 202);
    session.wait_idle().await.unwrap();
    let snapshot = session.snapshot().await.unwrap();
    let value = snapshot
        .execution
        .values
        .get(&snapshot.names["retention"].node)
        .unwrap_or_else(|| {
            panic!(
                "preview: {:?}",
                snapshot
                    .execution
                    .errors
                    .get(&snapshot.names["retention"].node)
            )
        });
    let Data::Record(fields) = value.data() else {
        panic!("typed preview");
    };
    assert_eq!(
        fields["generation"],
        Data::Text(reference.generation().to_string().into())
    );
    assert_eq!(fields["records"], Data::Text("3".into()));
    assert_eq!(
        fields["totalBytes"],
        Data::Text(cost.total_bytes.to_string().into())
    );
    assert_eq!(
        fields["sharedBytes"],
        Data::Text(cost.shared_bytes.to_string().into())
    );
    assert_eq!(
        fields["exclusiveBytes"],
        Data::Text(cost.exclusive_bytes.to_string().into())
    );
    assert_eq!(
        fields["capturedSourceBytes"],
        Data::Text(cost.captured_source_bytes.to_string().into())
    );
    assert!(value.metadata().is_some());
    assert!(
        value
            .provenance()
            .policy()
            .dataset_reads()
            .iter()
            .any(|origin| origin.dataset() == reference.dataset())
    );
    assert_eq!(
        fixture
            .worker
            .dataset_head(reference.clone())
            .await
            .unwrap()
            .reference,
        reference,
        "preview never changes the head"
    );
    let mismatch = command
        .replace(
            reference.manifest_digest(),
            &format!("sha256:{}", "f".repeat(64)),
        )
        .replace("> retention", "> changed");
    assert_eq!(fixture.source(&generation, "changed", &mismatch).await, 202);
    session.wait_idle().await.unwrap();
    let snapshot = session.snapshot().await.unwrap();
    assert!(
        snapshot
            .execution
            .errors
            .contains_key(&snapshot.names["changed"].node)
    );
    fixture.worker.dataset_withdraw(reference).await.unwrap();
    assert_eq!(
        fixture
            .source(
                &generation,
                "withdrawn",
                &command.replace("> retention", "> withdrawn")
            )
            .await,
        202
    );
    session.wait_idle().await.unwrap();
    let snapshot = session.snapshot().await.unwrap();
    assert!(
        snapshot
            .execution
            .errors
            .contains_key(&snapshot.names["withdrawn"].node)
    );
    assert_eq!(fixture.calls.load(Ordering::SeqCst), 0);
    drop(events);
    fixture.close().await;
}
