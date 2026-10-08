use super::*;
use tokio::sync::{mpsc, oneshot};
use wes_engine::{
    conversations::{ConversationIo, Input, InteractiveInvoker},
    tasks::{BoundTask, TaskExecutor},
};

struct Started {
    call: Call,
    io: ConversationIo,
    finish: oneshot::Sender<()>,
}
struct Conversation {
    entered: mpsc::UnboundedSender<Started>,
    result: i64,
}
impl InteractiveInvoker for Conversation {
    fn start(&self, call: Call, io: ConversationIo, _: CancellationToken) -> InvocationFuture {
        io.output.opened().unwrap();
        let result = self.result;
        let (finish, receive) = oneshot::channel();
        self.entered
            .send(Started { call, io, finish })
            .ok()
            .unwrap();
        Box::pin(async move {
            receive.await.unwrap();
            Ok(int(result))
        })
    }
}
fn register(providers: &mut Providers, result: i64) -> mpsc::UnboundedReceiver<Started> {
    let (entered, receive) = mpsc::unbounded_channel();
    providers.register_all_ports(
        metadata(),
        Arc::new(Fake {
            result: None,
            calls: Arc::default(),
        }),
        None,
        Some(Arc::new(Conversation { entered, result })),
    );
    receive
}
fn bind(providers: &Providers) -> BoundCall {
    providers
        .bind_interactive(invocation(
            providers,
            "@interactive catalog echo value:hello",
            &|_| None,
        ))
        .unwrap()
}

#[tokio::test]
async fn unscoped_interactive_registration_cannot_enter_the_provider() {
    let mut providers = Providers::new();
    let mut entered = register(&mut providers, 1);
    let result = CallExecutor::ephemeral()
        .execute_interactive(
            ticket(bind(&providers), IndexMap::new()),
            CancellationToken::new(),
        )
        .await;
    assert!(result.is_err());
    assert!(entered.try_recv().is_err());
}

#[tokio::test]
async fn bound_interactive_task_routes_driver_input_and_joins_its_final_result() {
    let mut providers =
        Providers::local(wes_engine::providers::LocalScope::new("fixture").unwrap());
    let mut entered = register(&mut providers, 9);
    let bound = BoundTask::Call(bind(&providers));
    let mut runtime = Runtime::new();
    let node = runtime.add(bound.clone(), [], bound.traits()).unwrap();
    let (handle, task) = driver::spawn(
        runtime,
        Arc::new(TaskExecutor::ephemeral()),
        NonZeroUsize::new(1).unwrap(),
    )
    .unwrap();
    let mut events = handle.subscribe_conversations().unwrap();
    handle.command(Command::Start).await.unwrap();
    let mut active = entered.recv().await.unwrap();
    let run = tokio::time::timeout(Duration::from_secs(3), async {
        loop {
            if let driver::ConversationEvent::Started(run) = events.recv().await.unwrap().as_ref() {
                break run.clone();
            }
        }
    })
    .await
    .unwrap();
    handle
        .input(node.clone(), run.id().clone(), b"answer")
        .await
        .unwrap();
    let Some(Input::Bytes(bytes)) = active.io.receive().await else {
        panic!("input")
    };
    assert_eq!(bytes, b"answer");
    active.finish.send(()).unwrap();
    handle.wait_idle().await.unwrap();
    let Reply::Snapshot(snapshot) = handle.command(Command::Snapshot).await.unwrap() else {
        panic!("snapshot")
    };
    assert_eq!(snapshot.values[&node], local_output(int(9)));
    handle.command(Command::Shutdown).await.unwrap();
    task.join().await.unwrap();
}

#[tokio::test]
async fn conversation_binding_checks_revision_port_and_call_kind_before_any_entry() {
    let mut providers =
        Providers::local(wes_engine::providers::LocalScope::new("fixture").unwrap());
    let _old = register(&mut providers, 1);
    let stale = bind(&providers).invocation().clone();
    let mut entered = register(&mut providers, 2);
    assert_eq!(
        providers.bind_interactive(stale).unwrap_err(),
        BindError::ProviderChanged
    );
    let call = bind(&providers).invocation().clone();
    assert_eq!(
        providers.bind_finite(call.clone()).unwrap_err(),
        BindError::NotFinite
    );
    assert_eq!(
        providers.bind_stream(call.clone()).unwrap_err(),
        BindError::NotStream
    );
    let mut forged = call;
    Arc::make_mut(&mut forged.capability).safety = Safety::Unsafe;
    assert_eq!(
        providers.bind_interactive(forged).unwrap_err(),
        BindError::ProviderChanged
    );
    let finite = invocation(&providers, "catalog echo value:hello", &|_| None);
    assert_eq!(
        providers.bind_interactive(finite.clone()).unwrap_err(),
        BindError::NotInteractive
    );
    let Err(report) = CallExecutor::ephemeral()
        .execute_interactive(
            ticket(providers.bind_finite(finite).unwrap(), IndexMap::new()),
            CancellationToken::new(),
        )
        .await
    else {
        panic!("finite refused")
    };
    assert!(matches!(report.outcome, Outcome::Failed(_)));
    assert!(matches!(
        execute(bind(&providers), IndexMap::new()).await,
        Outcome::Failed(_)
    ));
    assert!(
        CallExecutor::ephemeral()
            .execute_stream(
                ticket(bind(&providers), IndexMap::new()),
                CancellationToken::new()
            )
            .await
            .is_err()
    );
    assert!(entered.try_recv().is_err());
    super::register(&mut providers, None);
    assert_eq!(
        providers
            .bind_interactive(invocation(
                &providers,
                "@interactive catalog echo value:hello",
                &|_| None
            ))
            .unwrap_err(),
        BindError::MissingInteractivePort
    );
}

