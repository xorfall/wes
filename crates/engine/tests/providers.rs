use indexmap::IndexMap;
use std::{
    collections::BTreeSet,
    num::NonZeroUsize,
    sync::{Arc, Mutex},
    time::Duration,
};
use wes_core::{
    Data, Primitive, Provenance, Shape, Value,
    capability::{Capability, Parameter, ProviderDescription, Safety},
    contracts::ContractRegistry,
};
use wes_engine::{
    driver::{self, CancellationToken, Command, Executor, Reply},
    graph::{NodeId, NodeState, OutputPort, OutputRef},
    plan::{self, Guards, Invocation, Plan, Task},
    providers::*,
    runtime::*,
};
use wes_language::{Expression, SourceText, parse, resolve::resolve, templates::Templates};

fn int(n: i64) -> Value {
    Value::new(
        Shape::Primitive(Primitive::Int),
        Data::Int(n),
        Provenance::default(),
    )
    .unwrap()
}
fn local_output(value: Value) -> Value {
    let provenance = value
        .provenance()
        .clone()
        .with_policy(&wes_core::flow::FlowPolicy::default().from_origin("local:fixture"));
    value.with_provenance(provenance)
}
fn metadata() -> ProviderDescription {
    let mut echo = Capability::new(["echo"], Shape::Unknown, Safety::Safe);
    echo.parameters = vec![
        Parameter::new("value", Shape::Unknown, true),
        Parameter::new("extra", Shape::Unknown, false),
        Parameter::new("label", Shape::Primitive(Primitive::Text), false),
    ];
    ProviderDescription::new("catalog", [echo], vec![]).unwrap()
}
struct Fake {
    result: Option<Result<Value, InvocationError>>,
    calls: Arc<Mutex<Vec<Call>>>,
}
impl Invoker for Fake {
    fn invoke(&self, call: Call, _: CancellationToken) -> InvocationFuture {
        let result = self
            .result
            .clone()
            .unwrap_or_else(|| Ok(call.arguments["value"].clone()));
        self.calls.lock().unwrap().push(call);
        Box::pin(async move { result })
    }
}
fn register(
    providers: &mut Providers,
    result: Option<Result<Value, InvocationError>>,
) -> Arc<Mutex<Vec<Call>>> {
    let calls = Arc::new(Mutex::new(vec![]));
    providers.register(
        metadata(),
        Arc::new(Fake {
            result,
            calls: calls.clone(),
        }),
    );
    calls
}
fn invocation(
    providers: &Providers,
    source: &str,
    names: &impl Fn(&str) -> Option<OutputRef>,
) -> Invocation {
    let parsed = parse(&SourceText::new("test", source));
    assert!(parsed.diagnostics.is_empty());
    let statement = &parsed.script.statements[0];
    let Expression::Call(call) = &statement.expression else {
        panic!("call")
    };
    let resolution = resolve(call, providers.catalogue()).unwrap();
    let Plan::NewNode(node) = plan::plan(&resolution, statement, &Guards::new(), names).unwrap()
    else {
        panic!("node")
    };
    let Task::Invoke(invoke) = node.task else {
        panic!("invoke")
    };
    invoke
}
fn ticket(bound: BoundCall, inputs: IndexMap<NodeId, Value>) -> RunTicket<BoundCall> {
    let mut runtime = Runtime::new();
    let node = runtime.add(bound.clone(), [], bound.traits()).unwrap();
    let mut ticket = runtime
        .start(Duration::ZERO)
        .into_iter()
        .find_map(|effect| {
            if let Effect::Spawn(ticket) = effect {
                Some(ticket)
            } else {
                None
            }
        })
        .unwrap();
    assert_eq!(ticket.run.node(), &node);
    ticket.inputs = inputs;
    ticket
}
async fn execute(bound: BoundCall, inputs: IndexMap<NodeId, Value>) -> Outcome {
    CallExecutor::ephemeral()
        .execute(ticket(bound, inputs), CancellationToken::new())
        .await
        .outcome
}
fn produced(outcome: Outcome) -> Value {
    let Outcome::Produced(value) = outcome else {
        panic!("produced: {outcome:?}")
    };
    value
}

