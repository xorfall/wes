use indexmap::IndexMap;
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, AtomicUsize, Ordering},
    mpsc,
};
use tokio::sync::Notify;
use wes_core::{
    Data, Shape, Value,
    capability::{Capability, ProviderDescription, Safety},
};
use wes_engine::{
    driver::CancellationToken,
    imports::*,
    providers::{Call, InvocationFuture, Invoker},
};

struct Never;
impl Invoker for Never {
    fn invoke(&self, _: Call, _: CancellationToken) -> InvocationFuture {
        panic!("imports must never invoke a provider")
    }
}
fn product(name: &str, warnings: Vec<String>) -> Result<ImportProduct, ImportError> {
    ImportProduct::new(
        ProviderDescription::new(
            name,
            [Capability::new(["get"], Shape::Unknown, Safety::Unsafe)],
            vec![],
        )
        .unwrap(),
        Arc::new(Never),
        warnings,
    )
}
#[derive(Default)]
struct Fake {
    captures: AtomicUsize,
    builds: AtomicUsize,
    broken: AtomicBool,
    panic_read: AtomicBool,
    gate: Mutex<Option<mpsc::Receiver<()>>>,
    entered: Notify,
    build_gate: Mutex<Option<mpsc::Receiver<()>>>,
    build_entered: Notify,
    warning_bytes: AtomicUsize,
    modes: Mutex<Vec<ImportMode>>,
}
impl Importer for Fake {
    fn capture(&self, _: &ImportRequest, max_bytes: usize) -> Result<ImportRecipe, ImportError> {
        self.captures.fetch_add(1, Ordering::SeqCst);
        assert!(
            !self.panic_read.load(Ordering::SeqCst),
            "synthetic reader panic"
        );
        if let Some(gate) = self.gate.lock().unwrap().take() {
            self.entered.notify_one();
            gate.recv().unwrap();
        }
        let source = "captured-content-do-not-log";
        if source.len() > max_bytes {
            return Err(ImportError::Capacity);
        }
        ImportRecipe::new("fake/1".into(), source.into())
    }
    fn build(
        &self,
        snapshot: &ImportSnapshot,
        mode: ImportMode,
    ) -> Result<ImportProduct, ImportError> {
        self.modes.lock().unwrap().push(mode);
        self.builds.fetch_add(1, Ordering::SeqCst);
        if let Some(gate) = self.build_gate.lock().unwrap().take() {
            self.build_entered.notify_one();
            gate.recv().unwrap();
        }
        if self.broken.load(Ordering::SeqCst) || snapshot.recipe().format() != "fake/1" {
            return Err(ImportError::InvalidRecipe);
        }
        let warning = if self.warning_bytes.load(Ordering::SeqCst) > 0 {
            "x".repeat(self.warning_bytes.load(Ordering::SeqCst))
        } else {
            "A declared warning".into()
        };
        product(snapshot.recipe().source(), vec![warning])
    }
}
fn registry(fake: Arc<Fake>) -> Importers {
    let mut registry = Importers::default();
    registry.register("fake".into(), fake).unwrap();
    registry
}
fn request(id: &str, alias: Option<&str>) -> ImportRequest {
    ImportRequest::new(
        "fake".into(),
        alias.map(str::to_owned),
        [(
            "file".into(),
            Value::new(Shape::Unknown, Data::Text(id.into()), Default::default()).unwrap(),
        )]
        .into_iter()
        .collect(),
    )
    .unwrap()
}
fn snapshot(request: ImportRequest, format: &str, source: &str) -> ImportSnapshot {
    ImportSnapshot::new(
        request,
        ImportRecipe::new(format.into(), source.into()).unwrap(),
    )
}

