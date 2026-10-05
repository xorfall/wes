use super::*;
use wes_core::capability::{Capability, Safety};
use wes_engine::{
    conversations::{self, Channel, ConversationError, ConversationIo, Input, InteractiveInvoker},
    providers::{Call, InvocationError, InvocationFuture},
};

const TALK: ExecutionTraits = ExecutionTraits {
    pure: false,
    repeatable: false,
    bounded: false,
};
struct Talking {
    call: Call,
    io: ConversationIo,
    token: CancellationToken,
    finish: oneshot::Sender<Result<Value, InvocationError>>,
}
struct Conversation(mpsc::UnboundedSender<Talking>);
impl InteractiveInvoker for Conversation {
    fn start(&self, call: Call, io: ConversationIo, token: CancellationToken) -> InvocationFuture {
        io.output.write(Channel::Stdout, b"prompt: ").unwrap();
        io.output.opened().unwrap();
        let (finish, receive) = oneshot::channel();
        self.0
            .send(Talking {
                call,
                io,
                token,
                finish,
            })
            .ok()
            .unwrap();
        Box::pin(async move { receive.await.unwrap() })
    }
}
struct Execution {
    talk: mpsc::UnboundedSender<Talking>,
    finite: mpsc::UnboundedSender<Started>,
}
impl Executor<&'static str> for Execution {
    fn interactive(&self, payload: &&'static str) -> bool {
        *payload != "finite"
    }
    fn execute(
        &self,
        ticket: RunTicket<&'static str>,
        token: CancellationToken,
    ) -> ExecutionFuture {
        let (finish, receive) = oneshot::channel();
        self.finite
            .send(Started {
                ticket,
                token,
                finish,
            })
            .ok()
            .unwrap();
        Box::pin(async move { receive.await.unwrap().into() })
    }
    fn execute_interactive(
        &self,
        ticket: RunTicket<&'static str>,
        token: CancellationToken,
    ) -> InteractiveExecutionFuture {
        let provider = Arc::new(Conversation(self.talk.clone()));
        Box::pin(async move {
            let run = if ticket.payload == "wrong" {
                let mut different = Runtime::new();
                different.add((), [], TALK).unwrap();
                different
                    .start(Duration::ZERO)
                    .into_iter()
                    .find_map(|effect| match effect {
                        Effect::Spawn(ticket) => Some(ticket.run),
                        _ => None,
                    })
                    .unwrap()
            } else {
                ticket.run
            };
            let call = Call {
                authority: Default::default(),
                run,
                capability: Arc::new(Capability::new(["talk"], Shape::Unknown, Safety::Unsafe)),
                arguments: Default::default(),
            };
            let (handle, task) = conversations::spawn(call, provider, 128, token).unwrap();
            if ticket.payload == "wrong" {
                // Exercise a wrong handoff with work already physically entered. Cancelling an
                // earlier handoff is also valid and may prevent the provider from starting at all.
                let mut changes = handle.subscribe();
                while changes.borrow_and_update().phase == conversations::Phase::Opening {
                    changes.changed().await.unwrap();
                }
            }
            Ok(InteractiveExecution {
                handle,
                completion: Box::pin(async move {
                    match task.join().await.unwrap() {
                        Ok(value) => Outcome::Produced(value),
                        Err(InvocationError::Failed(error)) => Outcome::Failed(error),
                        Err(InvocationError::Cancelled) => {
                            Outcome::Cancelled(RuntimeCode::Cancelled.error("cancelled", None))
                        }
                    }
                    .into()
                }),
            })
        })
    }
}
fn driver(
    runtime: Runtime<&'static str>,
    limit: usize,
) -> (
    DriverHandle<&'static str>,
    DriverTask,
    mpsc::UnboundedReceiver<Talking>,
    mpsc::UnboundedReceiver<Started>,
) {
    let (talk, talks) = mpsc::unbounded_channel();
    let (finite, finites) = mpsc::unbounded_channel();
    let (handle, task) =
        spawn(runtime, Arc::new(Execution { talk, finite }), slots(limit)).unwrap();
    (handle, task, talks, finites)
}
async fn started(events: &mut broadcast::Receiver<Arc<ConversationEvent>>) -> Run {
    tokio::time::timeout(Duration::from_secs(3), async {
        loop {
            if let ConversationEvent::Started(run) = events.recv().await.unwrap().as_ref() {
                break run.clone();
            }
        }
    })
    .await
    .unwrap()
}
async fn output(
    events: &mut broadcast::Receiver<Arc<ConversationEvent>>,
) -> Arc<conversations::Output> {
    tokio::time::timeout(Duration::from_secs(3), async {
        loop {
            if let ConversationEvent::Output { batch, .. } = events.recv().await.unwrap().as_ref() {
                break batch.clone();
            }
        }
    })
    .await
    .unwrap()
}

