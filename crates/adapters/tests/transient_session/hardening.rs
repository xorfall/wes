use super::*;
use tokio::{io::AsyncReadExt, net::TcpListener};
use wes_adapters::process::{self, ProcessConfig, ProcessInvoker};
use wes_core::Value;
use wes_engine::storage::{
    LeafSelection, LoadedValue, StoredSnapshot, ValueHandle, ValueLease, ValueStore,
};

struct PausedAppend {
    values: TieredValues,
    entered: Option<tokio::sync::oneshot::Sender<()>>,
    release: std::sync::mpsc::Receiver<()>,
    aborted: Arc<AtomicUsize>,
}
struct ControlledProcess {
    process: ProcessInvoker,
    entered: Mutex<Option<tokio::sync::oneshot::Sender<CancellationToken>>>,
    calls: Arc<AtomicUsize>,
    completed: Arc<AtomicUsize>,
}
impl Invoker for ControlledProcess {
    fn invoke(&self, call: Call, cancel: CancellationToken) -> InvocationFuture {
        self.process.invoke(call, cancel)
    }
    fn invoke_acquired(
        &self,
        call: Call,
        cancel: CancellationToken,
        context: AcquisitionContext,
    ) -> AcquiredFuture {
        self.calls.fetch_add(1, Ordering::SeqCst);
        self.entered
            .lock()
            .unwrap()
            .take()
            .unwrap()
            .send(cancel.clone())
            .unwrap();
        let future = self.process.invoke_acquired(call, cancel, context);
        let completed = self.completed.clone();
        Box::pin(async move {
            let result = future.await;
            completed.fetch_add(1, Ordering::SeqCst);
            result
        })
    }
}
struct ReleaseAppend(Option<std::sync::mpsc::Sender<()>>);
impl Drop for ReleaseAppend {
    fn drop(&mut self) {
        if let Some(release) = self.0.take() {
            let _ = release.send(());
        }
    }
}

struct TypedSource {
    bytes: usize,
    stored: bool,
    restricted: bool,
}

struct InputPressure {
    values: Mutex<TieredValues>,
    sources: Vec<ValueHandle>,
    attempted: Arc<AtomicUsize>,
}
impl ValueStore for InputPressure {
    fn store(&mut self, value: &Value) -> Result<ValueHandle, StoreError> {
        self.values.lock().unwrap().store(value)
    }
    fn read(&self, h: &ValueHandle) -> Result<Option<LoadedValue>, StoreError> {
        self.values.lock().unwrap().read(h)
    }
    fn encoded(&self, h: &ValueHandle) -> Result<Option<Vec<u8>>, StoreError> {
        self.values.lock().unwrap().encoded(h)
    }
    fn size(&self, h: &ValueHandle) -> Result<Option<u64>, StoreError> {
        self.values.lock().unwrap().size(h)
    }
    fn release(&mut self, h: &ValueHandle) -> Result<bool, StoreError> {
        self.values.lock().unwrap().release(h)
    }
    fn digest(&self, h: &ValueHandle) -> Result<Option<String>, StoreError> {
        self.values.lock().unwrap().digest(h)
    }
    fn describe(
        &self,
        h: &ValueHandle,
        fields: &[String],
    ) -> Result<Option<StoredSnapshot>, StoreError> {
        self.values.lock().unwrap().describe(h, fields)
    }
    fn hold(&mut self, h: &ValueHandle) -> Result<Option<ValueLease>, StoreError> {
        self.values.lock().unwrap().hold(h)
    }
    fn begin_acquisition(
        &mut self,
        max: u64,
        owner: std::sync::Weak<()>,
    ) -> Result<ValueHandle, StoreError> {
        let h = self.values.lock().unwrap().begin_acquisition(max, owner)?;
        self.sources.push(h.clone());
        Ok(h)
    }
    fn append_acquisition(
        &mut self,
        h: &ValueHandle,
        port: &str,
        bytes: &[u8],
    ) -> Result<(), StoreError> {
        self.values
            .lock()
            .unwrap()
            .append_acquisition(h, port, bytes)
    }
    fn seal_acquisition(
        &mut self,
        h: &ValueHandle,
        shell: &Value,
        leaves: &[LeafSelection],
    ) -> Result<StoredSnapshot, StoreError> {
        self.values
            .lock()
            .unwrap()
            .seal_acquisition(h, shell, leaves)
    }
    fn abort_acquisition(&mut self, h: &ValueHandle) -> Result<(), StoreError> {
        self.values.lock().unwrap().abort_acquisition(h)
    }
    fn read_selected(
        &self,
        h: &ValueHandle,
        fields: &[String],
        budget: u64,
    ) -> Result<Option<LoadedValue>, StoreError> {
        let mut values = self.values.lock().unwrap();
        if self.sources.len() == 2 && self.attempted.load(Ordering::SeqCst) == 0 {
            let other = self.sources.iter().find(|other| *other != h).unwrap();
            let protected = matches!(values.release(other), Err(StoreError::Limit(_)));
            self.attempted
                .store(if protected { 1 } else { 2 }, Ordering::SeqCst);
        }
        values.read_selected(h, fields, budget)
    }
}

