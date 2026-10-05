use std::{
    num::NonZeroUsize,
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};
use tokio::sync::{broadcast, mpsc, oneshot};
use wes_core::{Data, Primitive, Provenance, Shape, Value};
use wes_engine::{
    driver::*,
    graph::{NodeId, NodeState, OutputPort, OutputRef},
    runtime::*,
};
#[path = "driver/waits.rs"]
mod waits;

const SAFE: ExecutionTraits = ExecutionTraits {
    pure: false,
    repeatable: true,
    bounded: true,
};
fn value(n: i64) -> Value {
    Value::new(
        Shape::Primitive(Primitive::Int),
        Data::Int(n),
        Provenance::default(),
    )
    .unwrap()
}
fn slots(n: usize) -> NonZeroUsize {
    NonZeroUsize::new(n).unwrap()
}
struct Started {
    ticket: RunTicket<&'static str>,
    token: CancellationToken,
    finish: oneshot::Sender<Outcome>,
}
struct Controlled {
    started: mpsc::UnboundedSender<Started>,
}
impl Executor<&'static str> for Controlled {
    fn execute(
        &self,
        ticket: RunTicket<&'static str>,
        token: CancellationToken,
    ) -> ExecutionFuture {
        let (finish, receive) = oneshot::channel();
        self.started
            .send(Started {
                ticket,
                token,
                finish,
            })
            .unwrap();
        Box::pin(async move {
            receive
                .await
                .expect("test must release every entered executor")
                .into()
        })
    }
}
fn controlled(
    runtime: Runtime<&'static str>,
    limit: usize,
) -> (
    DriverHandle<&'static str>,
    DriverTask,
    mpsc::UnboundedReceiver<Started>,
) {
    let (started, receive) = mpsc::unbounded_channel();
    let (handle, task) = spawn(runtime, Arc::new(Controlled { started }), slots(limit)).unwrap();
    (handle, task, receive)
}
async fn add(
    handle: &DriverHandle<&'static str>,
    payload: &'static str,
    dependencies: Vec<OutputRef>,
) -> NodeId {
    let Reply::Added(node) = handle
        .command(Command::Add {
            payload,
            dependencies,
            traits: SAFE,
        })
        .await
        .unwrap()
    else {
        panic!("added")
    };
    node
}
async fn snapshot(handle: &DriverHandle<&'static str>) -> Snapshot<&'static str> {
    let Reply::Snapshot(snapshot) = handle.command(Command::Snapshot).await.unwrap() else {
        panic!("snapshot")
    };
    *snapshot
}
async fn terminal(
    events: &mut broadcast::Receiver<Observation>,
    node: &NodeId,
    state: NodeState,
) -> Observation {
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let event = events.recv().await.unwrap();
            if event.node == *node && event.state == state {
                return event;
            }
        }
    })
    .await
    .expect("expected terminal observation")
}
async fn stop(handle: DriverHandle<&'static str>, task: DriverTask) {
    handle.command(Command::Shutdown).await.unwrap();
    tokio::time::timeout(Duration::from_secs(5), task.join())
        .await
        .unwrap()
        .unwrap();
}

#[tokio::test]
async fn actual_workers_follow_graph_order_and_receive_captured_values() {
    let (handle, task, mut entered) = controlled(Runtime::new(), 4);
    let mut events = handle.subscribe().unwrap();
    let a = add(&handle, "producer", vec![]).await;
    let b = add(&handle, "consumer", vec![OutputRef::data(a.clone())]).await;
    handle.command(Command::Start).await.unwrap();
    let first = entered.recv().await.unwrap();
    assert_eq!(first.ticket.run.node(), &a);
    assert!(entered.try_recv().is_err());
    first.finish.send(Outcome::Produced(value(5))).unwrap();
    let second = entered.recv().await.unwrap();
    assert_eq!(second.ticket.run.node(), &b);
    assert_eq!(second.ticket.inputs[&a], value(5));
    second.finish.send(Outcome::Produced(value(6))).unwrap();
    handle.wait_idle().await.unwrap();
    assert_eq!(
        terminal(&mut events, &b, NodeState::Ready).await.value,
        Some(value(6))
    );
    assert_eq!(snapshot(&handle).await.values[&a], value(5));
    handle.command(Command::Forget(a.clone())).await.unwrap();
    let after_eviction = snapshot(&handle).await;
    assert!(!after_eviction.values.contains_key(&a));
    assert_eq!(after_eviction.actual_typings[&a].shape, *value(5).shape());
    stop(handle, task).await;
}