#[test]
fn local_scope_requires_a_valid_explicit_identity() {
    for identity in ["".to_owned(), " ".into(), "a\nb".into(), "x".repeat(241)] {
        assert!(LocalScope::new(&identity).is_err());
    }
    assert_eq!(
        LocalScope::new("fixture").unwrap().origin(),
        "local:fixture"
    );
}

#[tokio::test]
async fn registration_without_execution_scope_does_not_authorize_a_call() {
    let mut providers = Providers::new();
    let calls = register(&mut providers, None);
    let bound = providers
        .bind_finite(invocation(&providers, "catalog echo value:hello", &|_| {
            None
        }))
        .unwrap();
    assert!(matches!(
        execute(bound, IndexMap::new()).await,
        Outcome::Failed(_)
    ));
    assert!(calls.lock().unwrap().is_empty());
}

#[tokio::test]
async fn local_scope_is_carried_by_values_and_shared_only_by_explicit_identity() {
    let mut first = Providers::local(LocalScope::new("first").unwrap());
    register(&mut first, None);
    let first = first.clone();
    let bound = first
        .bind_finite(invocation(&first, "catalog echo value:hello", &|_| None))
        .unwrap();
    let value = produced(execute(bound, IndexMap::new()).await);
    assert_eq!(
        value.provenance().policy().origins(),
        &BTreeSet::from(["local:first".into()])
    );
    let source = NodeId::new("source").unwrap();
    for (identity, allowed) in [("first", true), ("second", false)] {
        // Fresh registry, as when an embedding owner reopens a workspace.
        let mut registry = Providers::local(LocalScope::new(identity).unwrap());
        let calls = register(&mut registry, None);
        let call = invocation(&registry, "catalog echo value:$source", &|_| {
            Some(OutputRef::data(source.clone()))
        });
        let bound = registry.bind_finite(call).unwrap();
        let outcome = execute(bound.clone(), [(source.clone(), value.clone())].into()).await;
        assert_eq!(matches!(outcome, Outcome::Produced(_)), allowed);
        assert_eq!(calls.lock().unwrap().len(), usize::from(allowed));
        let unknown = value.clone().with_provenance(
            value
                .provenance()
                .clone()
                .with_policy(&wes_core::flow::FlowPolicy::default().unknown()),
        );
        assert!(matches!(
            execute(bound, [(source.clone(), unknown)].into()).await,
            Outcome::Failed(_)
        ));
        assert_eq!(calls.lock().unwrap().len(), usize::from(allowed));
    }
}

#[tokio::test]
async fn bound_nodes_keep_their_original_provider_after_reimport_and_removal() {
    let mut providers =
        Providers::local(wes_engine::providers::LocalScope::new("fixture").unwrap());
    let old_calls = register(&mut providers, Some(Ok(int(1))));
    let old = providers
        .bind_finite(invocation(&providers, "catalog echo value:hello", &|_| {
            None
        }))
        .unwrap();
    let new_calls = register(&mut providers, Some(Ok(int(2))));
    let new = providers
        .bind_finite(invocation(&providers, "catalog echo value:hello", &|_| {
            None
        }))
        .unwrap();
    providers.unregister("catalog");
    assert!(providers.catalogue().is_empty());
    assert_eq!(
        produced(execute(old, IndexMap::new()).await),
        local_output(int(1))
    );
    assert_eq!(
        produced(execute(new, IndexMap::new()).await),
        local_output(int(2))
    );
    assert_eq!(old_calls.lock().unwrap().len(), 1);
    assert_eq!(new_calls.lock().unwrap().len(), 1);
}

#[test]
fn stale_or_forged_resolution_cannot_bind_to_a_new_invoker() {
    let mut providers =
        Providers::local(wes_engine::providers::LocalScope::new("fixture").unwrap());
    register(&mut providers, None);
    let old = invocation(&providers, "catalog echo value:hello", &|_| None);
    register(&mut providers, None);
    assert_eq!(
        providers.bind_finite(old).unwrap_err(),
        BindError::ProviderChanged
    );
    let mut forged = invocation(&providers, "catalog echo value:hello", &|_| None);
    Arc::make_mut(&mut forged.capability).safety = Safety::Unsafe;
    assert_eq!(
        providers.bind_finite(forged).unwrap_err(),
        BindError::ProviderChanged
    );
    let mut interactive = invocation(&providers, "catalog echo value:hello", &|_| None);
    interactive.interactive = true;
    assert_eq!(
        providers.bind_finite(interactive).unwrap_err(),
        BindError::NotFinite
    );
}

