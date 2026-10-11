//! Controlled storage completion exercises the real engine → SSE projection boundary.
use super::*;
use wes_engine::storage::{LoadedValue, StoreError, ValueHandle, ValueStore};

struct GatedStore {
    values: TieredValues,
    gate: std::sync::mpsc::Receiver<bool>,
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
                    wes_core::Data::Bytes(vec![4; 6000].into()),
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