#[tokio::test]
async fn cancellation_is_prompt_but_idle_and_refresh_wait_for_physical_exit() {
    let (handle, task, mut entered) = controlled(Runtime::new(), 2);
    let mut events = handle.subscribe().unwrap();
    let a = add(&handle, "slow", vec![]).await;
    handle.command(Command::Start).await.unwrap();
    let first = entered.recv().await.unwrap();
    handle.command(Command::Cancel(a.clone())).await.unwrap();
    assert!(first.token.is_cancelled());
    let cancelled = terminal(&mut events, &a, NodeState::Cancelled).await;
    assert_eq!(cancelled.error.as_ref().unwrap().code(), "RUN003");
    assert!(
        tokio::time::timeout(Duration::from_millis(10), handle.wait_idle())
            .await
            .is_err()
    );
    assert!(matches!(
        handle.command(Command::Refresh(a.clone())).await,
        Err(DriverError::Runtime(RuntimeError::Busy(_)))
    ));
    assert_eq!(snapshot(&handle).await.runs[&a], *first.ticket.run.id());
    assert!(entered.try_recv().is_err());
    first.finish.send(Outcome::Produced(value(99))).unwrap();
    handle.wait_idle().await.unwrap();
    assert_eq!(snapshot(&handle).await.errors[&a], cancelled.error.unwrap());
    handle.command(Command::Refresh(a.clone())).await.unwrap();
    let second = entered.recv().await.unwrap();
    assert_ne!(second.ticket.run.id(), first.ticket.run.id());
    second.finish.send(Outcome::Produced(value(2))).unwrap();
    handle.wait_idle().await.unwrap();
    assert_eq!(snapshot(&handle).await.values[&a], value(2));
    stop(handle, task).await;
}

#[tokio::test]
async fn cancelled_queued_worker_never_calls_executor_or_consumes_the_next_run() {
    let (handle, task, mut entered) = controlled(Runtime::new(), 1);
    let a = add(&handle, "occupy slot", vec![]).await;
    handle.command(Command::Start).await.unwrap();
    let first = entered.recv().await.unwrap();
    assert_eq!(first.ticket.run.node(), &a);
    let b = add(&handle, "queued", vec![]).await;
    handle.command(Command::Start).await.unwrap();
    handle.command(Command::Cancel(b.clone())).await.unwrap();
    let old = snapshot(&handle).await.runs[&b].clone();
    handle.command(Command::Refresh(b.clone())).await.unwrap();
    let new = snapshot(&handle).await.runs[&b].clone();
    assert_ne!(old, new);
    first.finish.send(Outcome::Produced(value(1))).unwrap();
    let second = entered.recv().await.unwrap();
    assert_eq!(second.ticket.run.id(), &new);
    assert_eq!(second.ticket.run.node(), &b);
    second.finish.send(Outcome::Produced(value(2))).unwrap();
    handle.wait_idle().await.unwrap();
    assert!(entered.try_recv().is_err());
    stop(handle, task).await;
}

#[tokio::test(start_paused = true)]
async fn actual_timer_replacement_uses_run_start_and_preserves_timeout_identity() {
    let (handle, task, mut entered) = controlled(Runtime::new(), 1);
    let mut events = handle.subscribe().unwrap();
    let a = add(&handle, "slow", vec![]).await;
    handle
        .command(Command::Timeout {
            node: Some(a.clone()),
            budget: Duration::from_secs(10),
        })
        .await
        .unwrap();
    handle.command(Command::Start).await.unwrap();
    let first = entered.recv().await.unwrap();
    tokio::time::advance(Duration::from_secs(5)).await;
    handle
        .command(Command::Timeout {
            node: Some(a.clone()),
            budget: Duration::from_secs(20),
        })
        .await
        .unwrap();
    tokio::time::advance(Duration::from_secs(6)).await;
    assert_eq!(
        snapshot(&handle).await.graph.node(&a).unwrap().state(),
        NodeState::Running
    );
    tokio::time::advance(Duration::from_secs(9)).await;
    let event = terminal(&mut events, &a, NodeState::Cancelled).await;
    assert_eq!(event.error.unwrap().code(), "RUN002");
    assert!(first.token.is_cancelled());
    first.finish.send(Outcome::Produced(value(9))).unwrap();
    handle.wait_idle().await.unwrap();
    assert!(snapshot(&handle).await.values.is_empty());
    stop(handle, task).await;
}

