use super::*;
use crate::{
    driver::{self, Command, ExecutionCapacity, Executor, Reply, Snapshot},
    graph::{NodeId, OutputRef},
    providers::{Call, InvocationFuture, Invoker, LocalScope, Providers},
    runtime::{Run, Runtime},
    streams::{
        ControlContracts, ControlFuture, ControlLimits, ControlOutcome, LiveController,
        StreamFuture, StreamSink, StreamingInvoker,
    },
    tasks::TaskExecutor,
};
use std::{
    num::NonZeroUsize,
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
};
use tokio::sync::{mpsc, oneshot};
use wes_core::{
    Data, Primitive, Provenance, Shape,
    capability::{Capability, Parameter, ProviderDescription, Safety},
    contracts::ContractRegistry,
};
mod admission;
mod storage;

fn native() -> crate::environments::InvocationAuthority {
    crate::environments::InvocationAuthority::from_source(
        &crate::source::SourceInput::new("native-fixture".into(), "synthetic".into()).unwrap(),
    )
}
fn integer(n: i64) -> Value {
    Value::new(
        Shape::Primitive(Primitive::Int),
        Data::Int(n),
        Default::default(),
    )
    .unwrap()
}
fn confirmation() -> Value {
    Value::new(
        Shape::Primitive(Primitive::Bool),
        Data::Bool(true),
        Default::default(),
    )
    .unwrap()
}
struct Business {
    run: Run,
    token: CancellationToken,
    finish: oneshot::Sender<Result<Value, InvocationError>>,
}
struct BusinessPort {
    entered: mpsc::UnboundedSender<Business>,
    calls: Arc<AtomicUsize>,
    stored: bool,
}
impl BusinessPort {
    fn start(&self, call: Call, token: CancellationToken) -> InvocationFuture {
        self.calls.fetch_add(1, Ordering::SeqCst);
        let (finish, receive) = oneshot::channel();
        self.entered
            .send(Business {
                run: call.run,
                token,
                finish,
            })
            .ok()
            .unwrap();
        Box::pin(async move { receive.await.unwrap() })
    }
}
impl Invoker for BusinessPort {
    fn invoke(&self, call: Call, token: CancellationToken) -> InvocationFuture {
        assert!(!self.stored, "stored work requires native acquisition");
        self.start(call, token)
    }
    fn invoke_acquired(
        &self,
        call: Call,
        token: CancellationToken,
        context: crate::storage::AcquisitionContext,
    ) -> crate::storage::AcquiredFuture {
        let result = self.start(call, token);
        let stored = self.stored;
        Box::pin(async move {
            let value = result.await?;
            if stored {
                context
                    .begin(64)
                    .await
                    .map_err(crate::storage::acquisition_error)?
                    .finish(value, vec![])
                    .await
            } else {
                Ok(crate::storage::AcquiredOutput::Value(value))
            }
        })
    }
}
struct SourceEntered {
    run: Run,
    arguments: indexmap::IndexMap<String, Value>,
    sink: StreamSink,
    token: CancellationToken,
    finish: oneshot::Sender<Result<(), InvocationError>>,
}
struct SourcePort {
    entered: mpsc::UnboundedSender<SourceEntered>,
    calls: Arc<AtomicUsize>,
    contracts: ControlContracts,
    recording: Option<Arc<wes_core::contracts::Contract>>,
    profile: Arc<crate::providers::profiles::SourceProfile>,
}
impl Invoker for SourcePort {
    fn invoke(&self, _: Call, _: CancellationToken) -> InvocationFuture {
        panic!("source is streaming")
    }
}
impl StreamingInvoker for SourcePort {
    fn source_profile(
        &self,
        _: &Capability,
    ) -> Option<Arc<crate::providers::profiles::SourceProfile>> {
        Some(self.profile.clone())
    }
    fn records_bytes(&self, _: &Capability) -> bool {
        self.recording.is_some()
    }
    fn control_contracts(&self, _: &Capability) -> Option<ControlContracts> {
        Some(self.contracts.clone())
    }
    fn subscribe(&self, call: Call, sink: StreamSink, token: CancellationToken) -> StreamFuture {
        let entered = self.entered.clone();
        let calls = self.calls.clone();
        let contract = self.recording.clone();
        Box::pin(async move {
            // This models the declarative backend's reservation before its external launch.
            let recording = if let Some(contract) = &contract {
                let recording = sink
                    .begin_recording(64, contract.clone(), vec!["raw".into()])
                    .await?;
                recording.append(b"synthetic-packets".to_vec()).await?;
                Some(recording)
            } else {
                None
            };
            calls.fetch_add(1, Ordering::SeqCst);
            let (finish, receive) = oneshot::channel();
            entered
                .send(SourceEntered {
                    run: call.run,
                    arguments: call.arguments,
                    sink,
                    token,
                    finish,
                })
                .ok()
                .unwrap();
            let result = receive.await.unwrap();
            if let Some(recording) = recording {
                let contract = contract.unwrap();
                let shell = Value::new(
                    contract.shape(),
                    Data::Record([("raw".into(), Data::Bytes(vec![].into()))].into()),
                    Default::default(),
                )
                .unwrap();
                let end = if result.is_ok() {
                    crate::streams::RecordingEnd::Ended
                } else {
                    crate::streams::RecordingEnd::Failed
                };
                recording.finish(shell, end).await?;
            }
            result
        })
    }
}
struct Control {
    token: CancellationToken,
    action: Value,
    finish: oneshot::Sender<ControlOutcome>,
}
struct Controller {
    entered: mpsc::UnboundedSender<Control>,
    calls: AtomicUsize,
    allowed: AtomicBool,
}
impl LiveController for Controller {
    fn authorize(&self, caller: &crate::environments::InvocationAuthority, _: &Value) -> bool {
        caller.is_local_user() && self.allowed.load(Ordering::SeqCst)
    }
    fn invoke(&self, action: Value, token: CancellationToken) -> ControlFuture {
        self.calls.fetch_add(1, Ordering::SeqCst);
        let (finish, receive) = oneshot::channel();
        self.entered
            .send(Control {
                token,
                action,
                finish,
            })
            .ok()
            .unwrap();
        Box::pin(async move { receive.await.unwrap_or(ControlOutcome::Unknown) })
    }
}
struct Fixture {
    providers: Providers,
    businesses: mpsc::UnboundedReceiver<Business>,
    sources: mpsc::UnboundedReceiver<SourceEntered>,
    controls: mpsc::UnboundedReceiver<Control>,
    controller: Arc<Controller>,
    contracts: ControlContracts,
    business_calls: Arc<AtomicUsize>,
    source_calls: Arc<AtomicUsize>,
}
impl Fixture {
    fn new(recording: bool, stored: bool) -> Self {
        let mut registry = ContractRegistry::new();
        if recording {
            registry.load(r#"{"version":1,"types":{"RawCapture":{"base":"Record","fields":{"raw":"Bytes"}}}}"#).unwrap();
        }
        let contracts = ControlContracts {
            input: registry.resolve("Int").unwrap(),
            output: registry.resolve("Bool").unwrap(),
        };
        let (businesses, business_receive) = mpsc::unbounded_channel();
        let business_calls = Arc::new(AtomicUsize::new(0));
        let business = Arc::new(BusinessPort {
            entered: businesses,
            calls: business_calls.clone(),
            stored,
        });
        let mut work = Capability::new(["work"], Shape::Primitive(Primitive::Int), Safety::Safe);
        work.parameters = vec![Parameter::new(
            "number",
            Shape::Primitive(Primitive::Int),
            true,
        )];
        let (sources, source_receive) = mpsc::unbounded_channel();
        let source_calls = Arc::new(AtomicUsize::new(0));
        let source = Arc::new(SourcePort {
            entered: sources,
            calls: source_calls.clone(),
            contracts: contracts.clone(),
            recording: recording.then(|| registry.resolve("RawCapture").unwrap()),
            profile: Arc::new(
                crate::providers::profiles::SourceProfile::new(
                    crate::providers::profiles::ProfileDeclaration {
                        name: "synthetic-profile".into(),
                        kind: "metrics".into(),
                        default: true,
                        stop: integer(1),
                        ready_wait: Duration::from_secs(10),
                        finalization: FinalizationLimits::default(),
                        options: [(
                            "filter".into(),
                            crate::providers::profiles::ProfileOption {
                                parameter: "note".into(),
                                contract: registry.resolve("Text").unwrap(),
                                restriction:
                                    crate::providers::profiles::Restriction::BackendCondition,
                            },
                        )]
                        .into(),
                    },
                    contracts.clone(),
                )
                .unwrap(),
            ),
        });
        let mut watch = Capability::new(["watch"], Shape::Primitive(Primitive::Int), Safety::Safe);
        watch.streaming = true;
        watch.parameters = vec![Parameter::new(
            "note",
            Shape::Primitive(Primitive::Text),
            false,
        )];
        let mut providers = Providers::local(LocalScope::new("synthetic").unwrap());
        providers.register(
            ProviderDescription::new("business", [work], vec![]).unwrap(),
            business,
        );
        providers.register_ports(
            ProviderDescription::new("capture", [watch], vec![]).unwrap(),
            source.clone(),
            Some(source),
        );
        let (controls, control_receive) = mpsc::unbounded_channel();
        let controller = Arc::new(Controller {
            entered: controls,
            calls: AtomicUsize::new(0),
            allowed: AtomicBool::new(true),
        });
        Self {
            providers,
            businesses: business_receive,
            sources: source_receive,
            controls: control_receive,
            controller,
            contracts,
            business_calls,
            source_calls,
        }
    }
    fn call(&self, text: &str) -> BoundCall {
        let parsed = wes_language::parse(&wes_language::SourceText::new("synthetic", text));
        let statement = &parsed.script.statements[0];
        let wes_language::Expression::Call(call) = &statement.expression else {
            panic!("call")
        };
        let resolution = wes_language::resolve::resolve(call, self.providers.catalogue()).unwrap();
        let crate::plan::Plan::NewNode(node) =
            crate::plan::plan(&resolution, statement, &Default::default(), &|_| None).unwrap()
        else {
            panic!("node")
        };
        let crate::plan::Task::Invoke(invocation) = node.task else {
            panic!("invocation")
        };
        let mut bound = if invocation.capability.streaming {
            self.providers.bind_stream(invocation)
        } else {
            self.providers.bind_finite(invocation)
        }
        .unwrap();
        bound.set_authority(native());
        bound
    }
    fn task(&self, tail: Duration) -> BoundTask {
        BoundTask::Companion(Box::new(
            BoundCompanion::new(
                self.call("business work number:7"),
                self.call("capture watch"),
                integer(1),
                Duration::from_secs(10),
                FinalizationLimits {
                    tail,
                    ..Default::default()
                },
            )
            .unwrap(),
        ))
    }
    fn install(&self, source: &SourceEntered) {
        source
            .sink
            .install_controls(
                self.contracts.input.clone(),
                self.contracts.output.clone(),
                ControlLimits::default(),
                self.controller.clone(),
            )
            .unwrap();
    }
    fn ready(&self, source: &SourceEntered) {
        source.sink.opened().unwrap();
        self.install(source);
    }
}
struct Running {
    handle: driver::DriverHandle<BoundTask>,
    task: driver::DriverTask,
    node: NodeId,
    capacity: ExecutionCapacity,
    owner: Owner,
}
impl Running {
    async fn start(payload: BoundTask, storage: Option<crate::storage::StoreWorker>) -> Self {
        let owner = Owner::default();
        let capacity = ExecutionCapacity::with_stream_limit(
            NonZeroUsize::new(1).unwrap(),
            NonZeroUsize::new(1).unwrap(),
        )
        .unwrap();
        let executor = TaskExecutor::ephemeral()
            .with_recordings(owner.clone())
            .with_storage(storage);
        let (handle, task) =
            driver::spawn_with_capacity(Runtime::new(), Arc::new(executor), capacity.clone())
                .unwrap();
        let Reply::Added(node) = handle
            .command(Command::Add {
                traits: payload.traits(),
                dependencies: payload.dependencies().collect(),
                payload,
            })
            .await
            .unwrap()
        else {
            panic!("add")
        };
        handle.command(Command::Start).await.unwrap();
        Self {
            handle,
            task,
            node,
            capacity,
            owner,
        }
    }
    async fn snapshot(&self) -> Snapshot<BoundTask> {
        let Reply::Snapshot(snapshot) = self.handle.command(Command::Snapshot).await.unwrap()
        else {
            panic!("snapshot")
        };
        *snapshot
    }
    async fn close(self) {
        self.handle.command(Command::Shutdown).await.unwrap();
        self.task.join().await.unwrap();
    }
    async fn joined(&self) {
        let mut capacity = self.capacity.subscribe();
        tokio::time::timeout(Duration::from_secs(5), async {
            while capacity.borrow_and_update().streams.used != 0 {
                capacity.changed().await.unwrap();
            }
            // Physical completion preserves an already published business value;
            // it need not emit another data observation. Read serialized runtime state.
            while self.snapshot().await.executing.contains(&self.node) {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
    }
}
async fn next<T>(receive: &mut mpsc::UnboundedReceiver<T>) -> T {
    tokio::time::timeout(Duration::from_secs(5), receive.recv())
        .await
        .unwrap()
        .unwrap()
}

#[tokio::test]
async fn original_run_requires_ready_and_controls_then_releases_result_before_physical_stop_join() {
    let mut f = Fixture::new(false, false);
    let running = Running::start(f.task(Duration::ZERO), None).await;
    let source = next(&mut f.sources).await;
    assert_eq!(source.run.node(), &running.node);
    source.sink.opened().unwrap();
    assert_eq!(f.business_calls.load(Ordering::SeqCst), 0);
    assert!(!running.snapshot().await.values.contains_key(&running.node));
    f.install(&source);
    let business = next(&mut f.businesses).await;
    assert_eq!(business.run, source.run);
    let child = BoundTask::TypeCheck(crate::tasks::BoundTypeCheck {
        task: crate::plan::MetaTask {
            spec: wes_language::vocabulary::MetaCommand::Type.spec(&["check".into()]),
            tail: vec!["check".into()],
            target: None,
            subjects: vec![],
            guards: Default::default(),
            inputs: [(
                "value".into(),
                crate::plan::Input::FromNode(OutputRef::data(running.node.clone())),
            )]
            .into(),
        },
        contract: ContractRegistry::new().resolve("Int").unwrap(),
    });
    let Reply::Added(child) = running
        .handle
        .command(Command::Add {
            dependencies: child.dependencies().collect(),
            traits: child.traits(),
            payload: child,
        })
        .await
        .unwrap()
    else {
        panic!("child")
    };
    business.finish.send(Ok(integer(7))).unwrap();
    let control = next(&mut f.controls).await;
    assert_eq!(control.action.data(), integer(1).data());
    running.handle.wait_idle().await.unwrap();
    source.sink.push(integer(99)).unwrap();
    let snapshot = running.snapshot().await;
    assert_eq!(snapshot.values[&running.node].data(), integer(7).data());
    assert_eq!(snapshot.values[&child].data(), integer(7).data());
    assert!(snapshot.streaming.is_empty());
    assert!(snapshot.executing.contains(&running.node));
    assert_eq!(running.capacity.subscribe().borrow().streams.used, 1);
    control
        .finish
        .send(ControlOutcome::Confirmed(confirmation()))
        .ok()
        .unwrap();
    assert!(
        running
            .owner
            .source(&running.node, source.run.id().as_str())
            .is_ok()
    );
    source.finish.send(Ok(())).unwrap();
    running.joined().await;
    assert_eq!(f.controller.calls.load(Ordering::SeqCst), 1);
    assert_eq!(running.capacity.subscribe().borrow().streams.used, 0);
    running.close().await;
}

#[tokio::test]
async fn cancellation_starts_original_stop_while_business_and_controller_are_still_physically_held()
{
    let mut f = Fixture::new(false, false);
    let running = Running::start(f.task(Duration::from_secs(60)), None).await;
    let source = next(&mut f.sources).await;
    f.ready(&source);
    let business = next(&mut f.businesses).await;
    running
        .handle
        .command(Command::Cancel(running.node.clone()))
        .await
        .unwrap();
    let control = next(&mut f.controls).await;
    assert!(business.token.is_cancelled());
    assert!(
        !control.token.is_cancelled(),
        "cleanup is independent from the retired business token"
    );
    assert!(!source.token.is_cancelled());
    assert_eq!(running.capacity.subscribe().borrow().streams.used, 1);
    source.finish.send(Ok(())).unwrap();
    business
        .finish
        .send(Err(InvocationError::Cancelled))
        .unwrap();
    control
        .finish
        .send(ControlOutcome::Confirmed(confirmation()))
        .ok()
        .unwrap();
    running.joined().await;
    assert_eq!(f.business_calls.load(Ordering::SeqCst), 1);
    assert_eq!(f.controller.calls.load(Ordering::SeqCst), 1);
    running.close().await;
}

#[tokio::test]
async fn unknown_stop_cancels_without_retry_and_retains_capacity_until_source_and_controller_join()
{
    let mut f = Fixture::new(false, false);
    let running = Running::start(f.task(Duration::ZERO), None).await;
    let mut notices = running.handle.subscribe_notices().unwrap();
    let source = next(&mut f.sources).await;
    f.ready(&source);
    next(&mut f.businesses)
        .await
        .finish
        .send(Ok(integer(7)))
        .unwrap();
    let control = next(&mut f.controls).await;
    control.finish.send(ControlOutcome::Unknown).ok().unwrap();
    source.token.cancelled().await;
    running.handle.wait_idle().await.unwrap();
    assert_eq!(
        running.snapshot().await.values[&running.node].data(),
        integer(7).data()
    );
    assert_eq!(running.capacity.subscribe().borrow().streams.used, 1);
    assert_eq!(f.controller.calls.load(Ordering::SeqCst), 1);
    source.finish.send(Err(InvocationError::Cancelled)).unwrap();
    let notice = tokio::time::timeout(Duration::from_secs(5), notices.recv())
        .await
        .unwrap()
        .unwrap();
    assert!(notice.error.message().contains("No control was retried"));
    running.joined().await;
    assert_eq!(f.business_calls.load(Ordering::SeqCst), 1);
    running.close().await;
}

#[tokio::test]
async fn capture_failure_keeps_held_business_alive_and_protects_current_private_notice_without_tainting_result()
 {
    let mut f = Fixture::new(false, false);
    let running = Running::start(f.task(Duration::ZERO), None).await;
    let mut notices = running.handle.subscribe_notices().unwrap();
    let source = next(&mut f.sources).await;
    f.ready(&source);
    let business = next(&mut f.businesses).await;
    let private = integer(99).with_provenance(
        Provenance::default().with_policy(&wes_core::flow::FlowPolicy::default().private()),
    );
    source.sink.push(private).unwrap();
    source
        .finish
        .send(Err(InvocationError::Failed(
            RuntimeCode::ExecutionFailed.error("synthetic capture failure", None),
        )))
        .unwrap();
    let source_handle = running
        .owner
        .source(&running.node, source.run.id().as_str())
        .unwrap();
    let mut changes = source_handle.subscribe();
    while !matches!(changes.borrow_and_update().phase, Phase::Failed(_)) {
        changes.changed().await.unwrap();
    }
    assert!(!business.token.is_cancelled());
    business.finish.send(Ok(integer(7))).unwrap();
    running.handle.wait_idle().await.unwrap();
    assert!(
        !running.snapshot().await.values[&running.node]
            .provenance()
            .policy()
            .is_private()
    );
    let notice = tokio::time::timeout(Duration::from_secs(5), notices.recv())
        .await
        .unwrap()
        .unwrap();
    assert!(notice.error.policy().is_private());
    assert!(!notice.error.message().contains("synthetic capture failure"));
    running.joined().await;
    assert_eq!(f.business_calls.load(Ordering::SeqCst), 1);
    assert_eq!(f.controller.calls.load(Ordering::SeqCst), 0);
    running.close().await;
}

#[tokio::test]
async fn readiness_cancellation_has_zero_business_entry_and_holds_original_source_until_join() {
    let mut f = Fixture::new(false, false);
    let running = Running::start(f.task(Duration::ZERO), None).await;
    let source = next(&mut f.sources).await;
    running
        .handle
        .command(Command::Cancel(running.node.clone()))
        .await
        .unwrap();
    source.token.cancelled().await;
    assert_eq!(f.business_calls.load(Ordering::SeqCst), 0);
    assert_eq!(f.controller.calls.load(Ordering::SeqCst), 0);
    assert_eq!(running.capacity.subscribe().borrow().streams.used, 1);
    source.finish.send(Err(InvocationError::Cancelled)).unwrap();
    running.joined().await;
    running.close().await;
}

#[tokio::test]
async fn invalid_business_or_typed_stop_has_zero_external_source_and_business_entry() {
    for invalid_business in [true, false] {
        let f = Fixture::new(false, false);
        let mut task = f.task(Duration::ZERO);
        let BoundTask::Companion(companion) = &mut task else {
            unreachable!()
        };
        if invalid_business {
            companion.call = companion.call.clone().with_inputs(
                [("number".into(), crate::plan::Input::Literal(confirmation()))].into(),
            );
        } else {
            companion.stop = confirmation();
        }
        let running = Running::start(task, None).await;
        running.handle.wait_idle().await.unwrap();
        assert!(running.snapshot().await.errors.contains_key(&running.node));
        assert_eq!(f.business_calls.load(Ordering::SeqCst), 0);
        assert_eq!(f.source_calls.load(Ordering::SeqCst), 0);
        running.joined().await;
        running.close().await;
    }
}

#[tokio::test]
async fn declined_auxiliary_attachment_joins_original_source_and_never_enters_business() {
    let mut f = Fixture::new(false, false);
    let task = f.task(Duration::ZERO);
    let mut runtime = Runtime::new();
    runtime.add(task.clone(), [], task.traits()).unwrap();
    let ticket = runtime
        .start(Duration::ZERO)
        .into_iter()
        .find_map(|effect| match effect {
            crate::runtime::Effect::Spawn(ticket) => Some(ticket),
            _ => None,
        })
        .unwrap();
    let owner = Owner::default();
    let run = ticket.run.clone();
    let (publication, _results) = mpsc::channel(1);
    let (updates, mut source_updates) = mpsc::channel(1);
    let reporter = Reporter::new(
        run.clone(),
        publication,
        Arc::new(tokio::sync::Semaphore::new(65536)),
        Arc::new(std::sync::Mutex::new(None)),
    )
    .with_sources(updates);
    let work = TaskExecutor::ephemeral()
        .with_recordings(owner.clone())
        .execute_joined(
            ticket,
            CancellationToken::new(),
            driver::progress::Reporter::silent(),
            reporter,
        );
    let joined = tokio::spawn(work);
    let source = next(&mut f.sources).await;
    source_updates
        .recv()
        .await
        .unwrap()
        .register(&runtime, &mut Default::default());
    source.token.cancelled().await;
    assert!(!joined.is_finished());
    assert!(owner.source(run.node(), run.id().as_str()).is_ok());
    assert_eq!(f.business_calls.load(Ordering::SeqCst), 0);
    source.finish.send(Err(InvocationError::Cancelled)).unwrap();
    let report = joined.await.unwrap();
    assert!(matches!(report.outcome, Outcome::Failed(_)));
    assert!(owner.source(run.node(), run.id().as_str()).is_err());
}

#[tokio::test]
async fn business_error_and_unknown_outcome_are_published_once_and_preserved_through_failed_stop() {
    for code in ["SYN001", ErrorValue::REMOTE_OUTCOME_UNKNOWN] {
        let mut f = Fixture::new(false, false);
        let running = Running::start(f.task(Duration::ZERO), None).await;
        let source = next(&mut f.sources).await;
        f.ready(&source);
        let error = ErrorValue::new(
            wes_core::ErrorId::new(uuid::Uuid::new_v4().to_string()).unwrap(),
            code,
            "synthetic business error",
            vec![],
            None,
        )
        .unwrap();
        next(&mut f.businesses)
            .await
            .finish
            .send(Err(InvocationError::Failed(error)))
            .unwrap();
        let control = next(&mut f.controls).await;
        running.handle.wait_idle().await.unwrap();
        assert_eq!(running.snapshot().await.errors[&running.node].code(), code);
        control.finish.send(ControlOutcome::Refused).ok().unwrap();
        source.token.cancelled().await;
        source.finish.send(Err(InvocationError::Cancelled)).unwrap();
        running.joined().await;
        assert_eq!(running.snapshot().await.errors[&running.node].code(), code);
        assert_eq!(f.business_calls.load(Ordering::SeqCst), 1);
        assert_eq!(f.controller.calls.load(Ordering::SeqCst), 1);
        running.close().await;
    }
}

#[tokio::test(start_paused = true)]
async fn acknowledged_result_has_bounded_tail_before_one_original_stop() {
    let mut f = Fixture::new(false, false);
    let running = Running::start(f.task(Duration::from_secs(2)), None).await;
    let source = next(&mut f.sources).await;
    f.ready(&source);
    next(&mut f.businesses)
        .await
        .finish
        .send(Ok(integer(7)))
        .unwrap();
    running.handle.wait_idle().await.unwrap();
    // Synchronize the owner entering its tail before advancing the synthetic clock.
    for _ in 0..4 {
        tokio::task::yield_now().await;
    }
    assert_eq!(f.controller.calls.load(Ordering::SeqCst), 0);
    tokio::time::advance(Duration::from_secs(1)).await;
    assert_eq!(f.controller.calls.load(Ordering::SeqCst), 0);
    tokio::time::advance(Duration::from_secs(1)).await;
    let control = next(&mut f.controls).await;
    control
        .finish
        .send(ControlOutcome::Confirmed(confirmation()))
        .ok()
        .unwrap();
    source.finish.send(Ok(())).unwrap();
    running.joined().await;
    assert_eq!(f.controller.calls.load(Ordering::SeqCst), 1);
    running.close().await;
}

#[tokio::test]
async fn resolved_default_profile_executes_through_original_run_companion_with_native_principal_and_private_source_options()
 {
    let mut f = Fixture::new(false, false);
    let call = f.call("business work number:7");
    let condition = Value::new(
        Shape::Primitive(Primitive::Text),
        Data::Text("synthetic-condition".into()),
        Provenance::default().with_policy(&wes_core::flow::FlowPolicy::default().private()),
    )
    .unwrap();
    let profile = f
        .providers
        .resolve_source_profile(
            &crate::providers::profiles::ProfileSelector::Default("metrics".into()),
            Some(&call),
            &[("filter".into(), condition)].into(),
            None,
        )
        .unwrap();
    let mut payload = BoundTask::Companion(Box::new(profile.into_companion(call).unwrap()));
    // The real session stamps both captured providers at serialized native entry.
    payload.set_authority(native());
    let running = Running::start(payload, None).await;
    let source = next(&mut f.sources).await;
    assert_eq!(
        source.arguments["note"].data(),
        &Data::Text("synthetic-condition".into())
    );
    assert!(source.arguments["note"].provenance().policy().is_private());
    f.ready(&source);
    let business = next(&mut f.businesses).await;
    assert_eq!(business.run, source.run);
    business.finish.send(Ok(integer(7))).unwrap();
    let control = next(&mut f.controls).await;
    running.handle.wait_idle().await.unwrap();
    assert_eq!(
        running.snapshot().await.values[&running.node].data(),
        integer(7).data()
    );
    assert!(
        !running.snapshot().await.values[&running.node]
            .provenance()
            .policy()
            .is_private()
    );
    control
        .finish
        .send(ControlOutcome::Confirmed(confirmation()))
        .ok()
        .unwrap();
    source.finish.send(Ok(())).unwrap();
    running.joined().await;
    assert_eq!(f.business_calls.load(Ordering::SeqCst), 1);
    assert_eq!(f.source_calls.load(Ordering::SeqCst), 1);
    running.close().await;
}
