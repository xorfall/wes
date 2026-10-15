//! Owned loopback transport with synthetic revocable read authority; no external service.
use super::*;
use std::sync::{
    Mutex,
    atomic::{AtomicBool, AtomicU64},
};
use wes_engine::{
    provider_reads::{Changes, ReadAuthority},
    storage::{LoadedValue, StoreError, ValueHandle, ValueLease, ValueStore},
};
struct Gate {
    origin: wes_core::flow::ProviderReadOrigin,
    open: AtomicBool,
    epoch: AtomicU64,
    observers: wes_engine::credentials::material::ChangeObservers,
}
impl Gate {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            origin: wes_core::flow::ProviderReadOrigin::new(
                "00000000-0000-4000-8000-000000000076".into(),
                "a".repeat(64),
                "b".repeat(64),
            )
            .unwrap(),
            open: AtomicBool::new(true),
            epoch: AtomicU64::new(0),
            observers: Default::default(),
        })
    }
    fn set(&self, open: bool) {
        self.open.store(open, Ordering::SeqCst);
        self.epoch.fetch_add(1, Ordering::SeqCst);
        self.observers.changed();
    }
}
impl ReadAuthority for Gate {
    fn identity(&self) -> String {
        format!("loopback-fixture:{:p}", self)
    }
    fn realm(&self) -> Option<String> {
        Some(self.origin.realm().into())
    }
    fn closed(&self) -> bool {
        false
    }
    fn revision(&self) -> String {
        self.epoch.load(Ordering::SeqCst).to_string()
    }
    fn allows(&self, origin: &wes_core::flow::ProviderReadOrigin) -> bool {
        origin == &self.origin && self.open.load(Ordering::SeqCst)
    }
    fn observe(&self, observer: std::sync::Weak<Changes>) -> Result<(), &'static str> {
        self.observers
            .register(observer)
            .map_err(|_| "fixture observer capacity exceeded")
    }
}
struct ProtectedEcho(Arc<Gate>, Arc<AtomicUsize>);
impl Invoker for ProtectedEcho {
    fn invoke(&self, _: Call, _: CancellationToken) -> InvocationFuture {
        self.1.fetch_add(1, Ordering::SeqCst);
        let policy =
            wes_core::flow::FlowPolicy::default().with_provider_read(self.0.origin.clone());
        Box::pin(async move {
            Ok(wes_core::Value::new(
                Shape::Primitive(wes_core::Primitive::Text),
                wes_core::Data::Text("synthetic capture body".into()),
                wes_core::Provenance::default().with_policy(&policy),
            )
            .unwrap())
        })
    }
}
fn install(gate: Arc<Gate>, calls: Arc<AtomicUsize>) -> Install {
    Arc::new(move |ws| {
        ws.register_provider(
            ProviderDescription::new(
                "protected",
                [Capability::new(
                    ["read"],
                    Shape::Primitive(wes_core::Primitive::Text),
                    Safety::Safe,
                )],
                vec![],
            )
            .unwrap(),
            Arc::new(ProtectedEcho(gate.clone(), calls.clone())),
        )
        .map(|_| ())
    })
}
async fn read(f: &Fixture, generation: &str, handle: &str) -> reqwest::Response {
    f.client
        .get(f.url(&format!("/values/{handle}")))
        .header("X-Wes-Session", generation)
        .send()
        .await
        .unwrap()
}
#[tokio::test]
async fn provider_read_notice_precedes_metadata_and_same_capture_reopens_without_producer_replay() {
    let gate = Gate::new();
    let calls = Arc::new(AtomicUsize::new(0));
    let f = Fixture::configured(
        |_| wes::web::Services::default(),
        install(gate.clone(), calls.clone()),
    )
    .await;
    f.worker.provider_readers().register(gate.clone()).unwrap();
    let mut events = f.stream().await;
    let generation = events.generation().await;
    assert_eq!(
        f.source(&generation, "capture", "protected read > item")
            .await,
        202
    );
    let ready = events.until("ready").await;
    let node = ready["node"].as_str().unwrap();
    let handle = ready["handle"].as_str().unwrap();
    assert_eq!(read(&f, &generation, handle).await.status(), 200);
    let before = f.app.current().unwrap().session.snapshot().await.unwrap();
    let id = wes_engine::graph::NodeId::new(node).unwrap();
    let run = before.execution.runs[&id].clone();
    gate.set(false);
    let notice = events.until("result-access").await;
    assert_eq!(
        notice,
        json!({"event":"result-access","node":node,"readable":false})
    );
    let response = read(&f, &generation, handle).await;
    assert_eq!(response.status(), 403);
    let problem = response.text().await.unwrap();
    assert!(problem.contains("VALUE_ACCESS_WITHDRAWN"));
    assert!(!problem.contains("synthetic capture body"));
    let closed = f.app.current().unwrap().session.snapshot().await.unwrap();
    assert!(!closed.execution.values.contains_key(&id));
    assert!(!closed.execution.stored_values.contains_key(&id));
    gate.set(true); // Simulates the owner accepting a new explicit proof; native proof tested separately.
    let notice = events.until("result-access").await;
    assert_eq!(notice["readable"], true);
    assert_eq!(notice["handle"], handle);
    assert_eq!(notice["epoch"].as_str().unwrap().len(), 64);
    let ready_again = events.until("ready").await;
    assert_eq!(ready_again["handle"], handle);
    let response = read(&f, &generation, handle).await;
    assert_eq!(response.status(), 200);
    assert!(
        response
            .text()
            .await
            .unwrap()
            .contains("synthetic capture body")
    );
    let after = f.app.current().unwrap().session.snapshot().await.unwrap();
    assert_eq!(after.execution.runs[&id], run);
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    f.close().await;
}