#[tokio::test]
async fn every_stored_input_is_protected_before_the_first_payload_read() {
    let root = tempfile::tempdir().unwrap();
    let attempted = Arc::new(AtomicUsize::new(0));
    let values = InputPressure {
        values: Mutex::new(
            TieredValues::open(
                &root.path().join("live"),
                &root.path().join("archive"),
                Limits::default(),
                Durability::FileAndDirectory,
                None,
            )
            .unwrap(),
        ),
        sources: vec![],
        attempted: attempted.clone(),
    };
    let (worker, storage_task) = spawn_store(values, StoreWorkerLimits::default()).unwrap();
    let (mut base, _) = workspace(Arc::new(Definitions::default()));
    base.register_provider(
        ProviderDescription::new(
            "typed",
            [Capability::new(
                ["run"],
                typed_source(0, false).shape().clone(),
                Safety::Safe,
            )],
            vec![],
        )
        .unwrap(),
        Arc::new(TypedSource {
            bytes: 65536,
            stored: true,
            restricted: false,
        }),
    )
    .unwrap();
    let (handle, task) = session::spawn_with_storage(
        base,
        RecordingMode::Ephemeral,
        Arc::new(NoFiles),
        NonZeroUsize::new(2).unwrap(),
        SessionStorage {
            worker: worker.clone(),
            auto_keep: AutoKeep::default(),
        },
    )
    .unwrap();
    submit(
        &handle,
        "sources",
        ":transient { typed run > a\ntyped run > b } > study",
    )
    .await;
    let scope = handle
        .read_sandbox_export("study", true)
        .await
        .unwrap()
        .scope
        .unwrap();
    let (child, _) = handle.resolve_read_scope(&scope).await.unwrap();
    // Finish publication holds so they cannot accidentally protect the second input for us.
    child.wait_idle().await.unwrap();
    submit(
        &child,
        "consume",
        ":calc pure {return $a.exitCode+$b.body.code;} > total",
    )
    .await;
    child.wait_idle().await.unwrap();
    let got = read(&handle, "study.total").await;
    handle.shutdown().await.unwrap();
    task.join().await.unwrap();
    worker.shutdown().await.unwrap();
    storage_task.join().await.unwrap();
    assert_eq!(got, Data::Int(7));
    assert_eq!(
        attempted.load(Ordering::SeqCst),
        1,
        "the second exact input could be released while preparing the first"
    );
}
fn typed_source(bytes: usize, restricted: bool) -> Value {
    use wes_core::{Provenance, RecordShape};
    let int = Shape::Primitive(Primitive::Int);
    let body = Shape::Record(
        RecordShape::new(
            "Body",
            [
                ("code".into(), int.clone()),
                ("log".into(), Shape::Primitive(Primitive::Bytes)),
            ],
        )
        .unwrap(),
    );
    let shape = Shape::Record(
        RecordShape::new(
            "Source",
            [
                ("exitCode".into(), int.clone()),
                ("body".into(), body),
                ("items".into(), Shape::List(Box::new(int))),
            ],
        )
        .unwrap(),
    );
    let mut provenance = Provenance::default();
    if restricted {
        provenance = provenance.with_policy(&wes_core::flow::FlowPolicy::default().private());
    }
    Value::new(
        shape,
        Data::Record(indexmap::IndexMap::from([
            ("exitCode".into(), Data::Int(0)),
            (
                "body".into(),
                Data::Record(indexmap::IndexMap::from([
                    ("code".into(), Data::Int(7)),
                    ("log".into(), Data::Bytes(vec![b'x'; bytes].into())),
                ])),
            ),
            ("items".into(), Data::List(vec![Data::Int(2), Data::Int(1)])),
        ])),
        provenance,
    )
    .unwrap()
}
impl Invoker for TypedSource {
    fn invoke(&self, _: Call, _: CancellationToken) -> InvocationFuture {
        let (bytes, restricted) = (self.bytes, self.restricted);
        Box::pin(async move { Ok(typed_source(bytes, restricted)) })
    }
    fn invoke_acquired(
        &self,
        call: Call,
        token: CancellationToken,
        context: AcquisitionContext,
    ) -> AcquiredFuture {
        if !self.stored {
            let future = self.invoke(call, token);
            return Box::pin(async move {
                future.await.map(wes_engine::storage::AcquiredOutput::Value)
            });
        }
        let bytes = self.bytes;
        Box::pin(async move {
            let draft = context
                .begin(bytes as u64)
                .await
                .map_err(wes_engine::storage::acquisition_error)?;
            let buffer = vec![b'x'; wes_engine::storage::CHUNK_BYTES];
            for _ in 0..bytes / buffer.len() {
                draft
                    .append("log", buffer.clone())
                    .await
                    .map_err(wes_engine::storage::acquisition_error)?;
            }
            draft
                .finish(
                    typed_source(0, false),
                    vec![LeafSelection {
                        port: "log".into(),
                        fields: vec!["body".into(), "log".into()],
                        text: false,
                    }],
                )
                .await
        })
    }
}

