use super::*;
use tokio::sync::{mpsc, oneshot};
use wes_engine::streams::{Phase, StreamFuture, StreamHandle, StreamSink, StreamingInvoker};

pub(super) struct Started {
    pub call: Call,
    pub sink: StreamSink,
    pub finish: oneshot::Sender<()>,
}
pub(super) struct Stream {
    pub started: mpsc::UnboundedSender<Started>,
    pub first: i64,
}
impl StreamingInvoker for Stream {
    fn subscribe(&self, call: Call, sink: StreamSink, _: CancellationToken) -> StreamFuture {
        sink.push(int(self.first)).unwrap();
        sink.opened().unwrap();
        let (finish, end) = oneshot::channel();
        self.started.send(Started { call, sink, finish }).unwrap();
        Box::pin(async move {
            end.await.unwrap();
            Ok(())
        })
    }
}
fn register_stream(providers: &mut Providers, first: i64) -> mpsc::UnboundedReceiver<Started> {
    let (started, receive) = mpsc::unbounded_channel();
    let mut capability = Capability::new(["echo"], Shape::Primitive(Primitive::Int), Safety::Safe);
    capability.streaming = true;
    capability.parameters = vec![Parameter::new("value", Shape::Unknown, true)];
    providers.register_ports(
        ProviderDescription::new("catalog", [capability], vec![]).unwrap(),
        Arc::new(Fake {
            result: None,
            calls: Arc::default(),
        }),
        Some(Arc::new(Stream { started, first })),
    );
    receive
}
async fn open(handle: &StreamHandle) {
    let mut updates = handle.subscribe();
    tokio::time::timeout(Duration::from_secs(5), async {
        while updates.borrow_and_update().phase != Phase::Open {
            updates.changed().await.unwrap();
        }
    })
    .await
    .unwrap();
}
fn bind(providers: &Providers) -> BoundCall {
    providers
        .bind_stream(invocation(providers, "catalog echo value:hello", &|_| None))
        .unwrap()
}

