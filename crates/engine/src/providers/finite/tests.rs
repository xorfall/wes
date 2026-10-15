use super::*;
use std::{
    sync::atomic::{AtomicUsize, Ordering},
    time::Duration,
};
use tokio::sync::{mpsc, oneshot};
use wes_core::{
    Data, Primitive,
    capability::Parameter,
    environments::{CapturedSource, CapturedSources, Package},
};

fn native() -> crate::environments::InvocationAuthority {
    crate::environments::InvocationAuthority::from_source(
        &crate::source::SourceInput::new("native-fixture".into(), "synthetic".into()).unwrap(),
    )
}
struct Entered {
    _sink: crate::streams::StreamSink,
    finish: oneshot::Sender<()>,
}
struct Port {
    calls: Arc<AtomicUsize>,
    sources: mpsc::UnboundedSender<Entered>,
}
impl Invoker for Port {
    fn invoke(&self, call: Call, _: CancellationToken) -> InvocationFuture {
        self.calls.fetch_add(1, Ordering::SeqCst);
        Box::pin(async move {
            let (shape, data) = if call.capability.path == ["prove"] {
                (Shape::Primitive(Primitive::Bool), Data::Bool(true))
            } else {
                (Shape::Primitive(Primitive::Int), Data::Int(1))
            };
            Ok(Value::new(shape, data, Default::default()).unwrap())
        })
    }
}
impl crate::streams::StreamingInvoker for Port {
    fn subscribe(
        &self,
        _: Call,
        sink: crate::streams::StreamSink,
        _: CancellationToken,
    ) -> crate::streams::StreamFuture {
        sink.opened().unwrap();
        let (finish, receive) = oneshot::channel();
        self.sources
            .send(Entered {
                _sink: sink,
                finish,
            })
            .unwrap();
        Box::pin(async move {
            receive.await.unwrap();
            Ok(())
        })
    }
}
struct Fixture {
    providers: Providers,
    authority: crate::environments::Authority,
    binding: wes_core::environments::Binding,
    calls: Arc<AtomicUsize>,
    entered: mpsc::UnboundedReceiver<Entered>,
}
impl Fixture {
    fn new(read: bool) -> Self {
        Self::with_output(read, "public")
    }
    fn with_output(read: bool, output: &str) -> Self {
        let package = Package::parse(&format!("version: 1\ntargets: {{local: {{kind: local}}}}\nenvironments: {{lab: {{imports: {{fixture: {{source: {{kind: spec, file: synthetic.json}}, bind: {{target: local, output: {output}}}}}}}}}}}")).unwrap();
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
        let calls = Arc::new(AtomicUsize::new(0));
        let (sources, entered) = mpsc::unbounded_channel();
        let port = Arc::new(Port {
            calls: calls.clone(),
            sources,
        });
        let mut work = Capability::new(["work"], Shape::Primitive(Primitive::Int), Safety::Safe);
        let mut listen = work.clone();
        listen.path = vec!["listen".into()];
        listen.streaming = true;
        let mut prove = Capability::new(["prove"], Shape::Primitive(Primitive::Bool), Safety::Safe);
        for cap in [&mut work, &mut listen, &mut prove] {
            cap.parameters = vec![Parameter::new(
                "scope",
                Shape::Primitive(Primitive::Text),
                true,
            )];
        }
        let description =
            ProviderDescription::new("fixture", [work, listen, prove], vec![]).unwrap();
        let mut providers = Providers::local(LocalScope::new("fixture").unwrap());
        providers.register_ports(description, port.clone(), Some(port));
        let original = providers.providers["fixture"].clone();
        providers.providers.insert(
            "fixture".into(),
            Arc::new(Provider {
                scope: ExecutionScope::Environment {
                    binding: binding.clone(),
                    authority: Some(authority.clone()),
                },
                read_domain: read.then(|| crate::provider_reads::ReadDomain {
                    scope: vec!["scope".into()],
                    revalidate: vec!["prove".into()],
                }),
                ..(*original).clone()
            }),
        );
        Self {
            providers,
            authority,
            binding,
            calls,
            entered,
        }
    }
    fn ticket(&self, path: &str) -> RunTicket<BoundCall> {
        let parsed = wes_language::parse(&wes_language::SourceText::new(
            "synthetic",
            &format!("fixture {path} scope:lab"),
        ));
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
        let mut bound = if path == "listen" {
            self.providers.bind_stream(invocation)
        } else {
            self.providers.bind_finite(invocation)
        }
        .unwrap();
        bound.set_authority(native());
        let mut runtime = crate::runtime::Runtime::new();
        runtime.add(bound.clone(), [], bound.traits()).unwrap();
        runtime
            .start(Duration::ZERO)
            .into_iter()
            .find_map(|effect| match effect {
                crate::runtime::Effect::Spawn(ticket) => Some(ticket),
                _ => None,
            })
            .unwrap()
    }
    fn prove(&self) {
        let domain = self.providers.providers["fixture"]
            .read_domain
            .as_ref()
            .unwrap();
        let scope = domain
            .digest(&IndexMap::from([(
                "scope".into(),
                crate::values::text("lab"),
            )]))
            .unwrap();
        self.authority
            .begin_read_probe(&self.binding, "fixture", &scope, &native())
            .unwrap()
            .confirm()
            .unwrap();
    }
    async fn source(&mut self) -> (OwnedSource, Entered, SourcePrerequisite) {
        let owner = CallExecutor::ephemeral()
            .prepare_source(self.ticket("listen"), CancellationToken::new())
            .await
            .unwrap()
            .start_owned()
            .unwrap();
        let entered = self.entered.recv().await.unwrap();
        let prerequisite = owner
            .wait_ready(Duration::from_secs(1), CancellationToken::new())
            .await
            .unwrap();
        (owner, entered, prerequisite)
    }
}

