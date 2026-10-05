//! Controlled storage completion exercises the real engine → SSE projection boundary.
use super::*;
use wes_engine::storage::{LoadedValue, StoreError, ValueHandle, ValueStore};

struct GatedStore {
    values: TieredValues,
    gate: std::sync::mpsc::Receiver<bool>,
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
