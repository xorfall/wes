//! Streaming validation/admission shares the finite boundary without finite recovery attempts.
use super::*;
use crate::streams;
mod finalization;
pub use finalization::{BoundStop, FinalizationLimits, PreparedStop, SourceFinalization};

pub(super) fn prepare_source(
    journal: Option<CallJournal>,
    ticket: RunTicket<BoundCall>,
    cancellation: CancellationToken,
) -> PreparationFuture {
    prepare_source_with_storage(journal, ticket, cancellation, None)
}
pub(super) fn prepare_source_with_storage(
    journal: Option<CallJournal>,
    ticket: RunTicket<BoundCall>,
    cancellation: CancellationToken,
    storage: Option<crate::storage::StoreWorker>,
) -> PreparationFuture {
    let policy = ticket.payload.failure_policy(&ticket.inputs);
    let work = async move {
        let input_holds = ticket.input_holds;
        let validation_cancel = cancellation.clone();
        let prepared = tokio::task::spawn_blocking(move || {
            let bound = ticket.payload;
            if !bound.streaming()
                || bound.invocation.interactive
                || bound.provider.streams.is_none()
            {
                return Err(InvocationError::Failed(RuntimeCode::ExecutionFailed.error(
                    "The call has no compatible streaming execution port.",
                    None,
                )));
            }
            prepare(&bound.invocation, &ticket.inputs, &validation_cancel)
                .map(|(arguments, carried)| (bound, ticket.run, arguments, carried))
        })
        .await;
        let (bound, run, arguments, carried) = match prepared {
            Ok(Ok(prepared)) => prepared,
            Ok(Err(error)) => return Err(error.outcome().into()),
            Err(_) => {
                return Err(Outcome::Failed(
                    RuntimeCode::ExecutionFailed
                        .error("Input validation terminated unexpectedly.", None),
                )
                .into());
            }
        };
        let dispatch = bound
            .dispatch_guard()
            .map_err(|error| ExecutionReport::from(error.outcome()))?;
        if cancellation.is_cancelled() {
            return Err(InvocationError::Cancelled.outcome().into());
        }
        if let Some(journal) = journal {
            let Some(admission) = &bound.context.admission else {
                return Err(recording_denied(&cancellation));
            };
            // Join accepted writes even if cancellation arrives during interrupted admission repair.
            if journal.authorize(admission, &run).await.is_err() {
                return Err(recording_denied(&cancellation));
            }
        } else if bound.context.admission.is_some() {
            return Err(recording_denied(&cancellation));
        }
        if cancellation.is_cancelled() {
            return Err(InvocationError::Cancelled.outcome().into());
        }
        if let Err(reason) = bound.require_dispatch(&carried) {
            return Err(environment_denied(reason).into());
        }
        let read_access = bound
            .read_access(&arguments)
            .map_err(|e| ExecutionReport::from(e.outcome()))?;
        if read_access.is_probe() {
            return Err(super::read_access::refusal().outcome().into());
        }
        let attribution = read_access.protect(
            bound
                .output_provenance(&carried)
                .cautioned(bound.invocation.cautions.iter().cloned()),
        );
        Ok(PreparedSource {
            identity: Arc::new(()),
            bound,
            run,
            arguments,
            carried,
            attribution,
            cancellation,
            read_access,
            dispatch,
            storage,
            input_holds,
        })
    };
    Box::pin(async move {
        work.await.map_err(|mut report: ExecutionReport| {
            report.outcome = report.outcome.with_policy(&policy);
            report.notices = report
                .notices
                .into_iter()
                .map(|e| e.with_policy(&policy))
                .collect();
            report
        })
    })
}

pub type SourcePreparationFuture =
    Pin<Box<dyn Future<Output = Result<PreparedSource, ExecutionReport>> + Send>>;
pub(crate) type PreparationFuture = SourcePreparationFuture;

