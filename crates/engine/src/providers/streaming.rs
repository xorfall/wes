//! Streaming validation/admission shares the finite boundary without finite recovery attempts.
use super::*;
use crate::streams;

pub(super) fn prepare_source(
    journal: Option<CallJournal>,
    ticket: RunTicket<BoundCall>,
    cancellation: CancellationToken,
) -> PreparationFuture {
    let policy = ticket.payload.failure_policy(&ticket.inputs);
    let work = async move {
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
        let attribution = bound
            .output_provenance(&carried)
            .cautioned(bound.invocation.cautions.iter().cloned());
        Ok(PreparedSource {
            bound,
            run,
            arguments,
            carried,
            attribution,
            cancellation,
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

pub(crate) type PreparationFuture =
    Pin<Box<dyn Future<Output = Result<PreparedSource, ExecutionReport>> + Send>>;

/// Validated local admission held before producer dispatch. Archive setup may occur here,
/// but a stored result cannot construct this capability or recreate external execution.
pub(crate) struct PreparedSource {
    bound: BoundCall,
    run: Run,
    arguments: IndexMap<String, Value>,
    carried: Provenance,
    attribution: Provenance,
    cancellation: CancellationToken,
}
impl PreparedSource {
    pub(crate) fn refusal(&self, message: String) -> ExecutionReport {
        Outcome::Failed(RuntimeCode::ExecutionFailed.error(message, None))
            .with_policy(self.attribution.policy())
            .into()
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
        streams::spawn_with_archives(
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
        )
        .map_err(|_| {
            refused(Outcome::Failed(RuntimeCode::ExecutionFailed.error(
                "The stream metadata or window limits are invalid.",
                None,
            )))
        })
    }
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
