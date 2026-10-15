//! Controlled storage completion exercises the real engine → SSE projection boundary.
use super::*;
use wes_engine::storage::{LoadedValue, StoreError, ValueHandle, ValueStore};

struct GatedStore {
    values: TieredValues,
    gate: std::sync::mpsc::Receiver<bool>,
}

/// Refuse storage reads only after the producer's exact output has been published.
struct RefusingReads {
    values: TieredValues,
    refusal: Arc<std::sync::Mutex<Option<wes_engine::storage::ReadFailureKind>>>,
}
impl RefusingReads {
    fn check(&self, handle: &ValueHandle) -> Result<(), StoreError> {
        use wes_engine::storage::ReadFailureKind::*;
        Err(match *self.refusal.lock().unwrap() {
            None => return Ok(()),
            Some(Capacity) => StoreError::Limit("fixture read memory"),
            Some(Forbidden) => StoreError::Restricted,
            Some(Withdrawn) => StoreError::DatasetWithdrawn,
            Some(Stale) => StoreError::Conflict,
            Some(Unavailable) => StoreError::MissingValue,
            Some(Closed) => StoreError::Closed,
            Some(Corrupt) => StoreError::Corrupt,
            Some(Uncertain) => StoreError::Published {
                handle: handle.clone(),
                source: Box::new(std::io::Error::other("PRIVATE_READ_CAUSE")),
            },
            Some(Failed) => StoreError::backend(
                "reading fixture result",
                std::io::Error::other("PRIVATE_READ_CAUSE"),
            ),
        })
    }
}
impl ValueStore for RefusingReads {
    fn read_policy(
        &self,
        h: &ValueHandle,
    ) -> Result<Option<wes_core::flow::FlowPolicy>, StoreError> {
        self.check(h)?;
        self.values.read_policy(h)
    }
    fn store(&mut self, value: &wes_core::Value) -> Result<ValueHandle, StoreError> {
        self.values.store(value)
    }
    fn read(&self, h: &ValueHandle) -> Result<Option<LoadedValue>, StoreError> {
        self.check(h)?;
        self.values.read(h)
    }
    fn describe(
        &self,
        h: &ValueHandle,
        path: &[String],
    ) -> Result<Option<wes_engine::storage::StoredSnapshot>, StoreError> {
        self.check(h)?;
        self.values.describe(h, path)
    }
    fn encoded(&self, h: &ValueHandle) -> Result<Option<Vec<u8>>, StoreError> {
        self.values.encoded(h)
    }
    fn digest(&self, h: &ValueHandle) -> Result<Option<String>, StoreError> {
        self.values.digest(h)
    }
    fn size(&self, h: &ValueHandle) -> Result<Option<u64>, StoreError> {
        self.values.size(h)
    }
    fn hold(
        &mut self,
        h: &ValueHandle,
    ) -> Result<Option<wes_engine::storage::ValueLease>, StoreError> {
        self.values.hold(h)
    }
    fn keep(&mut self, h: &ValueHandle) -> Result<bool, StoreError> {
        self.values.keep(h)
    }
    fn is_kept(&self, h: &ValueHandle) -> Result<bool, StoreError> {
        self.values.is_kept(h)
    }
    fn release(&mut self, h: &ValueHandle) -> Result<bool, StoreError> {
        self.values.release(h)
    }
}

