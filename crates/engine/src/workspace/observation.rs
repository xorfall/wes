//! Native query arguments cross into an isolated scope without becoming source strings.
use super::*;
use crate::{
    graph::OutputRef,
    runtime::{ExecutionTraits, RestoredState},
    source::SourceInput,
};
use wes_core::Value;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Kind {
    Finite,
    Live,
}

impl Workspace {
    pub(crate) fn observation_workspace(
        &self,
        template: &str,
        input: Value,
        caller: &SourceInput,
    ) -> Result<(Self, SourceInput), WorkspaceError> {
        self.observation_workspace_mapped(template, None, input, caller)
    }
    pub(crate) fn observation_workspace_mapped(
        &self,
        template: &str,
        adapter: Option<&str>,
        input: Value,
        caller: &SourceInput,
    ) -> Result<(Self, SourceInput), WorkspaceError> {
        let invalid = |message| rejected("VIW003", Span::at(0), message);
        let definition = self
            .templates
            .snapshot()
            .get(template)
            .filter(|definition| {
                definition.parameters.len() == 1 && definition.parameters.contains("input")
            })
            .ok_or_else(|| {
                invalid(
                    "A view query needs a named template with exactly one typed input parameter",
                )
            })?;
        let contract = definition
            .contracts
            .get("input")
            .ok_or_else(|| invalid("View query input must have an explicit type"))?;
        if input.shape().contains_meta()
            || !input.data().is_storable_snapshot()
            || input.provenance().policy().is_private()
            || crate::value_size::value_charge(&input, 64 * 1024).is_none()
            || !input.shape().is_assignable_to(&contract.shape())
            || !contract.issues(input.data()).is_empty()
        {
            return Err(invalid(
                "View query input is private, over budget, or does not satisfy its parameter contract",
            ));
        }
        // Only a validated registry name enters source. The payload stays native and immutable.
        if !wes_language::binding_name(template) {
            return Err(invalid("Invalid query template name"));
        }
        if let Some(adapter) = adapter {
            if !wes_language::binding_name(adapter) || self.query_template(adapter).is_err() {
                return Err(invalid(
                    "A live query requires a typed calc adapter with one input",
                ));
            }
        }
        let mut child = self.sandbox_workspace()?;
        child.safe_observation = Some(if adapter.is_some() {
            Kind::Live
        } else {
            Kind::Finite
        });
        let id = NodeId::new(uuid::Uuid::new_v4().to_string()).expect("uuid");
        let traits = ExecutionTraits {
            pure: false,
            repeatable: true,
            bounded: true,
        };
        child.runtime.restore(
            id.clone(),
            BoundTask::Input(input.clone()),
            [],
            traits,
            RestoredState::Ready(input.clone()),
            None,
        )?;
        child.predictions.insert(
            id.clone(),
            Arc::new(wes_core::capability::Typing {
                shape: input.shape().clone(),
                provenance: input.provenance().clone(),
            }),
        );
        child
            .bindings
            .bind("viewInput", OutputRef::data(id), child.runtime.graph())
            .map_err(binding_error)?;
        let mut source = SourceInput::new(
            uuid::Uuid::new_v4().to_string(),
            match adapter {
                Some(adapter) => format!("{template} input:$viewInput > viewSource\n{adapter} input:$viewSource > viewResult"),
                None => format!("{template} input:$viewInput > viewResult"),
            },
        )
        .map_err(|_| invalid("View query source could not be created"))?
        .with_client(caller.client().into())
        .map_err(|_| invalid("View query client is unavailable"))?;
        if adapter.is_some() {
            source = source.with_reactive(true);
        }
        if caller.is_cooperative() {
            source = source.cooperative();
        }
        if let Some(environment) = caller.environments() {
            source = source
                .with_environments(environment.clone())
                .map_err(|_| invalid("View query environment is unavailable"))?;
        }
        Ok((child, source))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        driver::CancellationToken,
        providers::{Call, InvocationFuture, Invoker, LocalScope},
        session::{self, RecordingMode},
        type_sources::{TypeSourceError, TypeSourceReader},
    };
    use std::{
        num::NonZeroUsize,
        sync::atomic::{AtomicUsize, Ordering},
    };
    use wes_core::{
        Data, Primitive, Provenance, Shape,
        capability::{Capability, Parameter, ProviderDescription, Safety},
    };
    struct NoFiles;
    impl TypeSourceReader for NoFiles {
        fn read(&self, _: &str, _: usize) -> Result<String, TypeSourceError> {
            panic!("unexpected source read")
        }
    }
    struct Echo(Arc<AtomicUsize>);
    impl Invoker for Echo {
        fn invoke(&self, call: Call, cancellation: CancellationToken) -> InvocationFuture {
            self.0.fetch_add(1, Ordering::SeqCst);
            Box::pin(async move {
                if call.arguments["value"].data() == &Data::Int(99) {
                    cancellation.cancelled().await;
                }
                Ok(call.arguments["value"].clone())
            })
        }
    }
    fn define(workspace: &mut Workspace, source: &str) {
        let parsed =
            wes_language::parse(&wes_language::SourceText::new("query-definition", source));
        assert!(parsed.diagnostics.is_empty(), "{:?}", parsed.diagnostics);
        for statement in parsed.script.statements {
            let Preparation::Change(change) = workspace.prepare(&statement).unwrap() else {
                panic!("definition")
            };
            workspace.commit(change).unwrap();
        }
    }
    fn fixture() -> (Workspace, Arc<AtomicUsize>) {
        let mut workspace = Workspace::local(LocalScope::new("observation-fixture").unwrap());
        let calls = Arc::new(AtomicUsize::new(0));
        let mut capabilities = vec![];
        for (name, safety) in [("read", Safety::Safe), ("write", Safety::Unsafe)] {
            let mut capability = Capability::new([name], Shape::Primitive(Primitive::Int), safety);
            capability.parameters.push(Parameter::new(
                "value",
                Shape::Primitive(Primitive::Int),
                true,
            ));
            capabilities.push(capability);
        }
        workspace
            .register_provider(
                ProviderDescription::new("sensor", capabilities, vec![]).unwrap(),
                Arc::new(Echo(calls.clone())),
            )
            .unwrap();
        (workspace, calls)
    }
    fn caller() -> SourceInput {
        SourceInput::new("view-command".into(), ":help".into()).unwrap()
    }
    #[tokio::test]
    async fn native_input_is_typed_and_safe_query_reuses_normal_execution() {
        let (mut parent, calls) = fixture();
        define(
            &mut parent,
            ":def Observe(input: Int) -> Int as :calc { function read(v) { return call('sensor', ['read'], {value:v}); } return read(input); }",
        );
        let input = Value::new(
            Shape::Primitive(Primitive::Int),
            Data::Int(17),
            Provenance::default(),
        )
        .unwrap();
        let prepared =
            session::PreparedObservationQuery::capture(&parent, "Observe", input, &caller())
                .unwrap();
        assert_eq!(calls.load(Ordering::SeqCst), 0);
        let capacity =
            crate::driver::ExecutionCapacity::new(NonZeroUsize::new(1).unwrap()).unwrap();
        let query = prepared.start(Arc::new(NoFiles), capacity).await.unwrap();
        query.wait_idle().await.unwrap();
        assert_eq!(
            query.sample().await.unwrap().value.unwrap().data(),
            &Data::Int(17)
        );
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        assert!(parent.runtime().graph().is_empty());
        assert!(!query.close().await.unwrap());
    }
    #[tokio::test]
    async fn unsafe_hidden_branch_is_rejected_before_any_call_enters() {
        let (mut parent, calls) = fixture();
        define(
            &mut parent,
            ":def Observe(input: Int) -> Int as :calc { function mutate(v) { return call('sensor', ['write'], {value:v}); } if (input < 0) { return mutate(input); } return call('sensor', ['read'], {value:input}); }",
        );
        let input = Value::new(
            Shape::Primitive(Primitive::Int),
            Data::Int(17),
            Provenance::default(),
        )
        .unwrap();
        let (child, source) = parent
            .observation_workspace("Observe", input, &caller())
            .unwrap();
        let (handle, task) = session::spawn(
            child,
            RecordingMode::Ephemeral,
            Arc::new(NoFiles),
            NonZeroUsize::new(1).unwrap(),
        )
        .unwrap();
        assert!(
            matches!(handle.submit(source).await, Err(session::SessionError::AccessDenied(message)) if message.contains("SAFE"))
        );
        assert_eq!(calls.load(Ordering::SeqCst), 0);
        handle.shutdown().await.unwrap();
        task.join().await.unwrap();
    }
    #[tokio::test]
    async fn interval_option_does_not_round_trip_through_json_or_source() {
        let (mut parent, _) = fixture();
        define(
            &mut parent,
            ":def Identity(input: Option<Interval>) -> Option<Interval> as :calc pure { return input; }",
        );
        let range = wes_core::Interval::new(
            "2026-01-01T00:00:00.000000001Z".parse().unwrap(),
            "2026-01-01T00:00:01.000000002Z".parse().unwrap(),
        )
        .unwrap();
        let input = Value::new(
            Shape::Option(Box::new(Shape::Primitive(Primitive::Interval))),
            Data::Option(Some(Box::new(Data::Interval(range)))),
            Provenance::default(),
        )
        .unwrap();
        let (child, source) = parent
            .observation_workspace("Identity", input.clone(), &caller())
            .unwrap();
        assert!(!source.text().contains("2026"));
        let (handle, task) = session::spawn(
            child,
            RecordingMode::Ephemeral,
            Arc::new(NoFiles),
            NonZeroUsize::new(1).unwrap(),
        )
        .unwrap();
        handle.submit(source).await.unwrap();
        handle.wait_idle().await.unwrap();
        let state = handle.snapshot().await.unwrap();
        assert_eq!(
            state.execution.values[&state.names["viewResult"].node].data(),
            input.data()
        );
        handle.shutdown().await.unwrap();
        task.join().await.unwrap();
    }
    #[tokio::test]
    async fn query_scopes_share_operation_capacity_and_join_cancellation() {
        let (mut parent, calls) = fixture();
        define(
            &mut parent,
            ":def Observe(input: Int) as sensor read value:?input",
        );
        let capacity =
            crate::driver::ExecutionCapacity::new(NonZeroUsize::new(1).unwrap()).unwrap();
        let input = Value::new(
            Shape::Primitive(Primitive::Int),
            Data::Int(99),
            Provenance::default(),
        )
        .unwrap();
        let first = session::PreparedObservationQuery::capture(
            &parent,
            "Observe",
            input.clone(),
            &caller(),
        )
        .unwrap()
        .start(Arc::new(NoFiles), capacity.clone())
        .await
        .unwrap();
        let second =
            session::PreparedObservationQuery::capture(&parent, "Observe", input, &caller())
                .unwrap()
                .start(Arc::new(NoFiles), capacity.clone())
                .await
                .unwrap();
        tokio::time::timeout(std::time::Duration::from_secs(2), async {
            while calls.load(Ordering::SeqCst) == 0 {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        tokio::time::timeout(std::time::Duration::from_secs(2), first.close())
            .await
            .unwrap()
            .unwrap();
        tokio::time::timeout(std::time::Duration::from_secs(2), async {
            while calls.load(Ordering::SeqCst) < 2 {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        tokio::time::timeout(std::time::Duration::from_secs(2), second.close())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(capacity.subscribe().borrow().operations.used, 0);
    }
    #[tokio::test]
    async fn abandoned_query_closes_its_child_and_releases_the_shared_slot() {
        let (mut parent, calls) = fixture();
        define(
            &mut parent,
            ":def Observe(input: Int) as sensor read value:?input",
        );
        let capacity =
            crate::driver::ExecutionCapacity::new(NonZeroUsize::new(1).unwrap()).unwrap();
        let input = Value::new(
            Shape::Primitive(Primitive::Int),
            Data::Int(99),
            Provenance::default(),
        )
        .unwrap();
        let query =
            session::PreparedObservationQuery::capture(&parent, "Observe", input, &caller())
                .unwrap()
                .start(Arc::new(NoFiles), capacity.clone())
                .await
                .unwrap();
        tokio::time::timeout(std::time::Duration::from_secs(2), async {
            while calls.load(Ordering::SeqCst) == 0 {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        drop(query);
        tokio::time::timeout(std::time::Duration::from_secs(2), async {
            while capacity.subscribe().borrow().operations.used != 0 {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
    }
    #[test]
    fn query_argument_boundary_rejects_private_wrong_type_and_oversized_values() {
        let (mut parent, calls) = fixture();
        define(
            &mut parent,
            ":def Identity(input: Text) -> Text as :calc pure { return input; }",
        );
        for input in [
            Value::new(
                Shape::Primitive(Primitive::Int),
                Data::Int(1),
                Provenance::default(),
            )
            .unwrap(),
            Value::new(
                Shape::Primitive(Primitive::Text),
                Data::Text("x".repeat(65537).into()),
                Provenance::default(),
            )
            .unwrap(),
            Value::new(
                Shape::Primitive(Primitive::Text),
                Data::Text("secret".into()),
                Provenance::default().with_policy(&wes_core::flow::FlowPolicy::default().private()),
            )
            .unwrap(),
        ] {
            assert!(
                parent
                    .observation_workspace("Identity", input, &caller())
                    .is_err()
            );
        }
        assert_eq!(calls.load(Ordering::SeqCst), 0);
    }
    struct Feed {
        opened: tokio::sync::mpsc::UnboundedSender<(crate::streams::StreamSink, CancellationToken)>,
        calls: Arc<AtomicUsize>,
    }
    impl crate::streams::StreamingInvoker for Feed {
        fn subscribe(
            &self,
            _: Call,
            sink: crate::streams::StreamSink,
            token: CancellationToken,
        ) -> crate::streams::StreamFuture {
            self.calls.fetch_add(1, Ordering::SeqCst);
            let opened = self.opened.clone();
            Box::pin(async move {
                sink.opened().unwrap();
                opened.send((sink, token.clone())).unwrap();
                token.cancelled().await;
                Ok(())
            })
        }
    }
    fn live_fixture(
        safety: Safety,
    ) -> (
        Workspace,
        Arc<AtomicUsize>,
        tokio::sync::mpsc::UnboundedReceiver<(crate::streams::StreamSink, CancellationToken)>,
    ) {
        let (mut workspace, calls) = fixture();
        let (opened, arrivals) = tokio::sync::mpsc::unbounded_channel();
        let mut capability = Capability::new(["watch"], Shape::Primitive(Primitive::Int), safety);
        capability.streaming = true;
        capability.parameters.push(Parameter::new(
            "value",
            Shape::Primitive(Primitive::Int),
            true,
        ));
        workspace
            .register_provider_ports(
                ProviderDescription::new("feed", [capability], vec![]).unwrap(),
                Arc::new(Echo(calls.clone())),
                Some(Arc::new(Feed {
                    opened,
                    calls: calls.clone(),
                })),
            )
            .unwrap();
        define(
            &mut workspace,
            ":def Watch(input: Int) as feed watch value:?input",
        );
        define(
            &mut workspace,
            ":def Draw(input: List<Int>) -> Int as :calc pure { return length(input); }",
        );
        (workspace, calls, arrivals)
    }
    #[tokio::test]
    async fn live_query_owns_a_bounded_window_and_shared_stream_slot_until_joined_close() {
        let (parent, calls, mut arrivals) = live_fixture(Safety::Safe);
        let input = Value::new(
            Shape::Primitive(Primitive::Int),
            Data::Int(1),
            Provenance::default(),
        )
        .unwrap();
        let capacity =
            crate::driver::ExecutionCapacity::new(NonZeroUsize::new(1).unwrap()).unwrap();
        let query = session::PreparedObservationQuery::capture_live(
            &parent,
            "Watch",
            "Draw",
            input,
            &caller(),
        )
        .unwrap()
        .start(Arc::new(NoFiles), capacity.clone())
        .await
        .unwrap();
        let (sink, token) =
            tokio::time::timeout(std::time::Duration::from_secs(2), arrivals.recv())
                .await
                .unwrap()
                .unwrap();
        query.wait_idle().await.unwrap();
        assert!(
            query.live_active().await.unwrap(),
            "idle is not subscription completion"
        );
        assert_eq!(capacity.subscribe().borrow().streams.used, 1);
        for n in 0..600 {
            sink.send(
                Value::new(
                    Shape::Primitive(Primitive::Int),
                    Data::Int(n),
                    Provenance::default(),
                )
                .unwrap(),
            )
            .await
            .unwrap();
        }
        tokio::time::timeout(std::time::Duration::from_secs(3), async {
            loop {
                if query
                    .sample()
                    .await
                    .unwrap()
                    .value
                    .as_ref()
                    .is_some_and(|v| v.data() == &Data::Int(500))
                {
                    break;
                }
                tokio::time::sleep(std::time::Duration::from_millis(20)).await;
            }
        })
        .await
        .unwrap();
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        assert!(!query.close().await.unwrap());
        assert!(token.is_cancelled());
        assert_eq!(capacity.subscribe().borrow().streams.used, 0);
        assert_eq!(capacity.subscribe().borrow().operations.used, 0);
    }
    #[tokio::test]
    async fn live_query_rejects_unsafe_sources_wrong_adapters_and_finite_sources_before_execution()
    {
        for (safety, adapter, template) in [
            (Safety::Unsafe, "Draw", "Watch"),
            (Safety::Safe, "Wrong", "Watch"),
            (Safety::Safe, "UnsafeDraw", "Watch"),
            (Safety::Safe, "Draw", "Finite"),
        ] {
            let (mut parent, calls, _) = live_fixture(safety);
            define(
                &mut parent,
                ":def Wrong(input: List<Text>) -> Int as :calc pure { return length(input); }",
            );
            define(
                &mut parent,
                ":def UnsafeDraw(input: List<Int>) -> Int as :calc { return call('sensor',['write'],{value:length(input)}); }",
            );
            define(
                &mut parent,
                ":def Finite(input: Int) as sensor read value:?input",
            );
            let input = Value::new(
                Shape::Primitive(Primitive::Int),
                Data::Int(1),
                Provenance::default(),
            )
            .unwrap();
            let capacity =
                crate::driver::ExecutionCapacity::new(NonZeroUsize::new(1).unwrap()).unwrap();
            assert!(
                session::PreparedObservationQuery::capture_live(
                    &parent,
                    template,
                    adapter,
                    input,
                    &caller()
                )
                .unwrap()
                .start(Arc::new(NoFiles), capacity.clone())
                .await
                .is_err(),
                "{safety:?} {template} -> {adapter}"
            );
            assert_eq!(
                calls.load(Ordering::SeqCst),
                0,
                "whole batch rejection before provider entrance"
            );
            assert_eq!(capacity.subscribe().borrow().streams.used, 0);
        }
    }
}