#[tokio::test]
async fn unscoped_stream_registration_cannot_enter_the_provider() {
    let mut providers = Providers::new();
    let mut entered = register_stream(&mut providers, 1);
    let result = CallExecutor::ephemeral()
        .execute_stream(
            ticket(bind(&providers), IndexMap::new()),
            CancellationToken::new(),
        )
        .await;
    assert!(result.is_err());
    assert!(entered.try_recv().is_err());
}
#[test]
fn stream_binding_checks_metadata_revision_port_and_call_kind() {
    let mut providers =
        Providers::local(wes_engine::providers::LocalScope::new("fixture").unwrap());
    let _old = register_stream(&mut providers, 1);
    let stale = invocation(&providers, "catalog echo value:hello", &|_| None);
    let _new = register_stream(&mut providers, 2);
    assert_eq!(
        providers.bind_stream(stale).unwrap_err(),
        BindError::ProviderChanged
    );
    let call = invocation(&providers, "catalog echo value:hello", &|_| None);
    assert_eq!(
        providers.bind_finite(call.clone()).unwrap_err(),
        BindError::NotFinite
    );
    let mut forged = call.clone();
    Arc::make_mut(&mut forged.capability).safety = Safety::Unsafe;
    assert_eq!(
        providers.bind_stream(forged).unwrap_err(),
        BindError::ProviderChanged
    );
    let mut interactive = call;
    interactive.interactive = true;
    assert_eq!(
        providers.bind_stream(interactive).unwrap_err(),
        BindError::NotStream
    );
    let description = providers
        .catalogue()
        .provider("catalog")
        .unwrap()
        .as_ref()
        .clone();
    providers.register(
        description,
        Arc::new(Fake {
            result: None,
            calls: Arc::default(),
        }),
    );
    assert_eq!(
        providers
            .bind_stream(invocation(&providers, "catalog echo value:hello", &|_| {
                None
            }))
            .unwrap_err(),
        BindError::MissingStreamPort
    );
    register(&mut providers, None);
    assert_eq!(
        providers
            .bind_stream(invocation(&providers, "catalog echo value:hello", &|_| {
                None
            }))
            .unwrap_err(),
        BindError::NotStream
    );
}
#[tokio::test]
async fn bound_stream_retains_original_invoker_after_reimport_and_unregister() {
    let mut providers =
        Providers::local(wes_engine::providers::LocalScope::new("fixture").unwrap());
    let mut old = register_stream(&mut providers, 1);
    let bound_old = bind(&providers);
    let mut new = register_stream(&mut providers, 2);
    let bound_new = bind(&providers);
    providers.unregister("catalog");
    for (bound, receive, expected) in [(bound_old, &mut old, 1), (bound_new, &mut new, 2)] {
        let ticket = ticket(bound, IndexMap::new());
        let run = ticket.run.clone();
        let (handle, task) = CallExecutor::ephemeral()
            .execute_stream(ticket, CancellationToken::new())
            .await
            .unwrap_or_else(|_| panic!("open"));
        let started = receive.recv().await.unwrap();
        assert_eq!(started.call.run, run);
        started.finish.send(()).unwrap();
        task.join().await.unwrap();
        assert_eq!(handle.snapshot().phase, Phase::Ended);
        assert_eq!(
            handle.snapshot().window.data(),
            &Data::List(vec![Data::Int(expected)])
        );
    }
}
#[tokio::test]
async fn stream_contract_validation_and_missing_input_precede_provider_entry() {
    let mut providers =
        Providers::local(wes_engine::providers::LocalScope::new("fixture").unwrap());
    let mut entered = register_stream(&mut providers, 1);
    let source = NodeId::new("source").unwrap();
    let mut call = invocation(&providers, "catalog echo value:$source", &|_| {
        Some(OutputRef::data(source.clone()))
    });
    let mut types = ContractRegistry::new();
    types
        .load("types: {Positive: {base: Int, min: 1}}")
        .unwrap();
    call.guards
        .insert("value".into(), vec![types.resolve("Positive").unwrap()]);
    let bound = providers.bind_stream(call).unwrap();
    for inputs in [IndexMap::new(), [(source.clone(), int(0))].into()] {
        let Err(report) = CallExecutor::ephemeral()
            .execute_stream(ticket(bound.clone(), inputs), CancellationToken::new())
            .await
        else {
            panic!("denied")
        };
        assert!(matches!(report.outcome, Outcome::Failed(_)));
        assert!(entered.try_recv().is_err());
    }
    let token = CancellationToken::new();
    token.cancel();
    let Err(report) = CallExecutor::ephemeral()
        .execute_stream(ticket(bound, [(source, int(3))].into()), token)
        .await
    else {
        panic!("cancelled")
    };
    assert!(matches!(report.outcome, Outcome::Cancelled(_)));
    assert!(entered.try_recv().is_err());
}
#[tokio::test]
async fn every_stream_window_carries_validated_input_provenance_and_cautions() {
    let mut providers =
        Providers::local(wes_engine::providers::LocalScope::new("fixture").unwrap());
    let mut entered = register_stream(&mut providers, 1);
    let source = NodeId::new("source").unwrap();
    let mut call = invocation(&providers, "catalog echo value:$source", &|_| {
        Some(OutputRef::data(source.clone()))
    });
    let mut types = ContractRegistry::new();
    types
        .load("types: {Positive: {base: Int, min: 1}}")
        .unwrap();
    call.guards
        .insert("value".into(), vec![types.resolve("Positive").unwrap()]);
    call.cautions.insert("unchecked:extra".into());
    let input = Value::new(
        Shape::Unknown,
        Data::Int(3),
        Provenance::default()
            .with_fact("region", "test")
            .cautioned(["source-warning".into()]),
    )
    .unwrap();
    let (handle, task) = CallExecutor::ephemeral()
        .execute_stream(
            ticket(
                providers.bind_stream(call).unwrap(),
                [(source, input)].into(),
            ),
            CancellationToken::new(),
        )
        .await
        .unwrap_or_else(|_| panic!("open"));
    let started = entered.recv().await.unwrap();
    assert_eq!(
        started.call.arguments["value"].shape(),
        &Shape::Primitive(Primitive::Int)
    );
    open(&handle).await;
    let first = handle.snapshot();
    started.sink.push(int(2)).unwrap();
    started.finish.send(()).unwrap();
    task.join().await.unwrap();
    let final_ = handle.snapshot();
    for snapshot in [&first, &final_] {
        assert_eq!(snapshot.window.provenance().fact("region"), Some("test"));
        assert_eq!(
            snapshot.window.provenance().cautions(),
            &BTreeSet::from(["source-warning".into(), "unchecked:extra".into()])
        );
    }
    assert_eq!(
        final_.window.data(),
        &Data::List(vec![Data::Int(1), Data::Int(2)])
    );
}
#[tokio::test]
async fn direct_execution_cannot_cross_finite_and_stream_ports() {
    let mut providers =
        Providers::local(wes_engine::providers::LocalScope::new("fixture").unwrap());
    let mut entered = register_stream(&mut providers, 1);
    assert!(matches!(
        execute(bind(&providers), IndexMap::new()).await,
        Outcome::Failed(_)
    ));
    assert!(entered.try_recv().is_err());
    let calls = register(&mut providers, None);
    let finite = providers
        .bind_finite(invocation(&providers, "catalog echo value:hello", &|_| {
            None
        }))
        .unwrap();
    let Err(report) = CallExecutor::ephemeral()
        .execute_stream(ticket(finite, IndexMap::new()), CancellationToken::new())
        .await
    else {
        panic!("denied")
    };
    assert!(matches!(report.outcome, Outcome::Failed(_)));
    assert!(calls.lock().unwrap().is_empty());
}
#[tokio::test]
async fn task_dispatch_opens_bound_stream_through_generic_driver() {
    use wes_engine::tasks::{BoundTask, TaskExecutor};
    let mut providers =
        Providers::local(wes_engine::providers::LocalScope::new("fixture").unwrap());
    let mut entered = register_stream(&mut providers, 1);
    let bound = BoundTask::Call(bind(&providers));
    let mut runtime = Runtime::new();
    let node = runtime.add(bound.clone(), [], bound.traits()).unwrap();
    let (handle, task) = driver::spawn(
        runtime,
        Arc::new(TaskExecutor::ephemeral()),
        NonZeroUsize::new(1).unwrap(),
    )
    .unwrap();
    handle.command(Command::Start).await.unwrap();
    let started = entered.recv().await.unwrap();
    handle.wait_idle().await.unwrap();
    let Reply::Snapshot(snapshot) = handle.command(Command::Snapshot).await.unwrap() else {
        panic!("snapshot")
    };
    assert_eq!(snapshot.streaming, vec![node]);
    started.finish.send(()).unwrap();
    handle.command(Command::Shutdown).await.unwrap();
    task.join().await.unwrap();
}
