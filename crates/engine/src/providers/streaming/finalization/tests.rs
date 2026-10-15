use super::*;
use crate::streams::{
    ControlFuture, ControlLimits, ControlOutcome, LiveController, StreamFuture, StreamSink,
    StreamingInvoker,
};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use tokio::sync::{mpsc, oneshot};
use wes_core::{Data, Primitive, contracts::ContractRegistry};

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
struct Entered {
    sink: StreamSink,
    cancellation: CancellationToken,
    finish: oneshot::Sender<Result<(), InvocationError>>,
}
struct Control {
    action: Value,
    cancellation: CancellationToken,
    finish: oneshot::Sender<ControlOutcome>,
}
struct Port {
    contracts: Option<ControlContracts>,
    entered: mpsc::UnboundedSender<Entered>,
    calls: Arc<AtomicUsize>,
    resource: String,
}
impl Invoker for Port {
    fn invoke(&self, _: Call, _: CancellationToken) -> InvocationFuture {
        panic!("not finite")
    }
}
impl StreamingInvoker for Port {
    fn exclusive_resources(
        &self,
        _: &Call,
    ) -> Result<Vec<streams::ExclusiveResource>, streams::StreamError> {
        Ok(vec![streams::ExclusiveResource::new(
            "synthetic.finalization",
            vec![self.resource.clone()],
        )?])
    }
    fn control_contracts(&self, _: &Capability) -> Option<ControlContracts> {
        self.contracts.clone()
    }
    fn subscribe(
        &self,
        _: Call,
        sink: StreamSink,
        cancellation: CancellationToken,
    ) -> StreamFuture {
        self.calls.fetch_add(1, Ordering::SeqCst);
        sink.opened().unwrap();
        let (finish, receive) = oneshot::channel();
        self.entered
            .send(Entered {
                sink,
                cancellation,
                finish,
            })
            .unwrap();
        Box::pin(async move { receive.await.unwrap_or(Err(InvocationError::Cancelled)) })
    }
}
struct Controller {
    allowed: Arc<AtomicBool>,
    calls: Arc<AtomicUsize>,
    entered: mpsc::UnboundedSender<Control>,
}
impl LiveController for Controller {
    fn authorize(&self, caller: &crate::environments::InvocationAuthority, _: &Value) -> bool {
        caller.is_local_user() && self.allowed.load(Ordering::SeqCst)
    }
    fn invoke(&self, action: Value, cancellation: CancellationToken) -> ControlFuture {
        let entered = self.entered.clone();
        let calls = self.calls.clone();
        Box::pin(async move {
            calls.fetch_add(1, Ordering::SeqCst);
            let (finish, receive) = oneshot::channel();
            entered
                .send(Control {
                    action,
                    cancellation,
                    finish,
                })
                .unwrap();
            receive.await.unwrap_or(ControlOutcome::Unknown)
        })
    }
}
struct Fixture {
    prepared: Option<PreparedSource>,
    contracts: ControlContracts,
    started: mpsc::UnboundedReceiver<Entered>,
    controls: mpsc::UnboundedReceiver<Control>,
    controller: Arc<Controller>,
    source_calls: Arc<AtomicUsize>,
    control_calls: Arc<AtomicUsize>,
    authority: Option<crate::environments::Authority>,
    binding: Option<wes_core::environments::Binding>,
}
impl Fixture {
    async fn new(metadata: bool) -> Self {
        Self::scoped(metadata, false, false).await
    }
    async fn scoped(metadata: bool, environment: bool, read: bool) -> Self {
        let registry = ContractRegistry::new();
        let contracts = ControlContracts {
            input: registry.resolve("Int").unwrap(),
            output: registry.resolve("Bool").unwrap(),
        };
        let (entered, started) = mpsc::unbounded_channel();
        let source_calls = Arc::new(AtomicUsize::new(0));
        let port = Arc::new(Port {
            contracts: metadata.then(|| contracts.clone()),
            entered,
            calls: source_calls.clone(),
            resource: Uuid::new_v4().to_string(),
        });
        let mut cap = Capability::new(["watch"], Shape::Primitive(Primitive::Int), Safety::Safe);
        cap.streaming = true;
        let mut prove = Capability::new(["prove"], Shape::Primitive(Primitive::Bool), Safety::Safe);
        if read {
            for capability in [&mut cap, &mut prove] {
                capability.parameters = vec![wes_core::capability::Parameter::new(
                    "scope",
                    Shape::Primitive(Primitive::Text),
                    true,
                )];
            }
        }
        let mut providers = Providers::local(LocalScope::new("synthetic").unwrap());
        providers.register_ports(
            ProviderDescription::new("fixture", [cap, prove], vec![]).unwrap(),
            port.clone(),
            Some(port),
        );
        let (authority, binding) = if environment {
            use wes_core::environments::{CapturedSource, CapturedSources, Package};
            let package = Package::parse("version: 1\ntargets: {local: {kind: local}}\nenvironments: {lab: {imports: {fixture: {source: {kind: spec, file: synthetic.json}, bind: {target: local}}}}}").unwrap();
            let mut sources = CapturedSources::default();
            for key in package.required_sources() {
                sources
                    .insert(key, CapturedSource::new("synthetic/v1", "{}").unwrap())
                    .unwrap();
            }
            let mut registry = crate::environments::Registry::default();
            registry
                .apply(registry.plan(&package, &sources).unwrap())
                .unwrap();
            let binding = registry.inspect("lab").unwrap().bind("fixture").unwrap();
            let authority = crate::environments::Authority::default();
            authority.establish_namespace(&Uuid::new_v4().to_string());
            authority.enable("lab").unwrap();
            let original = providers.providers["fixture"].clone();
            let read_domain = read.then(|| crate::provider_reads::ReadDomain {
                scope: vec!["scope".into()],
                revalidate: vec!["prove".into()],
            });
            if let Some(domain) = &read_domain {
                let digest = domain
                    .digest(&IndexMap::from([(
                        "scope".into(),
                        crate::trace::text("lab"),
                    )]))
                    .unwrap();
                authority
                    .begin_read_probe(&binding, "fixture", &digest, &native())
                    .unwrap()
                    .confirm()
                    .unwrap();
            }
            providers.providers.insert(
                "fixture".into(),
                Arc::new(Provider {
                    scope: ExecutionScope::Environment {
                        binding: binding.clone(),
                        authority: Some(authority.clone()),
                    },
                    read_domain,
                    ..(*original).clone()
                }),
            );
            (Some(authority), Some(binding))
        } else {
            (None, None)
        };
        let parsed = wes_language::parse(&wes_language::SourceText::new(
            "synthetic",
            if read {
                "fixture watch scope:lab"
            } else {
                "fixture watch"
            },
        ));
        let statement = &parsed.script.statements[0];
        let wes_language::Expression::Call(call) = &statement.expression else {
            panic!("call")
        };
        let resolved = wes_language::resolve::resolve(call, providers.catalogue()).unwrap();
        let crate::plan::Plan::NewNode(node) =
            crate::plan::plan(&resolved, statement, &Default::default(), &|_| None).unwrap()
        else {
            panic!("node")
        };
        let crate::plan::Task::Invoke(invocation) = node.task else {
            panic!("invocation")
        };
        let mut bound = providers.bind_stream(invocation).unwrap();
        bound.set_authority(native());
        let mut runtime = crate::runtime::Runtime::new();
        runtime.add(bound.clone(), [], bound.traits()).unwrap();
        let ticket = runtime
            .start(Duration::ZERO)
            .into_iter()
            .find_map(|e| match e {
                crate::runtime::Effect::Spawn(ticket) => Some(ticket),
                _ => None,
            })
            .unwrap();
        let prepared = CallExecutor::ephemeral()
            .prepare_source(ticket, CancellationToken::new())
            .await
            .unwrap();
        let (entered, controls) = mpsc::unbounded_channel();
        let control_calls = Arc::new(AtomicUsize::new(0));
        let controller = Arc::new(Controller {
            allowed: Arc::new(AtomicBool::new(true)),
            calls: control_calls.clone(),
            entered,
        });
        Self {
            prepared: Some(prepared),
            contracts,
            started,
            controls,
            controller,
            source_calls,
            control_calls,
            authority,
            binding,
        }
    }
    fn install(&self, entered: &Entered) {
        entered
            .sink
            .install_controls(
                self.contracts.input.clone(),
                self.contracts.output.clone(),
                ControlLimits::default(),
                self.controller.clone(),
            )
            .unwrap();
    }
    async fn start(&mut self, limits: FinalizationLimits) -> (OwnedSource, BoundStop, Entered) {
        let plan = self
            .prepared
            .as_ref()
            .unwrap()
            .prepare_stop(integer(7), limits)
            .unwrap();
        let owner = self.prepared.take().unwrap().start_owned().unwrap();
        let entered = self.started.recv().await.unwrap();
        self.install(&entered);
        let ready = owner
            .wait_ready(Duration::from_secs(60), CancellationToken::new())
            .await
            .unwrap();
        let stop = owner.bind_stop(plan, &ready).unwrap();
        (owner, stop, entered)
    }
}