#[tokio::test]
async fn resident_and_stored_nested_records_keep_calculation_semantics_and_parent_policy() {
    let cases = [
        (
            "if(false) { return $p.body.log; } return $p.exitCode;",
            Data::Int(0),
        ),
        (
            "const x=false && length($p.body.log)>0; return $p.body.code;",
            Data::Int(7),
        ),
        (
            "const f=()=> $p.body.log; return $p.body.code;",
            Data::Int(7),
        ),
        (
            "function f(){return $p.body.code;} return f();",
            Data::Int(7),
        ),
        (
            "return $p.items.map(x=>x+1).reduce((a,x)=>a+x,0);",
            Data::Int(5),
        ),
        ("const key='exitCode'; return $p[key];", Data::Int(0)),
        (
            "const root=$p; return root.exitCode+$p.body.code;",
            Data::Int(7),
        ),
        ("const f=()=> $p.body.log; return 17;", Data::Int(17)),
    ];
    for (stored, restricted) in [(false, false), (true, false), (false, true)] {
        let root = tempfile::tempdir().unwrap();
        let (worker, storage_task) = spawn_store(
            TieredValues::open(
                &root.path().join("live"),
                &root.path().join("archive"),
                Limits::default(),
                Durability::FileAndDirectory,
                None,
            )
            .unwrap(),
            StoreWorkerLimits::default(),
        )
        .unwrap();
        let (mut base, _) = workspace(Arc::new(Definitions::default()));
        base.register_provider(
            ProviderDescription::new(
                "typed",
                [Capability::new(
                    ["run"],
                    typed_source(0, false).shape().clone(),
                    Safety::Safe,
                )],
                vec![],
            )
            .unwrap(),
            Arc::new(TypedSource {
                bytes: 131072,
                stored,
                restricted,
            }),
        )
        .unwrap();
        let (handle, task) = session::spawn_with_storage(
            base,
            RecordingMode::Ephemeral,
            Arc::new(NoFiles),
            NonZeroUsize::new(2).unwrap(),
            SessionStorage {
                worker: worker.clone(),
                auto_keep: AutoKeep::default(),
            },
        )
        .unwrap();
        let code = cases
            .iter()
            .enumerate()
            .map(|(n, (body, _))| format!(":calc pure {{ {body} }} > result_{n}\n"))
            .collect::<String>();
        let made = submit(
            &handle,
            "semantics",
            &format!(":transient {{\ntyped run > p\n{code}}} > study"),
        )
        .await;
        assert!(
            made.diagnostics
                .diagnostics
                .iter()
                .all(|d| d.severity != wes_language::Severity::Error),
            "{:?}",
            made.diagnostics
        );
        let scope = handle
            .read_sandbox_export("study", true)
            .await
            .unwrap()
            .scope
            .unwrap();
        let (child, _) = handle.resolve_read_scope(&scope).await.unwrap();
        child.wait_idle().await.unwrap();
        let snapshot = child.snapshot().await.unwrap();
        for (n, (_, expected)) in cases.iter().enumerate() {
            let node = &snapshot.names[&format!("result_{n}")].node;
            let value = snapshot.execution.values.get(node).unwrap_or_else(|| {
                panic!(
                    "stored={stored} private={restricted} case={n}: {:?}",
                    snapshot.execution.errors
                )
            });
            assert_eq!(value.data(), expected);
            assert_eq!(
                value.provenance().policy().is_private(),
                restricted,
                "case={n}"
            );
            assert!(
                !value.provenance().policy().origins().is_empty(),
                "source origin disappeared in case={n}"
            );
        }
        handle.shutdown().await.unwrap();
        task.join().await.unwrap();
        worker.shutdown().await.unwrap();
        storage_task.join().await.unwrap();
    }
}
impl ValueStore for PausedAppend {
    fn store(&mut self, value: &Value) -> Result<ValueHandle, wes_engine::storage::StoreError> {
        self.values.store(value)
    }
    fn read(
        &self,
        handle: &ValueHandle,
    ) -> Result<Option<LoadedValue>, wes_engine::storage::StoreError> {
        self.values.read(handle)
    }
    fn encoded(
        &self,
        handle: &ValueHandle,
    ) -> Result<Option<Vec<u8>>, wes_engine::storage::StoreError> {
        self.values.encoded(handle)
    }
    fn size(&self, handle: &ValueHandle) -> Result<Option<u64>, wes_engine::storage::StoreError> {
        self.values.size(handle)
    }
    fn release(&mut self, handle: &ValueHandle) -> Result<bool, wes_engine::storage::StoreError> {
        self.values.release(handle)
    }
    fn digest(
        &self,
        handle: &ValueHandle,
    ) -> Result<Option<String>, wes_engine::storage::StoreError> {
        self.values.digest(handle)
    }
    fn describe(
        &self,
        handle: &ValueHandle,
        fields: &[String],
    ) -> Result<Option<StoredSnapshot>, wes_engine::storage::StoreError> {
        self.values.describe(handle, fields)
    }
    fn hold(
        &mut self,
        handle: &ValueHandle,
    ) -> Result<Option<ValueLease>, wes_engine::storage::StoreError> {
        self.values.hold(handle)
    }
    fn begin_acquisition(
        &mut self,
        maximum: u64,
        owner: std::sync::Weak<()>,
    ) -> Result<ValueHandle, wes_engine::storage::StoreError> {
        self.values.begin_acquisition(maximum, owner)
    }
    fn append_acquisition(
        &mut self,
        handle: &ValueHandle,
        port: &str,
        bytes: &[u8],
    ) -> Result<(), wes_engine::storage::StoreError> {
        if let Some(entered) = self.entered.take() {
            let _ = entered.send(());
            self.release.recv().unwrap();
        }
        self.values.append_acquisition(handle, port, bytes)
    }
    fn seal_acquisition(
        &mut self,
        handle: &ValueHandle,
        shell: &Value,
        leaves: &[LeafSelection],
    ) -> Result<StoredSnapshot, wes_engine::storage::StoreError> {
        self.values.seal_acquisition(handle, shell, leaves)
    }
    fn abort_acquisition(
        &mut self,
        handle: &ValueHandle,
    ) -> Result<(), wes_engine::storage::StoreError> {
        self.aborted.fetch_add(1, Ordering::SeqCst);
        self.values.abort_acquisition(handle)
    }
}