#[tokio::test]
async fn cancellation_handler_runs_without_waiting_for_producer_lease_exit() {
    let (handle, task, mut entered) = controlled(Runtime::new(), 2);
    let a = add(&handle, "producer", vec![]).await;
    let b = add(
        &handle,
        "cancel handler",
        vec![OutputRef {
            node: a.clone(),
            port: OutputPort::Cancel,
        }],
    )
    .await;
    handle.command(Command::Start).await.unwrap();
    let first = entered.recv().await.unwrap();
    handle.command(Command::Cancel(a.clone())).await.unwrap();
    let second = entered.recv().await.unwrap();
    assert_eq!(second.ticket.run.node(), &b);
    assert_eq!(
        second.ticket.inputs[&a],
        snapshot(&handle).await.errors[&a].to_cancellation_value()
    );
    second.finish.send(Outcome::Produced(value(2))).unwrap();
    first.finish.send(Outcome::Produced(value(1))).unwrap();
    handle.wait_idle().await.unwrap();
    stop(handle, task).await;
}

struct Panics;
impl Executor<&'static str> for Panics {
    fn execute(&self, _: RunTicket<&'static str>, _: CancellationToken) -> ExecutionFuture {
        Box::pin(async { panic!("synthetic private host diagnostic") })
    }
}
#[tokio::test]
async fn executor_panic_is_observed_as_generic_failure_and_releases_the_lease() {
    let (handle, task) = spawn(Runtime::new(), Arc::new(Panics), slots(1)).unwrap();
    let mut events = handle.subscribe().unwrap();
    let a = add(&handle, "panic", vec![]).await;
    handle.command(Command::Start).await.unwrap();
    handle.wait_idle().await.unwrap();
    let error = terminal(&mut events, &a, NodeState::Failed)
        .await
        .error
        .unwrap();
    assert_eq!(error.code(), "RUN001");
    assert!(!error.message().contains("private"));
    assert!(snapshot(&handle).await.idle);
    stop(handle, task).await;
}

#[tokio::test]
async fn dropping_all_control_handles_requests_cancel_and_waits_for_existing_workers() {
    let (handle, task, mut entered) = controlled(Runtime::new(), 1);
    let a = add(&handle, "work", vec![]).await;
    handle.command(Command::Start).await.unwrap();
    let first = entered.recv().await.unwrap();
    let token = first.token.clone();
    drop(handle);
    tokio::time::timeout(Duration::from_secs(5), token.cancelled())
        .await
        .unwrap();
    first.finish.send(Outcome::Produced(value(1))).unwrap();
    tokio::time::timeout(Duration::from_secs(5), task.join())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(first.ticket.run.node(), &a);
}

#[tokio::test]
async fn shutdown_does_not_launch_handlers_and_closes_observation_delivery() {
    let (handle, task, mut entered) = controlled(Runtime::new(), 2);
    let mut events = handle.subscribe().unwrap();
    let a = add(&handle, "producer", vec![]).await;
    add(
        &handle,
        "cancel handler",
        vec![OutputRef {
            node: a,
            port: OutputPort::Cancel,
        }],
    )
    .await;
    handle.command(Command::Start).await.unwrap();
    let first = entered.recv().await.unwrap();
    handle.command(Command::Shutdown).await.unwrap();
    assert!(first.token.is_cancelled());
    first.finish.send(Outcome::Produced(value(1))).unwrap();
    task.join().await.unwrap();
    assert!(entered.try_recv().is_err());
    while events.try_recv().is_ok() {}
    assert_eq!(
        events.recv().await.unwrap_err(),
        broadcast::error::RecvError::Closed
    );
    assert!(matches!(handle.subscribe(), Err(DriverError::Stopped)));
}