#[tokio::test]
async fn withdrawal_and_regrant_never_revive_original_stop_authority() {
    for read in [false, true] {
        let mut f = Fixture::scoped(true, true, read).await;
        let (owner, stop, entered) = f.start(Default::default()).await;
        let authority = f.authority.as_ref().unwrap();
        if read {
            let domain = crate::provider_reads::ReadDomain {
                scope: vec!["scope".into()],
                revalidate: vec!["prove".into()],
            };
            let digest = domain
                .digest(&IndexMap::from([(
                    "scope".into(),
                    crate::trace::text("lab"),
                )]))
                .unwrap();
            authority
                .begin_read_probe(f.binding.as_ref().unwrap(), "fixture", &digest, &native())
                .unwrap()
                .confirm()
                .unwrap();
        } else {
            authority.disable("lab").unwrap();
            authority.enable("lab").unwrap();
        }
        let closing = tokio::spawn(owner.finalize(stop, CancellationToken::new()));
        entered.cancellation.cancelled().await;
        assert!(!closing.is_finished());
        assert_eq!(f.control_calls.load(Ordering::SeqCst), 0);
        entered
            .finish
            .send(Err(InvocationError::Cancelled))
            .unwrap();
        let receipt = closing.await.unwrap();
        assert_eq!(receipt.control, Some(Err(ControlError::Changed)));
        assert!(receipt.joined);
        assert_eq!(f.source_calls.load(Ordering::SeqCst), 1);
    }
}

