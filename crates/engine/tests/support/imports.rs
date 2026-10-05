use std::sync::{
    Arc, Mutex,
    atomic::{AtomicUsize, Ordering},
    mpsc,
};
use tokio::sync::oneshot;
use wes_core::{
    Data, Shape, Value,
    capability::{Capability, ProviderDescription, Safety},
};
use wes_engine::{
    driver::CancellationToken,
    imports::{
        ImportError, ImportMode, ImportProduct, ImportRecipe, ImportRequest, ImportSnapshot,
        Importer,
    },
    providers::{Call, InvocationFuture, Invoker},
};

pub type Gate = (oneshot::Sender<()>, mpsc::Receiver<()>);
#[derive(Default)]
pub struct Fixture {
    pub captures: AtomicUsize,
    pub calls: Arc<AtomicUsize>,
    pub modes: Mutex<Vec<ImportMode>>,
    pub gate: Mutex<Option<Gate>>,
}
struct Marker {
    value: Value,
    calls: Arc<AtomicUsize>,
}
impl Invoker for Marker {
    fn invoke(&self, _: Call, _: CancellationToken) -> InvocationFuture {
        self.calls.fetch_add(1, Ordering::SeqCst);
        let value = self.value.clone();
        Box::pin(async move { Ok(value) })
    }
}
impl Importer for Fixture {
    fn parameters(&self) -> Vec<wes_core::capability::Parameter> {
        vec![wes_core::capability::Parameter::new(
            "file",
            Shape::Primitive(wes_core::Primitive::Text),
            true,
        )]
    }
    fn capture(&self, request: &ImportRequest, _: usize) -> Result<ImportRecipe, ImportError> {
        self.captures.fetch_add(1, Ordering::SeqCst);
        if let Some((entered, release)) = self.gate.lock().unwrap().take() {
            entered.send(()).unwrap();
            release
                .recv_timeout(std::time::Duration::from_secs(5))
                .unwrap();
        }
        let Some(Data::Text(source)) = request.arguments().get("file").map(Value::data) else {
            return Err(ImportError::InvalidRecipe);
        };
        ImportRecipe::new("fixture/v1".into(), source.to_string())
    }
    fn build(
        &self,
        snapshot: &ImportSnapshot,
        mode: ImportMode,
    ) -> Result<ImportProduct, ImportError> {
        self.modes.lock().unwrap().push(mode);
        if snapshot.recipe().format() != "fixture/v1" || snapshot.recipe().source() == "bad" {
            return Err(ImportError::InvalidRecipe);
        }
        ImportProduct::new(
            ProviderDescription::new(
                "fixture-provider",
                [Capability::new(["get"], Shape::Unknown, Safety::Safe)],
                vec![],
            )
            .unwrap(),
            Arc::new(Marker {
                value: Value::new(
                    Shape::Unknown,
                    Data::Text(snapshot.recipe().source().into()),
                    Default::default(),
                )
                .unwrap(),
                calls: self.calls.clone(),
            }),
            vec!["Synthetic importer warning".into()],
        )
    }
}