#[tokio::test]
async fn reference_contracts_validate_at_execution_and_never_convert_text() {
    for data in [Data::Int(0), Data::Text("3".into())] {
        let mut providers =
            Providers::local(wes_engine::providers::LocalScope::new("fixture").unwrap());
        let calls = register(&mut providers, None);
        let source = NodeId::new("source").unwrap();
        let mut invoke = invocation(&providers, "catalog echo value:$source", &|_| {
            Some(OutputRef::data(source.clone()))
        });
        let mut types = ContractRegistry::new();
        types
            .load("types: {Positive: {base: Int, min: 1}}")
            .unwrap();
        invoke
            .guards
            .insert("value".into(), vec![types.resolve("Positive").unwrap()]);
        let value = Value::new(Shape::Unknown, data, Provenance::default()).unwrap();
        let Outcome::Failed(error) = execute(
            providers.bind_finite(invoke).unwrap(),
            [(source, value)].into(),
        )
        .await
        else {
            panic!("failure")
        };
        assert_eq!(error.code(), "TYP005");
        assert_eq!(error.issues()[0].path, "/arguments/value");
        assert!(calls.lock().unwrap().is_empty());
    }
}

#[tokio::test]
async fn validated_reference_is_argument_local_and_shares_producer_data() {
    let mut providers =
        Providers::local(wes_engine::providers::LocalScope::new("fixture").unwrap());
    let calls = register(&mut providers, None);
    let source = NodeId::new("source").unwrap();
    let mut invoke = invocation(&providers, "catalog echo value:$source", &|_| {
        Some(OutputRef::data(source.clone()))
    });
    let mut types = ContractRegistry::new();
    types
        .load("types: {Positive: {base: Int, min: 1}}")
        .unwrap();
    invoke
        .guards
        .insert("value".into(), vec![types.resolve("Positive").unwrap()]);
    let original = Value::new(
        Shape::Unknown,
        Data::Int(3),
        Provenance::default().with_fact("region", "example"),
    )
    .unwrap();
    let result = produced(
        execute(
            providers.bind_finite(invoke).unwrap(),
            [(source, original.clone())].into(),
        )
        .await,
    );
    assert_eq!(original.shape(), &Shape::Unknown);
    assert_eq!(result.shape(), &Shape::Primitive(Primitive::Int));
    assert!(std::ptr::eq(original.data(), result.data()));
    assert_eq!(
        calls.lock().unwrap()[0].arguments["value"].shape(),
        &Shape::Primitive(Primitive::Int)
    );
}

#[tokio::test]
async fn result_attribution_joins_references_excludes_literals_and_preserves_cautions() {
    let mut providers =
        Providers::local(wes_engine::providers::LocalScope::new("fixture").unwrap());
    let own = Provenance::default()
        .with_fact("origin", "producer")
        .cautioned(["provider-warning".into()]);
    register(&mut providers, Some(Ok(int(7).with_provenance(own))));
    let a = NodeId::new("a").unwrap();
    let b = NodeId::new("b").unwrap();
    let mut invoke = invocation(
        &providers,
        "catalog echo value:$a extra:$b label:literal",
        &|name| {
            Some(OutputRef::data(if name == "a" {
                a.clone()
            } else {
                b.clone()
            }))
        },
    );
    invoke.cautions = BTreeSet::from(["unchecked:value".into()]);
    let first = Provenance::default()
        .with_fact("origin", "input")
        .with_fact("shared", "yes")
        .with_fact("differs", "a")
        .cautioned(["input-a".into()]);
    let second = Provenance::default()
        .with_fact("origin", "input")
        .with_fact("shared", "yes")
        .with_fact("differs", "b")
        .cautioned(["input-b".into()]);
    let result = produced(
        execute(
            providers.bind_finite(invoke).unwrap(),
            [
                (a, int(1).with_provenance(first)),
                (b, int(2).with_provenance(second)),
            ]
            .into(),
        )
        .await,
    );
    assert_eq!(result.provenance().fact("origin"), Some("producer"));
    assert_eq!(result.provenance().fact("shared"), Some("yes"));
    assert_eq!(result.provenance().fact("differs"), None);
    assert_eq!(
        result.provenance().cautions(),
        &BTreeSet::from([
            "input-a".into(),
            "input-b".into(),
            "provider-warning".into(),
            "unchecked:value".into()
        ])
    );
}