#[tokio::test]
async fn stored_input_refusals_reach_execution_events_and_http_without_replaying_sources() {
    use wes_engine::storage::ReadFailureKind::*;
    struct BytesSource(Arc<AtomicUsize>);
    impl Invoker for BytesSource {
        fn invoke(&self, _: Call, _: CancellationToken) -> InvocationFuture {
            self.0.fetch_add(1, Ordering::SeqCst);
            Box::pin(async {
                Ok(wes_core::Value::new(
                    Shape::Primitive(wes_core::Primitive::Bytes),
                    wes_core::Data::Bytes(vec![4; 6000].into()),
                    Default::default(),
                )
                .unwrap())
            })
        }
    }
    for (kind, code, status, read_code) in [
        (Capacity, "RUN007", 413, "VALUE_CAPACITY"),
        (Unavailable, "RUN008", 404, "VALUE_UNAVAILABLE"),
        (Forbidden, "RUN009", 403, "VALUE_ACCESS_REFUSED"),
        (Withdrawn, "RUN009", 403, "VALUE_ACCESS_WITHDRAWN"),
        (Stale, "RUN010", 409, "VALUE_REVISION_CONFLICT"),
        (Closed, "RUN011", 503, "VALUE_OWNER_UNAVAILABLE"),
        (Corrupt, "RUN012", 500, "VALUE_CORRUPT"),
        (Uncertain, "RUN013", 409, "VALUE_STORAGE_UNCONFIRMED"),
        (Failed, "RUN014", 500, "VALUE_READ_FAILED"),
    ] {
        let refusal = Arc::new(std::sync::Mutex::new(None));
        let owner_refusal = refusal.clone();
        let calls = Arc::new(AtomicUsize::new(0));
        let invoked = calls.clone();
        let fixture = Fixture::configured_store(
            |_| wes::web::Services::default(),
            Arc::new(move |workspace| {
                workspace
                    .register_provider(
                        ProviderDescription::new(
                            "stored_source",
                            [Capability::new(
                                ["read"],
                                Shape::Primitive(wes_core::Primitive::Bytes),
                                Safety::Safe,
                            )],
                            vec![],
                        )
                        .unwrap(),
                        Arc::new(BytesSource(invoked.clone())),
                    )
                    .map(|_| ())
            }),
            |values| {
                let (worker, task) = spawn_store(
                    RefusingReads {
                        values,
                        refusal: owner_refusal,
                    },
                    StoreWorkerLimits::default(),
                )
                .unwrap();
                (
                    worker.with_resident_memory(wes_engine::storage::ResidentMemory::new(
                        20000, 10000,
                    )),
                    task,
                )
            },
        )
        .await;
        let current = fixture.app.current().unwrap();
        let generation = current.generation.clone();
        assert_eq!(
            fixture
                .post(
                    &generation,
                    json!({"request":"keeping", "automatic":false, "under":1024})
                )
                .await
                .status(),
            202
        );
        for name in ["first", "second", "third"] {
            assert_eq!(
                fixture
                    .source(&generation, name, &format!("stored_source read > {name}"))
                    .await,
                202
            );
            current.session.wait_idle().await.unwrap();
        }
        let before = current.session.snapshot().await.unwrap();
        let source = before.names["first"].node.clone();
        assert!(!before.execution.values.contains_key(&source));
        let captured = before.execution.stored_values[&source].clone();
        let mut events = fixture.stream().await;
        assert_eq!(events.generation().await, generation);
        *refusal.lock().unwrap() = Some(kind);
        let response = fixture
            .client
            .get(fixture.url(&format!("/values/{}", captured.snapshot.handle)))
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), status, "{kind:?}");
        let text = response.text().await.unwrap();
        assert!(!text.contains("PRIVATE_READ_CAUSE"));
        let body: Value = serde_json::from_str(&text).unwrap();
        assert_eq!(body["error"]["code"], read_code);
        assert_eq!(body["error"]["retryable"], false);
        assert_eq!(
            body["error"]["context"]["handle"],
            captured.snapshot.handle.as_str()
        );
        assert_eq!(
            fixture
                .source(
                    &generation,
                    "consumer",
                    "catalog echo value:$first > consumer"
                )
                .await,
            202
        );
        current.session.wait_idle().await.unwrap();
        let after = current.session.snapshot().await.unwrap();
        let consumer = &after.names["consumer"].node;
        let diagnostic = &after.execution.errors[consumer];
        assert_eq!(diagnostic.code(), code, "{kind:?}: {diagnostic:?}");
        assert!(!diagnostic.message().contains("PRIVATE_READ_CAUSE"));
        let failed = loop {
            let frame = events.next().await;
            if frame["event"] == "failed" && frame["node"] == consumer.as_str() {
                break frame;
            }
        };
        assert_eq!(failed["error"]["code"], code);
        assert!(
            failed["error"]["message"]
                .as_str()
                .unwrap()
                .contains("No provider was replayed")
        );
        assert_eq!(after.execution.stored_values[&source], captured);
        assert_eq!(
            after.execution.graph.node(&source).unwrap().state(),
            wes_engine::graph::NodeState::Ready
        );
        assert_eq!(calls.load(Ordering::SeqCst), 3, "source replayed");
        assert_eq!(
            fixture.calls.load(Ordering::SeqCst),
            0,
            "consumer entered despite read refusal"
        );
        assert!(
            fixture
                .worker
                .size(captured.snapshot.handle.clone())
                .await
                .unwrap()
                .is_some()
        );
        // Identity/generation changes have their own structured refusal, independent of storage.
        let response = fixture
            .client
            .get(fixture.url(&format!("/values/{}", captured.snapshot.handle)))
            .header("X-Wes-Session", "other-generation")
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), 409);
        let body: Value = serde_json::from_str(&response.text().await.unwrap()).unwrap();
        assert_eq!(body["error"]["code"], "VALUE_SESSION_CHANGED");
        *refusal.lock().unwrap() = None;
        drop(events);
        fixture.close().await;
    }
}

