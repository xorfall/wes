//! Bounded display reads retain the ordinary authority/generation fences and never execute work.
use super::*;
use wes_engine::storage::{LoadedValue, StoreError, ValueHandle, ValueLease, ValueStore};

async fn body(response: reqwest::Response) -> Value {
    serde_json::from_str(&response.text().await.unwrap()).unwrap()
}

struct CountReads {
    values: TieredValues,
    reads: Arc<AtomicUsize>,
    inspections: Arc<AtomicUsize>,
}
impl ValueStore for CountReads {
    fn read_policy(
        &self,
        h: &ValueHandle,
    ) -> Result<Option<wes_core::flow::FlowPolicy>, StoreError> {
        self.values.read_policy(h)
    }
    fn store(&mut self, v: &wes_core::Value) -> Result<ValueHandle, StoreError> {
        self.values.store(v)
    }
    fn read(&self, h: &ValueHandle) -> Result<Option<LoadedValue>, StoreError> {
        self.reads.fetch_add(1, Ordering::SeqCst);
        self.values.read(h)
    }
    fn describe(
        &self,
        h: &ValueHandle,
        path: &[String],
    ) -> Result<Option<wes_engine::storage::StoredSnapshot>, StoreError> {
        self.inspections.fetch_add(1, Ordering::SeqCst);
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
    fn hold(&mut self, h: &ValueHandle) -> Result<Option<ValueLease>, StoreError> {
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
async fn oversized_preview_is_refused_before_payload_read_and_full_read_still_works() {
    let reads = Arc::new(AtomicUsize::new(0));
    let inspections = Arc::new(AtomicUsize::new(0));
    let count = reads.clone();
    let inspect_count = inspections.clone();
    let f = Fixture::configured_store(
        |_| wes::web::Services::default(),
        Arc::new(|_| Ok(())),
        move |values| {
            spawn_store(
                CountReads {
                    values,
                    reads: count,
                    inspections: inspect_count,
                },
                StoreWorkerLimits::default(),
            )
            .unwrap()
        },
    )
    .await;
    let generation = f.app.current().unwrap().generation;
    let value = wes_core::Value::new(
        Shape::Primitive(wes_core::Primitive::Text),
        wes_core::Data::Text("x".repeat(6000).into()),
        Default::default(),
    )
    .unwrap();
    let handle = f.worker.store(value).await.unwrap();
    reads.store(0, Ordering::SeqCst);
    inspections.store(0, Ordering::SeqCst);
    let response = f
        .client
        .get(f.url(&format!("/values/{handle}?max_bytes=512")))
        .header("X-Wes-Session", &generation)
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 413);
    let problem: Value = body(response).await;
    assert_eq!(problem["error"]["code"], "VALUE_CAPACITY");
    assert_eq!(problem["error"]["retryable"], false);
    assert_eq!(reads.load(Ordering::SeqCst), 0);
    assert_eq!(
        inspections.load(Ordering::SeqCst),
        0,
        "the ordinary file descriptor itself decodes its payload"
    );
    let response = f
        .client
        .get(f.url(&format!("/values/{handle}")))
        .header("X-Wes-Session", &generation)
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 200);
    assert_eq!(body(response).await["data"], "x".repeat(6000));
    assert_eq!(reads.load(Ordering::SeqCst), 1);
    assert_eq!(f.calls.load(Ordering::SeqCst), 0);
    f.close().await;
}

#[tokio::test]
async fn small_preview_preserves_the_envelope_and_invalid_limits_or_generations_never_replay() {
    let f = Fixture::new().await;
    let mut events = f.stream().await;
    let generation = events.generation().await;
    assert_eq!(
        f.source(&generation, "source", "catalog echo value:synthetic > item")
            .await,
        202
    );
    let ready = events.until("ready").await;
    let handle = ready["handle"].as_str().unwrap();
    let response = f
        .client
        .get(f.url(&format!("/values/{handle}?max_bytes=4096")))
        .header("X-Wes-Session", &generation)
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 200);
    assert_eq!(
        body(response).await,
        json!({"type":{"kind":"unknown"},"provenance":{},"data":"synthetic"})
    );
    for query in [
        "max_bytes=0",
        "max_bytes=-1",
        "max_bytes=18446744073709551615",
        "max_bytes=32&max_bytes=64",
        "unknown=32",
        "max_bytes=word",
    ] {
        let response = f
            .client
            .get(f.url(&format!("/values/{handle}?{query}")))
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), 400, "{query}");
        assert_eq!(
            body(response).await["error"]["code"],
            "VALUE_READ_LIMIT_INVALID"
        );
    }
    let response = f
        .client
        .get(f.url(&format!("/values/{handle}?max_bytes=4096")))
        .header("X-Wes-Session", "other-generation")
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 409);
    assert_eq!(
        body(response).await["error"]["code"],
        "VALUE_SESSION_CHANGED"
    );
    let missing = ValueHandle::fresh();
    let response = f
        .client
        .get(f.url(&format!("/values/{missing}?max_bytes=4096")))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 404);
    assert_eq!(body(response).await["error"]["code"], "VALUE_UNAVAILABLE");
    assert_eq!(f.calls.load(Ordering::SeqCst), 1);
    drop(events);
    f.close().await;
}

#[tokio::test]
async fn bounded_read_does_not_make_an_unpublished_private_handle_accessible() {
    let f = Fixture::new().await;
    let private = wes_core::Value::new(
        Shape::Primitive(wes_core::Primitive::Text),
        wes_core::Data::Text("PRIVATE_FIXTURE_CANARY".repeat(300).into()),
        wes_core::Provenance::default()
            .with_policy(&wes_core::flow::FlowPolicy::default().private()),
    )
    .unwrap();
    let handle = f.worker.store(private).await.unwrap();
    for (suffix, status, code) in [
        ("", 410, "VALUE_PRIVATE_UNAVAILABLE"),
        ("?max_bytes=4096", 409, "VALUE_PREVIEW_UNAVAILABLE"),
    ] {
        let response = f
            .client
            .get(f.url(&format!("/values/{handle}{suffix}")))
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), status);
        let body = response.text().await.unwrap();
        assert!(body.contains(code));
        assert!(!body.contains("PRIVATE_FIXTURE_CANARY"));
    }
    assert_eq!(f.calls.load(Ordering::SeqCst), 0);
    f.close().await;
}

#[tokio::test]
async fn absent_preview_metadata_does_not_claim_that_an_ordinary_read_was_withdrawn() {
    let f = Fixture::new().await;
    let value = wes_core::Value::new(
        Shape::Primitive(wes_core::Primitive::Text),
        wes_core::Data::Text("synthetic".into()),
        wes_core::Provenance::default()
            .with_policy(&wes_core::flow::FlowPolicy::default().unknown()),
    )
    .unwrap();
    let handle = f.worker.store(value).await.unwrap();
    let preview = f
        .client
        .get(f.url(&format!("/values/{handle}?max_bytes=4096")))
        .send()
        .await
        .unwrap();
    assert_eq!(preview.status(), 409);
    assert_eq!(
        body(preview).await["error"]["code"],
        "VALUE_PREVIEW_UNAVAILABLE"
    );
    let full = f
        .client
        .get(f.url(&format!("/values/{handle}")))
        .send()
        .await
        .unwrap();
    assert_eq!(full.status(), 200);
    assert_eq!(body(full).await["data"], "synthetic");
    assert_eq!(f.calls.load(Ordering::SeqCst), 0);
    f.close().await;
}