#[tokio::test]
async fn already_cancelled_validation_and_missing_capture_never_enter_provider() {
    let mut providers =
        Providers::local(wes_engine::providers::LocalScope::new("fixture").unwrap());
    let calls = register(&mut providers, None);
    let source = NodeId::new("source").unwrap();
    let bound = providers
        .bind_finite(invocation(
            &providers,
            "catalog echo value:$source",
            &|_| Some(OutputRef::data(source.clone())),
        ))
        .unwrap();
    let cancellation = CancellationToken::new();
    cancellation.cancel();
    assert!(matches!(
        CallExecutor::ephemeral()
            .execute(ticket(bound.clone(), IndexMap::new()), cancellation)
            .await
            .outcome,
        Outcome::Cancelled(_)
    ));
    assert!(matches!(
        execute(bound, IndexMap::new()).await,
        Outcome::Failed(_)
    ));
    assert!(calls.lock().unwrap().is_empty());
}

#[tokio::test]
async fn provider_errors_keep_their_identity_and_local_cancel_is_not_an_error_output() {
    let mut providers =
        Providers::local(wes_engine::providers::LocalScope::new("fixture").unwrap());
    let original = RuntimeCode::ExecutionFailed.error("synthetic provider failure", None);
    register(
        &mut providers,
        Some(Err(InvocationError::Failed(original.clone()))),
    );
    let bound = providers
        .bind_finite(invocation(&providers, "catalog echo value:hello", &|_| {
            None
        }))
        .unwrap();
    let Outcome::Failed(error) = execute(bound, IndexMap::new()).await else {
        panic!("failed")
    };
    assert_eq!(
        error,
        original.with_policy(&wes_core::flow::FlowPolicy::default().from_origin("local:fixture"))
    );
    register(&mut providers, Some(Err(InvocationError::Cancelled)));
    let bound = providers
        .bind_finite(invocation(&providers, "catalog echo value:hello", &|_| {
            None
        }))
        .unwrap();
    let mut runtime = Runtime::new();
    let node = runtime.add(bound.clone(), [], bound.traits()).unwrap();
    let (handle, task) = driver::spawn(
        runtime,
        Arc::new(CallExecutor::ephemeral()),
        NonZeroUsize::new(1).unwrap(),
    )
    .unwrap();
    handle.command(Command::Start).await.unwrap();
    handle.wait_idle().await.unwrap();
    let Reply::Snapshot(snapshot) = handle.command(Command::Snapshot).await.unwrap() else {
        panic!("snapshot")
    };
    assert_eq!(
        snapshot.graph.node(&node).unwrap().state(),
        NodeState::Cancelled
    );
    assert_eq!(snapshot.errors[&node].code(), "RUN003");
    assert!(snapshot.values.is_empty());
    handle.command(Command::Shutdown).await.unwrap();
    task.join().await.unwrap();
}

