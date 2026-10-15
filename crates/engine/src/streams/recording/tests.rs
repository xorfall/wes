use super::*;
use crate::{
    runtime::{Effect, ExecutionTraits, Runtime},
    storage::{LoadedValue, StoredSnapshot, ValueHandle, ValueStore, spawn_store},
};
use std::sync::Condvar;
use wes_core::{
    Shape,
    capability::{Capability, Safety},
    contracts::ContractRegistry,
};

#[derive(Default)]
struct Gate {
    entered: Mutex<Option<tokio::sync::oneshot::Sender<()>>>,
    released: Mutex<bool>,
    changed: Condvar,
    hold: Mutex<Option<Arc<Gate>>>,
}
struct Store {
    gate: Arc<Gate>,
    raw: Vec<u8>,
    value: Option<Value>,
}
impl ValueStore for Store {
    fn read_policy(
        &self,
        _: &ValueHandle,
    ) -> Result<Option<wes_core::flow::FlowPolicy>, StoreError> {
        Ok(self
            .value
            .as_ref()
            .map(|value| value.provenance().policy().clone()))
    }
    fn store(&mut self, _: &Value) -> Result<ValueHandle, StoreError> {
        unreachable!()
    }
    fn read(&self, _: &ValueHandle) -> Result<Option<LoadedValue>, StoreError> {
        Ok(self.value.clone().map(|value| LoadedValue { value }))
    }
    fn digest(&self, _: &ValueHandle) -> Result<Option<String>, StoreError> {
        Ok(self
            .value
            .as_ref()
            .map(|_| format!("sha256:{}", "a".repeat(64))))
    }
    fn encoded(&self, _: &ValueHandle) -> Result<Option<Vec<u8>>, StoreError> {
        Ok(None)
    }
    fn keep(&mut self, _: &ValueHandle) -> Result<bool, StoreError> {
        Ok(false)
    }
    fn is_kept(&self, _: &ValueHandle) -> Result<bool, StoreError> {
        Ok(false)
    }
    fn size(&self, _: &ValueHandle) -> Result<Option<u64>, StoreError> {
        Ok(self.value.as_ref().map(|_| 128 + self.raw.len() as u64))
    }
    fn release(&mut self, _: &ValueHandle) -> Result<bool, StoreError> {
        Ok(false)
    }
    fn begin_acquisition(&mut self, _: u64, _: Weak<()>) -> Result<ValueHandle, StoreError> {
        Ok(ValueHandle::fresh())
    }
    fn append_acquisition(
        &mut self,
        _: &ValueHandle,
        _: &str,
        bytes: &[u8],
    ) -> Result<(), StoreError> {
        if !bytes.is_empty() {
            if let Some(entered) = self.gate.entered.lock().unwrap().take() {
                let _ = entered.send(());
            }
            let mut released = self.gate.released.lock().unwrap();
            while !*released {
                released = self.gate.changed.wait(released).unwrap();
            }
        }
        self.raw.extend_from_slice(bytes);
        Ok(())
    }
    fn seal_acquisition(
        &mut self,
        handle: &ValueHandle,
        shell: &Value,
        leaves: &[LeafSelection],
    ) -> Result<StoredSnapshot, StoreError> {
        let mut data = shell.data().clone();
        let mut leaf = &mut data;
        for field in &leaves[0].fields {
            let Data::Record(record) = leaf else {
                panic!("record")
            };
            leaf = record.get_mut(field).unwrap();
        }
        *leaf = Data::Bytes(self.raw.clone().into());
        let value = Value::new(shell.shape().clone(), data, shell.provenance().clone())
            .unwrap()
            .with_metadata(shell.metadata().cloned());
        let snapshot =
            StoredSnapshot::capture(handle.clone(), format!("sha256:{}", "a".repeat(64)), &value)
                .unwrap();
        self.value = Some(value);
        Ok(snapshot)
    }
    fn abort_acquisition(&mut self, _: &ValueHandle) -> Result<(), StoreError> {
        Ok(())
    }
    fn describe(
        &self,
        handle: &ValueHandle,
        fields: &[String],
    ) -> Result<Option<StoredSnapshot>, StoreError> {
        if !fields.is_empty() {
            return Ok(None);
        }
        Ok(self.value.as_ref().and_then(|value| {
            StoredSnapshot::capture(handle.clone(), format!("sha256:{}", "a".repeat(64)), value)
        }))
    }
    fn hold(&mut self, _: &ValueHandle) -> Result<Option<ValueLease>, StoreError> {
        let gate = self.gate.hold.lock().unwrap().clone();
        if let Some(gate) = gate {
            if let Some(entered) = gate.entered.lock().unwrap().take() {
                let _ = entered.send(());
            }
            let mut released = gate.released.lock().unwrap();
            while !*released {
                released = gate.changed.wait(released).unwrap();
            }
        }
        Ok(self.value.clone().map(ValueLease::retain))
    }
}
struct Provider(Arc<Contract>);
impl StreamingInvoker for Provider {
    fn subscribe(&self, _: Call, sink: StreamSink, token: CancellationToken) -> StreamFuture {
        let contract = self.0.clone();
        Box::pin(async move {
            let recording = sink
                .begin_recording(64, contract.clone(), vec!["raw".into()])
                .await?;
            let write = recording.append(b"payload".to_vec());
            tokio::pin!(write);
            tokio::select! {biased;()=token.cancelled()=>{write.await?;},result=&mut write=>{result?;token.cancelled().await;}}
            let shell = Value::new(
                contract.shape(),
                Data::Record([("raw".into(), Data::Bytes(vec![].into()))].into()),
                Default::default(),
            )
            .unwrap();
            recording.finish(shell, RecordingEnd::Cancelled).await?;
            Err(InvocationError::Cancelled)
        })
    }
}