/// Validated local admission held before producer dispatch. Archive setup may occur here,
/// but a stored result cannot construct this capability or recreate external execution.
#[must_use = "start or discard the prepared source"]
pub struct PreparedSource {
    identity: Arc<()>,
    bound: BoundCall,
    run: Run,
    arguments: IndexMap<String, Value>,
    carried: Provenance,
    attribution: Provenance,
    cancellation: CancellationToken,
    read_access: super::read_access::Attempt,
    dispatch: Option<crate::provider_reads::ReadGuard>,
    storage: Option<crate::storage::StoreWorker>,
    input_holds: Vec<crate::storage::ValueLease>,
}
impl PreparedSource {
    /// Start a source whose physical task is owned independently from finite work.
    /// Required byte recording is prepared by the ordinary source boundary.
    pub fn start_owned(self) -> Result<OwnedSource, ExecutionReport> {
        self.start_owned_with_archives(Vec::new())
    }
    pub(crate) fn start_owned_with_archives(
        self,
        archives: Vec<Arc<streams::archive::Branch>>,
    ) -> Result<OwnedSource, ExecutionReport> {
        let fence = SourceFence {
            identity: self.identity.clone(),
            bound: self.bound.clone(),
            carried: self.carried.clone(),
            read: self.read_access.stream_guard(),
            dispatch: self.dispatch.clone(),
            cancellation: self.cancellation.clone(),
            policy: self.attribution.policy().clone(),
        };
        let (handle, task) = self.start(archives)?;
        Ok(OwnedSource {
            handle,
            task,
            fence: Arc::new(fence),
        })
    }
    pub(crate) fn refusal(&self, message: String) -> ExecutionReport {
        Outcome::Failed(RuntimeCode::ExecutionFailed.error(message, None))
            .with_policy(self.attribution.policy())
            .into()
    }
    pub(crate) fn policy(&self) -> &wes_core::flow::FlowPolicy {
        self.attribution.policy()
    }
    pub(crate) fn records_bytes(&self) -> bool {
        self.bound
            .provider
            .streams
            .as_ref()
            .is_some_and(|streams| streams.records_bytes(&self.bound.invocation.capability))
    }
    pub(crate) fn recording_admission(&self) -> Option<(Arc<()>, wes_core::flow::FlowPolicy)> {
        self.bound
            .context
            .source_definition
            .clone()
            .map(|definition| (definition, self.attribution.policy().clone()))
    }
    pub(crate) fn start(
        self,
        archives: Vec<Arc<streams::archive::Branch>>,
    ) -> Result<(streams::StreamHandle, streams::StreamTask), ExecutionReport> {
        let refused = |outcome: Outcome| {
            ExecutionReport::from(outcome.with_policy(self.attribution.policy()))
        };
        if self.cancellation.is_cancelled() {
            return Err(refused(InvocationError::Cancelled.outcome()));
        }
        // Local archive preparation can await filesystem work. Permission must still
        // hold at the actual producer boundary, not only before that wait.
        if let Err(reason) = self.bound.require_dispatch(&self.carried) {
            return Err(refused(environment_denied(reason)));
        }
        if self.dispatch.as_ref().is_some_and(|guard| !guard.current()) {
            return Err(refused(InvocationError::Cancelled.outcome()));
        }
        if !self.read_access.current() {
            return Err(refused(super::read_access::refusal().outcome()));
        }
        let context = self.storage.map(|worker| {
            crate::storage::AcquisitionContext::new(
                worker.with_placement(crate::storage::ValuePlacement::MemoryAndDisk),
                self.attribution.clone(),
            )
        });
        let input_holds = self.input_holds;
        streams::spawn_with_recording(
            Call {
                authority: self.bound.context.authority.clone(),
                run: self.run,
                capability: self.bound.invocation.capability,
                arguments: self.arguments,
            },
            self.bound
                .provider
                .streams
                .as_ref()
                .expect("validated stream port")
                .clone(),
            self.attribution.clone(),
            streams::Limits::default(),
            self.cancellation,
            self.bound.context.stream_budget,
            None,
            archives,
            // Target adapters retain their live availability/cleanup guard and
            // its typed refusal. Admission generations fence entry; converting
            // them into a read withdrawal here would mask that terminal outcome.
            self.read_access.stream_guard(),
            context,
        )
        .map(|(handle, task)| (handle, task.after_join(move || drop(input_holds))))
        .map_err(|error| {
            refused(Outcome::Failed(
                RuntimeCode::ExecutionFailed.error(error.to_string(), None),
            ))
        })
    }
}

struct SourceFence {
    identity: Arc<()>,
    bound: BoundCall,
    carried: Provenance,
    read: Option<crate::provider_reads::ReadGuard>,
    dispatch: Option<crate::provider_reads::ReadGuard>,
    cancellation: CancellationToken,
    policy: wes_core::flow::FlowPolicy,
}
impl SourceFence {
    fn protect(&self, error: InvocationError) -> InvocationError {
        match error {
            InvocationError::Failed(error) => {
                InvocationError::Failed(error.with_policy(&self.policy))
            }
            other => other,
        }
    }
    fn check(&self) -> Result<(), InvocationError> {
        self.check_current().map_err(|error| self.protect(error))
    }
    fn check_current(&self) -> Result<(), InvocationError> {
        if self.cancellation.is_cancelled() {
            return Err(InvocationError::Cancelled);
        }
        if self.dispatch.as_ref().is_some_and(|guard| !guard.current()) {
            return Err(InvocationError::Cancelled);
        }
        self.bound
            .require_dispatch(&self.carried)
            .map_err(|reason| {
                let Outcome::Failed(error) = environment_denied(reason) else {
                    unreachable!()
                };
                InvocationError::Failed(error)
            })?;
        if self.read.as_ref().is_some_and(|guard| !guard.current()) {
            return Err(read_access::refusal());
        }
        Ok(())
    }
}