#[tokio::test]
async fn nested_template_reference_failure_routes_to_a_real_error_consumer() {
    let mut providers =
        Providers::local(wes_engine::providers::LocalScope::new("fixture").unwrap());
    let calls = register(&mut providers, None);
    let placeholder = providers
        .bind_finite(invocation(&providers, "catalog echo value:unused", &|_| {
            None
        }))
        .unwrap();
    let mut runtime = Runtime::new();
    let source = NodeId::new("source").unwrap();
    runtime
        .restore(
            source.clone(),
            placeholder.clone(),
            [],
            placeholder.traits(),
            RestoredState::Ready(int(0)),
            None,
        )
        .unwrap();
    let mut types = ContractRegistry::new();
    types
        .load("types: {Positive: {base: Int, min: 1}}")
        .unwrap();
    let mut templates = Templates::new();
    for definition in [
        ":def positive(value: Positive) as catalog echo value:?value",
        ":def outer(value: Unknown) as positive value:?value",
    ] {
        let Expression::Definition(definition) = parse(&SourceText::new("test", definition))
            .script
            .statements
            .remove(0)
            .expression
        else {
            panic!("definition")
        };
        templates.define(definition, &types).unwrap();
    }
    let statement = parse(&SourceText::new("test", "outer value:$source"))
        .script
        .statements
        .remove(0);
    let Expression::Call(call) = &statement.expression else {
        panic!("call")
    };
    let expanded = templates.expand(call, |_| false).unwrap();
    let resolution = resolve(&expanded.call, providers.catalogue()).unwrap();
    let Plan::NewNode(planned) = plan::plan(&resolution, &statement, &expanded.contracts, &|_| {
        Some(OutputRef::data(source.clone()))
    })
    .unwrap() else {
        panic!("plan")
    };
    let Task::Invoke(invoke) = planned.task else {
        panic!("invoke")
    };
    let bound = providers.bind_finite(invoke).unwrap();
    let checked = runtime
        .add(bound.clone(), bound.dependencies(), bound.traits())
        .unwrap();
    let consumer = providers
        .bind_finite(invocation(
            &providers,
            "catalog echo value:$checked::error",
            &|_| {
                Some(OutputRef {
                    node: checked.clone(),
                    port: OutputPort::Error,
                })
            },
        ))
        .unwrap();
    let handler = runtime
        .add(consumer.clone(), consumer.dependencies(), consumer.traits())
        .unwrap();
    let (handle, task) = driver::spawn(
        runtime,
        Arc::new(CallExecutor::ephemeral()),
        NonZeroUsize::new(2).unwrap(),
    )
    .unwrap();
    handle.command(Command::Start).await.unwrap();
    handle.wait_idle().await.unwrap();
    let Reply::Snapshot(snapshot) = handle.command(Command::Snapshot).await.unwrap() else {
        panic!("snapshot")
    };
    let failure = &snapshot.errors[&checked];
    assert_eq!(failure.code(), "TYP005");
    assert_eq!(snapshot.values[&handler], failure.to_value());
    assert_eq!(snapshot.values[&source], int(0));
    assert_eq!(calls.lock().unwrap().len(), 1); // only the error consumer reached a provider
    handle.command(Command::Shutdown).await.unwrap();
    task.join().await.unwrap();
}

#[path = "providers/interactive.rs"]
mod interactive;
#[path = "providers/streams.rs"]
mod streams;

#[tokio::test]
async fn field_arguments_are_selected_before_provider_validation_without_numeric_coercion() {
    use wes_core::RecordShape;
    for (field, expected_calls) in [
        (Some(Data::Text("value".into())), 1),
        (Some(Data::Int(7)), 0),
        (None, 0),
    ] {
        let mut providers =
            Providers::local(wes_engine::providers::LocalScope::new("fixture").unwrap());
        let calls = register(&mut providers, None);
        let source = NodeId::new("source").unwrap();
        let invoke = invocation(
            &providers,
            "catalog echo value:$source.id label:$source.id",
            &|name| {
                assert_eq!(name, "source");
                Some(OutputRef::data(source.clone()))
            },
        );
        let fields: IndexMap<_, _> = field
            .clone()
            .map(|data| ("id".into(), data))
            .into_iter()
            .collect();
        let shape = Shape::Record(
            RecordShape::new(
                "Item",
                field.map(|data| {
                    (
                        "id".into(),
                        match data {
                            Data::Int(_) => Shape::Primitive(Primitive::Int),
                            _ => Shape::Primitive(Primitive::Text),
                        },
                    )
                }),
            )
            .unwrap(),
        );
        let value = Value::new(
            shape,
            Data::Record(fields),
            Provenance::default().with_fact("source", "synthetic"),
        )
        .unwrap();
        let outcome = execute(
            providers.bind_finite(invoke).unwrap(),
            [(source, value)].into(),
        )
        .await;
        assert_eq!(calls.lock().unwrap().len(), expected_calls);
        if expected_calls == 1 {
            let result = produced(outcome);
            assert_eq!(result.data(), &Data::Text("value".into()));
            let calls = calls.lock().unwrap();
            assert_eq!(
                calls[0].arguments["label"].provenance().fact("source"),
                Some("synthetic")
            );
        } else {
            assert!(matches!(outcome, Outcome::Failed(_)));
        }
    }
}

