use super::*;
use wes_core::capability::{Capability, Safety};
use wes_engine::{
    providers::{Call, InvocationError},
    streams::{self, Limits, StreamFuture, StreamSink, StreamingInvoker},
};

struct StreamStarted {
    run: Run,
    sink: StreamSink,
    token: CancellationToken,
    finish: oneshot::Sender<Result<(), InvocationError>>,
}
struct StreamProvider(mpsc::UnboundedSender<StreamStarted>);
impl StreamingInvoker for StreamProvider {
    fn subscribe(&self, call: Call, sink: StreamSink, token: CancellationToken) -> StreamFuture {
        sink.push(value(0)).unwrap();
        sink.opened().unwrap();
        let (finish, receive) = oneshot::channel();
        self.0
            .send(StreamStarted {
                run: call.run,
                sink,
                token,
                finish,
            })
            .unwrap();
        // Deliberately wait for physical cleanup even after cancellation is requested.
        Box::pin(async move { receive.await.unwrap_or(Err(InvocationError::Cancelled)) })
    }
}
struct StreamExecutor {
    provider: Arc<StreamProvider>,
    finite: Arc<AtomicUsize>,
    wrong_run: Option<Run>,
}
impl Executor<&'static str> for StreamExecutor {
    fn streaming(&self, payload: &&'static str) -> bool {
        payload.starts_with("stream")
    }
    fn execute(&self, _: RunTicket<&'static str>, _: CancellationToken) -> ExecutionFuture {
        self.finite.fetch_add(1, Ordering::SeqCst);
        Box::pin(async { Outcome::Produced(value(42)).into() })
    }
    fn execute_stream(
        &self,
        ticket: RunTicket<&'static str>,
        token: CancellationToken,
    ) -> StreamExecutionFuture {
        let mut capability =
            Capability::new(["watch"], Shape::Primitive(Primitive::Int), Safety::Safe);
        capability.streaming = true;
        let call = Call {
            authority: Default::default(),
            run: self.wrong_run.clone().unwrap_or(ticket.run),
            capability: Arc::new(capability),
            arguments: Default::default(),
        };
        let provider = self.provider.clone();
        let wrong = self.wrong_run.is_some();
        Box::pin(async move {
            let (handle, task) = streams::spawn(
                call,
                provider,
                Provenance::default(),
                Limits::default(),
                token,
            )
            .map_err(|_| {
                ExecutionReport::from(Outcome::Failed(
                    RuntimeCode::ExecutionFailed.error("fixture setup failed", None),
                ))
            })?;
            if wrong {
                let mut updates = handle.subscribe();
                while updates.borrow_and_update().phase == streams::Phase::Opening {
                    if updates.changed().await.is_err() {
                        break;
                    }
                }
            }
            Ok((handle, task))
        })
    }
}
fn fixture(
    wrong_run: Option<Run>,
) -> (
    DriverHandle<&'static str>,
    DriverTask,
    mpsc::UnboundedReceiver<StreamStarted>,
    Arc<AtomicUsize>,
) {
    fixture_with_capacity(wrong_run, ExecutionCapacity::new(slots(1)).unwrap())
}
fn fixture_with_capacity(
    wrong_run: Option<Run>,
    capacity: ExecutionCapacity,
) -> (
    DriverHandle<&'static str>,
    DriverTask,
    mpsc::UnboundedReceiver<StreamStarted>,
    Arc<AtomicUsize>,
) {
    let (sent, receive) = mpsc::unbounded_channel();
    let finite = Arc::new(AtomicUsize::new(0));
    let executor = Arc::new(StreamExecutor {
        provider: Arc::new(StreamProvider(sent)),
        finite: finite.clone(),
        wrong_run,
    });
    let (handle, task) = spawn_with_capacity(Runtime::new(), executor, capacity).unwrap();
    (handle, task, receive, finite)
}
async fn seen(
    handle: &DriverHandle<&'static str>,
    predicate: impl Fn(&Snapshot<&'static str>) -> bool,
) -> Snapshot<&'static str> {
    tokio::time::timeout(Duration::from_secs(4), async {
        loop {
            let state = snapshot(handle).await;
            if predicate(&state) {
                return state;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap()
}
async fn started(entered: &mut mpsc::UnboundedReceiver<StreamStarted>) -> StreamStarted {
    tokio::time::timeout(Duration::from_secs(4), entered.recv())
        .await
        .unwrap()
        .unwrap()
}
#[tokio::test]
async fn open_stream_is_idle_and_releases_finite_capacity_but_its_final_window_is_still_joined() {
    let (handle, task, mut entered, finite) = fixture(None);
    let node = add(&handle, "stream", vec![]).await;
    handle.command(Command::Start).await.unwrap();
    let stream = started(&mut entered).await;
    let old = seen(&handle, |state| state.streaming.contains(&node)).await;
    tokio::time::timeout(Duration::from_secs(1), handle.wait_idle())
        .await
        .unwrap()
        .unwrap();
    let ordinary = add(&handle, "finite", vec![]).await;
    handle.command(Command::Start).await.unwrap();
    handle.wait_idle().await.unwrap();
    assert_eq!(finite.load(Ordering::SeqCst), 1);
    assert_eq!(
        snapshot(&handle).await.values.get(&ordinary),
        Some(&value(42))
    );
    stream.sink.push(value(1)).unwrap();
    stream.finish.send(Ok(())).unwrap();
    let final_ = seen(&handle, |state| state.streaming.is_empty() && state.idle).await;
    assert_eq!(
        final_.values[&node].data(),
        &Data::List(vec![Data::Int(0), Data::Int(1)])
    );
    assert_eq!(old.values[&node].data(), &Data::List(vec![Data::Int(0)]));
    handle.command(Command::Shutdown).await.unwrap();
    task.join().await.unwrap();
}
#[tokio::test]
async fn refresh_and_shutdown_wait_for_actual_stream_exit_without_accepting_old_callbacks() {
    let (handle, task, mut entered, _) = fixture(None);
    let node = add(&handle, "stream", vec![]).await;
    handle.command(Command::Start).await.unwrap();
    let old = started(&mut entered).await;
    seen(&handle, |state| state.streaming.contains(&node)).await;
    handle
        .command(Command::Refresh(node.clone()))
        .await
        .unwrap();
    assert!(old.token.is_cancelled());
    assert!(entered.try_recv().is_err());
    assert!(!snapshot(&handle).await.idle);
    assert!(old.sink.push(value(99)).is_err());
    old.finish.send(Ok(())).unwrap();
    let next = started(&mut entered).await;
    assert_ne!(old.run, next.run);
    assert_eq!(old.run.node(), next.run.node());
    seen(&handle, |state| state.streaming.contains(&node)).await;
    assert!(old.sink.push(value(100)).is_err());
    handle.command(Command::Shutdown).await.unwrap();
    let joined = tokio::spawn(task.join());
    assert!(next.token.is_cancelled());
    tokio::task::yield_now().await;
    assert!(!joined.is_finished());
    next.finish.send(Ok(())).unwrap();
    tokio::time::timeout(Duration::from_secs(3), joined)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
}
#[tokio::test]
async fn stream_capacity_is_reserved_before_entry_and_survives_cancelled_cleanup() {
    let capacity = ExecutionCapacity::with_stream_limit(slots(1), slots(2)).unwrap();
    let observed = capacity.subscribe();
    let (handle, task, mut entered, finite) = fixture_with_capacity(None, capacity);
    let mut streams = Vec::new();
    for count in 1..=2 {
        add(&handle, "stream", vec![]).await;
        handle.command(Command::Start).await.unwrap();
        streams.push(started(&mut entered).await);
        seen(&handle, |state| state.streaming.len() == count).await;
    }
    assert_eq!(observed.borrow().streams.used, 2);
    assert_eq!(observed.borrow().operations.used, 0);
    let denied = add(&handle, "stream", vec![]).await;
    handle.command(Command::Start).await.unwrap();
    handle.wait_idle().await.unwrap();
    assert!(
        snapshot(&handle).await.errors[&denied]
            .message()
            .contains("capacity")
    );
    assert!(entered.try_recv().is_err());
    add(&handle, "finite", vec![]).await;
    handle.command(Command::Start).await.unwrap();
    handle.wait_idle().await.unwrap();
    assert_eq!(finite.load(Ordering::SeqCst), 1);
    let first = streams.remove(0);
    handle
        .command(Command::Cancel(first.run.node().clone()))
        .await
        .unwrap();
    handle
        .command(Command::Refresh(denied.clone()))
        .await
        .unwrap();
    seen(&handle, |state| state.errors.contains_key(&denied)).await;
    assert!(entered.try_recv().is_err());
    assert_eq!(observed.borrow().streams.used, 2);
    first.finish.send(Ok(())).unwrap();
    handle.wait_idle().await.unwrap();
    handle.command(Command::Refresh(denied)).await.unwrap();
    streams.push(started(&mut entered).await);
    seen(&handle, |state| state.streaming.len() == 2).await;
    handle.command(Command::Shutdown).await.unwrap();
    for stream in streams {
        stream.finish.send(Ok(())).unwrap();
    }
    tokio::time::timeout(Duration::from_secs(4), task.join())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(observed.borrow().streams.used, 0);
    assert_eq!(observed.borrow().operations.used, 0);
}
#[tokio::test]
async fn an_explicit_stream_timeout_cancels_promptly_but_does_not_release_the_physical_lease() {
    let (handle, task, mut entered, _) = fixture(None);
    let node = add(&handle, "stream", vec![]).await;
    handle.command(Command::Start).await.unwrap();
    let stream = started(&mut entered).await;
    seen(&handle, |state| state.streaming.contains(&node)).await;
    handle
        .command(Command::Timeout {
            node: Some(node.clone()),
            budget: Duration::from_millis(60),
        })
        .await
        .unwrap();
    seen(&handle, |state| {
        state.graph.node(&node).unwrap().state() == NodeState::Cancelled
    })
    .await;
    assert!(stream.token.is_cancelled());
    assert!(!snapshot(&handle).await.idle);
    stream.finish.send(Ok(())).unwrap();
    handle.wait_idle().await.unwrap();
    assert_eq!(snapshot(&handle).await.errors[&node].code(), "RUN002");
    handle.command(Command::Shutdown).await.unwrap();
    task.join().await.unwrap();
}
#[tokio::test]
async fn mismatched_subscription_identity_is_closed_and_joined_before_failure_is_published() {
    let mut other = Runtime::new();
    other.add((), [], SAFE).unwrap();
    let run = other
        .start(Duration::ZERO)
        .into_iter()
        .find_map(|effect| match effect {
            Effect::Spawn(ticket) => Some(ticket.run),
            _ => None,
        })
        .unwrap();
    let (handle, task, mut entered, _) = fixture(Some(run));
    let node = add(&handle, "stream", vec![]).await;
    handle.command(Command::Start).await.unwrap();
    let held = started(&mut entered).await;
    tokio::time::timeout(Duration::from_secs(3), held.token.cancelled())
        .await
        .unwrap();
    assert!(!snapshot(&handle).await.idle);
    held.finish.send(Ok(())).unwrap();
    handle.wait_idle().await.unwrap();
    let state = snapshot(&handle).await;
    assert!(state.errors[&node].message().contains("different run"));
    assert!(!state.values.contains_key(&node));
    handle.command(Command::Shutdown).await.unwrap();
    task.join().await.unwrap();
}
#[tokio::test]
async fn adopting_an_open_idle_stream_runtime_is_rejected_until_its_lease_is_drained() {
    let mut runtime = Runtime::new();
    runtime.add("stream", [], SAFE).unwrap();
    let run = runtime
        .start(Duration::ZERO)
        .into_iter()
        .find_map(|effect| match effect {
            Effect::Spawn(ticket) => Some(ticket.run),
            _ => None,
        })
        .unwrap();
    runtime.enter_stream(&run);
    runtime
        .stream_window(&run, value(1), Duration::ZERO)
        .unwrap();
    assert!(runtime.is_idle());
    assert!(!runtime.is_drained());
    let (sent, _) = mpsc::unbounded_channel();
    let executor = Arc::new(StreamExecutor {
        provider: Arc::new(StreamProvider(sent)),
        finite: Arc::new(AtomicUsize::new(0)),
        wrong_run: None,
    });
    assert!(matches!(
        spawn(runtime, executor, slots(1)),
        Err(DriverError::ActiveRuntime)
    ));
}

#[tokio::test]
async fn source_readiness_is_bound_to_the_current_run_and_never_uses_a_retained_window() {
    let (handle, task, mut entered, finite) = fixture(None);
    let node = add(&handle, "stream", vec![]).await;
    handle.command(Command::Start).await.unwrap();
    let source = started(&mut entered).await;
    assert_eq!(
        handle
            .wait_source_ready(
                node.clone(),
                source.run.id().clone(),
                Duration::from_secs(2),
                CancellationToken::new()
            )
            .await,
        Ok(true)
    );
    let cancelled = CancellationToken::new();
    cancelled.cancel();
    assert_eq!(
        handle
            .wait_source_ready(
                node.clone(),
                source.run.id().clone(),
                Duration::from_secs(2),
                cancelled
            )
            .await,
        Err(DriverError::WaitCancelled)
    );
    source.finish.send(Ok(())).unwrap();
    seen(&handle, |s| s.idle && s.streaming.is_empty()).await;
    assert_eq!(
        handle
            .wait_source_ready(
                node.clone(),
                source.run.id().clone(),
                Duration::from_secs(2),
                CancellationToken::new()
            )
            .await,
        Err(DriverError::SourceClosed)
    );
    handle
        .command(Command::Refresh(node.clone()))
        .await
        .unwrap();
    let next = started(&mut entered).await;
    assert_eq!(
        handle
            .wait_source_ready(
                node,
                source.run.id().clone(),
                Duration::from_secs(2),
                CancellationToken::new()
            )
            .await,
        Err(DriverError::SourceChanged)
    );
    assert_eq!(finite.load(Ordering::SeqCst), 0);
    next.finish.send(Ok(())).unwrap();
    handle.command(Command::Shutdown).await.unwrap();
    task.join().await.unwrap();
}