struct Immediate {
    calls: AtomicUsize,
}
impl Executor<&'static str> for Immediate {
    fn execute(&self, _: RunTicket<&'static str>, _: CancellationToken) -> ExecutionFuture {
        self.calls.fetch_add(1, Ordering::SeqCst);
        Box::pin(async { Outcome::Produced(value(1)).into() })
    }
}
#[tokio::test]
async fn restored_runtime_does_not_execute_at_startup_even_with_reactive_policy() {
    let mut runtime = Runtime::new();
    runtime.set_default_policy(Policy::Reactive);
    let a = NodeId::new("id1000").unwrap();
    runtime
        .restore(
            a.clone(),
            "historical",
            [],
            SAFE,
            RestoredState::Stale,
            None,
        )
        .unwrap();
    let executor = Arc::new(Immediate {
        calls: AtomicUsize::new(0),
    });
    let (handle, task) = spawn(runtime, executor.clone(), slots(1)).unwrap();
    handle.command(Command::Start).await.unwrap();
    handle.wait_idle().await.unwrap();
    assert_eq!(executor.calls.load(Ordering::SeqCst), 0);
    handle.command(Command::Refresh(a)).await.unwrap();
    handle.wait_idle().await.unwrap();
    assert_eq!(executor.calls.load(Ordering::SeqCst), 1);
    stop(handle, task).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn parallel_completions_and_bounded_observation_lag_do_not_lose_runtime_state() {
    let executor = Arc::new(Immediate {
        calls: AtomicUsize::new(0),
    });
    let (handle, task) = spawn(Runtime::new(), executor.clone(), slots(8)).unwrap();
    let mut events = handle.subscribe().unwrap();
    for _ in 0..700 {
        add(&handle, "finite", vec![]).await;
    }
    handle.command(Command::Start).await.unwrap();
    tokio::time::timeout(Duration::from_secs(10), handle.wait_idle())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(executor.calls.load(Ordering::SeqCst), 700);
    let state = snapshot(&handle).await;
    assert_eq!(state.values.len(), 700);
    assert!(state.idle);
    assert!(matches!(
        events.recv().await,
        Err(broadcast::error::RecvError::Lagged(_))
    ));
    stop(handle, task).await;
}

#[tokio::test]
async fn blocking_adapter_keeps_lease_until_its_blocking_job_really_returns() {
    struct Blocking {
        entered: mpsc::UnboundedSender<CancellationToken>,
        release: Arc<std::sync::Barrier>,
    }
    impl Executor<&'static str> for Blocking {
        fn execute(&self, _: RunTicket<&'static str>, token: CancellationToken) -> ExecutionFuture {
            let entered = self.entered.clone();
            let release = self.release.clone();
            Box::pin(async move {
                tokio::task::spawn_blocking(move || {
                    entered.send(token).unwrap();
                    release.wait();
                    Outcome::Produced(value(1))
                })
                .await
                .unwrap()
                .into()
            })
        }
    }
    let (entered, mut receive) = mpsc::unbounded_channel();
    let release = Arc::new(std::sync::Barrier::new(2));
    let (handle, task) = spawn(
        Runtime::new(),
        Arc::new(Blocking {
            entered,
            release: release.clone(),
        }),
        slots(2),
    )
    .unwrap();
    let a = add(&handle, "blocking", vec![]).await;
    handle.command(Command::Start).await.unwrap();
    let token = receive.recv().await.unwrap();
    handle.command(Command::Cancel(a.clone())).await.unwrap();
    assert!(token.is_cancelled());
    assert!(
        tokio::time::timeout(Duration::from_millis(10), handle.wait_idle())
            .await
            .is_err()
    );
    assert_eq!(snapshot(&handle).await.executing, [a]);
    release.wait();
    handle.wait_idle().await.unwrap();
    assert!(snapshot(&handle).await.values.is_empty());
    stop(handle, task).await;
}

#[tokio::test]
async fn oversized_concurrency_is_a_configuration_error_not_a_panic() {
    let executor = Arc::new(Immediate {
        calls: AtomicUsize::new(0),
    });
    assert!(matches!(
        spawn(Runtime::new(), executor, slots(usize::MAX)),
        Err(DriverError::InvalidConcurrency)
    ));
}

#[tokio::test]
async fn adopting_an_already_started_runtime_is_rejected_without_losing_its_effects_silently() {
    let mut runtime = Runtime::new();
    runtime.add("started elsewhere", [], SAFE).unwrap();
    runtime.start(Duration::ZERO);
    let executor = Arc::new(Immediate {
        calls: AtomicUsize::new(0),
    });
    assert!(matches!(
        spawn(runtime, executor, slots(1)),
        Err(DriverError::ActiveRuntime)
    ));
}

#[path = "driver/conversations.rs"]
mod conversations;
#[path = "driver/streams.rs"]
mod streams;