#[test]
fn selected_trace_profile_is_bound_exactly_and_replacement_cannot_retarget_it() {
    struct Profiles;
    impl Invoker for Profiles {
        fn supports_trace(&self, profile: &str) -> bool {
            matches!(profile, "binary" | "grpc")
        }
        fn invoke(&self, _: Call, _: CancellationToken) -> InvocationFuture {
            panic!("binding never invokes")
        }
    }
    let mut providers =
        Providers::local(wes_engine::providers::LocalScope::new("fixture").unwrap());
    providers.register(metadata(), Arc::new(Profiles));
    let call = invocation(&providers, "@trace(grpc) catalog echo value:hello", &|_| {
        None
    });
    assert_eq!(call.trace_profile.as_deref(), Some("grpc"));
    let bound = providers.bind_finite(call.clone()).unwrap();
    for profile in ["http".to_owned(), String::new(), "a".repeat(65)] {
        let mut rejected = call.clone();
        rejected.trace_profile = Some(profile.into());
        assert_eq!(
            providers.bind_finite(rejected).unwrap_err(),
            BindError::UnsupportedTrace
        );
    }
    register(&mut providers, None);
    assert_eq!(bound.invocation().trace_profile.as_deref(), Some("grpc"));
    assert_eq!(
        providers.bind_finite(call).unwrap_err(),
        BindError::ProviderChanged
    );
    let current = invocation(
        &providers,
        "@trace(binary) catalog echo value:hello",
        &|_| None,
    );
    assert_eq!(
        providers.bind_finite(current).unwrap_err(),
        BindError::UnsupportedTrace
    );
    let plain = invocation(&providers, "catalog echo value:hello", &|_| None);
    assert!(plain.trace_profile.is_none());
    assert!(providers.bind_finite(plain).is_ok());
}

#[tokio::test]
async fn rejected_projections_never_enter_provider_and_preserve_structured_reasons() {
    use wes_core::RecordShape;
    use wes_engine::plan::InputProblem;
    let mut providers = Providers::local(LocalScope::new("projection-fixture").unwrap());
    let calls = register(&mut providers, None);
    let source = NodeId::new("source").unwrap();
    let bound = providers
        .bind_finite(invocation(
            &providers,
            "catalog echo value:$source.id",
            &|_| Some(OutputRef::data(source.clone())),
        ))
        .unwrap();
    let text = Data::Record([("id".into(), Data::Text("synthetic value".into()))].into());
    let generic = Shape::Record(RecordShape::new("Record", []).unwrap());
    let declared = Shape::Record(
        RecordShape::new("Row", [("id".into(), Shape::Primitive(Primitive::Text))]).unwrap(),
    );
    for (value, problem) in [
        (None, InputProblem::SourceUnavailable),
        (
            Some(Value::new(generic, text, Provenance::default()).unwrap()),
            InputProblem::FieldNotGuaranteed,
        ),
        (
            Some(
                Value::new(
                    Shape::Unknown,
                    Data::Record(Default::default()),
                    Provenance::default(),
                )
                .unwrap(),
            ),
            InputProblem::FieldMissing,
        ),
        (
            Some(
                Value::new(
                    declared,
                    Data::Record([("id".into(), Data::Int(1))].into()),
                    Provenance::default(),
                )
                .unwrap(),
            ),
            InputProblem::TypeMismatch,
        ),
    ] {
        let inputs = value.map(|v| (source.clone(), v)).into_iter().collect();
        let Outcome::Failed(error) = execute(bound.clone(), inputs).await else {
            panic!("failure expected");
        };
        assert_eq!(error.code(), "RUN004");
        assert_eq!(error.issues()[0].code, problem.code());
        assert_eq!(error.issues()[0].path, "/arguments/value/id");
        assert!(!error.message().contains("synthetic value"));
    }
    assert!(calls.lock().unwrap().is_empty());
}