#[tokio::test]
async fn capture_is_inert_cached_aliased_and_only_staged_inputs_are_promoted() {
    let fake = Arc::new(Fake::default());
    let mut capture = ImportCapture::live(registry(fake.clone()));
    let request = request("private-source-path", Some("library"));
    let first = capture
        .read(&request, CancellationToken::new())
        .await
        .unwrap();
    let second = capture
        .read(&request, CancellationToken::new())
        .await
        .unwrap();
    assert!(Arc::ptr_eq(first.product(), second.product()));
    assert_eq!(first.product().description().name(), "library");
    assert_eq!(first.product().warnings(), ["A declared warning"]);
    assert_eq!(fake.captures.load(Ordering::SeqCst), 1);
    assert_eq!(fake.builds.load(Ordering::SeqCst), 1);
    assert_eq!(*fake.modes.lock().unwrap(), [ImportMode::Live]);
    let rejected = capture
        .read(&self_request("rejected"), CancellationToken::new())
        .await
        .unwrap();
    let mut other = ImportCapture::live(registry(fake.clone()));
    assert_eq!(other.accept(&first), Err(ImportError::ForeignCapture));
    capture.accept(&first).unwrap();
    capture.accept(&second).unwrap();
    let snapshots = capture.finish();
    assert_eq!(snapshots.len(), 1);
    assert_eq!(snapshots[0].request(), &request);
    assert!(
        !format!("{:?} {:?} {:?}", snapshots, first, rejected)
            .contains("captured-content-do-not-log")
    );
    assert!(!format!("{snapshots:?}").contains("private-source-path"));
}
fn self_request(id: &str) -> ImportRequest {
    request(id, None)
}