#[tokio::test]
async fn cancellation_reaps_the_child_before_joining_a_paused_owned_append() {
    stopped_append(false).await;
}

#[tokio::test]
async fn deadline_reaps_the_child_before_joining_a_paused_owned_append() {
    stopped_append(true).await;
}

#[tokio::test]
async fn acquired_process_preserves_both_channels_nonzero_status_and_stdin_eof() {
    let root = tempfile::tempdir().unwrap();
    let executable = finite_fixture(root.path()).await;
    let (worker, storage_task) = spawn_store(
        TieredValues::open(
            &root.path().join("live"),
            &root.path().join("archive"),
            Limits::default(),
            Durability::FileAndDirectory,
            None,
        )
        .unwrap(),
        StoreWorkerLimits::default(),
    )
    .unwrap();
    let (mut base, _) = workspace(Arc::new(Definitions::default()));
    let calls = Arc::new(AtomicUsize::new(0));
    for mode in ["bytes", "flood", "stdin"] {
        let (description, inner) = process::provider(
            mode,
            vec![executable.to_str().unwrap().into(), mode.into()],
            "arg",
            ProcessConfig {
                output_bytes: 4096,
                ..Default::default()
            },
        )
        .unwrap();
        base.register_provider(
            description,
            Arc::new(CountProcess {
                inner,
                count: calls.clone(),
            }),
        )
        .unwrap();
    }
    let (handle, task) = session::spawn_with_storage(
        base,
        RecordingMode::Ephemeral,
        Arc::new(NoFiles),
        NonZeroUsize::new(2).unwrap(),
        SessionStorage {
            worker: worker.clone(),
            auto_keep: AutoKeep::default(),
        },
    )
    .unwrap();
    let made = submit(
        &handle,
        "channels",
        ":transient {\nbytes run > binary\nflood run > flood\nstdin run > eof\n} > study",
    )
    .await;
    assert!(
        made.diagnostics
            .diagnostics
            .iter()
            .all(|d| d.severity != wes_language::Severity::Error),
        "{:?}",
        made.diagnostics
    );
    assert_eq!(read(&handle, "study.binary.exitCode").await, Data::Int(7));
    assert_eq!(
        read(&handle, "study.binary.stdout").await,
        Data::Bytes([0, 255, 1, 10].as_slice().into())
    );
    assert_eq!(
        read(&handle, "study.binary.stderr").await,
        Data::Bytes([254, 0, 2].as_slice().into())
    );
    assert_eq!(
        read(&handle, "study.flood.stdout").await,
        Data::Bytes(vec![b'o'; 128 * 1024].into())
    );
    assert_eq!(
        read(&handle, "study.flood.stderr").await,
        Data::Bytes(vec![b'e'; 128 * 1024].into())
    );
    assert_eq!(
        read(&handle, "study.eof.stdout").await,
        Data::Bytes(b"0".as_slice().into())
    );
    assert_eq!(calls.load(Ordering::SeqCst), 3);
    assert_eq!(files(&root.path().join("archive")), 0);
    handle.shutdown().await.unwrap();
    task.join().await.unwrap();
    worker.shutdown().await.unwrap();
    storage_task.join().await.unwrap();
}