#[tokio::test]
async fn cancelled_source_joins_admitted_physical_write_before_sealing_its_exact_prefix() {
    let (entered, wait) = tokio::sync::oneshot::channel();
    let gate = Arc::new(Gate {
        entered: Mutex::new(Some(entered)),
        ..Default::default()
    });
    let (worker, writer) = spawn_store(
        Store {
            gate: gate.clone(),
            raw: vec![],
            value: None,
        },
        Default::default(),
    )
    .unwrap();
    let context = crate::storage::AcquisitionContext::new(
        worker
            .clone()
            .with_placement(crate::storage::ValuePlacement::MemoryAndDisk),
        Default::default(),
    );
    let mut registry = ContractRegistry::new();
    registry
        .load(r#"{"version":1,"types":{"Blob":{"base":"Record","fields":{"raw":"Bytes"}}}}"#)
        .unwrap();
    let mut runtime = Runtime::new();
    runtime
        .add(
            (),
            [],
            ExecutionTraits {
                pure: false,
                repeatable: false,
                bounded: false,
            },
        )
        .unwrap();
    let run = runtime
        .start(Duration::ZERO)
        .into_iter()
        .find_map(|effect| match effect {
            Effect::Spawn(ticket) => Some(ticket.run),
            _ => None,
        })
        .unwrap();
    let mut capability = Capability::new(
        ["watch"],
        Shape::Primitive(wes_core::Primitive::Int),
        Safety::Unsafe,
    );
    capability.streaming = true;
    let call = Call {
        authority: Default::default(),
        run,
        capability: Arc::new(capability),
        arguments: Default::default(),
    };
    let token = CancellationToken::new();
    let (handle, task) = super::super::spawn_with_recording(
        call,
        Arc::new(Provider(registry.resolve("Blob").unwrap())),
        Default::default(),
        Default::default(),
        token.clone(),
        None,
        None,
        vec![],
        None,
        Some(context),
    )
    .unwrap();
    wait.await.unwrap();
    token.cancel();
    let joined = tokio::spawn(task.join());
    tokio::task::yield_now().await;
    assert!(!joined.is_finished());
    assert!(handle.recorded().is_none());
    *gate.released.lock().unwrap() = true;
    gate.changed.notify_one();
    joined.await.unwrap().unwrap();
    let recorded = handle.recorded().expect("joined prefix");
    let (loaded, lease) = worker
        .materialize(recorded.output.snapshot.clone().unwrap())
        .await
        .unwrap();
    let Data::Record(fields) = loaded.value.data() else {
        panic!("recording")
    };
    assert_eq!(fields["acceptedBytes"], Data::Text("7".into()));
    assert_eq!(fields["complete"], Data::Bool(false));
    let Data::Record(value) = &fields["value"] else {
        panic!("shell")
    };
    assert_eq!(value["raw"], Data::Bytes(b"payload".to_vec().into()));
    drop(lease);
    drop(recorded);
    drop(handle);
    assert_eq!(worker.shutdown().await.unwrap().failed, 0);
    writer.join().await.unwrap();
}

#[tokio::test]
async fn auxiliary_recording_seals_under_the_original_business_run_only_after_physical_join() {
    let (entered, wait) = tokio::sync::oneshot::channel();
    let gate = Arc::new(Gate {
        entered: Mutex::new(Some(entered)),
        ..Default::default()
    });
    let (worker, writer) = spawn_store(
        Store {
            gate: gate.clone(),
            raw: vec![],
            value: None,
        },
        Default::default(),
    )
    .unwrap();
    let context = crate::storage::AcquisitionContext::new(
        worker
            .clone()
            .with_placement(crate::storage::ValuePlacement::MemoryAndDisk),
        Default::default(),
    );
    let mut registry = ContractRegistry::new();
    registry
        .load(r#"{"version":1,"types":{"Blob":{"base":"Record","fields":{"raw":"Bytes"}}}}"#)
        .unwrap();
    let mut runtime = Runtime::new();
    runtime
        .add(
            (),
            [],
            ExecutionTraits {
                pure: false,
                repeatable: false,
                bounded: true,
            },
        )
        .unwrap();
    let run = runtime
        .start(Duration::ZERO)
        .into_iter()
        .find_map(|e| match e {
            Effect::Spawn(ticket) => Some(ticket.run),
            _ => None,
        })
        .unwrap();
    assert!(runtime.enter_joined(&run));
    let original = crate::eventlog::owner::Owner::default();
    let reservation = original.reserve_source_with_recording(&run, true).unwrap();
    let mut capability = Capability::new(
        ["watch"],
        Shape::Primitive(wes_core::Primitive::Int),
        Safety::Unsafe,
    );
    capability.streaming = true;
    let source_cancel = CancellationToken::new();
    let (handle, task) = super::super::spawn_with_recording(
        Call {
            authority: Default::default(),
            run: run.clone(),
            capability: Arc::new(capability),
            arguments: Default::default(),
        },
        Arc::new(Provider(registry.resolve("Blob").unwrap())),
        Default::default(),
        Default::default(),
        source_cancel.clone(),
        None,
        None,
        vec![],
        None,
        Some(context),
    )
    .unwrap();
    reservation.install(handle.clone());
    let task = task.after_join({
        let original = original.clone();
        let run = run.clone();
        move || original.remove_source(&run)
    });
    wait.await.unwrap();
    let business_value = Value::new(
        Shape::Primitive(wes_core::Primitive::Int),
        Data::Int(7),
        Default::default(),
    )
    .unwrap();
    runtime
        .publish_result(
            &run,
            crate::runtime::Outcome::Produced(business_value.clone()),
            Duration::ZERO,
        )
        .unwrap();
    assert_eq!(runtime.value_of(run.node()), Some(&business_value));
    assert_eq!(runtime.run_of(run.node()), Some(run.id()));
    assert!(!runtime.is_streaming(run.node()));
    source_cancel.cancel();
    let joined = tokio::spawn(task.join());
    tokio::task::yield_now().await;
    assert!(!joined.is_finished());
    assert!(
        original
            .recorded_source(run.node(), run.id().as_str())
            .is_err()
    );
    assert!(original.source(run.node(), run.id().as_str()).is_ok());
    assert!(original.reserve_source_with_recording(&run, true).is_err());
    *gate.released.lock().unwrap() = true;
    gate.changed.notify_one();
    joined.await.unwrap().unwrap();
    let recorded = original
        .recorded_source(run.node(), run.id().as_str())
        .unwrap();
    assert!(original.recorded_source(run.node(), "foreign-run").is_err());
    runtime.complete(&run, crate::runtime::Outcome::Skipped, Duration::ZERO);
    original.retire_obsolete(&runtime);
    assert!(
        original
            .recorded_source(run.node(), run.id().as_str())
            .is_ok()
    );
    assert_eq!(runtime.value_of(run.node()), Some(&business_value));
    let (loaded, lease) = worker
        .materialize(recorded.output.snapshot.clone().unwrap())
        .await
        .unwrap();
    let Data::Record(fields) = loaded.value.data() else {
        panic!("recorded source")
    };
    assert_eq!(fields["source"], Data::Text(run.node().to_string().into()));
    assert_eq!(fields["run"], Data::Text(run.id().to_string().into()));
    assert_eq!(fields["acceptedBytes"], Data::Text("7".into()));
    assert_eq!(fields["complete"], Data::Bool(false));
    let Data::Record(raw) = &fields["value"] else {
        panic!("recording shell")
    };
    assert_eq!(raw["raw"], Data::Bytes(b"payload".to_vec().into()));
    drop(lease);
    drop(recorded);
    drop(original);
    drop(handle);
    assert_eq!(worker.shutdown().await.unwrap().failed, 0);
    writer.join().await.unwrap();
}

struct Reader {
    origin: wes_core::flow::ProviderReadOrigin,
    open: AtomicBool,
    epoch: AtomicU64,
}
impl Reader {
    fn change(&self, open: bool) {
        self.open.store(open, Ordering::Release);
        self.epoch.fetch_add(1, Ordering::AcqRel);
    }
}
impl crate::provider_reads::ReadAuthority for Reader {
    fn identity(&self) -> String {
        "synthetic-recording-reader".into()
    }
    fn realm(&self) -> Option<String> {
        Some(self.origin.realm().into())
    }
    fn closed(&self) -> bool {
        false
    }
    fn revision(&self) -> String {
        self.epoch.load(Ordering::Acquire).to_string()
    }
    fn allows(&self, origin: &wes_core::flow::ProviderReadOrigin) -> bool {
        origin == &self.origin && self.open.load(Ordering::Acquire)
    }
}
#[tokio::test]
async fn publication_preserves_withdrawn_raw_but_a_late_regrant_cannot_resurrect_an_admitted_read()
{
    let reader = Arc::new(Reader {
        origin: wes_core::flow::ProviderReadOrigin::new(
            "00000000-0000-4000-8000-000000000079".into(),
            "a".repeat(64),
            "b".repeat(64),
        )
        .unwrap(),
        open: AtomicBool::new(true),
        epoch: AtomicU64::new(0),
    });
    let provenance = Provenance::default().with_policy(
        &wes_core::flow::FlowPolicy::default().with_provider_read(reader.origin.clone()),
    );
    let (entered, wait) = tokio::sync::oneshot::channel();
    let gate = Arc::new(Gate {
        entered: Mutex::new(Some(entered)),
        ..Default::default()
    });
    let (worker, writer) = spawn_store(
        Store {
            gate: gate.clone(),
            raw: vec![],
            value: None,
        },
        Default::default(),
    )
    .unwrap();
    worker.provider_readers().register(reader.clone()).unwrap();
    let context = crate::storage::AcquisitionContext::new(
        worker
            .clone()
            .with_placement(crate::storage::ValuePlacement::MemoryAndDisk),
        provenance.clone(),
    );
    let mut registry = ContractRegistry::new();
    registry
        .load(r#"{"version":1,"types":{"Blob":{"base":"Record","fields":{"raw":"Bytes"}}}}"#)
        .unwrap();
    let mut runtime = Runtime::new();
    runtime
        .add(
            (),
            [],
            ExecutionTraits {
                pure: false,
                repeatable: false,
                bounded: false,
            },
        )
        .unwrap();
    let run = runtime
        .start(Duration::ZERO)
        .into_iter()
        .find_map(|effect| match effect {
            Effect::Spawn(ticket) => Some(ticket.run),
            _ => None,
        })
        .unwrap();
    let mut capability = Capability::new(
        ["watch"],
        Shape::Primitive(wes_core::Primitive::Int),
        Safety::Unsafe,
    );
    capability.streaming = true;
    let call = Call {
        authority: Default::default(),
        run,
        capability: Arc::new(capability),
        arguments: Default::default(),
    };
    let token = CancellationToken::new();
    let (handle, task) = super::super::spawn_with_recording(
        call,
        Arc::new(Provider(registry.resolve("Blob").unwrap())),
        provenance,
        Default::default(),
        token.clone(),
        None,
        None,
        vec![],
        None,
        Some(context),
    )
    .unwrap();
    wait.await.unwrap();
    reader.change(false);
    token.cancel();
    *gate.released.lock().unwrap() = true;
    gate.changed.notify_one();
    task.join().await.unwrap();
    let recorded = handle
        .recorded()
        .expect("owned raw survives read withdrawal without granting a read");
    assert!(matches!(
        worker
            .materialize(recorded.output.snapshot.clone().unwrap())
            .await,
        Err(StoreError::ProviderReadUnavailable)
    ));
    reader.change(true);
    let mut query = crate::tasks::BoundQuery::new(
        crate::plan::MetaTask {
            spec: wes_language::vocabulary::MetaCommand::SourceResult.spec(&[]),
            target: None,
            tail: vec![],
            subjects: vec![],
            inputs: Default::default(),
            guards: Default::default(),
        },
        None,
    );
    query.capture_recorded(Ok(recorded.clone()), &worker.provider_readers());
    let (entered, wait) = tokio::sync::oneshot::channel();
    let hold = Arc::new(Gate {
        entered: Mutex::new(Some(entered)),
        ..Default::default()
    });
    *gate.hold.lock().unwrap() = Some(hold.clone());
    let read = tokio::spawn(query.execute(
        Default::default(),
        Some(worker.clone()),
        CancellationToken::new(),
    ));
    wait.await.unwrap();
    reader.change(false);
    reader.change(true);
    *hold.released.lock().unwrap() = true;
    hold.changed.notify_one();
    let report = read.await.unwrap();
    assert!(
        matches!(report.outcome,crate::runtime::Outcome::Failed(error) if error.code()=="RUN019" || error.message().contains("changed"))
    );
    drop(recorded);
    drop(handle);
    assert_eq!(worker.shutdown().await.unwrap().failed, 0);
    writer.join().await.unwrap();
}