#[tokio::test]
async fn cold_ready_results_keep_their_ui_handle_and_capacity_is_not_withdrawal() {
    struct BytesSource(Arc<AtomicUsize>);
    impl Invoker for BytesSource {
        fn invoke(&self, _: Call, _: CancellationToken) -> InvocationFuture {
            self.0.fetch_add(1, Ordering::SeqCst);
            Box::pin(async {
                Ok(wes_core::Value::new(
                    Shape::Primitive(wes_core::Primitive::Bytes),
                    // Larger than the shared resident budget even after every cache drops.
                    wes_core::Data::Bytes(vec![4; 20001].into()),
                    Default::default(),
                )
                .unwrap())
            })
        }
    }
    let calls = Arc::new(AtomicUsize::new(0));
    let invoked = calls.clone();
    let fixture = Fixture::configured_store(
        |_| wes::web::Services::default(),
        Arc::new(move |workspace| {
            workspace
                .register_provider(
                    ProviderDescription::new(
                        "bytes_source",
                        [Capability::new(
                            ["read"],
                            Shape::Primitive(wes_core::Primitive::Bytes),
                            Safety::Safe,
                        )],
                        vec![],
                    )
                    .unwrap(),
                    Arc::new(BytesSource(invoked.clone())),
                )
                .map(|_| ())
        }),
        |values| {
            let (worker, task) = spawn_store(values, StoreWorkerLimits::default()).unwrap();
            (
                worker.with_resident_memory(wes_engine::storage::ResidentMemory::new(20000, 10000)),
                task,
            )
        },
    )
    .await;
    let generation = fixture.app.current().unwrap().generation;
    assert_eq!(
        fixture
            .post(
                &generation,
                json!({"request":"keeping","automatic":false,"under":1024})
            )
            .await
            .status(),
        202
    );
    for name in ["first", "second", "third"] {
        assert_eq!(
            fixture
                .source(&generation, name, &format!("bytes_source read > {name}"))
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
    let snapshot = fixture
        .app
        .current()
        .unwrap()
        .session
        .snapshot()
        .await
        .unwrap();
    let first = &snapshot.names["first"].node;
    assert!(!snapshot.execution.values.contains_key(first));
    let captured = &snapshot.execution.stored_values[first];
    assert!(captured.snapshot.charge > fixture.worker.resident_memory().limit());
    let mut stream = fixture.stream().await;
    assert_eq!(stream.generation().await, generation);
    let ready = loop {
        let frame = stream.next().await;
        if frame["node"] == first.as_str() && frame["event"] == "ready" {
            break frame;
        }
    };
    assert_eq!(ready["handle"], captured.snapshot.handle.as_str());
    assert_eq!(ready["type"], "Bytes");
    assert_eq!(ready["publication"]["state"], "available");
    let response = fixture
        .client
        .get(fixture.url(&format!("/values/{}", captured.snapshot.handle)))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 413);
    let failure: Value = serde_json::from_str(&response.text().await.unwrap()).unwrap();
    assert_eq!(failure["error"]["code"], "VALUE_CAPACITY");
    assert_eq!(failure["error"]["retryable"], false);
    assert!(
        fixture
            .worker
            .size(captured.snapshot.handle.clone())
            .await
            .unwrap()
            .is_some()
    );
    assert_eq!(calls.load(Ordering::SeqCst), 3);
    drop(stream);
    drop(snapshot);
    fixture.close().await;
}
impl ValueStore for GatedStore {
    fn read_policy(
        &self,
        h: &ValueHandle,
    ) -> Result<Option<wes_core::flow::FlowPolicy>, StoreError> {
        self.values.read_policy(h)
    }
    fn store(&mut self, value: &wes_core::Value) -> Result<ValueHandle, StoreError> {
        // Test-only deadline guarantees cleanup even if a regression prevents a pending frame.
        if self
            .gate
            .recv_timeout(Duration::from_secs(8))
            .map_err(|_| StoreError::Closed)?
        {
            return Err(StoreError::backend(
                "synthetic publication",
                std::io::Error::other("private fixture detail"),
            ));
        }
        self.values.store(value)
    }
    fn read(&self, handle: &ValueHandle) -> Result<Option<LoadedValue>, StoreError> {
        self.values.read(handle)
    }
    fn encoded(&self, handle: &ValueHandle) -> Result<Option<Vec<u8>>, StoreError> {
        self.values.encoded(handle)
    }
    fn size(&self, handle: &ValueHandle) -> Result<Option<u64>, StoreError> {
        self.values.size(handle)
    }
    fn release(&mut self, handle: &ValueHandle) -> Result<bool, StoreError> {
        self.values.release(handle)
    }
}

#[tokio::test]
async fn successful_execution_waits_for_publication_then_reports_its_actual_outcome_without_replay()
{
    for fail in [false, true] {
        let (release, gate) = std::sync::mpsc::channel();
        let fixture = Fixture::configured_store(
            |_| wes::web::Services::default(),
            Arc::new(|_| Ok(())),
            |values| {
                spawn_store(GatedStore { values, gate }, StoreWorkerLimits::default()).unwrap()
            },
        )
        .await;
        let mut events = fixture.stream().await;
        let generation = events.generation().await;
        // No retention: the synthetic store controls only publication, not journal durability.
        assert_eq!(
            fixture
                .post(
                    &generation,
                    json!({"request":"keeping","automatic":false,"under":1024})
                )
                .await
                .status(),
            202
        );
        assert_eq!(
            fixture
                .source(&generation, "one", "catalog echo value:hello > result")
                .await,
            202
        );
        let pending = loop {
            let frame = events.next().await;
            if frame["publication"]["state"] == "pending" {
                break frame;
            }
        };
        assert_eq!(pending["state"], "ready");
        assert!(pending["publication"]["run"].is_string());
        assert!(pending["publication"]["handle"].is_null());
        assert_eq!(fixture.calls.load(Ordering::SeqCst), 1);
        release.send(fail).unwrap();
        let final_frame = loop {
            let frame = events.next().await;
            if frame["node"] == pending["node"]
                && frame["publication"]["state"]
                    .as_str()
                    .is_some_and(|s| s != "pending")
            {
                break frame;
            }
        };
        assert_eq!(
            final_frame["publication"]["run"],
            pending["publication"]["run"]
        );
        if fail {
            assert_eq!(final_frame["event"], "node");
            assert_eq!(final_frame["state"], "ready");
            assert_eq!(final_frame["publication"]["state"], "unavailable");
            assert_eq!(final_frame["publication"]["problem"]["code"], "RUN005");
            assert!(final_frame["publication"]["handle"].is_null());
            let notice = events.until("log-notice").await;
            assert_eq!(
                notice["record"]["error"],
                final_frame["publication"]["problem"]
            );
            assert_eq!(
                notice["record"]["context"]["run"],
                pending["publication"]["run"]
            );
            assert!(!final_frame.to_string().contains("private fixture detail"));
        } else {
            assert_eq!(final_frame["event"], "ready");
            assert_eq!(final_frame["publication"]["state"], "available");
            let handle = final_frame["publication"]["handle"].as_str().unwrap();
            assert_eq!(final_frame["handle"], handle);
            assert_eq!(
                fixture
                    .client
                    .get(fixture.url(&format!("/values/{handle}")))
                    .send()
                    .await
                    .unwrap()
                    .status(),
                200
            );
        }
        let mut reconnect = fixture.stream().await;
        assert_eq!(reconnect.generation().await, generation);
        loop {
            let replay = reconnect.next().await;
            if replay["node"] == final_frame["node"] && replay.get("publication").is_some() {
                assert_eq!(replay["publication"], final_frame["publication"]);
                break;
            }
        }
        assert_eq!(fixture.calls.load(Ordering::SeqCst), 1);
        drop(reconnect);
        drop(events);
        fixture.close().await;
    }
}