#[tokio::test]
async fn conversation_stays_running_and_busy_routes_ephemeral_input_and_flushes_before_terminal() {
    let mut runtime = Runtime::new();
    let node = runtime.add("talk", [], TALK).unwrap();
    let (handle, task, mut talks, _) = driver(runtime, 1);
    let mut events = handle.subscribe_conversations().unwrap();
    handle.command(Command::Start).await.unwrap();
    let mut active = talks.recv().await.unwrap();
    let run = started(&mut events).await;
    assert_eq!(run, active.call.run);
    assert_eq!(output(&mut events).await.text, "prompt: ");
    let state = snapshot(&handle).await;
    assert_eq!(state.graph.node(&node).unwrap().state(), NodeState::Running);
    assert!(!state.idle);
    assert!(state.values.is_empty());
    assert!(
        tokio::time::timeout(Duration::from_millis(20), handle.wait_idle())
            .await
            .is_err()
    );
    assert!(matches!(
        handle
            .input(
                node.clone(),
                run.id().clone(),
                &vec![0; conversations::input_bytes() + 1]
            )
            .await,
        Err(DriverError::Conversation(ConversationError::Capacity))
    ));
    handle
        .input(node.clone(), run.id().clone(), b"answer")
        .await
        .unwrap();
    let Some(Input::Bytes(bytes)) = active.io.receive().await else {
        panic!("input")
    };
    assert_eq!(bytes, b"answer");
    handle.eof(node.clone(), run.id().clone()).await.unwrap();
    assert!(matches!(active.io.receive().await, Some(Input::Eof)));
    active
        .io
        .output
        .write(Channel::Stdout, b"tail\xf0\x9f")
        .unwrap();
    active.finish.send(Ok(value(7))).unwrap();
    assert_eq!(output(&mut events).await.text, "tail�");
    let ended = events.recv().await.unwrap();
    assert!(matches!(ended.as_ref(), ConversationEvent::Ended(ended) if *ended == run));
    handle.wait_idle().await.unwrap();
    assert_eq!(snapshot(&handle).await.values[&node], value(7));
    stop(handle, task).await;
}

#[tokio::test]
async fn cancellation_revokes_input_immediately_and_retains_capacity_until_cleanup() {
    let mut runtime = Runtime::new();
    let node = runtime.add("talk", [], TALK).unwrap();
    let (handle, task, mut talks, mut finites) = driver(runtime, 1);
    let mut events = handle.subscribe_conversations().unwrap();
    handle.command(Command::Start).await.unwrap();
    let active = talks.recv().await.unwrap();
    let run = started(&mut events).await;
    let finite = add(&handle, "finite", vec![]).await;
    handle.command(Command::Start).await.unwrap();
    handle.command(Command::Cancel(node.clone())).await.unwrap();
    assert!(
        handle
            .input(node.clone(), run.id().clone(), b"late")
            .await
            .is_err()
    );
    active.token.cancelled().await;
    assert!(finites.try_recv().is_err());
    assert!(!snapshot(&handle).await.idle);
    active.finish.send(Ok(value(99))).unwrap();
    let next = finites.recv().await.unwrap();
    assert_eq!(next.ticket.run.node(), &finite);
    next.finish.send(Outcome::Produced(value(3))).unwrap();
    handle.wait_idle().await.unwrap();
    let state = snapshot(&handle).await;
    assert_eq!(
        state.graph.node(&node).unwrap().state(),
        NodeState::Cancelled
    );
    assert!(!state.values.contains_key(&node));
    assert_eq!(state.values[&finite], value(3));
    stop(handle, task).await;
}

#[tokio::test]
async fn refreshed_node_refuses_input_and_callbacks_from_previous_run() {
    let mut runtime = Runtime::new();
    let node = runtime.add("talk", [], TALK).unwrap();
    let (handle, task, mut talks, _) = driver(runtime, 1);
    let mut events = handle.subscribe_conversations().unwrap();
    handle.command(Command::Start).await.unwrap();
    let old = talks.recv().await.unwrap();
    let old_run = started(&mut events).await;
    old.finish.send(Ok(value(1))).unwrap();
    handle.wait_idle().await.unwrap();
    handle
        .command(Command::Refresh(node.clone()))
        .await
        .unwrap();
    let mut new = talks.recv().await.unwrap();
    let new_run = started(&mut events).await;
    assert_ne!(old_run.id(), new_run.id());
    assert!(
        handle
            .input(node.clone(), old_run.id().clone(), b"wrong answer")
            .await
            .is_err()
    );
    assert_eq!(
        old.io.output.write(Channel::Stdout, b"old output"),
        Err(ConversationError::Closed)
    );
    handle
        .input(node.clone(), new_run.id().clone(), b"new answer")
        .await
        .unwrap();
    let Some(Input::Bytes(bytes)) = new.io.receive().await else {
        panic!("new input")
    };
    assert_eq!(bytes, b"new answer");
    new.finish.send(Ok(value(2))).unwrap();
    handle.wait_idle().await.unwrap();
    assert_eq!(snapshot(&handle).await.values[&node], value(2));
    stop(handle, task).await;
}