type ReadBarrier = Arc<
    Mutex<
        Option<(
            tokio::sync::oneshot::Sender<()>,
            std::sync::mpsc::Receiver<()>,
        )>,
    >,
>;
struct BlockingRead {
    values: TieredValues,
    barrier: ReadBarrier,
}
impl ValueStore for BlockingRead {
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
        if let Some((entered, released)) = self.barrier.lock().unwrap().take() {
            entered.send(()).unwrap();
            released
                .recv_timeout(Duration::from_secs(5))
                .map_err(|_| StoreError::Closed)?;
        }
        self.values.read(h)
    }
    fn describe(
        &self,
        h: &ValueHandle,
        fields: &[String],
    ) -> Result<Option<wes_engine::storage::StoredSnapshot>, StoreError> {
        self.values.describe(h, fields)
    }
    fn encoded(&self, h: &ValueHandle) -> Result<Option<Vec<u8>>, StoreError> {
        self.values.encoded(h)
    }
    fn size(&self, h: &ValueHandle) -> Result<Option<u64>, StoreError> {
        self.values.size(h)
    }
    fn digest(&self, h: &ValueHandle) -> Result<Option<String>, StoreError> {
        self.values.digest(h)
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
async fn revoke_then_revalidate_during_a_physical_read_discards_its_old_reply() {
    let gate = Gate::new();
    let calls = Arc::new(AtomicUsize::new(0));
    let barrier: ReadBarrier = Default::default();
    let held = barrier.clone();
    let f = Fixture::configured_store(
        |_| wes::web::Services::default(),
        install(gate.clone(), calls.clone()),
        move |values| {
            spawn_store(
                BlockingRead {
                    values,
                    barrier: held,
                },
                StoreWorkerLimits::default(),
            )
            .unwrap()
        },
    )
    .await;
    f.worker.provider_readers().register(gate.clone()).unwrap();
    let mut events = f.stream().await;
    let generation = events.generation().await;
    assert_eq!(
        f.source(&generation, "capture", "protected read > item")
            .await,
        202
    );
    let ready = events.until("ready").await;
    let handle = ready["handle"].as_str().unwrap();
    let (entered, waiting) = tokio::sync::oneshot::channel();
    let (release, released) = std::sync::mpsc::channel();
    *barrier.lock().unwrap() = Some((entered, released));
    let client = f.client.clone();
    let url = f.url(&format!("/values/{handle}"));
    let captured = generation.clone();
    let reading = tokio::spawn(async move {
        client
            .get(url)
            .header("X-Wes-Session", captured)
            .send()
            .await
            .unwrap()
    });
    tokio::time::timeout(Duration::from_secs(5), waiting)
        .await
        .unwrap()
        .unwrap();
    gate.set(false);
    gate.set(true);
    release.send(()).unwrap();
    let response = reading.await.unwrap();
    assert_eq!(response.status(), 409);
    let body = response.text().await.unwrap();
    assert!(!body.contains("synthetic capture body"));
    assert!(
        body.contains("VALUE_REVISION_CONFLICT") || body.contains("VALUE_READ_AUTHORITY_CHANGED")
    );
    assert_eq!(read(&f, &generation, handle).await.status(), 200);
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    f.close().await;
}