#[tokio::test]
async fn replay_never_captures_live_input_and_uses_a_fixed_registry_snapshot() {
    let old = Arc::new(Fake::default());
    let replacement = Arc::new(Fake::default());
    let mut registry = registry(old.clone());
    let input = request("file", None);
    let recorded = snapshot(input.clone(), "fake/1", "historic-provider");
    let mut replay =
        ImportCapture::replay(registry.clone(), vec![recorded.clone(), recorded]).unwrap();
    registry
        .register("fake".into(), replacement.clone())
        .unwrap();
    let captured = replay.read(&input, CancellationToken::new()).await.unwrap();
    assert_eq!(captured.product().description().name(), "historic-provider");
    assert_eq!(old.captures.load(Ordering::SeqCst), 0);
    assert_eq!(old.builds.load(Ordering::SeqCst), 1);
    assert_eq!(*old.modes.lock().unwrap(), [ImportMode::Replay]);
    assert_eq!(replacement.builds.load(Ordering::SeqCst), 0);
    assert_eq!(
        replay
            .read(&request("missing", None), CancellationToken::new())
            .await
            .unwrap_err(),
        ImportError::MissingSnapshot
    );
    assert!(
        ImportCapture::replay(
            registry.clone(),
            vec![
                snapshot(input.clone(), "fake/1", "one"),
                snapshot(input.clone(), "fake/1", "two")
            ]
        )
        .is_err()
    );
    let mut unsupported =
        ImportCapture::replay(registry, vec![snapshot(input.clone(), "fake/2", "changed")])
            .unwrap();
    assert_eq!(
        unsupported
            .read(&input, CancellationToken::new())
            .await
            .unwrap_err(),
        ImportError::InvalidRecipe
    );
    assert_eq!(replacement.captures.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn rejected_metadata_keeps_successful_input_capture_without_live_reread() {
    let fake = Arc::new(Fake::default());
    fake.broken.store(true, Ordering::SeqCst);
    let mut capture = ImportCapture::live(registry(fake.clone()));
    let request = request("input", None);
    assert_eq!(
        capture
            .read(&request, CancellationToken::new())
            .await
            .unwrap_err(),
        ImportError::InvalidRecipe
    );
    fake.broken.store(false, Ordering::SeqCst);
    let product = capture
        .read(&request, CancellationToken::new())
        .await
        .unwrap();
    assert_eq!(fake.captures.load(Ordering::SeqCst), 1);
    assert_eq!(fake.builds.load(Ordering::SeqCst), 2);
    assert!(capture.finish().is_empty());
    assert_eq!(
        product.product().description().name(),
        "captured-content-do-not-log"
    );
}

#[tokio::test]
async fn cancellation_joins_the_entered_reader_and_does_not_build_after_cancel() {
    let fake = Arc::new(Fake::default());
    let (release, gate) = mpsc::channel();
    *fake.gate.lock().unwrap() = Some(gate);
    let mut capture = ImportCapture::live(registry(fake.clone()));
    let token = CancellationToken::new();
    let worker_token = token.clone();
    let request = request("input", None);
    let task = tokio::spawn(async move { capture.read(&request, worker_token).await });
    fake.entered.notified().await;
    token.cancel();
    tokio::task::yield_now().await;
    assert!(!task.is_finished());
    release.send(()).unwrap();
    assert_eq!(task.await.unwrap().unwrap_err(), ImportError::Cancelled);
    assert_eq!(fake.builds.load(Ordering::SeqCst), 0);
    let mut capture = ImportCapture::live(registry(fake.clone()));
    assert_eq!(
        capture
            .read(&self_request("input"), token)
            .await
            .unwrap_err(),
        ImportError::Cancelled
    );
    assert_eq!(fake.captures.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn worker_panics_unknown_importers_and_entry_limits_are_explicit() {
    let fake = Arc::new(Fake::default());
    fake.panic_read.store(true, Ordering::SeqCst);
    let mut capture = ImportCapture::live(registry(fake.clone()));
    assert_eq!(
        capture
            .read(&request("input", None), CancellationToken::new())
            .await
            .unwrap_err(),
        ImportError::Worker
    );
    let mut absent = ImportCapture::live(Importers::default());
    assert_eq!(
        absent
            .read(&request("input", None), CancellationToken::new())
            .await
            .unwrap_err(),
        ImportError::UnknownImporter
    );
    fake.panic_read.store(false, Ordering::SeqCst);
    for i in 0..256 {
        capture
            .read(&request(&i.to_string(), None), CancellationToken::new())
            .await
            .unwrap();
    }
    assert_eq!(
        capture
            .read(&request("overflow", None), CancellationToken::new())
            .await
            .unwrap_err(),
        ImportError::Capacity
    );
    assert_eq!(fake.captures.load(Ordering::SeqCst), 257); // The panicking attempt plus 256 admitted inputs.
    assert!(capture.finish().is_empty());
}

#[test]
fn request_recipe_replay_and_product_budgets_reject_before_any_importer_work() {
    for name in ["", " ", "bad\nname", &"a".repeat(257)] {
        assert!(ImportRequest::new(name.into(), None, IndexMap::new()).is_err());
        assert!(ImportRecipe::new(name.into(), "".into()).is_err());
    }
    let large = Value::new(
        Shape::Unknown,
        Data::Text("x".repeat(128 * 1024).into()),
        Default::default(),
    )
    .unwrap();
    assert!(
        ImportRequest::new(
            "fake".into(),
            None,
            [("arg".into(), large)].into_iter().collect()
        )
        .is_err()
    );
    assert!(ImportRecipe::new("fake/1".into(), "x".repeat(max_recipe_bytes() + 1)).is_err());
    let fake = Arc::new(Fake::default());
    let snapshots = (0..9)
        .map(|i| {
            snapshot(
                request(&i.to_string(), None),
                "fake/1",
                &"x".repeat(max_recipe_bytes()),
            )
        })
        .collect();
    assert!(ImportCapture::replay(registry(fake.clone()), snapshots).is_err());
    assert!(product("provider", vec![String::new(); 1001]).is_err());
    assert!(product("provider", vec!["x".repeat(3 * 1024 * 1024)]).is_err());
    assert_eq!(fake.captures.load(Ordering::SeqCst), 0);
    assert_eq!(fake.builds.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn cancelled_build_is_joined_and_reuses_the_already_captured_input_on_explicit_retry() {
    let fake = Arc::new(Fake::default());
    let (release, gate) = mpsc::channel();
    *fake.build_gate.lock().unwrap() = Some(gate);
    let mut capture = ImportCapture::live(registry(fake.clone()));
    let token = CancellationToken::new();
    let cancelling = token.clone();
    let task = tokio::spawn(async move {
        let result = capture.read(&request("input", None), cancelling).await;
        (capture, result)
    });
    fake.build_entered.notified().await;
    token.cancel();
    tokio::task::yield_now().await;
    assert!(!task.is_finished());
    release.send(()).unwrap();
    let (mut capture, result) = task.await.unwrap();
    assert_eq!(result.unwrap_err(), ImportError::Cancelled);
    let imported = capture
        .read(&request("input", None), CancellationToken::new())
        .await
        .unwrap();
    capture.accept(&imported).unwrap();
    assert_eq!(capture.finish().len(), 1);
    assert_eq!(fake.captures.load(Ordering::SeqCst), 1);
    assert_eq!(fake.builds.load(Ordering::SeqCst), 2);
}

#[tokio::test]
async fn aggregate_metadata_capacity_preserves_earlier_admitted_products() {
    let fake = Arc::new(Fake::default());
    fake.warning_bytes.store(1024 * 1024, Ordering::SeqCst);
    let mut capture = ImportCapture::live(registry(fake.clone()));
    let first = capture
        .read(&request("0", None), CancellationToken::new())
        .await
        .unwrap();
    for i in 1..5 {
        capture
            .read(&request(&i.to_string(), None), CancellationToken::new())
            .await
            .unwrap();
    }
    assert_eq!(
        capture
            .read(&request("6", None), CancellationToken::new())
            .await
            .unwrap_err(),
        ImportError::Capacity
    );
    let cached = capture
        .read(&request("0", None), CancellationToken::new())
        .await
        .unwrap();
    assert!(Arc::ptr_eq(first.product(), cached.product()));
    assert_eq!(fake.captures.load(Ordering::SeqCst), 6);
    assert_eq!(fake.builds.load(Ordering::SeqCst), 6);
    capture.accept(&first).unwrap();
    assert_eq!(capture.finish().len(), 1);
}

#[test]
fn importer_hints_are_cached_bounded_optional_and_replaced_atomically() {
    use wes_core::capability::Parameter;
    struct Hints(Vec<Parameter>, Arc<AtomicUsize>);
    impl Importer for Hints {
        fn parameters(&self) -> Vec<Parameter> {
            self.1.fetch_add(1, Ordering::SeqCst);
            self.0.clone()
        }
        fn capture(&self, _: &ImportRequest, _: usize) -> Result<ImportRecipe, ImportError> {
            panic!("reading hints must not capture inputs")
        }
        fn build(&self, _: &ImportSnapshot, _: ImportMode) -> Result<ImportProduct, ImportError> {
            panic!("reading hints must not build a provider")
        }
    }
    let mut importers = registry(Arc::new(Fake::default()));
    assert!(importers.parameters()["fake"].is_empty());
    let calls = Arc::new(AtomicUsize::new(0));
    let file = Parameter::new("file", Shape::Unknown, false);
    importers
        .register(
            "custom".into(),
            Arc::new(Hints(vec![file.clone()], calls.clone())),
        )
        .unwrap();
    assert_eq!(importers.parameters()["custom"], vec![file.clone()]);
    assert_eq!(importers.clone().parameters()["custom"], vec![file.clone()]);
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    for invalid in [
        vec![file.clone(), file.clone()],
        vec![Parameter::new("as", Shape::Unknown, true)],
        vec![file.clone(); 257],
        vec![file.clone().written_in("x".repeat(100_000))],
    ] {
        assert!(
            importers
                .register("custom".into(), Arc::new(Hints(invalid, calls.clone())))
                .is_err()
        );
        assert_eq!(importers.parameters()["custom"], vec![file.clone()]);
    }
    importers
        .register("custom".into(), Arc::new(Fake::default()))
        .unwrap();
    assert!(importers.parameters()["custom"].is_empty());
}