#[tokio::test]
async fn a_declared_source_cannot_finish_successfully_without_installing_its_controls() {
    let mut f = Fixture::new(true).await;
    let owner = f.prepared.take().unwrap().start_owned().unwrap();
    let handle = owner.handle().clone();
    let entered = f.started.recv().await.unwrap();
    entered.finish.send(Ok(())).unwrap();
    owner.join().await.unwrap();
    assert!(matches!(handle.snapshot().phase, Phase::Failed(_)));
    assert_eq!(f.control_calls.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn static_stop_validation_refuses_before_any_subscription() {
    let mut f = Fixture::new(false).await;
    assert!(
        f.prepared
            .as_ref()
            .unwrap()
            .prepare_stop(integer(7), FinalizationLimits::default())
            .is_err()
    );
    f = Fixture::new(true).await;
    let malformed = Value::new(
        Shape::Primitive(Primitive::Text),
        Data::Text("invalid".into()),
        Default::default(),
    )
    .unwrap();
    assert!(
        f.prepared
            .as_ref()
            .unwrap()
            .prepare_stop(malformed, FinalizationLimits::default())
            .is_err()
    );
    for limits in [
        FinalizationLimits {
            tail: Duration::from_secs(61),
            ..Default::default()
        },
        FinalizationLimits {
            control: Duration::ZERO,
            ..Default::default()
        },
        FinalizationLimits {
            graceful: Duration::ZERO,
            ..Default::default()
        },
    ] {
        assert!(
            f.prepared
                .as_ref()
                .unwrap()
                .prepare_stop(integer(7), limits)
                .is_err()
        );
    }
    assert_eq!(f.source_calls.load(Ordering::SeqCst), 0);
}

#[tokio::test(start_paused = true)]
async fn declared_controls_are_part_of_readiness_and_stop_is_bound_before_business_entry() {
    let mut f = Fixture::new(true).await;
    let plan = f
        .prepared
        .as_ref()
        .unwrap()
        .prepare_stop(integer(7), FinalizationLimits::default())
        .unwrap();
    let owner = f.prepared.take().unwrap().start_owned().unwrap();
    let entered = f.started.recv().await.unwrap();
    let waiting = tokio::spawn(async move {
        let ready = owner
            .wait_ready(Duration::from_secs(60), CancellationToken::new())
            .await;
        (owner, ready)
    });
    tokio::task::yield_now().await;
    tokio::time::advance(Duration::from_secs(1)).await;
    assert!(
        !waiting.is_finished(),
        "opened without declared controls is not usable readiness"
    );
    f.install(&entered);
    let (owner, ready) = waiting.await.unwrap();
    let stop = owner.bind_stop(plan, &ready.unwrap()).unwrap();
    let closing = tokio::spawn(owner.finalize(stop, CancellationToken::new()));
    let control = f.controls.recv().await.unwrap();
    assert_eq!(control.action.data(), integer(7).data());
    control
        .finish
        .send(ControlOutcome::Confirmed(confirmation()))
        .unwrap_or_else(|_| panic!("control receiver"));
    entered.finish.send(Ok(())).unwrap();
    let receipt = closing.await.unwrap();
    assert_eq!(receipt.control, Some(Ok(())));
    assert!(!receipt.cancellation_requested);
    assert!(receipt.joined);
    assert_eq!(receipt.phase, Phase::Ended);
}

#[tokio::test(start_paused = true)]
async fn bounded_tail_does_not_send_early_and_confirmation_is_not_physical_close() {
    let mut f = Fixture::new(true).await;
    let plan = f
        .prepared
        .as_ref()
        .unwrap()
        .prepare_stop(
            integer(7),
            FinalizationLimits {
                tail: Duration::from_secs(5),
                ..Default::default()
            },
        )
        .unwrap();
    let owner = f.prepared.take().unwrap().start_owned().unwrap();
    let entered = f.started.recv().await.unwrap();
    f.install(&entered);
    let ready = owner
        .wait_ready(Duration::from_secs(60), CancellationToken::new())
        .await
        .unwrap();
    let stop = owner.bind_stop(plan, &ready).unwrap();
    let closing = tokio::spawn(owner.finalize(stop, CancellationToken::new()));
    tokio::task::yield_now().await;
    tokio::time::advance(Duration::from_secs(4)).await;
    assert!(f.controls.try_recv().is_err());
    assert!(!closing.is_finished());
    tokio::time::advance(Duration::from_secs(1)).await;
    let control = f.controls.recv().await.unwrap();
    control
        .finish
        .send(ControlOutcome::Confirmed(confirmation()))
        .unwrap_or_else(|_| panic!("control receiver"));
    tokio::task::yield_now().await;
    assert!(!closing.is_finished());
    assert!(!entered.cancellation.is_cancelled());
    entered.finish.send(Ok(())).unwrap();
    let receipt = closing.await.unwrap();
    assert_eq!(receipt.control, Some(Ok(())));
    assert!(!receipt.graceful_expired);
    assert_eq!(f.control_calls.load(Ordering::SeqCst), 1);
}

#[tokio::test(start_paused = true)]
async fn caller_cancellation_shortens_tail_and_still_sends_only_one_owned_stop() {
    let mut f = Fixture::new(true).await;
    let (owner, stop, entered) = f
        .start(FinalizationLimits {
            tail: Duration::from_secs(60),
            ..Default::default()
        })
        .await;
    let caller = CancellationToken::new();
    let closing = tokio::spawn(owner.finalize(stop, caller.clone()));
    tokio::task::yield_now().await;
    assert!(f.controls.try_recv().is_err());
    caller.cancel();
    let control = f.controls.recv().await.unwrap();
    assert!(
        !entered.cancellation.is_cancelled(),
        "cancellation first requests typed closure"
    );
    assert!(
        control
            .finish
            .send(ControlOutcome::Confirmed(confirmation()))
            .is_ok()
    );
    entered.finish.send(Ok(())).unwrap();
    let receipt = closing.await.unwrap();
    assert_eq!(receipt.control, Some(Ok(())));
    assert!(receipt.joined);
    assert_eq!(f.control_calls.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn finalization_metadata_does_not_discard_private_confirmation_policy() {
    let mut f = Fixture::new(true).await;
    let (owner, stop, entered) = f.start(Default::default()).await;
    let source_policy = owner.fence.policy.clone();
    let closing = tokio::spawn(owner.finalize(stop, CancellationToken::new()));
    let control = f.controls.recv().await.unwrap();
    let policy = wes_core::flow::FlowPolicy::default().private();
    let value = confirmation().with_provenance(Provenance::default().with_policy(&policy));
    assert!(
        control
            .finish
            .send(ControlOutcome::Confirmed(value))
            .is_ok()
    );
    control.cancellation.cancelled().await;
    entered
        .finish
        .send(Err(InvocationError::Failed(
            RuntimeCode::ExecutionFailed.error("Synthetic detail", None),
        )))
        .unwrap();
    let receipt = closing.await.unwrap();
    assert_eq!(receipt.control, Some(Ok(())));
    let expected = source_policy.join(&policy);
    assert_eq!(receipt.policy, expected);
    let Phase::Failed(error) = receipt.phase else {
        panic!("source failed")
    };
    assert_eq!(error.code(), "ENV021");
    assert_eq!(error.policy(), &expected);
    assert!(receipt.joined);
}

#[tokio::test]
async fn naturally_ended_source_skips_the_tail_and_stop_without_recreating_any_owner() {
    let mut f = Fixture::new(true).await;
    let (owner, stop, entered) = f
        .start(FinalizationLimits {
            tail: Duration::from_secs(60),
            ..Default::default()
        })
        .await;
    let mut changes = owner.handle().subscribe();
    entered.finish.send(Ok(())).unwrap();
    tokio::time::timeout(Duration::from_secs(5), async {
        while changes.borrow_and_update().phase != Phase::Ended {
            changes.changed().await.unwrap();
        }
    })
    .await
    .unwrap();
    let receipt = owner.finalize(stop, CancellationToken::new()).await;
    assert!(receipt.control.is_none());
    assert!(!receipt.cancellation_requested);
    assert!(receipt.joined);
    assert_eq!(receipt.phase, Phase::Ended);
    assert_eq!(f.source_calls.load(Ordering::SeqCst), 1);
    assert_eq!(f.control_calls.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn withdrawn_live_control_permission_requests_only_owned_cancellation() {
    let mut f = Fixture::new(true).await;
    let (owner, stop, entered) = f.start(Default::default()).await;
    f.controller.allowed.store(false, Ordering::SeqCst);
    let closing = tokio::spawn(owner.finalize(stop, CancellationToken::new()));
    entered.cancellation.cancelled().await;
    assert!(!closing.is_finished());
    assert_eq!(f.control_calls.load(Ordering::SeqCst), 0);
    entered
        .finish
        .send(Err(InvocationError::Cancelled))
        .unwrap();
    let receipt = closing.await.unwrap();
    assert_eq!(receipt.control, Some(Err(ControlError::Refused)));
    assert!(receipt.cancellation_requested);
    assert!(receipt.joined);
}

#[tokio::test]
async fn refused_or_unknown_stop_requests_cancellation_but_retains_the_physical_source() {
    for unknown in [false, true] {
        let mut f = Fixture::new(true).await;
        let (owner, stop, entered) = f.start(Default::default()).await;
        let closing = tokio::spawn(owner.finalize(stop, CancellationToken::new()));
        let control = f.controls.recv().await.unwrap();
        assert!(
            control
                .finish
                .send(if unknown {
                    ControlOutcome::Unknown
                } else {
                    ControlOutcome::Refused
                })
                .is_ok()
        );
        entered.cancellation.cancelled().await;
        assert!(!closing.is_finished());
        assert_eq!(f.control_calls.load(Ordering::SeqCst), 1);
        entered
            .finish
            .send(Err(InvocationError::Cancelled))
            .unwrap();
        let receipt = closing.await.unwrap();
        assert_eq!(
            receipt.control,
            Some(Err(if unknown {
                ControlError::Unknown
            } else {
                ControlError::Refused
            }))
        );
        assert!(receipt.cancellation_requested);
        assert!(receipt.joined);
        assert_eq!(receipt.phase, Phase::Cancelled);
        assert!(f.controls.try_recv().is_err());
    }
}

#[tokio::test(start_paused = true)]
async fn stop_timeout_is_unknown_and_even_an_ended_producer_cannot_release_its_unjoined_controller()
{
    let mut f = Fixture::new(true).await;
    let (owner, stop, entered) = f
        .start(FinalizationLimits {
            control: Duration::from_secs(1),
            ..Default::default()
        })
        .await;
    let original_bound = owner.fence.bound.clone();
    let closing = tokio::spawn(owner.finalize(stop, CancellationToken::new()));
    let control = f.controls.recv().await.unwrap();
    tokio::time::advance(Duration::from_secs(1)).await;
    control.cancellation.cancelled().await;
    entered.cancellation.cancelled().await;
    entered.finish.send(Ok(())).unwrap();
    tokio::task::yield_now().await;
    assert!(
        !closing.is_finished(),
        "the dispatched controller is still owned after timeout"
    );
    let mut runtime = crate::runtime::Runtime::new();
    runtime
        .add(original_bound.clone(), [], original_bound.traits())
        .unwrap();
    let ticket = runtime
        .start(Duration::ZERO)
        .into_iter()
        .find_map(|e| match e {
            crate::runtime::Effect::Spawn(ticket) => Some(ticket),
            _ => None,
        })
        .unwrap();
    let second = CallExecutor::ephemeral()
        .prepare_source(ticket, CancellationToken::new())
        .await
        .unwrap();
    assert!(
        second.start_owned().is_err(),
        "exclusive resource follows physical controller join too"
    );
    assert_eq!(f.source_calls.load(Ordering::SeqCst), 1);
    assert!(
        control
            .finish
            .send(ControlOutcome::Confirmed(confirmation()))
            .is_ok()
    );
    let receipt = closing.await.unwrap();
    assert_eq!(receipt.control, Some(Err(ControlError::Unknown)));
    assert!(receipt.cancellation_requested);
    assert!(receipt.joined);
    assert_eq!(f.control_calls.load(Ordering::SeqCst), 1);
    assert!(f.controls.try_recv().is_err());
}

#[tokio::test(start_paused = true)]
async fn confirmed_stop_has_a_bounded_graceful_window_and_still_waits_for_forced_join() {
    let mut f = Fixture::new(true).await;
    let (owner, stop, entered) = f
        .start(FinalizationLimits {
            graceful: Duration::from_secs(2),
            ..Default::default()
        })
        .await;
    let closing = tokio::spawn(owner.finalize(stop, CancellationToken::new()));
    let control = f.controls.recv().await.unwrap();
    assert!(
        control
            .finish
            .send(ControlOutcome::Confirmed(confirmation()))
            .is_ok()
    );
    control.cancellation.cancelled().await; // Reply consumed; physical grace starts now.
    assert!(!entered.cancellation.is_cancelled());
    tokio::time::advance(Duration::from_secs(2)).await;
    entered.cancellation.cancelled().await;
    assert!(!closing.is_finished());
    entered
        .finish
        .send(Err(InvocationError::Cancelled))
        .unwrap();
    let receipt = closing.await.unwrap();
    assert_eq!(
        receipt.control,
        Some(Ok(())),
        "confirmation and physical stop remain distinct"
    );
    assert!(receipt.graceful_expired);
    assert!(receipt.cancellation_requested);
    assert!(receipt.joined);
    assert_eq!(receipt.phase, Phase::Cancelled);
}

#[tokio::test]
async fn foreign_preparation_and_runtime_contract_mismatch_cannot_bind_a_stop() {
    let foreign = Fixture::new(true).await;
    let foreign_plan = foreign
        .prepared
        .as_ref()
        .unwrap()
        .prepare_stop(integer(7), Default::default())
        .unwrap();
    let mut f = Fixture::new(true).await;
    let plan = f
        .prepared
        .as_ref()
        .unwrap()
        .prepare_stop(integer(7), Default::default())
        .unwrap();
    let owner = f.prepared.take().unwrap().start_owned().unwrap();
    let entered = f.started.recv().await.unwrap();
    entered
        .sink
        .install_controls(
            f.contracts.input.clone(),
            f.contracts.input.clone(),
            ControlLimits::default(),
            f.controller.clone(),
        )
        .unwrap();
    let ready = owner
        .wait_ready(Duration::from_secs(60), CancellationToken::new())
        .await
        .unwrap();
    assert!(owner.bind_stop(foreign_plan, &ready).is_err());
    assert!(owner.bind_stop(plan, &ready).is_err());
    assert_eq!(f.control_calls.load(Ordering::SeqCst), 0);
    entered.finish.send(Ok(())).unwrap();
    owner.shutdown().await.unwrap();
    assert_eq!(foreign.source_calls.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn a_bound_stop_cannot_target_another_source_owner() {
    let mut a = Fixture::new(true).await;
    let (first, bound, entered_a) = a.start(Default::default()).await;
    let mut b = Fixture::new(true).await;
    let (other, unused, entered_b) = b.start(Default::default()).await;
    drop(unused);
    let closing = tokio::spawn(other.finalize(bound, CancellationToken::new()));
    entered_b.cancellation.cancelled().await;
    assert!(!closing.is_finished());
    assert_eq!(a.control_calls.load(Ordering::SeqCst), 0);
    assert_eq!(b.control_calls.load(Ordering::SeqCst), 0);
    entered_b.finish.send(Ok(())).unwrap();
    let receipt = closing.await.unwrap();
    assert_eq!(receipt.control, Some(Err(ControlError::Changed)));
    assert!(receipt.joined);
    assert!(!entered_a.cancellation.is_cancelled());
    entered_a.finish.send(Ok(())).unwrap();
    first.shutdown().await.unwrap();
}

#[tokio::test]
async fn original_stop_never_rebinds_to_a_replacement_connection_epoch() {
    let mut f = Fixture::new(true).await;
    let (owner, bound, entered) = f.start(Default::default()).await;
    let fresh = entered.sink.reopening().await.unwrap();
    fresh.opened().unwrap();
    fresh
        .install_controls(
            f.contracts.input.clone(),
            f.contracts.output.clone(),
            ControlLimits::default(),
            f.controller.clone(),
        )
        .unwrap();
    let closing = tokio::spawn(owner.finalize(bound, CancellationToken::new()));
    entered.cancellation.cancelled().await;
    assert!(!closing.is_finished());
    assert_eq!(f.control_calls.load(Ordering::SeqCst), 0);
    entered.finish.send(Ok(())).unwrap();
    let receipt = closing.await.unwrap();
    assert_eq!(receipt.control, Some(Err(ControlError::Changed)));
    assert!(receipt.joined);
    assert_eq!(f.source_calls.load(Ordering::SeqCst), 1);
}