async fn stopped_append(deadline: bool) {
    let root = tempfile::tempdir().unwrap();
    let executable = super::finite_fixture(root.path()).await;
    let lock = root.path().join("child.lock");
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let (entered, entering) = tokio::sync::oneshot::channel();
    let (release, waiting) = std::sync::mpsc::channel();
    let mut release = ReleaseAppend(Some(release));
    let aborted = Arc::new(AtomicUsize::new(0));
    let values = PausedAppend {
        values: TieredValues::open(
            &root.path().join("live"),
            &root.path().join("archive"),
            Limits::default(),
            Durability::FileAndDirectory,
            None,
        )
        .unwrap(),
        entered: Some(entered),
        release: waiting,
        aborted: aborted.clone(),
    };
    let (worker, storage_task) = spawn_store(values, StoreWorkerLimits::default()).unwrap();
    let (mut base, _) = workspace(Arc::new(Definitions::default()));
    let (description, invoker) = process::provider(
        "writer",
        vec![
            executable.to_str().unwrap().into(),
            "finite".into(),
            lock.to_str().unwrap().into(),
            listener.local_addr().unwrap().to_string(),
        ],
        "arg",
        ProcessConfig {
            timeout: std::time::Duration::from_secs(if deadline { 10 } else { 60 }),
            output_bytes: 4096,
            ..Default::default()
        },
    )
    .unwrap();
    let calls = Arc::new(AtomicUsize::new(0));
    let completed = Arc::new(AtomicUsize::new(0));
    let (invoking, invoked) = tokio::sync::oneshot::channel();
    base.register_provider(
        description,
        Arc::new(ControlledProcess {
            process: invoker,
            entered: Mutex::new(Some(invoking)),
            calls: calls.clone(),
            completed: completed.clone(),
        }),
    )
    .unwrap();
    let (handle, task) = session::spawn_with_storage(
        base,
        RecordingMode::Ephemeral,
        Arc::new(NoFiles),
        NonZeroUsize::new(2).unwrap(),
        SessionStorage {
            worker: worker.clone(),
            auto_keep: AutoKeep::default(),
        },
    )
    .unwrap();
    let submitting = handle.clone();
    let making = tokio::spawn(async move {
        submit(
            &submitting,
            "writer",
            ":transient { writer run arg:[\"1048576\"] > raw } > study",
        )
        .await
    });
    let (mut control, _) =
        tokio::time::timeout(std::time::Duration::from_secs(5), listener.accept())
            .await
            .unwrap()
            .unwrap();
    let mut ready = [0; 5];
    control.read_exact(&mut ready).await.unwrap();
    assert_eq!(&ready, b"ready");
    let token = invoked.await.unwrap();
    entering.await.unwrap();
    if deadline {
        // The real process and held append are ready before advancing its existing deadline.
        tokio::time::pause();
        tokio::time::advance(std::time::Duration::from_secs(11)).await;
        tokio::time::resume();
    } else {
        token.cancel();
    }
    let closed =
        tokio::time::timeout(std::time::Duration::from_secs(2), control.read(&mut [0; 1])).await;
    let joined_early = completed.load(Ordering::SeqCst) != 0;
    let aborts_before_ack = aborted.load(Ordering::SeqCst);
    // Always release the fixture before asserting, including when the regression fails.
    release.0.take().unwrap().send(()).unwrap();
    making.await.unwrap();
    handle.shutdown().await.unwrap();
    task.join().await.unwrap();
    worker.shutdown().await.unwrap();
    storage_task.join().await.unwrap();
    assert!(
        matches!(closed, Ok(Ok(0))),
        "child survived cancellation/deadline while the owned append was pending: {closed:?}"
    );
    assert!(
        !joined_early,
        "pending physical storage completion was abandoned"
    );
    assert_eq!(aborts_before_ack, 0);
    assert_eq!(aborted.load(Ordering::SeqCst), 1);
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    assert!(
        std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(lock)
            .unwrap()
            .try_lock()
            .is_ok()
    );
}