/// Owns one source through physical join. Weak readiness prerequisites never own
/// its lifetime or confer live controls. Retain this owner even after cancellation.
#[must_use = "retain and physically join the owned source"]
pub struct OwnedSource {
    handle: streams::StreamHandle,
    task: streams::StreamTask,
    fence: Arc<SourceFence>,
}
impl OwnedSource {
    pub(crate) fn after_join(mut self, cleanup: impl FnOnce() + Send + 'static) -> Self {
        self.task = self.task.after_join(cleanup);
        self
    }
    pub(crate) fn into_stream(self) -> (streams::StreamHandle, streams::StreamTask) {
        (self.handle, self.task)
    }
    pub fn handle(&self) -> &streams::StreamHandle {
        &self.handle
    }
    pub async fn wait_ready(
        &self,
        budget: std::time::Duration,
        caller: CancellationToken,
    ) -> Result<SourcePrerequisite, InvocationError> {
        self.fence.check()?;
        let ready = self
            .handle
            .wait_ready(&self.handle.snapshot().run, budget, caller)
            .await
            .map_err(|error| {
                self.fence.protect(match error {
                    streams::ReadyError::Cancelled => InvocationError::Cancelled,
                    _ => prerequisite_refusal(),
                })
            })?;
        self.fence.check()?;
        Ok(SourcePrerequisite {
            ready,
            fence: self.fence.clone(),
        })
    }
    pub async fn join(self) -> Result<(), tokio::task::JoinError> {
        self.task.join().await
    }
    pub async fn shutdown(self) -> Result<(), tokio::task::JoinError> {
        self.task.shutdown().await
    }
}

/// Native, nonserializable prerequisite from one original prepared source. A
/// stored descriptor, alias or readiness event cannot construct this capability.
pub struct SourcePrerequisite {
    ready: streams::ReadyReceipt,
    fence: Arc<SourceFence>,
}
impl std::fmt::Debug for SourcePrerequisite {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SourcePrerequisite")
            .field("run", self.ready.run())
            .finish_non_exhaustive()
    }
}
impl SourcePrerequisite {
    pub(super) fn check(&self) -> Result<(), InvocationError> {
        self.admit(|| Ok(()))
    }
    pub(super) fn admit<R>(
        &self,
        admit: impl FnOnce() -> Result<R, InvocationError>,
    ) -> Result<R, InvocationError> {
        // Lock order is source -> local permission/read scopes, as in live
        // source admission. Never execute an adapter while the source is locked.
        self.ready
            .admit(self.ready.run(), || {
                self.fence.check()?;
                admit()
            })
            .map_err(|_| self.fence.protect(prerequisite_refusal()))?
    }
}
fn prerequisite_refusal() -> InvocationError {
    InvocationError::Failed(RuntimeCode::ExecutionFailed.error(
        "Required source readiness is unavailable or changed; the provider was not invoked. Nothing was replayed.", None))
}