#[tokio::test]
async fn finite_environment_withdrawal_and_reenable_cannot_revive_prepared_dispatch() {
    let fixture = Fixture::new(false);
    let prepared = CallExecutor::ephemeral()
        .prepare_finite(fixture.ticket("work"), CancellationToken::new())
        .await
        .unwrap();
    fixture.authority.disable("lab").unwrap();
    fixture.authority.enable("lab").unwrap();
    assert!(matches!(
        prepared.execute().await.outcome,
        Outcome::Cancelled(_)
    ));
    assert_eq!(fixture.calls.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn source_environment_withdrawal_and_reenable_retire_original_ready_prerequisite() {
    let mut source = Fixture::new(false);
    let business = Fixture::new(false);
    let call = CallExecutor::ephemeral()
        .prepare_finite(business.ticket("work"), CancellationToken::new())
        .await
        .unwrap();
    let (owner, entered, prerequisite) = source.source().await;
    source.authority.disable("lab").unwrap();
    source.authority.enable("lab").unwrap();
    assert!(matches!(
        call.execute_after(prerequisite).await.outcome,
        Outcome::Failed(_) | Outcome::Cancelled(_)
    ));
    assert_eq!(business.calls.load(Ordering::SeqCst), 0);
    entered.finish.send(()).unwrap();
    owner.join().await.unwrap();
}

#[tokio::test]
async fn source_and_business_read_permits_are_independent_and_reproof_cannot_revive_either() {
    for withdraw_source in [true, false] {
        let mut source = Fixture::new(true);
        source.prove();
        let business = Fixture::new(true);
        business.prove();
        let call = CallExecutor::ephemeral()
            .prepare_finite(business.ticket("work"), CancellationToken::new())
            .await
            .unwrap();
        let (owner, entered, prerequisite) = source.source().await;
        if withdraw_source {
            source.prove();
        } else {
            business.prove();
        }
        assert!(matches!(
            call.execute_after(prerequisite).await.outcome,
            Outcome::Failed(_)
        ));
        assert_eq!(business.calls.load(Ordering::SeqCst), 0);
        entered.finish.send(()).unwrap();
        owner.join().await.unwrap();
    }
}

#[tokio::test]
async fn final_probe_guard_is_readonly_and_obsolete_probe_cannot_admit_or_confirm() {
    let fixture = Fixture::new(true);
    let first = CallExecutor::ephemeral()
        .prepare_finite(fixture.ticket("prove"), CancellationToken::new())
        .await
        .unwrap();
    let guard = first.read_access.dispatch_guard().unwrap();
    assert!(guard.current());
    fixture.prove();
    assert!(!guard.current());
    assert!(matches!(first.execute().await.outcome, Outcome::Failed(_)));
    assert_eq!(fixture.calls.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn completed_native_read_still_checks_its_original_grant_at_coordinator_publication() {
    let fixture = Fixture::new(true);
    fixture.prove();
    let report = CallExecutor::ephemeral()
        .prepare_finite(fixture.ticket("work"), CancellationToken::new())
        .await
        .unwrap()
        .execute()
        .await;
    let Outcome::Produced(value) = &report.outcome else {
        panic!("result")
    };
    let policy = value.provenance().policy().clone();
    assert!(!policy.provider_reads().is_empty());
    assert!(matches!(
        crate::driver::publication::checked(&report),
        Outcome::Produced(_)
    ));
    fixture.prove();
    let Outcome::Failed(refusal) = crate::driver::publication::checked(&report) else {
        panic!("old result grant")
    };
    assert_eq!(refusal.code(), "RUN009");
    assert_eq!(refusal.policy(), &policy);
    assert_eq!(
        fixture.calls.load(Ordering::SeqCst),
        1,
        "no automatic replay after a new grant"
    );
}

#[tokio::test]
async fn successful_probe_confirmation_is_not_treated_as_read_grant_expiry() {
    let fixture = Fixture::new(true);
    let report = CallExecutor::ephemeral()
        .prepare_finite(fixture.ticket("prove"), CancellationToken::new())
        .await
        .unwrap()
        .execute()
        .await;
    assert!(report.publication_guard.is_none());
    assert!(matches!(
        crate::driver::publication::checked(&report),
        Outcome::Produced(_)
    ));
    assert!(
        CallExecutor::ephemeral()
            .prepare_finite(fixture.ticket("work"), CancellationToken::new())
            .await
            .is_ok()
    );
    assert_eq!(fixture.calls.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn private_ready_source_is_control_admission_and_not_a_business_read_dependency() {
    let mut source = Fixture::with_output(true, "private");
    source.prove();
    let business = Fixture::new(true);
    business.prove();
    let call = CallExecutor::ephemeral()
        .prepare_finite(business.ticket("work"), CancellationToken::new())
        .await
        .unwrap();
    let (owner, entered, prerequisite) = source.source().await;
    let capture = owner
        .handle()
        .snapshot()
        .window
        .provenance()
        .policy()
        .clone();
    assert!(capture.is_private());
    assert_eq!(capture.provider_reads().len(), 1);
    let report = call.execute_after(prerequisite).await;
    let Outcome::Produced(value) = &report.outcome else {
        panic!("business result")
    };
    assert!(!value.provenance().policy().is_private());
    assert_eq!(value.provenance().policy().provider_reads().len(), 1);
    assert!(
        value
            .provenance()
            .policy()
            .provider_reads()
            .is_disjoint(capture.provider_reads())
    );
    source.authority.disable("lab").unwrap();
    let readers = crate::provider_reads::Readers::default();
    readers
        .register(Arc::new(source.authority.clone()))
        .unwrap();
    readers
        .register(Arc::new(business.authority.clone()))
        .unwrap();
    assert!(readers.blocks_policy(&capture));
    assert!(!readers.blocks_policy(value.provenance().policy()));
    assert!(matches!(
        crate::driver::publication::checked(&report),
        Outcome::Produced(_)
    ));
    assert_eq!(business.calls.load(Ordering::SeqCst), 1);
    entered.finish.send(()).unwrap();
    owner.join().await.unwrap();
}

#[tokio::test]
async fn source_related_predispatch_refusal_keeps_private_source_and_read_scope_policy() {
    let mut source = Fixture::with_output(true, "private");
    source.prove();
    let business = Fixture::new(false);
    let call = CallExecutor::ephemeral()
        .prepare_finite(business.ticket("work"), CancellationToken::new())
        .await
        .unwrap();
    let (owner, entered, prerequisite) = source.source().await;
    let capture = owner
        .handle()
        .snapshot()
        .window
        .provenance()
        .policy()
        .clone();
    owner.handle().cancel();
    let report = call.execute_after(prerequisite).await;
    let Outcome::Failed(error) = report.outcome else {
        panic!("capture refusal")
    };
    assert!(error.policy().is_private());
    assert!(
        capture
            .provider_reads()
            .is_subset(error.policy().provider_reads())
    );
    assert_eq!(business.calls.load(Ordering::SeqCst), 0);
    entered.finish.send(()).unwrap();
    owner.join().await.unwrap();
}

struct BusinessEntered {
    cancellation: CancellationToken,
    finish: oneshot::Sender<Result<Value, InvocationError>>,
}
struct HeldBusiness {
    entered: mpsc::UnboundedSender<BusinessEntered>,
    calls: Arc<AtomicUsize>,
}

struct NoValues;
impl crate::storage::ValueStore for NoValues {
    fn read_policy(
        &self,
        _: &crate::storage::ValueHandle,
    ) -> Result<Option<wes_core::flow::FlowPolicy>, crate::storage::StoreError> {
        Ok(None)
    }
    fn store(
        &mut self,
        _: &Value,
    ) -> Result<crate::storage::ValueHandle, crate::storage::StoreError> {
        unreachable!()
    }
    fn read(
        &self,
        _: &crate::storage::ValueHandle,
    ) -> Result<Option<crate::storage::LoadedValue>, crate::storage::StoreError> {
        Ok(None)
    }
    fn digest(
        &self,
        _: &crate::storage::ValueHandle,
    ) -> Result<Option<String>, crate::storage::StoreError> {
        Ok(None)
    }
    fn encoded(
        &self,
        _: &crate::storage::ValueHandle,
    ) -> Result<Option<Vec<u8>>, crate::storage::StoreError> {
        Ok(None)
    }
    fn keep(
        &mut self,
        _: &crate::storage::ValueHandle,
    ) -> Result<bool, crate::storage::StoreError> {
        unreachable!()
    }
    fn is_kept(&self, _: &crate::storage::ValueHandle) -> Result<bool, crate::storage::StoreError> {
        Ok(false)
    }
    fn size(
        &self,
        _: &crate::storage::ValueHandle,
    ) -> Result<Option<u64>, crate::storage::StoreError> {
        Ok(None)
    }
    fn release(
        &mut self,
        _: &crate::storage::ValueHandle,
    ) -> Result<bool, crate::storage::StoreError> {
        Ok(false)
    }
}
struct AcquisitionPolicy(std::sync::Mutex<Option<wes_core::flow::FlowPolicy>>);
impl Invoker for AcquisitionPolicy {
    fn invoke(&self, _: Call, _: CancellationToken) -> InvocationFuture {
        panic!("unrelated private readiness must not disable protected acquisition")
    }
    fn invoke_acquired(
        &self,
        _: Call,
        _: CancellationToken,
        context: crate::storage::AcquisitionContext,
    ) -> crate::storage::AcquiredFuture {
        *self.0.lock().unwrap() = Some(context.provenance().policy().clone());
        Box::pin(async {
            Ok(crate::storage::AcquiredOutput::Value(
                Value::new(
                    Shape::Primitive(Primitive::Int),
                    Data::Int(7),
                    Default::default(),
                )
                .unwrap(),
            ))
        })
    }
}

#[tokio::test]
async fn protected_acquisition_receives_business_policy_before_provider_entry_without_capture_scope()
 {
    let mut source = Fixture::with_output(true, "private");
    source.prove();
    let mut business = Fixture::with_output(true, "confidential");
    business.prove();
    let policy = Arc::new(AcquisitionPolicy(std::sync::Mutex::new(None)));
    let original = business.providers.providers["fixture"].clone();
    business.providers.providers.insert(
        "fixture".into(),
        Arc::new(Provider {
            invoker: policy.clone(),
            ..(*original).clone()
        }),
    );
    let (worker, writer) = crate::storage::spawn_store(NoValues, Default::default()).unwrap();
    let scoped = worker.transient_scope().unwrap();
    let prepared = CallExecutor::ephemeral()
        .with_storage(Some(scoped.clone()))
        .prepare_finite(business.ticket("work"), CancellationToken::new())
        .await
        .unwrap();
    let (owner, entered, prerequisite) = source.source().await;
    let capture = owner
        .handle()
        .snapshot()
        .window
        .provenance()
        .policy()
        .clone();
    let report = prepared.execute_after(prerequisite).await;
    let received = policy
        .0
        .lock()
        .unwrap()
        .clone()
        .expect("protected acquisition entry");
    assert!(received.is_confidential());
    assert!(!received.is_private());
    assert_eq!(received.provider_reads().len(), 1);
    assert!(
        received
            .provider_reads()
            .is_disjoint(capture.provider_reads())
    );
    assert_eq!(report.outcome.flow_policy(), received);
    entered.finish.send(()).unwrap();
    owner.join().await.unwrap();
    drop(report);
    scoped.close_transient_scope().await.unwrap();
    assert_eq!(worker.shutdown().await.unwrap().failed, 0);
    writer.join().await.unwrap();
}
impl Invoker for HeldBusiness {
    fn invoke(&self, _: Call, cancellation: CancellationToken) -> InvocationFuture {
        self.calls.fetch_add(1, Ordering::SeqCst);
        let (finish, receive) = oneshot::channel();
        self.entered
            .send(BusinessEntered {
                cancellation,
                finish,
            })
            .ok()
            .unwrap();
        Box::pin(async move { receive.await.unwrap() })
    }
}

#[tokio::test]
async fn postdispatch_capture_scope_loss_preserves_business_success_error_and_unknown_under_own_scope()
 {
    for outcome in ["success", "failure", "unknown"] {
        let mut source = Fixture::with_output(true, "private");
        source.prove();
        let mut business = Fixture::new(true);
        business.prove();
        let (entered, mut started) = mpsc::unbounded_channel();
        let original = business.providers.providers["fixture"].clone();
        business.providers.providers.insert(
            "fixture".into(),
            Arc::new(Provider {
                invoker: Arc::new(HeldBusiness {
                    entered,
                    calls: business.calls.clone(),
                }),
                ..(*original).clone()
            }),
        );
        let prepared = CallExecutor::ephemeral()
            .prepare_finite(business.ticket("work"), CancellationToken::new())
            .await
            .unwrap();
        let (owner, source_entered, prerequisite) = source.source().await;
        let capture = owner
            .handle()
            .snapshot()
            .window
            .provenance()
            .policy()
            .clone();
        let pending = tokio::spawn(prepared.execute_after(prerequisite));
        let invocation = started.recv().await.unwrap();
        source.authority.disable("lab").unwrap();
        assert!(!owner.handle().read_access_current());
        assert!(!invocation.cancellation.is_cancelled());
        source_entered.finish.send(()).unwrap();
        owner.join().await.unwrap();
        assert!(!pending.is_finished());
        let result = if outcome == "success" {
            Ok(Value::new(
                Shape::Primitive(Primitive::Int),
                Data::Int(7),
                Default::default(),
            )
            .unwrap())
        } else {
            Err(InvocationError::Failed(if outcome == "unknown" {
                RuntimeCode::ExternalOutcomeUnknown.error("Synthetic unknown", None)
            } else {
                RuntimeCode::ExecutionFailed.error("Synthetic failure", None)
            }))
        };
        invocation.finish.send(result).unwrap();
        let report = pending.await.unwrap();
        let policy = report.outcome.flow_policy();
        assert!(!policy.is_private());
        assert_eq!(policy.provider_reads().len(), 1);
        assert!(
            policy
                .provider_reads()
                .is_disjoint(capture.provider_reads())
        );
        let readers = crate::provider_reads::Readers::default();
        readers
            .register(Arc::new(source.authority.clone()))
            .unwrap();
        readers
            .register(Arc::new(business.authority.clone()))
            .unwrap();
        assert!(readers.blocks_policy(&capture));
        assert!(!readers.blocks_policy(&policy));
        match &report.outcome {
            Outcome::Produced(value) => {
                assert_eq!(outcome, "success");
                assert_eq!(value.data(), &Data::Int(7));
            }
            Outcome::Failed(error) => assert_eq!(
                error.code(),
                if outcome == "unknown" {
                    wes_core::ErrorValue::REMOTE_OUTCOME_UNKNOWN
                } else {
                    "RUN001"
                }
            ),
            _ => panic!("ordinary outcome changed"),
        }
        assert_eq!(business.calls.load(Ordering::SeqCst), 1);
        assert_eq!(report.notices.len(), usize::from(outcome == "unknown"));
        assert!(report.notices.iter().all(|e| e.policy() == &policy));
        business.authority.disable("lab").unwrap();
        business.authority.enable("lab").unwrap();
        business.prove();
        let Outcome::Failed(error) = crate::driver::publication::checked(&report) else {
            panic!("original business proof must still expire")
        };
        assert_eq!(error.code(), "RUN009");
        assert_eq!(error.policy(), &policy);
        assert_eq!(business.calls.load(Ordering::SeqCst), 1);
    }
}