#[tokio::test]
async fn structured_arguments_join_private_policy_and_refuse_large_or_missing_children_before_effects()
 {
    let mut providers = Providers::local(LocalScope::new("structured-fixture").unwrap());
    let calls = register(&mut providers, None);
    let secret = NodeId::new("secret").unwrap();
    let missing = NodeId::new("missing").unwrap();
    let names = |name: &str| match name {
        "secret" => Some(OutputRef::data(secret.clone())),
        "missing" => Some(OutputRef::data(missing.clone())),
        _ => None,
    };
    let private = int(7).with_provenance(
        Provenance::default().with_policy(&wes_core::flow::FlowPolicy::default().private()),
    );
    let bound = providers
        .bind_finite(invocation(
            &providers,
            "catalog echo value:{mode:ready, nested:[$secret]}",
            &names,
        ))
        .unwrap();
    assert_eq!(
        bound.dependencies().collect::<Vec<_>>(),
        [OutputRef::data(secret.clone())]
    );
    let Outcome::Produced(value) = execute(bound, [(secret.clone(), private.clone())].into()).await
    else {
        panic!("private result")
    };
    assert!(value.provenance().policy().is_private());
    assert_eq!(calls.lock().unwrap().len(), 1);
    let bound = providers
        .bind_finite(invocation(
            &providers,
            "catalog echo value:{sensitive:$secret, nested:[$missing]}",
            &names,
        ))
        .unwrap();
    let Outcome::Failed(error) = execute(bound, [(secret.clone(), private)].into()).await else {
        panic!("missing child")
    };
    assert!(error.policy().is_private());
    assert!(error.issues().is_empty());
    assert!(!error.message().contains("nested"));
    assert_eq!(calls.lock().unwrap().len(), 1);
    let bound = providers
        .bind_finite(invocation(
            &providers,
            "catalog echo value:{large:$secret}",
            &names,
        ))
        .unwrap();
    let large = Value::new(
        Shape::Primitive(Primitive::Bytes),
        Data::Bytes(vec![0; 130 * 1024].into()),
        Provenance::default(),
    )
    .unwrap();
    let Outcome::Failed(error) = execute(bound, [(secret.clone(), large)].into()).await else {
        panic!("oversized child")
    };
    assert_eq!(error.issues()[0].code, "INP005");
    assert_eq!(calls.lock().unwrap().len(), 1);
    let bound = providers
        .bind_finite(invocation(
            &providers,
            "catalog echo value:{nested:[$missing]}",
            &names,
        ))
        .unwrap();
    let Outcome::Failed(error) = execute(bound, IndexMap::new()).await else {
        panic!("missing root")
    };
    assert_eq!(error.issues()[0].path, "/arguments/value/nested/0");
    assert_eq!(error.issues()[0].code, "INP001");
}

#[tokio::test]
async fn separate_constructor_arguments_share_one_budget_before_provider_effects() {
    let mut providers = Providers::local(LocalScope::new("constructed-budget").unwrap());
    let calls = register(&mut providers, None);
    let source = NodeId::new("source").unwrap();
    let bound = providers
        .bind_finite(invocation(
            &providers,
            "catalog echo value:{first:$source} extra:{second:$source}",
            &|name| (name == "source").then(|| OutputRef::data(source.clone())),
        ))
        .unwrap();
    let payload = Value::new(
        Shape::Primitive(Primitive::Bytes),
        Data::Bytes(vec![0; 70 * 1024].into()),
        Provenance::default(),
    )
    .unwrap();
    let Outcome::Failed(error) = execute(bound, [(source, payload)].into()).await else {
        panic!("aggregate constructor refusal")
    };
    assert_eq!(error.issues()[0].code, "INP005");
    assert!(calls.lock().unwrap().is_empty());
}