#[tokio::test(start_paused = true)]
async fn human_wait_ignores_default_timeout_but_explicit_deadline_cancels_and_joins() {
    let mut runtime = Runtime::new();
    runtime
        .set_default_timeout(Duration::from_millis(1))
        .unwrap();
    let node = runtime.add("talk", [], TALK).unwrap();
    let (handle, task, mut talks, _) = driver(runtime, 1);
    let mut events = handle.subscribe_conversations().unwrap();
    handle.command(Command::Start).await.unwrap();
    let active = talks.recv().await.unwrap();
    started(&mut events).await;
    tokio::time::advance(Duration::from_secs(60)).await;
    assert_eq!(
        snapshot(&handle).await.graph.node(&node).unwrap().state(),
        NodeState::Running
    );
    assert!(!active.token.is_cancelled());
    let mut states = handle.subscribe().unwrap();
    handle
        .command(Command::Timeout {
            node: Some(node.clone()),
            budget: Duration::from_secs(1),
        })
        .await
        .unwrap();
    let cancelled = terminal(&mut states, &node, NodeState::Cancelled).await;
    assert_eq!(cancelled.error.unwrap().code(), "RUN002");
    active.token.cancelled().await;
    assert!(!snapshot(&handle).await.idle);
    active.finish.send(Err(InvocationError::Cancelled)).unwrap();
    handle.wait_idle().await.unwrap();
    stop(handle, task).await;
}

#[tokio::test]
async fn conversation_capacity_is_bounded_before_entry_and_shutdown_joins_every_owner() {
    let mut runtime = Runtime::new();
    for _ in 0..33 {
        runtime.add("talk", [], TALK).unwrap();
    }
    let (handle, task, mut talks, _) = driver(runtime, 33);
    let mut states = handle.subscribe().unwrap();
    handle.command(Command::Start).await.unwrap();
    let mut active = Vec::new();
    for _ in 0..32 {
        active.push(talks.recv().await.unwrap());
    }
    let failed = tokio::time::timeout(Duration::from_secs(3), async {
        loop {
            let state = states.recv().await.unwrap();
            if state.state == NodeState::Failed {
                break state;
            }
        }
    })
    .await
    .unwrap();
    assert!(failed.error.unwrap().message().contains("capacity"));
    assert!(talks.try_recv().is_err());
    handle.command(Command::Shutdown).await.unwrap();
    let mut joined = Box::pin(task.join());
    assert!(
        tokio::time::timeout(Duration::from_millis(20), &mut joined)
            .await
            .is_err()
    );
    for active in active {
        active.token.cancelled().await;
        active.finish.send(Err(InvocationError::Cancelled)).unwrap();
    }
    tokio::time::timeout(Duration::from_secs(3), joined)
        .await
        .unwrap()
        .unwrap();
}

#[tokio::test]
async fn misbound_conversation_is_cancelled_and_joined_before_lease_release() {
    let mut runtime = Runtime::new();
    let node = runtime.add("wrong", [], TALK).unwrap();
    let (handle, task, mut talks, mut finites) = driver(runtime, 1);
    handle.command(Command::Start).await.unwrap();
    let active = talks.recv().await.unwrap();
    active.token.cancelled().await;
    add(&handle, "finite", vec![]).await;
    handle.command(Command::Start).await.unwrap();
    assert!(!snapshot(&handle).await.idle);
    assert!(finites.try_recv().is_err());
    active.finish.send(Err(InvocationError::Cancelled)).unwrap();
    let finite = finites.recv().await.unwrap();
    finite.finish.send(Outcome::Produced(value(4))).unwrap();
    handle.wait_idle().await.unwrap();
    assert!(
        snapshot(&handle).await.errors[&node]
            .message()
            .contains("different run")
    );
    stop(handle, task).await;
}

#[tokio::test(start_paused = true)]
async fn slow_output_observers_report_lag_without_blocking_execution_or_inventing_replay() {
    let mut runtime = Runtime::new();
    runtime.add("talk", [], TALK).unwrap();
    let (handle, task, mut talks, _) = driver(runtime, 1);
    let mut fast = handle.subscribe_conversations().unwrap();
    let mut slow = handle.subscribe_conversations().unwrap();
    handle.command(Command::Start).await.unwrap();
    let active = talks.recv().await.unwrap();
    started(&mut fast).await;
    assert_eq!(output(&mut fast).await.text, "prompt: ");
    for n in 0..40 {
        active
            .io
            .output
            .write(Channel::Stdout, n.to_string().as_bytes())
            .unwrap();
        assert_eq!(output(&mut fast).await.text, n.to_string());
    }
    assert!(matches!(
        slow.recv().await,
        Err(broadcast::error::RecvError::Lagged(_))
    ));
    active.finish.send(Ok(value(7))).unwrap();
    handle.wait_idle().await.unwrap();
    stop(handle, task).await;
}