#[tokio::test]
async fn captured_conversations_survive_provider_replacement_and_dispatch_without_implicit_repeat()
{
    let mut providers =
        Providers::local(wes_engine::providers::LocalScope::new("fixture").unwrap());
    let mut old = register(&mut providers, 1);
    let first = bind(&providers);
    let mut new = register(&mut providers, 2);
    let second = bind(&providers);
    providers.unregister("catalog");
    for (bound, entered, expected) in [(first, &mut old, 1), (second, &mut new, 2)] {
        assert!(!bound.traits().repeatable);
        assert!(!bound.traits().bounded);
        let ticket = ticket(bound, IndexMap::new());
        let executor = TaskExecutor::ephemeral();
        let work = RunTicket {
            payload: BoundTask::Call(ticket.payload),
            run: ticket.run,
            inputs: ticket.inputs,
            input_origins: ticket.input_origins,
        };
        assert!(executor.interactive(&work.payload));
        assert!(!executor.streaming(&work.payload));
        let active = executor
            .execute_interactive(work, CancellationToken::new())
            .await
            .ok()
            .unwrap();
        let mut started = entered.recv().await.unwrap();
        active.handle.send(b"ephemeral answer").unwrap();
        let Some(Input::Bytes(bytes)) = started.io.receive().await else {
            panic!("input")
        };
        assert_eq!(bytes, b"ephemeral answer");
        started.finish.send(()).unwrap();
        assert_eq!(
            produced(active.completion.await.outcome).data(),
            &Data::Int(expected)
        );
    }
}

#[tokio::test]
async fn interactive_contracts_cancel_and_missing_inputs_precede_entry_and_final_value_carries_origins()
 {
    let mut providers =
        Providers::local(wes_engine::providers::LocalScope::new("fixture").unwrap());
    let mut entered = register(&mut providers, 1);
    let source = NodeId::new("source").unwrap();
    let mut call = invocation(
        &providers,
        "@interactive catalog echo value:$source",
        &|_| Some(OutputRef::data(source.clone())),
    );
    let mut types = ContractRegistry::new();
    types
        .load("types: {Positive: {base: Int, min: 1}}")
        .unwrap();
    call.guards
        .insert("value".into(), vec![types.resolve("Positive").unwrap()]);
    call.cautions.insert("unchecked:extra".into());
    let bound = providers.bind_interactive(call).unwrap();
    for inputs in [IndexMap::new(), [(source.clone(), int(0))].into()] {
        assert!(
            CallExecutor::ephemeral()
                .execute_interactive(ticket(bound.clone(), inputs), CancellationToken::new())
                .await
                .is_err()
        );
        assert!(entered.try_recv().is_err());
    }
    let token = CancellationToken::new();
    token.cancel();
    let Err(report) = CallExecutor::ephemeral()
        .execute_interactive(
            ticket(bound.clone(), [(source.clone(), int(3))].into()),
            token,
        )
        .await
    else {
        panic!("cancelled")
    };
    assert!(matches!(report.outcome, Outcome::Cancelled(_)));
    assert!(entered.try_recv().is_err());
    let input = int(3).with_provenance(
        Provenance::default()
            .with_fact("region", "test")
            .cautioned(["source-warning".into()]),
    );
    let active = CallExecutor::ephemeral()
        .execute_interactive(
            ticket(bound, [(source, input)].into()),
            CancellationToken::new(),
        )
        .await
        .ok()
        .unwrap();
    let started = entered.recv().await.unwrap();
    assert_eq!(
        started.call.arguments["value"].shape(),
        &Shape::Primitive(Primitive::Int)
    );
    started.finish.send(()).unwrap();
    let value = produced(active.completion.await.outcome);
    assert_eq!(value.provenance().fact("region"), Some("test"));
    assert_eq!(
        value.provenance().cautions(),
        &BTreeSet::from(["source-warning".into(), "unchecked:extra".into()])
    );
}