pub(super) fn execute(
    journal: Option<CallJournal>,
    ticket: RunTicket<BoundCall>,
    cancellation: CancellationToken,
) -> StreamExecutionFuture {
    let prepared = prepare_source(journal, ticket, cancellation);
    Box::pin(async move { prepared.await?.start(Vec::new()) })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        sync::atomic::{AtomicUsize, Ordering},
        time::Duration,
    };
    use wes_core::{
        Data, Primitive,
        environments::{CapturedSource, CapturedSources, Package},
    };

    struct Synthetic(Arc<AtomicUsize>);
    impl Invoker for Synthetic {
        fn invoke(&self, _: Call, _: CancellationToken) -> InvocationFuture {
            panic!("finite port must not be called")
        }
    }
    impl streams::StreamingInvoker for Synthetic {
        fn subscribe(
            &self,
            _: Call,
            sink: streams::StreamSink,
            _: CancellationToken,
        ) -> streams::StreamFuture {
            self.0.fetch_add(1, Ordering::SeqCst);
            sink.push(
                Value::new(
                    Shape::Primitive(Primitive::Int),
                    Data::Int(7),
                    Provenance::default(),
                )
                .unwrap(),
            )
            .unwrap();
            sink.opened().unwrap();
            Box::pin(async { Ok(()) })
        }
    }
    fn ticket(
        environment: bool,
    ) -> (
        RunTicket<BoundCall>,
        Arc<AtomicUsize>,
        crate::environments::Authority,
    ) {
        let calls = Arc::new(AtomicUsize::new(0));
        let port = Arc::new(Synthetic(calls.clone()));
        let mut capability =
            Capability::new(["events"], Shape::Primitive(Primitive::Int), Safety::Safe);
        capability.streaming = true;
        let description = ProviderDescription::new("synthetic", [capability], vec![]).unwrap();
        let mut providers = Providers::local(LocalScope::new("synthetic").unwrap());
        providers.register_ports(description, port.clone(), Some(port));
        let source = wes_language::SourceText::new("synthetic", "synthetic events");
        let script = wes_language::parse(&source);
        assert!(script.diagnostics.is_empty());
        let statement = &script.script.statements[0];
        let wes_language::Expression::Call(call) = &statement.expression else {
            panic!("call")
        };
        let resolution = wes_language::resolve::resolve(call, providers.catalogue()).unwrap();
        let crate::plan::Plan::NewNode(node) =
            crate::plan::plan(&resolution, statement, &Default::default(), &|_| None).unwrap()
        else {
            panic!("node")
        };
        let crate::plan::Task::Invoke(invocation) = node.task else {
            panic!("invocation")
        };
        let mut bound = providers.bind_stream(invocation).unwrap();
        let authority = crate::environments::Authority::default();
        if environment {
            let package = Package::parse("version: 1\ntargets: {local: {kind: local}}\nenvironments: {synthetic: {imports: {fixture: {source: {kind: spec, file: fixture.json}, bind: {target: local}}}}}\n").unwrap();
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
            let binding = registry
                .inspect("synthetic")
                .unwrap()
                .bind("fixture")
                .unwrap();
            authority.establish_namespace("synthetic");
            authority.enable("synthetic").unwrap();
            bound.provider = Arc::new(Provider {
                scope: ExecutionScope::Environment {
                    binding,
                    authority: Some(authority.clone()),
                },
                ..(*bound.provider).clone()
            });
        }
        let mut runtime = crate::runtime::Runtime::new();
        runtime.add(bound.clone(), [], bound.traits()).unwrap();
        let ticket = runtime
            .start(Duration::ZERO)
            .into_iter()
            .find_map(|effect| match effect {
                crate::runtime::Effect::Spawn(ticket) => Some(ticket),
                _ => None,
            })
            .unwrap();
        (ticket, calls, authority)
    }
    #[tokio::test]
    async fn native_preparation_never_subscribes_and_rechecks_permission_after_the_wait() {
        let (ticket, calls, authority) = ticket(true);
        let prepared = prepare_source(None, ticket, CancellationToken::new())
            .await
            .unwrap();
        assert_eq!(calls.load(Ordering::SeqCst), 0);
        authority.disable("synthetic").unwrap();
        let error = match prepared.start(Vec::new()) {
            Err(error) => error,
            Ok(_) => panic!("revoked source dispatched"),
        };
        assert!(matches!(error.outcome, Outcome::Failed(error) if error.code() == "ENV020"));
        assert_eq!(calls.load(Ordering::SeqCst), 0);
    }
    #[tokio::test]
    async fn cancelled_prepared_source_never_enters_the_producer() {
        let (ticket, calls, _) = ticket(false);
        let token = CancellationToken::new();
        let prepared = prepare_source(None, ticket, token.clone()).await.unwrap();
        token.cancel();
        assert!(matches!(
            prepared.start(Vec::new()),
            Err(ExecutionReport {
                outcome: Outcome::Cancelled(_),
                ..
            })
        ));
        assert_eq!(calls.load(Ordering::SeqCst), 0);
    }
    #[tokio::test]
    async fn one_native_dispatch_preserves_synchronous_first_delivery() {
        let (ticket, calls, _) = ticket(false);
        let prepared = prepare_source(None, ticket, CancellationToken::new())
            .await
            .unwrap();
        assert_eq!(calls.load(Ordering::SeqCst), 0);
        let (handle, task) = prepared.start(Vec::new()).unwrap();
        task.join().await.unwrap();
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        assert_eq!(
            handle.snapshot().window.data(),
            &Data::List(vec![Data::Int(7)].into())
        );
    }
}
